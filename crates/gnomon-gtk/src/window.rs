//! Widget tree and update handling. Every call here runs on the main thread.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::{DateTime, Utc};
use gtk::glib;
use gtk::prelude::*;
use gnomon_core::{LimitWindow, UsageSnapshot};

use crate::feed::{self, Update};

const SEVERITY_CLASSES: [&str; 3] = ["sev-normal", "sev-warning", "sev-error"];

/// One rendered limit window, kept so the countdown can tick without a rebuild.
struct Row {
    countdown: gtk::Label,
    resets_at: Option<DateTime<Utc>>,
}

/// What is currently on screen.
struct State {
    windows: Vec<LimitWindow>,
    rows: Vec<Row>,
    loaded: bool,
}

/// Build the content tree, start the feed, and wire up updates.
pub fn build() -> gtk::Box {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();
    root.set_widget_name("root");

    let status = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .build();
    status.add_css_class("dim-label");

    let state = Rc::new(RefCell::new(State {
        windows: Vec::new(),
        rows: Vec::new(),
        loaded: false,
    }));

    // Something to look at before the first snapshot lands.
    render(&root, &status, &state);

    debug_css_on_realize(&root);

    let (tx, rx) = async_channel::unbounded::<Update>();
    feed::spawn(tx);

    {
        let root = root.clone();
        let status = status.clone();
        let state = state.clone();
        glib::spawn_future_local(async move {
            while let Ok(update) = rx.recv().await {
                match update {
                    Update::Snapshot(snapshot, _origin) => {
                        if apply(&state, snapshot) {
                            render(&root, &status, &state);
                        } else {
                            // Payload unchanged: clear any stale error without
                            // rebuilding the bars.
                            status.set_visible(false);
                        }
                    }
                    Update::Error(message) => {
                        status.set_text(&message);
                        status.set_visible(true);
                    }
                }
            }
        });
    }

    // Countdowns tick locally. This must never touch the network or the socket.
    {
        let state = state.clone();
        glib::timeout_add_seconds_local(1, move || {
            for row in &state.borrow().rows {
                row.countdown.set_text(&countdown(row.resets_at));
            }
            glib::ControlFlow::Continue
        });
    }

    root
}

/// Named colours the panel depends on.
const PROBED_COLORS: [&str; 5] = [
    "window_bg_color",
    "borders",
    "accent_bg_color",
    "warning_bg_color",
    "error_bg_color",
];

/// With `GNOMON_DEBUG_CSS` set, report once whether the themed colours resolve.
///
/// GTK 4 exposes no public getter for a widget's *computed* background, so this
/// asks the style context to resolve each named colour instead — which is the
/// question that actually matters: an unresolvable name means the declaration
/// using it was dropped and the panel fell back to the literal. `lookup_color`
/// is deprecated in GTK 4.10 but still functional, and is the only route to
/// this answer without a private API.
fn debug_css_on_realize(root: &gtk::Box) {
    if std::env::var_os("GNOMON_DEBUG_CSS").is_none() {
        return;
    }

    root.connect_realize(|widget| {
        #[allow(deprecated)]
        let ctx = widget.style_context();

        for name in PROBED_COLORS {
            #[allow(deprecated)]
            match ctx.lookup_color(name) {
                Some(c) => eprintln!(
                    "gnomon: @{name} resolves to rgba({:.3}, {:.3}, {:.3}, {:.3}){}",
                    c.red(),
                    c.green(),
                    c.blue(),
                    c.alpha(),
                    if c.alpha() < 1.0 { "  <-- NOT OPAQUE" } else { "" }
                ),
                None => eprintln!("gnomon: @{name} DID NOT RESOLVE (declaration dropped)"),
            }
        }
    });
}

/// Store a snapshot. Returns true when the screen needs rebuilding.
///
/// Both transports report the same server state, so the most recently received
/// snapshot wins regardless of origin — but an identical payload is dropped.
fn apply(state: &Rc<RefCell<State>>, snapshot: UsageSnapshot) -> bool {
    let mut state = state.borrow_mut();

    if state.loaded && state.windows == snapshot.windows {
        return false;
    }

    state.windows = snapshot.windows;
    state.loaded = true;
    true
}

/// Rebuild the children of `root` from the stored state.
fn render(root: &gtk::Box, status: &gtk::Label, state: &Rc<RefCell<State>>) {
    while let Some(child) = root.first_child() {
        root.remove(&child);
    }

    let mut rows = Vec::new();
    let windows = state.borrow().windows.clone();
    let loaded = state.borrow().loaded;

    if !loaded {
        let loading = gtk::Label::builder().label("Loading usage…").build();
        loading.add_css_class("dim-label");
        root.append(&loading);
    } else if windows.is_empty() {
        let empty = gtk::Label::builder().label("No limit windows reported").build();
        empty.add_css_class("dim-label");
        root.append(&empty);
    } else {
        for window in &windows {
            let (widget, row) = build_row(window);
            root.append(&widget);
            rows.push(row);
        }
    }

    root.append(status);
    state.borrow_mut().rows = rows;
}

/// One limit window: label + percent, a bar, and a countdown.
fn build_row(window: &LimitWindow) -> (gtk::Box, Row) {
    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();

    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .build();

    let name = gtk::Label::builder().label(window.label()).xalign(0.0).build();
    let percent = gtk::Label::builder()
        .label(format!("{:.1}%", window.percent))
        .xalign(1.0)
        .hexpand(true)
        .build();

    header.append(&name);
    header.append(&percent);

    let bar = gtk::ProgressBar::new();
    bar.set_fraction((window.percent / 100.0).clamp(0.0, 1.0));
    set_severity(&bar, window.severity_class());

    let countdown_label = gtk::Label::builder()
        .label(countdown(window.resets_at))
        .xalign(0.0)
        .build();
    countdown_label.add_css_class("dim-label");

    container.append(&header);
    container.append(&bar);
    container.append(&countdown_label);

    (
        container,
        Row {
            countdown: countdown_label,
            resets_at: window.resets_at,
        },
    )
}

/// Apply exactly one severity class, so they cannot accumulate.
fn set_severity(bar: &gtk::ProgressBar, class: &str) {
    for existing in SEVERITY_CLASSES {
        bar.remove_css_class(existing);
    }
    bar.add_css_class(&format!("sev-{class}"));
}

/// "resets in 3h 32m", or "—" when the window carries no reset time.
pub fn countdown(resets_at: Option<DateTime<Utc>>) -> String {
    let Some(resets_at) = resets_at else {
        return "—".to_string();
    };

    let minutes = (resets_at - Utc::now()).num_minutes();
    if minutes <= 0 {
        return "resets now".to_string();
    }

    let days = minutes / 1440;
    let hours = (minutes % 1440) / 60;
    let mins = minutes % 60;

    if days > 0 {
        format!("resets in {days}d {hours}h")
    } else if hours > 0 {
        format!("resets in {hours}h {mins}m")
    } else {
        format!("resets in {mins}m")
    }
}
