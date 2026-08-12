//! Widget tree and update handling. Every call here runs on the main thread.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use chrono::{DateTime, Utc};
use gnomon_core::{LimitWindow, UsageSnapshot};
use gtk::{glib, pango};
use gtk::prelude::*;

use crate::feed::{self, Update};

const SEVERITY_CLASSES: [&str; 3] = ["sev-normal", "sev-warning", "sev-error"];
/// Below this width the panel drops the countdowns and the decimal place.
const COMPACT_WIDTH: i32 = 240;
/// Below either of these the panel also tightens its padding and spacing.
const TIGHT_WIDTH: i32 = 200;
const TIGHT_HEIGHT: i32 = 150;
/// Root box spacing, normal and tightened.
const SPACING: i32 = 14;
const SPACING_TIGHT: i32 = 6;
/// Edge length of the resize grip.
const GRIP_SIZE: i32 = 14;

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
    compact: bool,
}

/// The pieces app.rs needs to wire up interaction.
pub struct Content {
    /// What the window's content is set to.
    pub overlay: gtk::Overlay,
    pub root: gtk::Box,
    pub grip: gtk::DrawingArea,
}

/// Build the content tree, start the feed, and wire up updates.
pub fn build() -> Content {
    // No widget margins: #root's 16px CSS padding provides the inset *inside*
    // the painted background. Margins would push the background away from the
    // surface edge and strand the overlay-aligned grip outside it.
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(SPACING)
        .build();
    root.set_widget_name("root");
    // Natural content width must not be a floor, or the resize grip cannot
    // shrink the panel below whatever the labels happen to need.
    root.set_size_request(0, -1);

    let status = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    status.add_css_class("dim-label");

    // Pinned to the panel's bottom-right corner by the overlay, so no box
    // layout decision can move it.
    let grip = gtk::DrawingArea::builder()
        .content_width(GRIP_SIZE)
        .content_height(GRIP_SIZE)
        .width_request(GRIP_SIZE)
        .height_request(GRIP_SIZE)
        .halign(gtk::Align::End)
        .valign(gtk::Align::End)
        .margin_end(8)
        .margin_bottom(8)
        // Revealed on hover, and only when the panel is interactive.
        .visible(false)
        .build();
    grip.set_widget_name("grip");
    draw_grip(&grip);

    let state = Rc::new(RefCell::new(State {
        windows: Vec::new(),
        rows: Vec::new(),
        loaded: false,
        compact: false,
    }));

    // The ScrolledWindow is what decouples the surface size from the content's
    // natural size: without it the surface refused to shrink below 300x176.
    // External policy means the scrollbars exist but are never drawn.
    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::External)
        .vscrollbar_policy(gtk::PolicyType::External)
        .propagate_natural_width(false)
        .propagate_natural_height(false)
        // No momentum panning from a touch drag.
        .kinetic_scrolling(false)
        .child(&root)
        .build();

    block_scrolling(&scrolled);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&scrolled));

    render(&root, &status, &state);
    debug_css_on_realize(&root);
    watch_width(&overlay, &root, &status, &state);

    // Added last so the grip sits above the probe.
    overlay.add_overlay(&grip);

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

    Content {
        overlay,
        root,
        grip,
    }
}

/// Track the panel's width from its allocation, not from a timer.
///
/// GTK 4 removed the consumer-facing `size-allocate` signal, and `GtkWidget`
/// exposes no notifiable width property. A zero-height `GtkDrawingArea` that
/// spans the row does have a `resize` signal, and it fires on allocation — so
/// it serves as an allocation probe without drawing anything.
fn watch_width(
    overlay: &gtk::Overlay,
    root: &gtk::Box,
    status: &gtk::Label,
    state: &Rc<RefCell<State>>,
) {
    // Fills the overlay so its `resize` signal reports both dimensions.
    let probe = gtk::DrawingArea::builder()
        .content_width(0)
        .content_height(0)
        .hexpand(true)
        .vexpand(true)
        .can_target(false)
        .build();

    let last_compact = Rc::new(Cell::new(false));
    let last_tight = Rc::new(Cell::new(false));
    let root_c = root.clone();
    let status_c = status.clone();
    let state_c = state.clone();

    probe.connect_resize(move |_, width, height| {
        let compact = width > 0 && width < COMPACT_WIDTH;
        let tight =
            (width > 0 && width < TIGHT_WIDTH) || (height > 0 && height < TIGHT_HEIGHT);

        // Padding and spacing: no rebuild needed, just restyle.
        if tight != last_tight.get() {
            last_tight.set(tight);
            if tight {
                root_c.add_css_class("compact");
                root_c.set_spacing(SPACING_TIGHT);
            } else {
                root_c.remove_css_class("compact");
                root_c.set_spacing(SPACING);
            }
        }

        // Label content: this one does need a rebuild.
        if compact != last_compact.get() {
            last_compact.set(compact);
            state_c.borrow_mut().compact = compact;
            render(&root_c, &status_c, &state_c);
        }
    });

    // An overlay child, not a box child: render() clears the box, which
    // previously orphaned the probe on the first snapshot and silently killed
    // the responsive mode.
    overlay.add_overlay(&probe);
}

/// Swallow scroll events before the ScrolledWindow can pan.
///
/// The ScrolledWindow exists only to stop natural content size acting as a
/// floor on the surface; panning is never wanted. A Capture-phase scroll
/// controller sees events before the scrolled window does and stops them, which
/// covers the wheel and touchpad. Kinetic scrolling is off, so a touch drag
/// cannot fling it either.
fn block_scrolling(scrolled: &gtk::ScrolledWindow) {
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
    scroll.connect_scroll(|_, _, _| glib::Propagation::Stop);
    scrolled.add_controller(scroll);
}

/// Three dots stepping up the diagonal, in the theme's foreground colour.
///
/// The 0.35 opacity comes from `#grip` in the stylesheet, so this just uses the
/// resolved foreground directly.
fn draw_grip(grip: &gtk::DrawingArea) {
    grip.set_draw_func(|area, cr, width, height| {
        let c = area.color();
        cr.set_source_rgba(
            c.red() as f64,
            c.green() as f64,
            c.blue() as f64,
            c.alpha() as f64,
        );

        for i in 0..3 {
            let offset = 3.5 + (i as f64) * 4.0;
            cr.arc(
                width as f64 - offset,
                height as f64 - offset,
                1.2,
                0.0,
                std::f64::consts::TAU,
            );
            let _ = cr.fill();
        }
    });
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
                    // @borders is intentionally translucent; only the panel
                    // background must be fully opaque.
                    if name == "window_bg_color" && c.alpha() < 1.0 {
                        "  <-- NOT OPAQUE"
                    } else {
                        ""
                    }
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
    // The probe and the grip live in the overlay, so rebuilding the box's
    // children cannot destroy them.
    while let Some(child) = root.first_child() {
        root.remove(&child);
    }

    let mut rows = Vec::new();
    let (windows, loaded, compact) = {
        let s = state.borrow();
        (s.windows.clone(), s.loaded, s.compact)
    };

    if !loaded {
        let loading = gtk::Label::builder()
            .label("Loading usage…")
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        loading.add_css_class("dim-label");
        root.append(&loading);
    } else if windows.is_empty() {
        let empty = gtk::Label::builder()
            .label("No limit windows reported")
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        empty.add_css_class("dim-label");
        root.append(&empty);
    } else {
        for window in &windows {
            let (widget, row) = build_row(window, compact);
            root.append(&widget);
            rows.push(row);
        }
    }

    root.append(status);
    state.borrow_mut().rows = rows;
}

/// One limit window: label + percent, a bar, and a countdown.
fn build_row(window: &LimitWindow, compact: bool) -> (gtk::Box, Row) {
    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();

    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .build();

    let name = gtk::Label::builder()
        .label(window.label())
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    let percent = gtk::Label::builder()
        .label(if compact {
            format!("{:.0}%", window.percent)
        } else {
            format!("{:.1}%", window.percent)
        })
        .xalign(1.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::End)
        .build();

    header.append(&name);
    header.append(&percent);

    let bar = gtk::ProgressBar::new();
    bar.set_fraction((window.percent / 100.0).clamp(0.0, 1.0));
    set_severity(&bar, window.severity_class());

    let countdown_label = gtk::Label::builder()
        .label(countdown(window.resets_at))
        .xalign(0.0)
        .visible(!compact)
        .ellipsize(pango::EllipsizeMode::End)
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
