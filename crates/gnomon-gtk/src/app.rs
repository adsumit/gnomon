//! Application setup: styling, layer-shell placement, and interaction.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::geom::{self, Margins};
use crate::{pin, window};

const APP_ID: &str = "com.gnomon.Gnomon";
const STYLE: &str = include_str!("style.css");

/// Everything the gestures and the signal handler need to share.
struct Panel {
    win: adw::ApplicationWindow,
    grip: gtk::DrawingArea,
    pinned: Cell<bool>,
    margins: Cell<Margins>,
    /// Margins at the moment a drag began.
    drag_origin: Cell<Margins>,
    /// Window size at the moment a grip drag began.
    resize_origin: Cell<(i32, i32)>,
    monitor: Cell<(i32, i32)>,
    layered: bool,
}

impl Panel {
    fn panel_size(&self) -> (i32, i32) {
        (self.win.width().max(1), self.win.height().max(1))
    }

    /// Push the stored margins onto the layer surface.
    fn apply_margins(&self) {
        if !self.layered {
            return;
        }
        let m = self.margins.get();
        self.win.set_margin(Edge::Left, m.left);
        self.win.set_margin(Edge::Top, m.top);
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
        .default_width(300)
        .default_height(150)
        .resizable(true)
        .title("gnomon")
        .build();

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
    win.set_content(Some(&content.root));

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
        resize_origin: Cell::new((300, 150)),
        monitor: Cell::new((0, 0)),
        layered: !toplevel,
    });

    wire_drag(&panel, &content.root);
    wire_grip(&panel);
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

    let margins = if monitor.0 > 0 {
        geom::initial_margins(panel.panel_size(), monitor)
    } else {
        // Monitor unknown: sit at the left gap rather than guess.
        Margins {
            left: geom::EDGE_GAP,
            top: geom::EDGE_GAP,
        }
    };

    panel.margins.set(margins);
    panel.apply_margins();
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
        drag.connect_drag_begin(move |_, _, _| {
            panel.drag_origin.set(panel.margins.get());
        });
    }

    {
        let panel = panel.clone();
        drag.connect_drag_update(move |_, dx, dy| {
            if panel.pinned.get() || !panel.layered {
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
            panel.apply_margins();
        });
    }

    {
        let panel = panel.clone();
        drag.connect_drag_end(move |_, _, _| {
            if panel.pinned.get() || !panel.layered {
                return;
            }
            let m = panel.margins.get();
            let snapped = geom::snap_margins(m.left, m.top, panel.panel_size(), panel.monitor.get());
            panel.margins.set(snapped);
            panel.apply_margins();
        });
    }

    root.add_controller(drag);
}

/// Resize from the bottom-right grip. Inert while pinned.
fn wire_grip(panel: &Rc<Panel>) {
    let drag = gtk::GestureDrag::new();

    {
        let panel = panel.clone();
        drag.connect_drag_begin(move |_, _, _| {
            panel.resize_origin.set(panel.panel_size());
        });
    }

    {
        let panel = panel.clone();
        drag.connect_drag_update(move |_, dx, dy| {
            if panel.pinned.get() {
                return;
            }
            let (w0, h0) = panel.resize_origin.get();
            let (w, h) = geom::clamp_size(w0 + dx as i32, h0 + dy as i32, panel.monitor.get());
            panel.win.set_size_request(w, h);
        });
    }

    panel.grip.add_controller(drag);
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

    panel.grip.set_visible(!pinned);
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
