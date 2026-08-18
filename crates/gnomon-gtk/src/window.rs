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
/// CSS name of the painted shell. `render` refuses to clear a box with it.
const SHELL_NAME: &str = "root";

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
    tight: bool,
    /// Surface size this content needs with BOTH responsive modes OFF.
    ///
    /// The yardstick every responsive decision is measured against. `None`
    /// until the first `remeasure_natural`.
    natural: Option<(i32, i32)>,
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
    /// The painted container. Fills the surface; owns padding and border.
    shell: gtk::Box,
    /// Between the two, unstyled. Its natural size is deliberately suppressed.
    scrolled: gtk::ScrolledWindow,
    /// The rows themselves. The only widget that knows how big the data is.
    content: gtk::Box,
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

/// Does this kind list already account for every kind the snapshot reports?
///
/// The wildcard (an empty list) is a cover by definition: it renders everything,
/// including kinds that do not exist yet. An explicit list covers the snapshot
/// when every kind in the snapshot appears in it. Extra entries do not spoil a
/// cover — a kind the API has stopped reporting does not make a panel any less
/// complete — so a superset counts.
///
/// An empty snapshot is deliberately NOT a cover for an explicit list. "Every
/// kind in nothing appears in the list" is vacuously true, and promoting a
/// panel to the wildcard on that basis would be a decision made from no
/// evidence at all.
fn covers_every_kind(kinds: &[String], all: &[LimitWindow]) -> bool {
    if kinds.is_empty() {
        return true;
    }
    if all.is_empty() {
        return false;
    }
    all.iter()
        .all(|w| kinds.iter().any(|k| k == &w.kind))
}

/// Does a freshly filtered set replace what is on screen?
///
/// Three cases, and the first is the one a torn-off panel depends on. A panel
/// that has never loaded ALWAYS takes the new set, even an empty one, because
/// taking it is what clears the "Loading usage…" placeholder — so seeding a new
/// panel from the cached snapshot is guaranteed to get it off that placeholder.
///
/// A panel that has loaded keeps its rows when the new set is empty: "nothing
/// to say about these kinds" is not "these kinds are gone". Otherwise it takes
/// the new set only when it genuinely differs, which is what makes an unchanged
/// poll free.
fn takes_snapshot(loaded: bool, current: &[LimitWindow], next: &[LimitWindow]) -> bool {
    if !loaded {
        return true;
    }
    !next.is_empty() && current != next
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

    /// Has a snapshot ever been rendered? Until it has, there is nothing worth
    /// sizing the surface to.
    pub fn is_loaded(&self) -> bool {
        self.state.borrow().loaded
    }

    /// Is `shell -> scrolled -> content` still intact?
    ///
    /// Walks upward from `content`, which is the direction that matters: the
    /// question is not "does the shell still have children" but "is the box the
    /// rows are being appended to still reachable from the window". Unconditional
    /// in debug builds; behind `GNOMON_DEBUG_GEOM` in release, because it walks
    /// the tree on every render.
    ///
    /// HONEST LIMIT: this checks ATTACHMENT, not visibility. It would NOT have
    /// caught the defect it was written for — the tree was fully intact and the
    /// ScrolledWindow was simply allocated 0x0. [`Self::verify_allocation`] is
    /// the check that catches that one.
    pub fn verify_tree(&self, phase: &str) {
        if !cfg!(debug_assertions) && std::env::var_os("GNOMON_DEBUG_GEOM").is_none() {
            return;
        }

        let scrolled: gtk::Widget = self.scrolled.clone().upcast();
        let shell: gtk::Widget = self.shell.clone().upcast();

        let mut chain = Vec::new();
        let (mut seen_scrolled, mut seen_shell) = (false, false);

        let mut node = self.content.parent();
        while let Some(widget) = node {
            chain.push(widget.type_().name().to_string());
            seen_scrolled |= widget == scrolled;
            seen_shell |= widget == shell;
            node = widget.parent();
        }

        if !seen_scrolled || !seen_shell {
            eprintln!(
                "gnomon: DETACHED WIDGET TREE at {phase} — rows are being appended to a box \
that is not in the window. content's ancestors: [{}]; reached scrolled={seen_scrolled} \
shell={seen_shell}",
                chain.join(" -> "),
            );
        }
    }

    /// Did the scrolling area actually get any space?
    ///
    /// This is the check that catches an intact-but-invisible tree. The shell
    /// paints its background from its own allocation, so a ScrolledWindow
    /// allocated 0x0 inside a correctly-sized shell renders as an empty
    /// coloured box with working chrome — which is exactly what a missing
    /// `vexpand` produces, and exactly what no attachment check can see.
    pub fn verify_allocation(&self, phase: &str) {
        if !cfg!(debug_assertions) && std::env::var_os("GNOMON_DEBUG_GEOM").is_none() {
            return;
        }

        let (shell_w, shell_h) = (self.shell.width(), self.shell.height());
        let (inner_w, inner_h) = (self.scrolled.width(), self.scrolled.height());

        // Only meaningful once the shell itself has been given room.
        if shell_w <= 0 || shell_h <= 0 {
            return;
        }

        if inner_w <= 0 || inner_h <= 0 {
            eprintln!(
                "gnomon: SCROLLED AREA HAS NO SIZE at {phase} — shell={shell_w}x{shell_h} but \
scrolled={inner_w}x{inner_h}, so the panel will render as an empty box. Its natural size is \
suppressed by design, so it must carry hexpand and vexpand to receive any."
            );
        }
    }

    /// The surface size this panel's content wants, chrome included.
    ///
    /// WHICH WIDGET IS MEASURED, and why it has to be that one. `content` — the
    /// box holding the rows — is measured directly, on both orientations.
    /// Nothing above it can answer the question: `scrolled` carries
    /// `propagate_natural_width(false)` and `propagate_natural_height(false)`,
    /// whose entire purpose is to stop the child's natural size reaching the
    /// parent, so measuring the ScrolledWindow, the shell or the window asks the
    /// one widget in the tree configured to hide the answer. Height is measured
    /// for the width just measured, because a wrapped label's height depends on
    /// it.
    ///
    /// HOW THE CHROME IS ACCOUNTED FOR. Not from the CSS constants — those would
    /// be a second copy of the stylesheet, silently wrong the moment either is
    /// edited, and they change with `.compact`. It is taken from the widgets at
    /// runtime: `shell` contains only `scrolled`, so the difference between the
    /// two widgets' natural sizes IS the shell's padding plus border, whatever
    /// the stylesheet currently says.
    pub fn natural_size(&self) -> (i32, i32) {
        let (_, width, _, _) = self.content.measure(gtk::Orientation::Horizontal, -1);
        let (_, height, _, _) = self.content.measure(gtk::Orientation::Vertical, width);

        let (chrome_w, chrome_h) = self.chrome();
        ((width + chrome_w).max(0), (height + chrome_h).max(0))
    }

    /// The shell's padding and border, in pixels, on each axis.
    ///
    /// `shell` wraps `scrolled` and nothing else, so subtracting the child's
    /// natural size from the parent's leaves exactly the CSS box it adds. This
    /// tracks `#root.compact` automatically, which a hardcoded 16 would not.
    fn chrome(&self) -> (i32, i32) {
        let (_, shell_w, _, _) = self.shell.measure(gtk::Orientation::Horizontal, -1);
        let (_, shell_h, _, _) = self.shell.measure(gtk::Orientation::Vertical, -1);
        let (_, inner_w, _, _) = self.scrolled.measure(gtk::Orientation::Horizontal, -1);
        let (_, inner_h, _, _) = self.scrolled.measure(gtk::Orientation::Vertical, -1);

        ((shell_w - inner_w).max(0), (shell_h - inner_h).max(0))
    }

    /// Components of the last fit, for the geometry trace.
    pub fn fit_breakdown(&self) -> (i32, i32, i32, i32) {
        let (_, width, _, _) = self.content.measure(gtk::Orientation::Horizontal, -1);
        let (_, height, _, _) = self.content.measure(gtk::Orientation::Vertical, width);
        let (chrome_w, chrome_h) = self.chrome();
        (width, height, chrome_w, chrome_h)
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

    /// Collapse a complete explicit kind list back to the wildcard.
    ///
    /// A tear followed by a merge would otherwise cost a panel its wildcard
    /// status permanently: `remove_kind` materialises the wildcard into an
    /// explicit list, and `absorb` unions two explicit lists, so the panel would
    /// never again pick up a kind the API starts reporting later. Once the list
    /// covers everything again, the explicit form carries no more information
    /// than the wildcard did — so it goes back to being the wildcard.
    ///
    /// Returns whether it restored, which is what the debug trace reports.
    pub fn restore_wildcard_if_complete(&self, all: &[LimitWindow]) -> bool {
        let restore = {
            let s = self.state.borrow();
            !s.kinds.is_empty() && covers_every_kind(&s.kinds, all)
        };

        if restore {
            self.set_kinds(Vec::new());
        }
        restore
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
            let next = visible(&s.kinds, &snapshot.windows);

            if takes_snapshot(s.loaded, &s.windows, &next) {
                // `all` and `windows` are written together and never apart. A
                // rejected snapshot must not be stored either: `all` would then
                // describe one poll while `windows` describes an older one, and
                // the next kind-list edit — which re-filters from `all` — would
                // blank the panel to rows the user never asked to lose. Keeping
                // the last snapshot that actually mentioned these kinds is the
                // whole point of rejecting the new one.
                s.all = snapshot.windows.clone();
                s.windows = next;
                s.loaded = true;
                true
            } else {
                false
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

    /// Re-render and refresh the natural-size yardstick.
    ///
    /// Every path that changes the ROW SET goes through here, and only through
    /// here — a kind change, a merge, a new snapshot. The responsive re-render
    /// in `watch_width` deliberately does NOT, because that one is a
    /// consequence of the size rather than a change of content.
    fn rerender(&self) {
        self.remeasure_natural();
        self.verify_tree("re-render");
    }

    /// Cache the size the content wants with compact and tight OFF.
    ///
    /// HOW THE LOOP IS PREVENTED, structurally rather than by convention. A
    /// measurement taken while compact was active would record the tightened
    /// size — smaller padding, no countdowns — and every later comparison would
    /// be against that shrunken yardstick, which is precisely the feedback the
    /// absolute thresholds used to produce. So this function does not trust the
    /// current state: it CLEARS both latches, applies the un-tightened styling,
    /// re-renders, and only then measures. `natural` is written here and
    /// nowhere else, so there is no path to a measurement that skipped those
    /// steps.
    ///
    /// It is called only when the row set changes, never from the responsive
    /// path and never during a resize, so a compact panel keeps the yardstick
    /// it was measured against.
    fn remeasure_natural(&self) {
        {
            let mut s = self.state.borrow_mut();
            s.compact = false;
            s.tight = false;
        }
        apply_tight(&self.shell, &self.content, false);
        render(&self.content, &self.status, &self.state);

        let measured = self.natural_size();
        self.state.borrow_mut().natural = Some(measured);

        // Put the latches back, from the size the surface ACTUALLY has.
        //
        // Clearing them above is required to get an honest measurement, but
        // leaving them cleared was a regression: the only other place they are
        // derived is the probe's `resize` signal, which fires when the SURFACE
        // changes size — and a snapshot that changes the rows of a user-sized
        // panel changes no surface size at all. Every poll therefore knocked a
        // small panel out of compact and tight permanently, and its content
        // began overflowing at full padding.
        self.rederive_responsive(measured);
    }

    /// Recompute compact/tight against the current allocation and apply them.
    ///
    /// Uses plain `render`, never `remeasure_natural`, so it cannot move the
    /// yardstick it was just handed — which is what keeps this off the feedback
    /// path that the absolute thresholds used to create.
    fn rederive_responsive(&self, natural: (i32, i32)) {
        let allocated = (self.shell.width(), self.shell.height());
        if allocated.0 <= 0 || allocated.1 <= 0 {
            // Never allocated yet. The first real allocation will drive the
            // probe, which derives them the usual way.
            return;
        }

        let (compact, tight) = geom::responsive_state(
            allocated,
            natural,
            geom::RESPONSIVE_BAND,
            false,
            false,
        );

        if tight {
            self.state.borrow_mut().tight = true;
            apply_tight(&self.shell, &self.content, true);
        }
        if compact {
            self.state.borrow_mut().compact = true;
            render(&self.content, &self.status, &self.state);
        }
    }

    /// The cached natural size, if one has been measured.
    pub fn natural_cached(&self) -> Option<(i32, i32)> {
        self.state.borrow().natural
    }
}

/// Build the content tree for one panel.
///
/// THE TREE, and why it is this shape:
///
/// ```text
///   overlay          hosts the allocation probe without disturbing layout
///   └─ shell   #root the PAINTED container: background, border, radius,
///      │             padding. Always exactly the surface, so the chrome can
///      │             never be scrolled out of view.
///      └─ scrolled   unstyled. Carries propagate_natural_*(false) so the
///         │          content's natural size cannot become a floor under the
///         │          surface. Clips only the rows.
///         └─ content the rows. No padding and no name; the shell owns both.
/// ```
///
/// The shell exists because the painted box used to be INSIDE the scrolled
/// viewport. Whenever the surface was shorter than the content, the viewport
/// scrolled the painted box's bottom edge — and its rounded corners — out of
/// sight, so shrinking the panel clipped its chrome rather than just its text.
pub fn build(kinds: Vec<String>) -> Content {
    // The rows. No widget margins and no padding: inset is the shell's job now.
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(SPACING)
        .build();
    // Natural content width must not be a floor, or an edge drag cannot shrink
    // the panel below whatever the labels happen to need.
    content.set_size_request(0, -1);
    // Rows span the viewport's width, so the right-aligned percent labels sit
    // against the panel's edge rather than against the longest label.
    content.set_hexpand(true);

    // `max_width_chars` bounds the NATURAL width. Without it a wrapping label
    // reports its natural width as the entire message on one unwrapped line, so
    // a long error string would drag the whole panel out to several hundred
    // pixels the moment `fit_to_content` measured it.
    let status = gtk::Label::builder()
        .wrap(true)
        .max_width_chars(28)
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
        tight: false,
        natural: None,
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
        // MUST expand, and this is not cosmetic. `propagate_natural_*(false)`
        // above makes this widget's natural size ~0 on purpose — that is how it
        // stops the content acting as a floor under the surface. A GtkBox
        // allocates a non-expanding child its NATURAL size, so inside the shell
        // that would be 0x0 and the rows would be allocated nothing at all,
        // while the shell's own background and border painted normally: an
        // empty grey box. Before the shell existed this widget was the
        // overlay's child, and GtkOverlay hands its main child the whole
        // allocation regardless of these flags, which is why the omission only
        // became a defect when the box was introduced.
        .hexpand(true)
        .vexpand(true)
        .child(&content)
        .build();

    block_scrolling(&scrolled);

    // The painted container. It fills the surface, so its border and rounded
    // corners are always drawn at the surface's own edge.
    let shell = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    shell.set_widget_name(SHELL_NAME);
    shell.set_size_request(0, 0);
    shell.append(&scrolled);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&shell));

    debug_css_on_realize(&shell);
    let (probe, set_resizing) = watch_width(&overlay, &shell, &content, &status, &state);

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

    let built = Content {
        overlay,
        probe,
        set_resizing,
        shell,
        scrolled,
        content,
        status,
        state,
    };
    // Seeds `natural` as well as drawing the placeholder, so the responsive
    // path has a yardstick from the very first allocation.
    built.remeasure_natural();
    built.verify_tree("construction");
    built
}

/// Track the panel's size from its allocation, not from a timer.
///
/// GTK 4 removed the consumer-facing `size-allocate` signal, and `GtkWidget`
/// exposes no notifiable width property. A `GtkDrawingArea` filling the overlay
/// does have a `resize` signal, and it fires on allocation — so it serves as an
/// allocation probe without drawing anything.
fn watch_width(
    overlay: &gtk::Overlay,
    shell: &gtk::Box,
    content: &gtk::Box,
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

    // Compact state the allocation implies, which may not be on screen yet
    // because a resize is in progress. The LATCHES themselves live in `state`,
    // so `remeasure_natural` can clear them before taking a measurement.
    let wanted_compact = Rc::new(Cell::new(false));
    let resizing = Rc::new(Cell::new(false));

    {
        let shell_c = shell.clone();
        let content_c = content.clone();
        let status_c = status.clone();
        let state_c = state.clone();
        let wanted_compact = wanted_compact.clone();
        let resizing = resizing.clone();

        probe.connect_resize(move |_, width, height| {
            let (natural, was_compact, was_tight) = {
                let s = state_c.borrow();
                (s.natural, s.compact, s.tight)
            };

            // No yardstick yet: nothing has been rendered, so there is no
            // content whose size could justify tightening anything.
            let Some(natural) = natural else {
                return;
            };

            let (compact, tight) = geom::responsive_state(
                (width, height),
                natural,
                geom::RESPONSIVE_BAND,
                was_compact,
                was_tight,
            );

            // Restyling is cheap, so it stays live during a drag.
            if tight != was_tight {
                state_c.borrow_mut().tight = tight;
                apply_tight(&shell_c, &content_c, tight);
            }

            // Rebuilding every widget is not cheap. Mid-drag it is the most
            // visible cost per frame, so it waits for the drag to end.
            //
            // NOTE: `render`, never `remeasure_natural`. This re-render is a
            // consequence of the size, not a change of content, so it must not
            // move the yardstick it was just measured against.
            wanted_compact.set(compact);
            if !resizing.get() && compact != was_compact {
                state_c.borrow_mut().compact = compact;
                render(&content_c, &status_c, &state_c);
            }
        });
    }

    // An overlay child, not a box child: render() clears the box, which
    // previously orphaned the probe on the first snapshot and silently killed
    // the responsive mode.
    overlay.add_overlay(&probe);

    let set_resizing: Rc<dyn Fn(bool)> = {
        let content_c = content.clone();
        let status_c = status.clone();
        let state_c = state.clone();
        Rc::new(move |active: bool| {
            resizing.set(active);
            if active {
                return;
            }
            // Drag over: settle whatever the last allocation implied.
            let compact = wanted_compact.get();
            if compact != state_c.borrow().compact {
                state_c.borrow_mut().compact = compact;
                render(&content_c, &status_c, &state_c);
            }
        })
    };

    (probe, set_resizing)
}

/// Tightened padding and spacing for a small panel.
///
/// Padding belongs to the shell now — it is the painted box — while spacing
/// between rows belongs to the content box.
fn apply_tight(shell: &gtk::Box, content: &gtk::Box, tight: bool) {
    if tight {
        shell.add_css_class("compact");
        content.set_spacing(SPACING_TIGHT);
    } else {
        shell.remove_css_class("compact");
        content.set_spacing(SPACING);
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

/// Rebuild the children of the CONTENT box from the stored state.
///
/// The parameter is the content box and never the shell. Passing the shell here
/// would remove the ScrolledWindow from the tree on the first render and then
/// append rows into a box attached to nothing — the failure mode the assertion
/// below exists to make impossible rather than merely unlikely.
fn render(content: &gtk::Box, status: &gtk::Label, state: &Rc<RefCell<State>>) {
    debug_assert_ne!(
        content.widget_name(),
        SHELL_NAME,
        "render() must be given the content box, never the shell: clearing the \
shell detaches the ScrolledWindow and the rows go nowhere"
    );

    // The probe lives in the overlay, so rebuilding the box's children cannot
    // destroy it.
    while let Some(child) = content.first_child() {
        content.remove(&child);
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
        content.append(&loading);
    } else if windows.is_empty() {
        let empty = gtk::Label::builder()
            .label("No limit windows reported")
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        empty.add_css_class("dim-label");
        content.append(&empty);
    } else {
        for window in &windows {
            let (widget, row) = build_row(window, compact);
            content.append(&widget);
            rows.push(row);
        }
    }

    content.append(status);
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

#[cfg(test)]
mod tests {
    use super::*;
    use gnomon_core::WindowSource;

    fn window(kind: &str, percent: f64) -> LimitWindow {
        LimitWindow {
            kind: kind.to_string(),
            group: "g".to_string(),
            percent,
            severity: None,
            resets_at: None,
            scope: None,
            is_active: None,
            source: WindowSource::Api,
        }
    }

    fn all() -> Vec<LimitWindow> {
        vec![window("five_hour", 12.0), window("seven_day", 40.0)]
    }

    // ---- kind filtering ----

    #[test]
    fn an_empty_kind_list_is_the_wildcard() {
        assert_eq!(visible(&[], &all()), all());
    }

    #[test]
    fn a_kind_list_selects_only_its_own_kinds() {
        let kinds = vec!["seven_day".to_string()];
        assert_eq!(visible(&kinds, &all()), vec![window("seven_day", 40.0)]);
    }

    #[test]
    fn filtering_preserves_snapshot_order_not_kind_list_order() {
        let kinds = vec!["seven_day".to_string(), "five_hour".to_string()];
        assert_eq!(visible(&kinds, &all()), all());
    }

    #[test]
    fn a_kind_absent_from_the_snapshot_yields_nothing() {
        let kinds = vec!["not_a_real_kind".to_string()];
        assert!(visible(&kinds, &all()).is_empty());
    }

    // ---- wildcard restoration ----
    //
    // A tear materialises the wildcard into an explicit list. These decide when
    // a later merge is allowed to turn it back into the wildcard, which is what
    // keeps the panel picking up kinds the API adds in future.

    fn kinds(names: &[&str]) -> Vec<String> {
        names.iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn an_exact_cover_restores_the_wildcard() {
        assert!(covers_every_kind(&kinds(&["five_hour", "seven_day"]), &all()));
    }

    #[test]
    fn cover_ignores_the_order_of_the_kind_list() {
        assert!(covers_every_kind(&kinds(&["seven_day", "five_hour"]), &all()));
    }

    #[test]
    fn a_superset_still_covers() {
        // A kind the API has stopped reporting does not make the panel any less
        // complete, so the extra entry is harmless.
        let list = kinds(&["five_hour", "seven_day", "retired_kind"]);
        assert!(covers_every_kind(&list, &all()));
    }

    #[test]
    fn missing_one_kind_is_not_a_cover() {
        assert!(!covers_every_kind(&kinds(&["five_hour"]), &all()));
    }

    #[test]
    fn an_empty_snapshot_does_not_promote_an_explicit_list() {
        // Vacuously "covers everything", but on no evidence at all — so it must
        // not be treated as a cover.
        assert!(!covers_every_kind(&kinds(&["five_hour"]), &[]));
    }

    #[test]
    fn a_wildcard_is_already_a_cover() {
        assert!(covers_every_kind(&[], &all()));
        // Including against an empty snapshot: it renders whatever arrives.
        assert!(covers_every_kind(&[], &[]));
    }

    // ---- the seeding decision ----
    //
    // The first two pin defect A: a panel torn off between polls is seeded from
    // the cached snapshot, and that seed must always get it off the loading
    // placeholder.

    #[test]
    fn an_unloaded_panel_always_takes_the_snapshot() {
        assert!(takes_snapshot(false, &[], &all()));
    }

    #[test]
    fn an_unloaded_panel_takes_even_an_empty_snapshot() {
        // Otherwise `loaded` never flips and the panel shows "Loading usage…"
        // forever, which is exactly the defect.
        assert!(takes_snapshot(false, &[], &[]));
    }

    #[test]
    fn a_loaded_panel_ignores_an_identical_snapshot() {
        assert!(!takes_snapshot(true, &all(), &all()));
    }

    #[test]
    fn a_loaded_panel_takes_a_changed_snapshot() {
        let next = vec![window("five_hour", 13.0), window("seven_day", 40.0)];
        assert!(takes_snapshot(true, &all(), &next));
    }

    #[test]
    fn a_loaded_panel_keeps_its_rows_when_the_snapshot_says_nothing_about_them() {
        // Empty means "no news about these kinds", not "these kinds are gone".
        assert!(!takes_snapshot(true, &all(), &[]));
    }
}
