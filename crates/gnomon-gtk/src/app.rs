//! Application setup: styling, layer-shell placement, and interaction.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::geom::{self, Margins};
use crate::{pin, window};

const APP_ID: &str = "com.gnomon.Gnomon";
/// Initial window size, and the size used to place the panel before the first
/// allocation makes a real measurement available.
const DEFAULT_SIZE: (i32, i32) = (300, 150);
const STYLE: &str = include_str!("style.css");
/// Frames to wait for an allocation before assuming the request was ignored.
/// Without this, one unacknowledged request would freeze resizing for good.
const IN_FLIGHT_TIMEOUT_FRAMES: u32 = 8;

/// Everything the gestures and the signal handler need to share.
struct Panel {
    win: adw::ApplicationWindow,
    grip: gtk::DrawingArea,
    pinned: Cell<bool>,
    margins: Cell<Margins>,
    /// Margins at the moment a drag began.
    drag_origin: Cell<Margins>,
    /// Window size at the moment a resize drag began.
    resize_origin: Cell<(i32, i32)>,
    /// Set when a left-drag began inside the grip, so the move gesture stands
    /// down for the whole sequence rather than only at drag-begin.
    drag_ignored: Cell<bool>,
    /// Pointer is somewhere over the panel.
    hovered: Cell<bool>,
    /// Size the drag currently wants. Updated freely, applied at most once per
    /// frame by the tick callback.
    target: Cell<Option<(i32, i32)>>,
    /// A request handed to the compositor that has not yet come back as an
    /// allocation. While set, no new request is issued.
    in_flight: Cell<Option<(i32, i32)>>,
    /// Frames elapsed since `in_flight` was set, for the stuck-state timeout.
    in_flight_frames: Cell<u32>,
    /// Most recent request, for comparing against an arriving allocation.
    last_requested: Cell<Option<(i32, i32)>>,
    /// Most recent size the probe actually reported.
    last_allocated: Cell<Option<(i32, i32)>>,
    /// Phase label for the trace.
    resize_phase: Cell<&'static str>,
    monitor: Cell<(i32, i32)>,
    /// The size the window was configured with, valid before allocation.
    default_size: (i32, i32),
    layered: bool,
}

impl Panel {
    fn panel_size(&self) -> (i32, i32) {
        (self.win.width().max(1), self.win.height().max(1))
    }

    /// Push the stored margins onto the layer surface.
    ///
    /// `size` and `size_source` exist for the diagnostics: knowing *which*
    /// number fed the calculation is the whole point of the probe.
    fn apply_margins(&self, phase: &str, size: (i32, i32), size_source: &str) {
        if self.layered {
            let m = self.margins.get();
            self.win.set_margin(Edge::Left, m.left);
            self.win.set_margin(Edge::Top, m.top);
        }
        self.debug_geom(phase, size, size_source);
    }

    /// Issue at most one size request, called once per frame.
    ///
    /// Returns without acting unless there is a target, nothing is in flight,
    /// and the target actually differs from what is already on screen.
    fn tick_resize(self: &Rc<Self>) {
        if self.pinned.get() || !self.layered {
            return;
        }

        // Stuck-state timeout. A request that never produces an allocation
        // change — because the compositor clamped it, or because the size it
        // settled on happens to equal the previous one — would otherwise leave
        // `in_flight` set forever and freeze resizing permanently.
        if self.in_flight.get().is_some() {
            let frames = self.in_flight_frames.get() + 1;
            self.in_flight_frames.set(frames);
            if frames < IN_FLIGHT_TIMEOUT_FRAMES {
                return;
            }
            self.debug_resize_timeout();
            self.in_flight.set(None);
            self.in_flight_frames.set(0);
        }

        let Some(target) = self.target.get() else {
            return;
        };

        // Equality escape: never request a size the surface already has, which
        // is the case most likely to produce no allocation at all.
        if self.last_allocated.get() == Some(target) {
            self.target.set(None);
            return;
        }

        self.in_flight.set(Some(target));
        self.in_flight_frames.set(0);
        self.last_requested.set(Some(target));
        self.win.set_default_size(target.0, target.1);
        self.debug_resize_request(target);
    }

    /// An allocation arrived: the surface has genuinely changed size.
    ///
    /// This is the only place `in_flight` is cleared on the success path, and
    /// the only honest place to measure the settled size — an idle callback
    /// runs before the compositor's configure lands and reads a stale value,
    /// which is what produced the bogus mismatch markers.
    fn on_allocation(self: &Rc<Self>, width: i32, height: i32) {
        let allocated = (width, height);
        self.last_allocated.set(Some(allocated));
        self.in_flight.set(None);
        self.in_flight_frames.set(0);

        if self.target.get() == Some(allocated) {
            self.target.set(None);
        }

        self.debug_resize_allocated(allocated);
    }

    fn debug_resize_request(&self, requested: (i32, i32)) {
        if std::env::var_os("GNOMON_DEBUG_GEOM").is_none() {
            return;
        }
        eprintln!(
            "gnomon geom[{}]: requested={}x{}",
            self.resize_phase.get(),
            requested.0,
            requested.1,
        );
    }

    /// Compare only against the MOST RECENT request. Comparing against a
    /// superseded one is what generated the false mismatch markers.
    fn debug_resize_allocated(&self, allocated: (i32, i32)) {
        if std::env::var_os("GNOMON_DEBUG_GEOM").is_none() {
            return;
        }
        let Some(requested) = self.last_requested.get() else {
            return;
        };
        eprintln!(
            "gnomon geom[{}-allocated]: requested={}x{} allocated={}x{}{}",
            self.resize_phase.get(),
            requested.0,
            requested.1,
            allocated.0,
            allocated.1,
            if allocated == requested {
                ""
            } else {
                "  <-- differs from the latest request"
            },
        );
    }

    fn debug_resize_timeout(&self) {
        if std::env::var_os("GNOMON_DEBUG_GEOM").is_none() {
            return;
        }
        if let Some(req) = self.in_flight.get() {
            eprintln!(
                "gnomon geom[{}-timeout]: no allocation for {}x{} after {} frames, releasing",
                self.resize_phase.get(),
                req.0,
                req.1,
                IN_FLIGHT_TIMEOUT_FRAMES,
            );
        }
    }

    /// Unconditional geometry trace, behind `GNOMON_DEBUG_GEOM`.
    fn debug_geom(&self, phase: &str, size: (i32, i32), size_source: &str) {
        if std::env::var_os("GNOMON_DEBUG_GEOM").is_none() {
            return;
        }
        let m = self.margins.get();
        let mon = self.monitor.get();
        eprintln!(
            "gnomon geom[{phase}]: monitor={}x{} panel={}x{} (source: {}) \
margin_left={} margin_top={} anchors={} pinned={}",
            mon.0,
            mon.1,
            size.0,
            size.1,
            size_source,
            m.left,
            m.top,
            if self.layered { "Top+Left" } else { "n/a (toplevel)" },
            self.pinned.get(),
        );
    }
}

/// Run the GUI. `toplevel` skips layer-shell entirely — a debugging escape
/// hatch for compositors where the layer surface misbehaves.
pub fn run(toplevel: bool) -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();

    app.connect_startup(|_| load_css());
    app.connect_activate(move |app| build_window(app, toplevel));
    app.connect_shutdown(|_| pin::remove_pid_file());

    // Our own flags are already parsed; do not hand them to GTK.
    app.run_with_args::<&str>(&[])
}

fn load_css() {
    let provider = gtk::CssProvider::new();

    // A dropped declaration is otherwise silent, which is exactly how the
    // transparent-panel defect stayed invisible.
    provider.connect_parsing_error(|_, section, error| {
        eprintln!("gnomon: CSS error at {section}: {error}");
    });

    provider.load_from_string(STYLE);

    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );
    }
}

fn build_window(app: &adw::Application, toplevel: bool) {
    // Before any other thread exists, so they all inherit the blocked mask.
    let (sig_tx, sig_rx) = async_channel::unbounded::<()>();
    pin::watch_sigusr1(sig_tx);

    let win = adw::ApplicationWindow::builder()
        .application(app)
        .default_width(DEFAULT_SIZE.0)
        .default_height(DEFAULT_SIZE.1)
        .resizable(true)
        .title("gnomon")
        .build();

    // A layer surface negotiates from the default size; the size request must
    // never become a floor under it.
    win.set_size_request(1, 1);

    if !toplevel {
        // Scopes the transparency rule, so --toplevel keeps a solid window.
        win.add_css_class("gnomon-layer");

        // All of this must happen before the window is realized.
        win.init_layer_shell();
        win.set_layer(Layer::Overlay);
        // Anchored Top+Left so the margins are an absolute position.
        win.set_anchor(Edge::Top, true);
        win.set_anchor(Edge::Left, true);
        win.set_anchor(Edge::Right, false);
        win.set_anchor(Edge::Bottom, false);
        win.set_margin(Edge::Top, geom::EDGE_GAP);
        win.set_margin(Edge::Left, geom::EDGE_GAP);
        // Never take focus, in either pin state. Never reserve space.
        win.set_keyboard_mode(KeyboardMode::None);
        win.set_exclusive_zone(0);
    }

    let content = window::build();
    win.set_content(Some(&content.overlay));

    let panel = Rc::new(Panel {
        win: win.clone(),
        grip: content.grip.clone(),
        pinned: Cell::new(false),
        margins: Cell::new(Margins {
            left: geom::EDGE_GAP,
            top: geom::EDGE_GAP,
        }),
        drag_origin: Cell::new(Margins {
            left: geom::EDGE_GAP,
            top: geom::EDGE_GAP,
        }),
        resize_origin: Cell::new(DEFAULT_SIZE),
        drag_ignored: Cell::new(false),
        hovered: Cell::new(false),
        target: Cell::new(None),
        in_flight: Cell::new(None),
        in_flight_frames: Cell::new(0),
        last_requested: Cell::new(None),
        last_allocated: Cell::new(None),
        resize_phase: Cell::new("resize"),
        monitor: Cell::new((0, 0)),
        default_size: DEFAULT_SIZE,
        layered: !toplevel,
    });

    // One request per frame, decided by the flow control in tick_resize.
    {
        let panel = panel.clone();
        win.add_tick_callback(move |_, _| {
            panel.tick_resize();
            glib::ControlFlow::Continue
        });
    }

    // The probe's resize signal is the real "the surface changed size" event.
    {
        let panel = panel.clone();
        content.probe.connect_resize(move |_, width, height| {
            panel.on_allocation(width, height);
        });
    }

    wire_drag(&panel, &content.root);
    if !toplevel {
        // In toplevel mode the compositor provides resize handles; ours would
        // only fight it by pinning a minimum size.
        wire_grip(&panel, content.set_resizing.clone());
        // Independent of the grip: right-drag resizes whether or not the grip
        // is currently revealed.
        wire_right_button_resize(&panel, &content.root, content.set_resizing.clone());
    }
    wire_hover(&panel, &content.overlay);
    update_grip_visibility(&panel);
    wire_signal(&panel, sig_rx);

    {
        let panel = panel.clone();
        win.connect_realize(move |win| {
            place_initially(&panel);
            watch_surface(&panel, win);
        });
    }

    pin::write_pid_file();
    win.present();
}

/// Monitor geometry, and the initial top-right position derived from it.
///
/// The monitor is found from the realized surface, which is the only way to
/// learn which output the compositor actually put us on.
fn place_initially(panel: &Rc<Panel>) {
    let monitor = monitor_size(&panel.win);
    panel.monitor.set(monitor);

    if !panel.layered {
        return;
    }

    // Deliberately NOT panel_size(): at realize the window has not been
    // allocated, so its width reads 0 and the panel lands off-screen right.
    let size = panel.default_size;

    let (margins, source) = if monitor.0 > 0 && monitor.1 > 0 {
        (
            geom::initial_margins(size, monitor),
            "configured default size",
        )
    } else {
        // Monitor unknown: do not compute an offset at all.
        (
            Margins {
                left: geom::EDGE_GAP,
                top: geom::EDGE_GAP,
            },
            "monitor unknown, no offset computed",
        )
    };

    panel.margins.set(margins);
    panel.apply_margins("realize", size, source);
}

/// Ask the display which monitor holds our surface, then read its geometry.
fn monitor_size(win: &adw::ApplicationWindow) -> (i32, i32) {
    let Some(display) = gdk::Display::default() else {
        return (0, 0);
    };

    let monitor = win
        .surface()
        .and_then(|surface| display.monitor_at_surface(&surface))
        // Fall back to the first monitor the display lists.
        .or_else(|| {
            display
                .monitors()
                .item(0)
                .and_then(|obj| obj.downcast::<gdk::Monitor>().ok())
        });

    match monitor {
        Some(m) => {
            let geo = m.geometry();
            (geo.width(), geo.height())
        }
        None => (0, 0),
    }
}

/// Re-apply the input region whenever the surface is laid out again.
///
/// A move or a resize produces a new configuration and drops the region, so
/// applying it once at startup is not enough.
fn watch_surface(panel: &Rc<Panel>, win: &adw::ApplicationWindow) {
    pin::apply_input_region(win, panel.pinned.get());

    let Some(surface) = win.surface() else {
        return;
    };

    let panel = panel.clone();
    surface.connect_layout(move |_, _, _| {
        pin::apply_input_region(&panel.win, panel.pinned.get());
    });
}

/// Drag the panel by its body. Inert while pinned.
fn wire_drag(panel: &Rc<Panel>, root: &gtk::Box) {
    let drag = gtk::GestureDrag::new();

    {
        let panel = panel.clone();
        let root_for_bounds = root.clone();
        drag.connect_drag_begin(move |gesture, x, y| {
            // Belt: refuse a sequence that began on the grip. The grip also
            // claims it (braces) — either alone can lose to propagation order.
            if in_grip(&panel.grip, &root_for_bounds, x, y) {
                panel.drag_ignored.set(true);
                gesture.set_state(gtk::EventSequenceState::Denied);
                return;
            }
            panel.drag_ignored.set(false);
            panel.drag_origin.set(panel.margins.get());
        });
    }

    {
        let panel = panel.clone();
        drag.connect_drag_update(move |_, dx, dy| {
            if panel.drag_ignored.get() || panel.pinned.get() || !panel.layered {
                return;
            }
            let origin = panel.drag_origin.get();
            let clamped = geom::clamp_margins(
                origin.left + dx as i32,
                origin.top + dy as i32,
                panel.panel_size(),
                panel.monitor.get(),
            );
            panel.margins.set(clamped);
            panel.apply_margins("drag", panel.panel_size(), "allocated");
        });
    }

    {
        let panel = panel.clone();
        drag.connect_drag_end(move |_, _, _| {
            if panel.drag_ignored.get() || panel.pinned.get() || !panel.layered {
                return;
            }
            let m = panel.margins.get();
            let snapped = geom::snap_margins(m.left, m.top, panel.panel_size(), panel.monitor.get());
            panel.margins.set(snapped);
            panel.apply_margins("snap", panel.panel_size(), "allocated");
        });
    }

    root.add_controller(drag);
}

/// Reveal the grip on hover, hide it on leave.
fn wire_hover(panel: &Rc<Panel>, overlay: &gtk::Overlay) {
    let motion = gtk::EventControllerMotion::new();

    {
        let panel = panel.clone();
        motion.connect_enter(move |_, _, _| {
            panel.hovered.set(true);
            update_grip_visibility(&panel);
        });
    }

    {
        let panel = panel.clone();
        motion.connect_leave(move |_| {
            panel.hovered.set(false);
            update_grip_visibility(&panel);
        });
    }

    overlay.add_controller(motion);
}

/// The single place that decides whether the grip is on screen.
///
/// The two hard conditions are evaluated FIRST and combined into `interactive`;
/// hover is only ANDed on afterwards. Hover can therefore never resurrect the
/// grip while pinned or in toplevel mode — it can only reveal a grip that is
/// already permitted. Every path that changes any of the three inputs calls
/// this, so there is no second place for the rule to drift.
fn update_grip_visibility(panel: &Rc<Panel>) {
    let interactive = panel.layered && !panel.pinned.get();
    panel.grip.set_visible(interactive && panel.hovered.get());
}

/// Is this point, in `origin`'s coordinate space, inside the grip?
///
/// `compute_bounds` translates between the two widgets explicitly, so this does
/// not assume the grip and the root share an origin.
fn in_grip(grip: &gtk::DrawingArea, origin: &gtk::Box, x: f64, y: f64) -> bool {
    if !grip.is_visible() {
        return false;
    }

    match grip.compute_bounds(origin) {
        Some(r) => {
            x >= r.x() as f64
                && x <= (r.x() + r.width()) as f64
                && y >= r.y() as f64
                && y <= (r.y() + r.height()) as f64
        }
        None => false,
    }
}

/// Resize from the bottom-right grip. Inert while pinned.
fn wire_grip(panel: &Rc<Panel>, set_resizing: Rc<dyn Fn(bool)>) {
    let drag = gtk::GestureDrag::new();

    {
        let panel = panel.clone();
        let set_resizing = set_resizing.clone();
        drag.connect_drag_begin(move |gesture, _, _| {
            // Braces: take the sequence so the root's move gesture cannot also
            // act on it.
            gesture.set_state(gtk::EventSequenceState::Claimed);
            panel.resize_origin.set(panel.panel_size());
            set_resizing(true);
        });
    }

    {
        let panel = panel.clone();
        drag.connect_drag_update(move |_, dx, dy| {
            resize_by(&panel, dx, dy, "resize-grip");
        });
    }

    drag.connect_drag_end(move |_, _, _| set_resizing(false));

    panel.grip.add_controller(drag);
}

/// Right-button drag anywhere on the panel resizes it.
///
/// This is the primary gesture; the corner grip is the discoverable affordance
/// for it.
fn wire_right_button_resize(
    panel: &Rc<Panel>,
    root: &gtk::Box,
    set_resizing: Rc<dyn Fn(bool)>,
) {
    let drag = gtk::GestureDrag::new();
    drag.set_button(gtk::gdk::BUTTON_SECONDARY);

    {
        let panel = panel.clone();
        let set_resizing = set_resizing.clone();
        drag.connect_drag_begin(move |gesture, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            panel.resize_origin.set(panel.panel_size());
            set_resizing(true);
        });
    }

    {
        let panel = panel.clone();
        drag.connect_drag_update(move |_, dx, dy| {
            resize_by(&panel, dx, dy, "resize-rmb");
        });
    }

    drag.connect_drag_end(move |_, _, _| set_resizing(false));

    root.add_controller(drag);
}

/// Shared resize maths. Layer-mode only; the compositor owns a toplevel's size.
///
/// A layer surface negotiates from the window's *default* size, so
/// `set_default_size` is the lever. `set_size_request` only sets a minimum,
/// which is why it produced a hard 300x150 floor.
///
/// A drag-update only records the desired size. The frame clock decides when
/// to ask for it, and the flow control decides whether to ask at all.
fn resize_by(panel: &Rc<Panel>, dx: f64, dy: f64, phase: &'static str) {
    if panel.pinned.get() || !panel.layered {
        return;
    }

    let (w0, h0) = panel.resize_origin.get();
    let size = geom::clamp_size(w0 + dx as i32, h0 + dy as i32, panel.monitor.get());

    panel.resize_phase.set(phase);
    panel.target.set(Some(size));
}

/// Drain the SIGUSR1 channel on the main thread and toggle the pin there.
fn wire_signal(panel: &Rc<Panel>, rx: async_channel::Receiver<()>) {
    let panel = panel.clone();
    glib::spawn_future_local(async move {
        while rx.recv().await.is_ok() {
            set_pinned(&panel, !panel.pinned.get());
        }
    });
}

fn set_pinned(panel: &Rc<Panel>, pinned: bool) {
    panel.pinned.set(pinned);

    if pinned {
        panel.win.add_css_class("pinned");
    } else {
        panel.win.remove_css_class("pinned");
    }

    update_grip_visibility(panel);
    pin::apply_input_region(&panel.win, pinned);

    eprintln!(
        "gnomon: {}",
        if pinned {
            "pinned (click-through)"
        } else {
            "unpinned"
        }
    );
}
