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
    pinned: Cell<bool>,
    margins: Cell<Margins>,
    /// Margins at the moment a drag began.
    drag_origin: Cell<Margins>,
    /// Window size at the moment a resize drag began.
    resize_origin: Cell<(i32, i32)>,
    /// Mode chosen at drag-begin. Never changes mid-drag.
    drag_zone: Cell<geom::Zone>,
    /// Surface-local point where the drag began.
    grab: Cell<(f64, f64)>,
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

    /// The pointer's position in monitor space, from a gesture offset.
    ///
    /// COORDINATE SPACE. `grab` is the press point in surface-local
    /// coordinates, and `(dx, dy)` is the gesture offset in that same space, so
    /// `grab + offset` is the pointer's current surface-local point. Adding the
    /// *current* margins — which are the surface's origin, because the layer
    /// surface is anchored Top and Left — lifts it into monitor space.
    ///
    /// This is self-correcting, which is the whole point. If we move the origin
    /// by d, a stationary pointer's surface-local coordinate shifts by -d and
    /// the margin we add shifts by +d, so the monitor-space result is
    /// unchanged. Accumulated deltas have no such property: the widget the
    /// gesture is attached to is the widget being resized, so its origin slides
    /// under the pointer and every delta is measured against a moved ruler.
    fn pointer_in_monitor(&self, dx: f64, dy: f64) -> (i32, i32) {
        let (gx, gy) = self.grab.get();
        let m = self.margins.get();
        (
            m.left + (gx + dx) as i32,
            m.top + (gy + dy) as i32,
        )
    }

    /// Resize from an absolute pointer position.
    fn apply_resize(self: &Rc<Self>, zone: geom::Zone, point: (i32, i32), phase: &'static str) {
        let (margins, size) = geom::resize_from(
            zone,
            point,
            self.drag_origin.get(),
            self.resize_origin.get(),
            self.monitor.get(),
        );

        if margins != self.margins.get() {
            self.margins.set(margins);
            self.apply_margins(phase, size, "resize anchor");
        }

        self.resize_phase.set(phase);
        self.target.set(Some(size));
    }

    /// Move so the grabbed point stays under the pointer.
    fn apply_move(self: &Rc<Self>, point: (i32, i32)) {
        let (gx, gy) = self.grab.get();
        let clamped = geom::clamp_margins(
            point.0 - gx as i32,
            point.1 - gy as i32,
            self.panel_size(),
            self.monitor.get(),
        );
        self.margins.set(clamped);
        self.apply_margins("drag", self.panel_size(), "allocated");
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
        drag_zone: Cell::new(geom::Zone::None),
        grab: Cell::new((0.0, 0.0)),
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

    wire_drag(&panel, &content.root, content.set_resizing.clone());
    if !toplevel {
        // In toplevel mode the compositor owns resizing entirely.
        wire_right_button_resize(&panel, &content.root, content.set_resizing.clone());
        wire_cursor(&panel, &content.overlay);
    }
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

/// One gesture, two modes. The mode is decided at drag-begin from the zone
/// under the press and never changes for the rest of the sequence.
fn wire_drag(panel: &Rc<Panel>, root: &gtk::Box, set_resizing: Rc<dyn Fn(bool)>) {
    let drag = gtk::GestureDrag::new();

    {
        let panel = panel.clone();
        let set_resizing = set_resizing.clone();
        drag.connect_drag_begin(move |_, x, y| {
            if panel.pinned.get() {
                panel.drag_zone.set(geom::Zone::None);
                return;
            }

            let zone = geom::zone_at(x as i32, y as i32, panel.panel_size());
            panel.drag_zone.set(zone);
            panel.grab.set((x, y));
            panel.drag_origin.set(panel.margins.get());
            panel.resize_origin.set(panel.panel_size());

            if zone.is_resize() {
                set_resizing(true);
            }
        });
    }

    {
        let panel = panel.clone();
        drag.connect_drag_update(move |_, dx, dy| {
            if panel.pinned.get() || !panel.layered {
                return;
            }
            let point = panel.pointer_in_monitor(dx, dy);
            let zone = panel.drag_zone.get();

            if zone.is_resize() {
                panel.apply_resize(zone, point, "resize-edge");
            } else {
                panel.apply_move(point);
            }
        });
    }

    {
        let panel = panel.clone();
        drag.connect_drag_end(move |_, _, _| {
            let zone = panel.drag_zone.get();
            panel.drag_zone.set(geom::Zone::None);

            if zone.is_resize() {
                set_resizing(false);
                return;
            }
            if panel.pinned.get() || !panel.layered {
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

/// Track the pointer and show the matching resize cursor.
fn wire_cursor(panel: &Rc<Panel>, overlay: &gtk::Overlay) {
    let motion = gtk::EventControllerMotion::new();

    {
        let panel = panel.clone();
        motion.connect_motion(move |_, x, y| {
            if panel.pinned.get() {
                panel.win.set_cursor(None);
                return;
            }
            let zone = geom::zone_at(x as i32, y as i32, panel.panel_size());
            match zone.cursor_name() {
                Some(name) => panel.win.set_cursor_from_name(Some(name)),
                None => panel.win.set_cursor(None),
            }
        });
    }

    {
        let panel = panel.clone();
        motion.connect_leave(move |_| panel.win.set_cursor(None));
    }

    overlay.add_controller(motion);
}

/// Right-button drag resizes from the bottom-right, wherever it starts.
///
/// The edge zones are only 8px, so this stays the forgiving way to resize.
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
        drag.connect_drag_begin(move |gesture, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            panel.grab.set((x, y));
            panel.drag_origin.set(panel.margins.get());
            panel.resize_origin.set(panel.panel_size());
            set_resizing(true);
        });
    }

    {
        let panel = panel.clone();
        drag.connect_drag_update(move |_, dx, dy| {
            if panel.pinned.get() || !panel.layered {
                return;
            }
            let point = panel.pointer_in_monitor(dx, dy);
            panel.apply_resize(geom::Zone::BottomRight, point, "resize-rmb");
        });
    }

    drag.connect_drag_end(move |_, _, _| set_resizing(false));

    root.add_controller(drag);
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

    if pinned {
        // No resize cursors on a click-through panel.
        panel.win.set_cursor(None);
    }
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
