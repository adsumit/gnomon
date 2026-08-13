//! Widget tree and update handling. Every call here runs on the main thread.
//!
//! One `Content` per panel. It owns the rows it renders and knows nothing about
//! the feed, which belongs to the layout: there is one feed for many panels.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use chrono::{DateTime, Utc};
use gnomon_core::{LimitWindow, UsageSnapshot};
use gtk::prelude::*;
use gtk::{glib, pango};

use crate::geom;

const SEVERITY_CLASSES: [&str; 3] = ["sev-normal", "sev-warning", "sev-error"];
/// Root box spacing, normal and tightened.
const SPACING: i32 = 14;
const SPACING_TIGHT: i32 = 6;

/// One rendered limit window, kept so the countdown can tick without a rebuild
/// and so a drag can tell which row it started on.
struct Row {
    container: gtk::Box,
    kind: String,
    countdown: gtk::Label,
    resets_at: Option<DateTime<Utc>>,
}

/// What is currently on screen.
struct State {
    /// Kinds this panel renders. EMPTY means "everything" — the state the
    /// single M4 panel is in, so new kinds appear without configuration.
    kinds: Vec<String>,
    /// The most recent full snapshot, so a change of kinds can re-filter
    /// without waiting for the next poll.
    all: Vec<LimitWindow>,
    /// What is actually rendered: `all` filtered by `kinds`.
    windows: Vec<LimitWindow>,
    rows: Vec<Row>,
    loaded: bool,
    compact: bool,
}

/// The pieces app.rs needs to wire up interaction.
pub struct Content {
    /// What the window's content is set to.
    pub overlay: gtk::Overlay,
    /// Fires on every real allocation. app.rs uses it as the resize
    /// acknowledgement; window.rs uses it for the responsive thresholds.
    pub probe: gtk::DrawingArea,
    /// Called with true when an interactive resize starts and false when it
    /// ends, so the expensive rebuild can be deferred to the end.
    pub set_resizing: Rc<dyn Fn(bool)>,
    root: gtk::Box,
    status: gtk::Label,
    state: Rc<RefCell<State>>,
}

/// Filter a snapshot down to the kinds a panel renders, in snapshot order.
fn visible(kinds: &[String], all: &[LimitWindow]) -> Vec<LimitWindow> {
    if kinds.is_empty() {
        return all.to_vec();
    }
    all.iter()
        .filter(|w| kinds.iter().any(|k| k == &w.kind))
        .cloned()
        .collect()
}

impl Content {
    /// Kinds this panel renders. Empty means "everything".
    pub fn kinds(&self) -> Vec<String> {
        self.state.borrow().kinds.clone()
    }

    /// Number of rows currently on screen.
    pub fn row_count(&self) -> usize {
        self.state.borrow().rows.len()
    }

    /// Replace the kind list and re-render from the last snapshot.
    pub fn set_kinds(&self, kinds: Vec<String>) {
        {
            let mut s = self.state.borrow_mut();
            s.kinds = kinds;
            s.windows = visible(&s.kinds, &s.all);
        }
        self.rerender();
    }

    /// Append another panel's kinds to this one. An empty list is the wildcard,
    /// so absorbing one makes this panel the wildcard too.
    pub fn absorb(&self, kinds: Vec<String>) {
        let mut merged = self.kinds();
        if merged.is_empty() || kinds.is_empty() {
            merged = Vec::new();
        } else {
            for kind in kinds {
                if !merged.contains(&kind) {
                    merged.push(kind);
                }
            }
        }
        self.set_kinds(merged);
    }

    /// Drop one kind. A wildcard panel is first materialised into the explicit
    /// list of what it currently shows, so the removal has something to bite on.
    pub fn remove_kind(&self, kind: &str) {
        let mut kinds = self.kinds();
        if kinds.is_empty() {
            kinds = self
                .state
                .borrow()
                .windows
                .iter()
                .map(|w| w.kind.clone())
                .collect();
        }
        kinds.retain(|k| k != kind);
        self.set_kinds(kinds);
    }

    /// Which row's kind sits at this overlay-local y, if any.
    pub fn kind_at(&self, y: f64) -> Option<String> {
        let state = self.state.borrow();
        for row in &state.rows {
            if let Some(b) = row.container.compute_bounds(&self.overlay) {
                if y >= b.y() as f64 && y <= (b.y() + b.height()) as f64 {
                    return Some(row.kind.clone());
                }
            }
        }
        None
    }

    /// The top of a row in overlay-local coordinates.
    pub fn row_top(&self, kind: &str) -> Option<f64> {
        let state = self.state.borrow();
        state
            .rows
            .iter()
            .find(|r| r.kind == kind)
            .and_then(|r| r.container.compute_bounds(&self.overlay))
            .map(|b| b.y() as f64)
    }

    /// Store a snapshot and re-render if what this panel shows changed.
    ///
    /// A panel whose kinds are all absent from the snapshot keeps its existing
    /// rows: an empty result means "nothing to say about these", not "these are
    /// gone".
    pub fn apply_snapshot(&self, snapshot: &UsageSnapshot) {
        let changed = {
            let mut s = self.state.borrow_mut();
            s.all = snapshot.windows.clone();

            let next = visible(&s.kinds, &s.all);

            // Two distinct reasons to leave the rows alone, both no-ops:
            // nothing in this snapshot concerns us, or nothing changed.
            let nothing_for_us = next.is_empty() && s.loaded;
            let unchanged = s.loaded && s.windows == next;

            if nothing_for_us || unchanged {
                false
            } else {
                s.windows = next;
                s.loaded = true;
                true
            }
        };

        if changed {
            self.rerender();
        } else {
            self.status.set_visible(false);
        }
    }

    pub fn show_error(&self, message: &str) {
        self.status.set_text(message);
        self.status.set_visible(true);
    }

    fn rerender(&self) {
        render(&self.root, &self.status, &self.state);
    }
}

/// Build the content tree for one panel.
pub fn build(kinds: Vec<String>) -> Content {
    // No widget margins: #root's 16px CSS padding provides the inset *inside*
    // the painted background. Margins would push the background away from the
    // surface edge, leaving a dead transparent band around the panel.
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(SPACING)
        .build();
    root.set_widget_name("root");
    // Natural content width must not be a floor, or an edge drag cannot shrink
    // the panel below whatever the labels happen to need.
    root.set_size_request(0, -1);

    let status = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    status.add_css_class("dim-label");

    let state = Rc::new(RefCell::new(State {
        kinds,
        all: Vec::new(),
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
    let (probe, set_resizing) = watch_width(&overlay, &root, &status, &state);

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
        probe,
        set_resizing,
        root,
        status,
        state,
    }
}

/// Track the panel's size from its allocation, not from a timer.
///
/// GTK 4 removed the consumer-facing `size-allocate` signal, and `GtkWidget`
/// exposes no notifiable width property. A `GtkDrawingArea` filling the overlay
/// does have a `resize` signal, and it fires on allocation — so it serves as an
/// allocation probe without drawing anything.
fn watch_width(
    overlay: &gtk::Overlay,
    root: &gtk::Box,
    status: &gtk::Label,
    state: &Rc<RefCell<State>>,
) -> (gtk::DrawingArea, Rc<dyn Fn(bool)>) {
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
    // Compact state the allocation implies, which may not be on screen yet
    // because a resize is in progress.
    let wanted_compact = Rc::new(Cell::new(false));
    let resizing = Rc::new(Cell::new(false));

    {
        let root_c = root.clone();
        let status_c = status.clone();
        let state_c = state.clone();
        let last_compact = last_compact.clone();
        let last_tight = last_tight.clone();
        let wanted_compact = wanted_compact.clone();
        let resizing = resizing.clone();

        probe.connect_resize(move |_, width, height| {
            let (compact, tight) =
                geom::responsive_state(width, height, last_compact.get(), last_tight.get());

            // Restyling is cheap, so it stays live during a drag.
            if tight != last_tight.get() {
                last_tight.set(tight);
                apply_tight(&root_c, tight);
            }

            // Rebuilding every widget is not cheap. Mid-drag it is the most
            // visible cost per frame, so it waits for the drag to end.
            wanted_compact.set(compact);
            if !resizing.get() && compact != last_compact.get() {
                last_compact.set(compact);
                state_c.borrow_mut().compact = compact;
                render(&root_c, &status_c, &state_c);
            }
        });
    }

    // An overlay child, not a box child: render() clears the box, which
    // previously orphaned the probe on the first snapshot and silently killed
    // the responsive mode.
    overlay.add_overlay(&probe);

    let set_resizing: Rc<dyn Fn(bool)> = {
        let root_c = root.clone();
        let status_c = status.clone();
        let state_c = state.clone();
        Rc::new(move |active: bool| {
            resizing.set(active);
            if active {
                return;
            }
            // Drag over: settle whatever the last allocation implied.
            let compact = wanted_compact.get();
            if compact != last_compact.get() {
                last_compact.set(compact);
                state_c.borrow_mut().compact = compact;
                render(&root_c, &status_c, &state_c);
            }
        })
    };

    (probe, set_resizing)
}

/// Tightened padding and spacing for a small panel.
fn apply_tight(root: &gtk::Box, tight: bool) {
    if tight {
        root.add_css_class("compact");
        root.set_spacing(SPACING_TIGHT);
    } else {
        root.remove_css_class("compact");
        root.set_spacing(SPACING);
    }
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

/// Rebuild the children of `root` from the stored state.
fn render(root: &gtk::Box, status: &gtk::Label, state: &Rc<RefCell<State>>) {
    // The probe lives in the overlay, so rebuilding the box's children cannot
    // destroy it.
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

    let row = Row {
        container: container.clone(),
        kind: window.kind.clone(),
        countdown: countdown_label,
        resets_at: window.resets_at,
    };

    (container, row)
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
