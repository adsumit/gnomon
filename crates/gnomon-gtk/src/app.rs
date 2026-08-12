//! Application setup: styling, and the window the layer surface lives in.

use adw::prelude::*;
use gtk::{gdk, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::window;

const APP_ID: &str = "com.gnomon.Gnomon";
const STYLE: &str = include_str!("style.css");

/// Run the GUI. `toplevel` skips layer-shell entirely — a debugging escape
/// hatch for compositors where the layer surface misbehaves.
pub fn run(toplevel: bool) -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();

    app.connect_startup(|_| load_css());
    app.connect_activate(move |app| build_window(app, toplevel));

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
        win.set_anchor(Edge::Top, true);
        win.set_anchor(Edge::Right, true);
        win.set_anchor(Edge::Bottom, false);
        win.set_anchor(Edge::Left, false);
        win.set_margin(Edge::Top, 12);
        win.set_margin(Edge::Right, 12);
        // Never take focus, never reserve space.
        win.set_keyboard_mode(KeyboardMode::None);
        win.set_exclusive_zone(0);
    }

    win.set_content(Some(&window::build()));
    win.present();
}
