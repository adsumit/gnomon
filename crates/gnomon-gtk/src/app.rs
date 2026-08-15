//! Application setup, and the panels themselves.
//!
//! A `Panel` is one layer surface with its own position, size and pin state.
//! `Layout` owns them all and is the only thing that knows how many exist.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gnomon_core::UsageSnapshot;
use gtk::{gdk, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::feed::{self, Update};
use crate::geom::{self, Margins, Zone};
use crate::{pin, window};

const APP_ID: &str = "com.gnomon.Gnomon";
/// Initial window size, and the size used to place the first panel before any
/// allocation makes a real measurement available.
const DEFAULT_SIZE: (i32, i32) = (300, 150);
const STYLE: &str = include_str!("style.css");
/// Frames to wait for an allocation before assuming the request was ignored.
/// Without this, one unacknowledged request would freeze resizing for good.
const IN_FLIGHT_TIMEOUT_FRAMES: u32 = 8;

/// One panel: one layer surface, one set of kinds, its own everything.
pub struct Panel {
    win: adw::ApplicationWindow,
    content: window::Content,
    layout: Weak<Layout>,
    layered: bool,

    monitor: Cell<(i32, i32)>,
    /// Only the first panel derives its position from the monitor; torn-off
    /// panels are placed explicitly by the tear.
    auto_place: Cell<bool>,
    default_size: (i32, i32),
    /// Has the user ever resized THIS panel by hand?
    ///
    /// Latched by the first interactive resize and never cleared. It is the
    /// whole difference between the two sizing rules: until it is set the
    /// surface follows its content, and once it is set the size the user chose
    /// is authoritative and nothing re-fits it. Programmatic sizing — the fit
    /// itself — must never set it, or the first fit would disable all the rest.
    user_sized: Cell<bool>,
    /// Is a gesture in progress on this panel right now?
    ///
    /// Auto-fit is suppressed for the whole of it. A poll landing mid-drag
    /// would otherwise resize the panel out from under the pointer.
    dragging: Cell<bool>,

    // ---- per-window interaction state ----
    pinned: Cell<bool>,
    margins: Cell<Margins>,
    /// Margins at the moment a drag began.
    drag_origin: Cell<Margins>,
    /// Window size at the moment a drag began — any drag, not just a resize.
    /// A resize measures from it, and the tear test uses it as the second half
    /// of the frozen rectangle the pointer has to leave.
    resize_origin: Cell<(i32, i32)>,
    /// Mode chosen at drag-begin. Never changes mid-drag.
    drag_zone: Cell<Zone>,
    /// Surface-local point where the drag began.
    grab: Cell<(f64, f64)>,
    /// The row the drag began on, and where its top was, so a tear can place
    /// the new panel under the same part of the pointer.
    drag_row: RefCell<Option<String>>,
    drag_row_top: Cell<f64>,
    /// Once a tear has happened, the rest of this gesture drives that panel.
    drag_target: RefCell<Option<Rc<Panel>>>,

    /// Last point and zone the motion controller saw, purely for the
    /// drag-begin trace: printing both on one line is what exposes a
    /// coordinate-space disagreement between the two paths.
    last_motion: Cell<(f64, f64)>,
    last_motion_zone: Cell<Zone>,

    // ---- resize flow control ----
    target: Cell<Option<(i32, i32)>>,
    in_flight: Cell<Option<(i32, i32)>>,
    in_flight_frames: Cell<u32>,
    last_requested: Cell<Option<(i32, i32)>>,
    last_allocated: Cell<Option<(i32, i32)>>,
    resize_phase: Cell<&'static str>,

    /// One-shot traces armed by a tear. The row removal is synchronous but the
    /// allocation and any size request that follow it are not, so watching the
    /// source panel across a tear means arming the two later events too.
    trace_next_alloc: Cell<bool>,
    trace_next_request: Cell<bool>,
}

impl Panel {
    /// Create a panel rendering `kinds`, at `margins`, sized `size`.
    ///
    /// An empty `kinds` means "render everything", which is the state the
    /// single startup panel is in.
    pub fn new(
        layout: &Rc<Layout>,
        kinds: Vec<String>,
        margins: Margins,
        size: (i32, i32),
    ) -> Rc<Panel> {
        let toplevel = layout.toplevel;

        let win = adw::ApplicationWindow::builder()
            .application(&layout.app)
            .default_width(size.0)
            .default_height(size.1)
            .resizable(true)
            .title("gnomon")
            .build();

        // A layer surface negotiates from the default size; the size request
        // must never become a floor under it.
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
            win.set_margin(Edge::Top, margins.top);
            win.set_margin(Edge::Left, margins.left);
            // Never take focus, in either pin state. Never reserve space.
            win.set_keyboard_mode(KeyboardMode::None);
            win.set_exclusive_zone(0);
        }

        let content = window::build(kinds);

        // Seed from the cached snapshot BEFORE the window is ever shown. The
        // feed only delivers to panels that exist when an update arrives, and
        // the OAuth leg polls at 180s, so a panel torn off between polls would
        // otherwise sit on the loading placeholder for up to three minutes.
        if let Some(snapshot) = layout.last_snapshot.borrow().as_ref() {
            content.apply_snapshot(snapshot);
        }

        win.set_content(Some(&content.overlay));

        let panel = Rc::new(Panel {
            win: win.clone(),
            content,
            layout: Rc::downgrade(layout),
            layered: !toplevel,
            monitor: Cell::new((0, 0)),
            auto_place: Cell::new(false),
            default_size: size,
            user_sized: Cell::new(false),
            dragging: Cell::new(false),
            pinned: Cell::new(false),
            margins: Cell::new(margins),
            drag_origin: Cell::new(margins),
            resize_origin: Cell::new(size),
            drag_zone: Cell::new(Zone::None),
            grab: Cell::new((0.0, 0.0)),
            drag_row: RefCell::new(None),
            drag_row_top: Cell::new(0.0),
            drag_target: RefCell::new(None),
            last_motion: Cell::new((-1.0, -1.0)),
            last_motion_zone: Cell::new(Zone::None),
            target: Cell::new(None),
            in_flight: Cell::new(None),
            in_flight_frames: Cell::new(0),
            last_requested: Cell::new(None),
            last_allocated: Cell::new(None),
            resize_phase: Cell::new("resize"),
            trace_next_alloc: Cell::new(false),
            trace_next_request: Cell::new(false),
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
            let panel_for_probe = panel.clone();
            panel.content.probe.connect_resize(move |_, w, h| {
                panel_for_probe.on_allocation(w, h);
            });
        }

        // Every pointer path is attached to the overlay so they share one
        // coordinate space, and all of them measure against the window's size.
        wire_drag(&panel);
        wire_middle_click(&panel);
        if !toplevel {
            wire_right_button_resize(&panel);
            wire_cursor(&panel);
        }

        {
            let panel = panel.clone();
            win.connect_realize(move |win| {
                panel.on_realize(win);
            });
        }

        // Deliberately NOT presented here. `gtk_window_present` realizes the
        // window synchronously, so `connect_realize` fires before this function
        // returns — and anything the caller must set up first, such as the
        // startup panel's auto-placement, would be set too late to be seen.
        // `Layout::add` presents, and is the only way a panel enters the layout.
        panel
    }

    pub fn rect(&self) -> geom::Rect {
        geom::Rect::new(self.margins.get(), self.panel_size())
    }

    /// The rectangle this panel occupied when the current drag began.
    ///
    /// The tear test measures against this, never against `rect()`. A move
    /// drags the panel with the pointer one-for-one, so in the LIVE rectangle
    /// the pointer never moves relative to the panel at all and could never
    /// leave it. Frozen at the press, the footprint stays still while the
    /// pointer travels, so leaving it means something.
    fn drag_origin_rect(&self) -> geom::Rect {
        geom::Rect::new(self.drag_origin.get(), self.resize_origin.get())
    }

    fn panel_size(&self) -> (i32, i32) {
        (self.win.width().max(1), self.win.height().max(1))
    }

    fn close(&self) {
        self.win.close();
    }

    /// Monitor geometry, and — for the first panel only — its position.
    fn on_realize(self: &Rc<Self>, win: &adw::ApplicationWindow) {
        // Only overwrite with a real answer. A torn-off panel inherits its
        // source's monitor before being presented, and clobbering that with the
        // (0, 0) that `monitor_size` returns when it cannot identify an output
        // would throw away the one good value we had.
        let mut monitor = monitor_size(win);
        if monitor.0 > 0 && monitor.1 > 0 {
            self.monitor.set(monitor);
        } else {
            monitor = self.monitor.get();
        }

        if self.layered && self.auto_place.get() {
            // Deliberately NOT panel_size(): at realize the window has not been
            // allocated, so its width reads 0 and the panel lands off-screen.
            let size = self.default_size;

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

            self.margins.set(margins);
            self.apply_margins("realize", size, source);
            // Spent. A window can be realized more than once, and a second
            // pass would drag the panel back to its startup corner.
            self.auto_place.set(false);
        }

        self.watch_surface(win);
    }

    /// Re-apply the input region whenever the surface is laid out again.
    ///
    /// A move or a resize produces a new configuration and drops the region, so
    /// applying it once at startup is not enough.
    fn watch_surface(self: &Rc<Self>, win: &adw::ApplicationWindow) {
        pin::apply_input_region(win, self.pinned.get());

        let Some(surface) = win.surface() else {
            return;
        };

        let panel = self.clone();
        surface.connect_layout(move |_, _, _| {
            pin::apply_input_region(&panel.win, panel.pinned.get());
        });
    }

    /// Push the stored margins onto the layer surface.
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
    /// COORDINATE SPACE. The reference frame is fixed at drag-begin and never
    /// re-read: `drag_origin` is the margins at the press, `grab` is the press
    /// point in surface-local coordinates, and `(dx, dy)` is the gesture's
    /// offset accumulated from that same press point. Their sum is the
    /// pointer's monitor-space position.
    ///
    /// The current margins are deliberately NOT consulted. GTK anchors the
    /// gesture offset to the press point and does not re-measure it when the
    /// surface origin moves, so the offset already contains every pixel of
    /// movement we have applied. Adding it to margins that have themselves been
    /// moved by that offset counts it twice, and the panel accelerates away
    /// from the cursor at roughly double speed, compounding each event.
    fn pointer_in_monitor(&self, dx: f64, dy: f64) -> (i32, i32) {
        let (gx, gy) = self.grab.get();
        geom::drag_point(
            self.drag_origin.get(),
            (gx as i32, gy as i32),
            dx as i32,
            dy as i32,
        )
    }

    /// Resize from an absolute pointer position.
    fn apply_resize(self: &Rc<Self>, zone: Zone, point: (i32, i32), phase: &'static str) {
        let (margins, size) = geom::resize_from(
            zone,
            point,
            self.drag_origin.get(),
            self.resize_origin.get(),
            self.monitor.get(),
        );

        // The only place this latch is ever set, and only when the gesture has
        // actually changed the size. A bare click inside the 8px resize band
        // delivers drag events with no real movement, and latching on those
        // would opt the panel out of auto-fit for good over a pixel of jitter.
        if size != self.resize_origin.get() {
            self.user_sized.set(true);
        }

        if margins != self.margins.get() {
            self.margins.set(margins);
            self.apply_margins(phase, size, "resize anchor");
        }

        self.resize_phase.set(phase);
        self.target.set(Some(size));
    }

    /// Move by the gesture offset, measured from the drag-begin margins.
    fn apply_move(self: &Rc<Self>, dx: f64, dy: f64) {
        let moved = geom::move_margins(
            self.drag_origin.get(),
            dx as i32,
            dy as i32,
            self.panel_size(),
            self.monitor.get(),
        );
        self.margins.set(moved);
        self.apply_margins("drag", self.panel_size(), "allocated");
    }

    /// Shrink or grow the surface to whatever the content now needs.
    ///
    /// HOW THE SURFACE ACTUALLY SHRINKS. There is no "unset the size" lever
    /// here: the ScrolledWindow's `propagate_natural_*(false)` means GTK's own
    /// negotiation can never learn the content's size, which is exactly what
    /// stops natural size acting as a floor under an interactive resize. So the
    /// content is measured explicitly and the answer is pushed back through the
    /// SAME path a drag uses — `target`, then one `set_default_size` per frame
    /// from `tick_resize`. `set_default_size` is a request, not a minimum, so a
    /// smaller number really does shrink the layer surface.
    ///
    /// Reusing the drag path also inherits its protections for free: the
    /// one-in-flight gate, the equality escape that skips a size the surface
    /// already has, and the eight-frame timeout.
    fn fit_to_content(self: &Rc<Self>) {
        // The user's chosen size is authoritative. This is the guard that keeps
        // M4's resize behaviour untouched.
        if self.user_sized.get() || !self.layered {
            return;
        }
        // A poll can land at any moment, including mid-gesture. Resizing the
        // panel under the pointer while the user is dragging it would be the
        // window moving on its own; `user_sized` does not protect against this
        // because a MOVE drag never sets it, and even a resize drag has a gap
        // between drag-begin and its first motion event.
        if self.dragging.get() {
            return;
        }
        // Nothing has been rendered yet, so there is nothing to fit to; sizing
        // to the "Loading usage…" placeholder would only cause a visible jump
        // when the first snapshot lands.
        if !self.content.is_loaded() {
            return;
        }

        // The CACHED natural size, which `remeasure_natural` guarantees was
        // taken with compact and tight off. Measuring live here would read the
        // tightened size whenever compact happened to be on, and the fit would
        // then chase its own tail downward.
        let Some(natural) = self.content.natural_cached() else {
            return;
        };
        if natural.0 <= 0 || natural.1 <= 0 {
            return;
        }

        // NO MINIMUM. `MIN_WIDTH`/`MIN_HEIGHT` exist to stop the USER dragging a
        // panel down to uselessness; an automatic fit is content-sized by
        // definition, so raising it to a floor taller than the content is how
        // dead space got manufactured in the first place — 184x144 became
        // 200x144, 200 fell under the old compact threshold, the padding halved
        // and the countdowns went, and the panel ended up 200x100 wrapping 78px
        // of content. The floor still applies to every interactive resize.
        //
        // The monitor size IS still a ceiling: a panel larger than the output
        // cannot be positioned sensibly and could not be dragged back.
        let monitor = self.monitor.get();
        let fitted = (
            if monitor.0 > 0 { natural.0.min(monitor.0) } else { natural.0 },
            if monitor.1 > 0 { natural.1.min(monitor.1) } else { natural.1 },
        );

        self.resize_phase.set("fit");
        self.target.set(Some(fitted));
        self.debug_fit(fitted);

        // Growing can push a snapped panel off the edge it was snapped to, and
        // no other size-change path leaves the position unchecked.
        let clamped = geom::clamp_margins(
            self.margins.get().left,
            self.margins.get().top,
            fitted,
            self.monitor.get(),
        );
        if clamped != self.margins.get() {
            self.margins.set(clamped);
            self.apply_margins("fit", fitted, "natural");
        }
    }

    /// Issue at most one size request, called once per frame.
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

        if self.trace_next_request.replace(false) {
            self.debug_source(
                "tear-source-request",
                &format!("requested={}x{}", target.0, target.1),
            );
        }
    }

    /// An allocation arrived: the surface has genuinely changed size.
    fn on_allocation(self: &Rc<Self>, width: i32, height: i32) {
        let allocated = (width, height);
        self.last_allocated.set(Some(allocated));
        self.in_flight.set(None);
        self.in_flight_frames.set(0);

        if self.target.get() == Some(allocated) {
            self.target.set(None);
        }

        self.debug_resize_allocated(allocated);

        if self.trace_next_alloc.replace(false) {
            self.debug_source("tear-source-alloc", &format!("allocated={width}x{height}"));
        }

        // The tree is intact only if the scrolling area got real space. An
        // allocation is the first moment that question has an answer.
        self.content.verify_allocation("allocation");

        // An allocation is also how the responsive modes change: window.rs
        // connected its own handler to this probe FIRST, so by the time we get
        // here it has already decided compact/tight and re-rendered. Entering
        // compact hides the countdowns and tightens the padding, which shortens
        // the content — and without a re-fit that shortening reappears as dead
        // space, the exact defect this rule exists to remove.
        //
        // This cannot spin. The fit measures, and `tick_resize`'s equality
        // escape drops any target the surface already has, so a fit that finds
        // an unchanged natural size issues no request and produces no further
        // allocation. When the size HAS changed, the responsive thresholds have
        // separate enter and exit values, so a shrink can never re-cross the
        // boundary it just crossed: the sequence normal -> compact -> tight is
        // one-way and at most two steps long.
        self.fit_to_content();
    }

    /// One line of this panel's position and size, for watching a tear.
    ///
    /// `panel_size()` reads the window, which can still be a frame behind an
    /// allocation, so callers that have a fresher number pass it in `note`.
    fn debug_source(&self, label: &str, note: &str) {
        if std::env::var_os("GNOMON_DEBUG_GEOM").is_none() {
            return;
        }
        let m = self.margins.get();
        let size = self.panel_size();
        eprintln!(
            "gnomon geom[{label}]: margin_left={} margin_top={} size={}x{} rows={} {note}",
            m.left,
            m.top,
            size.0,
            size.1,
            self.content.row_count(),
        );
    }

    /// The fit's inputs, so a reported height can be checked against its parts.
    fn debug_fit(&self, fitted: (i32, i32)) {
        if std::env::var_os("GNOMON_DEBUG_GEOM").is_none() {
            return;
        }
        let (cw, ch, chrome_w, chrome_h) = self.content.fit_breakdown();
        eprintln!(
            "gnomon geom[fit]: rows={} content={cw}x{ch} chrome={chrome_w}x{chrome_h} \
sum={}x{} fitted={}x{}{}",
            self.content.row_count(),
            cw + chrome_w,
            ch + chrome_h,
            fitted.0,
            fitted.1,
            if fitted != (cw + chrome_w, ch + chrome_h) {
                "  <-- capped by the monitor"
            } else {
                ""
            },
        );
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

    /// What the drag gesture decided, beside what the motion controller
    /// decided. If the two disagree they are not measuring the same rectangle.
    fn debug_drag_begin(&self, x: f64, y: f64, size: (i32, i32), zone: Zone) {
        if std::env::var_os("GNOMON_DEBUG_GEOM").is_none() {
            return;
        }
        let (mx, my) = self.last_motion.get();
        eprintln!(
            "gnomon geom[drag-begin]: gesture=({:.1},{:.1}) size={}x{} zone={:?} mode={} \
| motion=({:.1},{:.1}) zone={:?} rows={}",
            x,
            y,
            size.0,
            size.1,
            zone,
            if zone.is_resize() { "resize" } else { "move" },
            mx,
            my,
            self.last_motion_zone.get(),
            self.content.row_count(),
        );
    }

    fn debug_tear(&self, kind: &str, margins: Margins) {
        if std::env::var_os("GNOMON_DEBUG_GEOM").is_none() {
            return;
        }
        eprintln!(
            "gnomon geom[tear]: kind={kind} new panel at {},{}",
            margins.left, margins.top,
        );
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

/// Owns every panel. The only thing that knows how many windows exist.
pub struct Layout {
    app: adw::Application,
    toplevel: bool,
    panels: RefCell<Vec<Rc<Panel>>>,
    /// The most recent snapshot, kept so a panel created between polls can be
    /// seeded at construction. Without it a torn-off panel has nothing to
    /// render until the next update, which on the OAuth leg is up to 180s away.
    last_snapshot: RefCell<Option<UsageSnapshot>>,
}

impl Layout {
    fn new(app: &adw::Application, toplevel: bool) -> Rc<Layout> {
        Rc::new(Layout {
            app: app.clone(),
            toplevel,
            panels: RefCell::new(Vec::new()),
            last_snapshot: RefCell::new(None),
        })
    }

    /// The startup panel: one window, every kind, monitor-derived position.
    fn spawn_initial(self: &Rc<Self>) {
        let panel = Panel::new(
            self,
            Vec::new(),
            Margins {
                left: geom::EDGE_GAP,
                top: geom::EDGE_GAP,
            },
            DEFAULT_SIZE,
        );
        // Set BEFORE `add` presents, because presenting realizes the window and
        // realize is where auto-placement happens.
        panel.auto_place.set(true);
        self.add(panel);
    }

    /// Adopt a panel and show it.
    ///
    /// Presenting lives here rather than in `Panel::new` so that a caller can
    /// finish configuring a panel — placement flag, monitor, back-dated drag
    /// origin — before the synchronous realize that presenting triggers.
    fn add(self: &Rc<Self>, panel: Rc<Panel>) {
        self.panels.borrow_mut().push(panel.clone());
        panel.win.present();
        // A torn-off panel is seeded with the cached snapshot, so it already
        // knows its one row and can size to it on the first frame.
        panel.fit_to_content();
    }

    fn remove(self: &Rc<Self>, panel: &Rc<Panel>) {
        self.panels.borrow_mut().retain(|p| !Rc::ptr_eq(p, panel));
        panel.close();
    }

    fn count(&self) -> usize {
        self.panels.borrow().len()
    }

    /// One feed, many panels: every panel sees every snapshot and renders the
    /// subset it owns.
    fn dispatch(&self, update: Update) {
        let panels = self.panels.borrow().clone();
        match update {
            Update::Snapshot(snapshot, _origin) => {
                // Cached first, so a panel born during this dispatch — or at
                // any point before the next one — has something to render.
                *self.last_snapshot.borrow_mut() = Some(snapshot.clone());

                for panel in &panels {
                    panel.content.apply_snapshot(&snapshot);
                    // The rows may have changed shape, and a panel the user has
                    // not sized follows them.
                    panel.fit_to_content();
                }
            }
            Update::Error(message) => {
                for panel in &panels {
                    panel.content.show_error(&message);
                }
            }
        }
    }

    /// The nearest other panel within the merge threshold, if any.
    fn merge_target(&self, dragged: &Rc<Panel>) -> Option<Rc<Panel>> {
        let rect = dragged.rect();
        let mut best: Option<(i32, Rc<Panel>)> = None;

        for other in self.panels.borrow().iter() {
            if Rc::ptr_eq(other, dragged) {
                continue;
            }
            let other_rect = other.rect();
            if !geom::rects_within(rect, other_rect, geom::MERGE_THRESHOLD) {
                continue;
            }
            // Ranked by gap so the closest wins when several are in range.
            let gap = geom::rect_gap(rect, other_rect);
            let closer = match &best {
                Some((best_gap, _)) => gap < *best_gap,
                None => true,
            };
            if closer {
                best = Some((gap, other.clone()));
            }
        }

        best.map(|(_, panel)| panel)
    }

    /// The stationary panel absorbs the dragged one, keeping its own position,
    /// size and pin state. The dragged window is destroyed.
    fn merge(self: &Rc<Self>, dragged: &Rc<Panel>, into: &Rc<Panel>) {
        // `absorb` re-renders from the survivor's own copy of the last
        // snapshot, and that copy is always current: every panel is seeded at
        // construction and fed by every dispatch, so `all` holds the same
        // snapshot the cache does. Re-applying the cache here would therefore
        // be a no-op on the rows — and NOT a no-op overall, because the
        // unchanged path in `apply_snapshot` hides the status label, which
        // would silently wipe an error banner the survivor is displaying.
        into.content.absorb(dragged.content.kinds());

        // Reading the cache here is safe in a way that re-applying it is not:
        // this path goes through `set_kinds`, which re-renders without touching
        // the status label, so an error banner on the survivor survives.
        let restored = match self.last_snapshot.borrow().as_ref() {
            Some(snapshot) => into.content.restore_wildcard_if_complete(&snapshot.windows),
            None => false,
        };

        // The survivor gained rows; if it is not user-sized it grows to hold
        // them rather than scrolling them out of sight.
        into.fit_to_content();

        self.remove(dragged);

        if std::env::var_os("GNOMON_DEBUG_GEOM").is_some() {
            eprintln!(
                "gnomon geom[merge]: absorbed{}, {} panels remain",
                if restored {
                    ", wildcard restored"
                } else {
                    ""
                },
                self.count()
            );
        }
    }

    /// SIGUSR1: if any panel is pinned, unpin them all; otherwise pin them all.
    ///
    /// Asymmetric on purpose. A click-through panel cannot be clicked to
    /// recover, so the signal must always be able to reach a state where every
    /// panel is interactive again.
    fn toggle_all_pins(self: &Rc<Self>) {
        let panels = self.panels.borrow().clone();
        let any_pinned = panels.iter().any(|p| p.pinned.get());

        for panel in &panels {
            set_pinned(panel, !any_pinned);
        }
    }
}

/// Run the GUI. `toplevel` skips layer-shell entirely — a debugging escape
/// hatch for compositors where the layer surface misbehaves.
pub fn run(toplevel: bool) -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();

    app.connect_startup(|_| {
        // First point at which GTK is initialised, so this is where its worker
        // threads exist to be checked.
        pin::verify_sigusr1_blocked("GTK startup");
        load_css();
    });
    app.connect_activate(move |app| build_layout(app, toplevel));
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

fn build_layout(app: &adw::Application, toplevel: bool) {
    // The MASK is installed in main(), before any library can spawn a thread.
    // Only the sigwait thread is created here, and it can be created at any
    // time — it inherits the mask like everything else.
    let (sig_tx, sig_rx) = async_channel::unbounded::<()>();
    pin::watch_sigusr1(sig_tx);

    // The surface exists by now, so the render thread GSK spawns lazily is
    // included in this sweep as well.
    pin::verify_sigusr1_blocked("layout construction");

    let layout = Layout::new(app, toplevel);
    layout.spawn_initial();

    // One feed for every panel.
    {
        let layout = layout.clone();
        let (tx, rx) = async_channel::unbounded::<Update>();
        feed::spawn(tx);
        glib::spawn_future_local(async move {
            while let Ok(update) = rx.recv().await {
                layout.dispatch(update);
            }
        });
    }

    // SIGUSR1 toggles every panel at once.
    {
        let layout = layout.clone();
        glib::spawn_future_local(async move {
            while sig_rx.recv().await.is_ok() {
                layout.toggle_all_pins();
            }
        });
    }

    pin::write_pid_file();
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

/// One gesture, three outcomes: resize, tear-off, or move. The mode is decided
/// at drag-begin and never changes for the rest of the sequence.
fn wire_drag(panel: &Rc<Panel>) {
    let drag = gtk::GestureDrag::new();
    drag.set_button(gdk::BUTTON_PRIMARY);
    // Capture: see the press before any child can consume it. The 8px band sits
    // over the ScrolledWindow and the root box, either of which could otherwise
    // take the sequence first.
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);

    {
        let panel = panel.clone();
        drag.connect_drag_begin(move |_, x, y| {
            panel.dragging.set(true);
            *panel.drag_target.borrow_mut() = None;
            *panel.drag_row.borrow_mut() = None;

            // A new gesture ends the observation window opened by any previous
            // tear. Without this a request minutes later would still print
            // under the tear label and read as a consequence of the tear.
            panel.trace_next_alloc.set(false);
            panel.trace_next_request.set(false);

            if panel.pinned.get() {
                panel.drag_zone.set(Zone::None);
                return;
            }

            let size = panel.panel_size();
            let zone = geom::zone_at(x as i32, y as i32, size);
            panel.debug_drag_begin(x, y, size, zone);

            panel.drag_zone.set(zone);
            panel.grab.set((x, y));
            panel.drag_origin.set(panel.margins.get());
            panel.resize_origin.set(size);

            // An edge press always resizes and never tears. A row press only
            // becomes a candidate for tearing when there is more than one row
            // to tear from.
            if !zone.is_resize() && panel.content.row_count() >= 2 {
                if let Some(kind) = panel.content.kind_at(y) {
                    panel
                        .drag_row_top
                        .set(panel.content.row_top(&kind).unwrap_or(y));
                    *panel.drag_row.borrow_mut() = Some(kind);
                }
            }

            if zone.is_resize() {
                (panel.content.set_resizing)(true);
            }
        });
    }

    {
        let panel = panel.clone();
        drag.connect_drag_update(move |_, dx, dy| {
            if panel.pinned.get() || !panel.layered {
                return;
            }

            // Once torn, this gesture drives the NEW panel and the source stays
            // exactly where it is.
            let torn = panel.drag_target.borrow().clone();
            if let Some(target) = torn {
                target.apply_move(dx, dy);
                return;
            }

            let zone = panel.drag_zone.get();
            if zone.is_resize() {
                let point = panel.pointer_in_monitor(dx, dy);
                panel.apply_resize(zone, point, "resize-edge");
                return;
            }

            // A drag that began on a row tears only once the pointer LEAVES the
            // panel. While it is still over the panel the gesture is an ordinary
            // move and falls through, so the panel tracks the pointer exactly as
            // a non-row drag does. The source therefore keeps whatever position
            // the in-panel part of the drag gave it, which is where the user put
            // it, and stops moving the moment the row tears out.
            let candidate = panel.drag_row.borrow().clone();
            if let Some(kind) = candidate {
                let pointer = panel.pointer_in_monitor(dx, dy);
                if geom::point_outside(panel.drag_origin_rect(), pointer) {
                    if let Some(new_panel) = tear_off(&panel, &kind, dx, dy) {
                        *panel.drag_target.borrow_mut() = Some(new_panel);
                        *panel.drag_row.borrow_mut() = None;
                        return;
                    }
                    // The tear could not happen at all. Stop testing for it so
                    // the rest of the gesture is a plain move.
                    *panel.drag_row.borrow_mut() = None;
                }
            }

            panel.apply_move(dx, dy);
        });
    }

    {
        let panel = panel.clone();
        drag.connect_drag_end(move |_, dx, dy| {
            panel.dragging.set(false);
            let zone = panel.drag_zone.get();
            panel.drag_zone.set(Zone::None);

            *panel.drag_row.borrow_mut() = None;

            // Whoever was actually being moved is who settles.
            let torn = panel.drag_target.borrow().clone();
            *panel.drag_target.borrow_mut() = None;
            let moved = torn.clone().unwrap_or_else(|| panel.clone());

            if zone.is_resize() {
                (panel.content.set_resizing)(false);
                // A resize gesture that never changed the size leaves the panel
                // auto-fitting, and the fits it suppressed can now happen.
                panel.fit_to_content();
                return;
            }
            if panel.pinned.get() || !panel.layered {
                return;
            }

            // A press with no movement is a click, and a click settles nothing.
            // GestureDrag reports begin-then-end for a bare click, so without
            // this a click on a panel resting near another would merge the two
            // and destroy a window the user never dragged.
            if torn.is_none() && dx as i32 == 0 && dy as i32 == 0 {
                return;
            }

            // Merge takes priority: snapping a panel that is about to be
            // absorbed would just be wasted motion.
            let merged = match moved.layout.upgrade() {
                Some(layout) => match layout.merge_target(&moved) {
                    Some(into) => {
                        layout.merge(&moved, &into);
                        true
                    }
                    None => false,
                },
                None => false,
            };

            if !merged {
                snap_into_place(&moved);
            }

            // A tear leaves the SOURCE wherever the in-panel part of the drag
            // put it, which can be hard against a monitor edge. It snaps like
            // the end of any other move — but it is never merged: the gesture
            // was a tear, and silently absorbing the panel the user tore FROM
            // would be the opposite of what they asked for.
            if !Rc::ptr_eq(&moved, &panel) {
                snap_into_place(&panel);
            }
        });
    }

    panel.content.overlay.add_controller(drag);
}

/// End-of-move settle: pull the panel to a monitor edge if it is close enough.
fn snap_into_place(panel: &Rc<Panel>) {
    let m = panel.margins.get();
    let snapped = geom::snap_margins(m.left, m.top, panel.panel_size(), panel.monitor.get());
    panel.margins.set(snapped);
    panel.apply_margins("snap", panel.panel_size(), "allocated");
}

/// Detach one row into a panel of its own.
///
/// The new panel is placed so the pointer keeps the same position within the
/// torn row that it had before the tear, and its drag origin is back-dated so
/// the source panel's still-running gesture drives it seamlessly.
fn tear_off(panel: &Rc<Panel>, kind: &str, dx: f64, dy: f64) -> Option<Rc<Panel>> {
    let layout = panel.layout.upgrade()?;

    let (gx, gy) = panel.grab.get();
    let row_top = panel.drag_row_top.get();
    // Where the pointer sat inside the row, which is where it should sit inside
    // the new panel.
    let new_grab = (gx, gy - row_top);

    let pointer = panel.pointer_in_monitor(dx, dy);
    let placed = Margins {
        left: pointer.0 - new_grab.0 as i32,
        top: pointer.1 - new_grab.1 as i32,
    };

    let size = panel.panel_size();

    // Four points across the tear, so the source panel's geometry can be
    // followed through the synchronous row removal and the two asynchronous
    // events that follow it.
    panel.debug_source("tear-source-before", "about to remove the row");

    // The source loses the row, and materialises its wildcard in the process.
    panel.content.remove_kind(kind);

    panel.debug_source("tear-source-after", "row removed and re-rendered");
    panel.trace_next_alloc.set(true);
    panel.trace_next_request.set(true);

    // The source has one fewer row. If it is not user-sized it closes the gap
    // that row left behind rather than keeping the hole.
    panel.fit_to_content();

    let new_panel = Panel::new(&layout, vec![kind.to_string()], placed, size);
    new_panel.monitor.set(panel.monitor.get());

    // The source's size is inherited only if the source's size was a decision.
    // Otherwise `size` is merely a provisional value that avoids a zero-sized
    // flash, and `Layout::add` immediately fits the new panel to its one row.
    new_panel.user_sized.set(panel.user_sized.get());

    // Back-date the origin so `origin + cumulative offset` lands on `placed`
    // right now, and tracks the pointer from here on.
    new_panel.grab.set(new_grab);
    new_panel.drag_origin.set(Margins {
        left: placed.left - dx as i32,
        top: placed.top - dy as i32,
    });
    new_panel.resize_origin.set(size);

    panel.debug_tear(kind, placed);
    layout.add(new_panel.clone());
    Some(new_panel)
}

/// Track the pointer and show the matching resize cursor.
fn wire_cursor(panel: &Rc<Panel>) {
    let motion = gtk::EventControllerMotion::new();

    {
        let panel = panel.clone();
        motion.connect_motion(move |_, x, y| {
            if panel.pinned.get() {
                panel.win.set_cursor(None);
                return;
            }
            let zone = geom::zone_at(x as i32, y as i32, panel.panel_size());
            panel.last_motion.set((x, y));
            panel.last_motion_zone.set(zone);

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

    panel.content.overlay.add_controller(motion);
}

/// Middle-click toggles THIS panel's pin.
///
/// Only reachable while unpinned, which is correct: a click-through panel
/// cannot be clicked, and SIGUSR1 is the escape hatch.
fn wire_middle_click(panel: &Rc<Panel>) {
    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_MIDDLE);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);

    {
        let panel = panel.clone();
        click.connect_pressed(move |gesture, _, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            set_pinned(&panel, !panel.pinned.get());
        });
    }

    panel.content.overlay.add_controller(click);
}

/// Right-button drag resizes from the bottom-right, wherever it starts.
///
/// The edge zones are only 8px, so this stays the forgiving way to resize.
fn wire_right_button_resize(panel: &Rc<Panel>) {
    let drag = gtk::GestureDrag::new();
    drag.set_button(gdk::BUTTON_SECONDARY);
    // Same widget and same phase as the primary drag, so both read the same
    // coordinate space.
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);

    {
        let panel = panel.clone();
        drag.connect_drag_begin(move |gesture, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            panel.dragging.set(true);
            panel.grab.set((x, y));
            panel.drag_origin.set(panel.margins.get());
            panel.resize_origin.set(panel.panel_size());
            (panel.content.set_resizing)(true);
        });
    }

    {
        let panel = panel.clone();
        drag.connect_drag_update(move |_, dx, dy| {
            if panel.pinned.get() || !panel.layered {
                return;
            }
            let point = panel.pointer_in_monitor(dx, dy);
            panel.apply_resize(Zone::BottomRight, point, "resize-rmb");
        });
    }

    {
        let panel = panel.clone();
        drag.connect_drag_end(move |_, _, _| {
            panel.dragging.set(false);
            (panel.content.set_resizing)(false);
            // A resize that ended without changing the size never latched
            // `user_sized`, so the panel is still auto-fitting: settle it.
            panel.fit_to_content();
        });
    }

    panel.content.overlay.add_controller(drag);
}

fn set_pinned(panel: &Rc<Panel>, pinned: bool) {
    panel.pinned.set(pinned);

    if pinned {
        panel.win.add_css_class("pinned");
        // No resize cursors on a click-through panel.
        panel.win.set_cursor(None);
    } else {
        panel.win.remove_css_class("pinned");
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
