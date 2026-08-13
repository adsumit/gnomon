//! Pure positioning maths for the layer surface.
//!
//! The surface is anchored Top+Left, so its margins *are* its absolute position
//! from the monitor's top-left corner. Nothing here touches GTK.

/// An edge closer than this to the monitor's edge snaps to it.
pub const SNAP_THRESHOLD: i32 = 48;
/// The gap a snapped edge settles at.
pub const EDGE_GAP: i32 = 12;

/// Smallest panel an edge drag will produce.
pub const MIN_WIDTH: i32 = 200;
pub const MIN_HEIGHT: i32 = 100;

/// Absolute position from the monitor's top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Margins {
    pub left: i32,
    pub top: i32,
}

/// Where the panel should sit initially: top-right, with a 12px gap.
///
/// The result is clamped. A caller that passes a not-yet-allocated panel size
/// would otherwise compute a position almost entirely off the right edge, which
/// is precisely how the panel went missing after M4.
pub fn initial_margins(panel: (i32, i32), monitor: (i32, i32)) -> Margins {
    let raw = Margins {
        left: monitor.0 - panel.0 - EDGE_GAP,
        top: EDGE_GAP,
    };

    clamp_margins(raw.left, raw.top, panel, monitor)
}

/// Keep the panel entirely on screen.
///
/// Allowing it to hang off an edge made release-time snapping look like a yank:
/// the panel would sit half off the bottom during the drag, then jump fully
/// back. Constraining the drag itself makes snap a small correction instead.
pub fn clamp_margins(left: i32, top: i32, panel: (i32, i32), monitor: (i32, i32)) -> Margins {
    let (panel_w, panel_h) = panel;
    let (mon_w, mon_h) = monitor;

    let max_left = mon_w - panel_w;
    let max_top = mon_h - panel_h;

    Margins {
        // A panel larger than the monitor crosses the bounds, so `min` keeps
        // the lower edge authoritative rather than producing a value above it.
        left: left.clamp(0.min(max_left), max_left.max(0)),
        top: top.clamp(0.min(max_top), max_top.max(0)),
    }
}

/// Snap each axis independently to whichever monitor edge is within reach.
///
/// Returns the position unchanged when no edge is close, so a corner snaps on
/// both axes and a free-floating panel snaps on neither.
pub fn snap_margins(left: i32, top: i32, panel: (i32, i32), monitor: (i32, i32)) -> Margins {
    let (panel_w, panel_h) = panel;
    let (mon_w, mon_h) = monitor;

    // Distance from each panel edge to the matching monitor edge.
    let right_gap = mon_w - (left + panel_w);
    let bottom_gap = mon_h - (top + panel_h);

    let snapped_left = if left.abs() <= SNAP_THRESHOLD {
        EDGE_GAP
    } else if right_gap.abs() <= SNAP_THRESHOLD {
        mon_w - panel_w - EDGE_GAP
    } else {
        left
    };

    let snapped_top = if top.abs() <= SNAP_THRESHOLD {
        EDGE_GAP
    } else if bottom_gap.abs() <= SNAP_THRESHOLD {
        mon_h - panel_h - EDGE_GAP
    } else {
        top
    };

    Margins {
        left: snapped_left,
        top: snapped_top,
    }
}

// Responsive thresholds. Enter and exit differ so a drag hovering near a
// boundary cannot oscillate: crossing in and crossing back out need different
// widths, and the gap between them is a dead band where nothing changes.
pub const COMPACT_ENTER_W: i32 = 240;
pub const COMPACT_EXIT_W: i32 = 252;
pub const TIGHT_ENTER_W: i32 = 200;
pub const TIGHT_EXIT_W: i32 = 212;
pub const TIGHT_ENTER_H: i32 = 150;
pub const TIGHT_EXIT_H: i32 = 162;

/// Decide the responsive flags from an allocation, with hysteresis.
///
/// Pure, so the oscillation behaviour is testable without a display. Returns
/// `(compact, tight)`; an unallocated size leaves both unchanged.
pub fn responsive_state(
    width: i32,
    height: i32,
    compact: bool,
    tight: bool,
) -> (bool, bool) {
    if width <= 0 || height <= 0 {
        return (compact, tight);
    }

    let compact = if compact {
        // Stay compact until comfortably clear of the entry threshold.
        width <= COMPACT_EXIT_W
    } else {
        width < COMPACT_ENTER_W
    };

    let tight = if tight {
        // Both axes must clear their exit thresholds to leave tight mode.
        !(width > TIGHT_EXIT_W && height > TIGHT_EXIT_H)
    } else {
        width < TIGHT_ENTER_W || height < TIGHT_ENTER_H
    };

    (compact, tight)
}

/// New margins for a move drag.
///
/// `origin` is the margins captured at drag-begin and `(dx, dy)` is the
/// gesture's cumulative offset from the press point. The origin must be the
/// drag-begin value on every event — feeding the previous result back in
/// double-counts the offset and the panel accelerates away from the pointer.
pub fn move_margins(
    origin: Margins,
    dx: i32,
    dy: i32,
    panel: (i32, i32),
    monitor: (i32, i32),
) -> Margins {
    clamp_margins(origin.left + dx, origin.top + dy, panel, monitor)
}

/// The pointer's monitor-space position during a drag.
///
/// `origin` and `grab` are captured at drag-begin and `(dx, dy)` is the
/// gesture's cumulative offset from the press point. All three are drag-begin
/// values: using the *current* margins here re-adds movement the offset already
/// contains, which is exactly how the panel came to accelerate away from the
/// cursor.
pub fn drag_point(origin: Margins, grab: (i32, i32), dx: i32, dy: i32) -> (i32, i32) {
    (origin.left + grab.0 + dx, origin.top + grab.1 + dy)
}

/// Thickness of the interactive edge and corner resize band.
pub const RESIZE_EDGE: i32 = 8;

/// Which part of the panel a point falls in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    None,
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Zone {
    /// The CSS cursor name for this zone, or `None` for the default cursor.
    pub fn cursor_name(self) -> Option<&'static str> {
        match self {
            Zone::None => None,
            Zone::Top => Some("n-resize"),
            Zone::Bottom => Some("s-resize"),
            Zone::Left => Some("w-resize"),
            Zone::Right => Some("e-resize"),
            Zone::TopLeft => Some("nw-resize"),
            Zone::TopRight => Some("ne-resize"),
            Zone::BottomLeft => Some("sw-resize"),
            Zone::BottomRight => Some("se-resize"),
        }
    }

    pub fn is_resize(self) -> bool {
        self != Zone::None
    }
}

/// Classify a surface-local point. Corners win over edges.
pub fn zone_at(x: i32, y: i32, size: (i32, i32)) -> Zone {
    let (w, h) = size;

    // Outside the surface is not a resize zone.
    if x < 0 || y < 0 || x >= w || y >= h {
        return Zone::None;
    }

    let left = x < RESIZE_EDGE;
    let right = x >= w - RESIZE_EDGE;
    let top = y < RESIZE_EDGE;
    let bottom = y >= h - RESIZE_EDGE;

    // Corners first, so a corner never degrades to a single edge.
    if top && left {
        return Zone::TopLeft;
    }
    if top && right {
        return Zone::TopRight;
    }
    if bottom && left {
        return Zone::BottomLeft;
    }
    if bottom && right {
        return Zone::BottomRight;
    }

    if top {
        Zone::Top
    } else if bottom {
        Zone::Bottom
    } else if left {
        Zone::Left
    } else if right {
        Zone::Right
    } else {
        Zone::None
    }
}

/// New margins and size for a resize drag.
///
/// `point` is the pointer in monitor space. `margins` and `size` are the values
/// captured at drag-begin, so every frame is computed from the pointer's
/// absolute position rather than from accumulated deltas.
///
/// A Left or Top drag pins the opposite edge: the size grows and the margin
/// shrinks by the same amount. Crucially the margin is derived from the
/// *clamped* size, so when the minimum is reached the panel stops moving too
/// rather than sliding while refusing to shrink.
pub fn resize_from(
    zone: Zone,
    point: (i32, i32),
    margins: Margins,
    size: (i32, i32),
    monitor: (i32, i32),
) -> (Margins, (i32, i32)) {
    let (w0, h0) = size;
    let right_edge = margins.left + w0;
    let bottom_edge = margins.top + h0;

    let mut left = margins.left;
    let mut top = margins.top;
    let mut width = w0;
    let mut height = h0;

    match zone {
        Zone::Right | Zone::TopRight | Zone::BottomRight => {
            // Left edge pinned: grow right, up to the monitor's right edge.
            let available = (monitor.0 - margins.left).max(MIN_WIDTH);
            width = (point.0 - margins.left).clamp(MIN_WIDTH, available);
        }
        Zone::Left | Zone::TopLeft | Zone::BottomLeft => {
            // Right edge pinned: grow left, up to the monitor's left edge.
            let available = right_edge.max(MIN_WIDTH);
            width = (right_edge - point.0).clamp(MIN_WIDTH, available);
            left = right_edge - width;
        }
        _ => {}
    }

    match zone {
        Zone::Bottom | Zone::BottomLeft | Zone::BottomRight => {
            let available = (monitor.1 - margins.top).max(MIN_HEIGHT);
            height = (point.1 - margins.top).clamp(MIN_HEIGHT, available);
        }
        Zone::Top | Zone::TopLeft | Zone::TopRight => {
            let available = bottom_edge.max(MIN_HEIGHT);
            height = (bottom_edge - point.1).clamp(MIN_HEIGHT, available);
            top = bottom_edge - height;
        }
        _ => {}
    }

    (Margins { left, top }, (width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PANEL: (i32, i32) = (300, 150);
    const MONITOR: (i32, i32) = (1920, 1080);

    #[test]
    fn initial_position_is_top_right() {
        assert_eq!(
            initial_margins(PANEL, MONITOR),
            Margins {
                left: 1920 - 300 - 12,
                top: 12
            }
        );
    }

    #[test]
    fn initial_margins_survive_a_zero_width_panel() {
        // The M4 regression: reading the panel size before allocation yields 0,
        // which naively places the panel off the right edge of the monitor.
        let m = initial_margins((0, 150), MONITOR);

        // Whatever it returns must already satisfy the clamp — that is, feeding
        // it back through must change nothing.
        assert_eq!(
            clamp_margins(m.left, m.top, (0, 150), MONITOR),
            m,
            "initial_margins must return an already-clamped position"
        );
        assert!(
            m.left <= MONITOR.0,
            "left {} would put the panel off the right edge",
            m.left
        );
    }

    // ---- clamp: one test per edge ----

    #[test]
    fn clamp_past_left_edge() {
        // The panel must stay fully on screen, so it stops flush at 0.
        let m = clamp_margins(-1000, 100, PANEL, MONITOR);
        assert_eq!(m.left, 0);
        assert_eq!(m.top, 100, "the other axis is untouched");
    }

    #[test]
    fn clamp_past_right_edge() {
        let m = clamp_margins(9999, 100, PANEL, MONITOR);
        assert_eq!(m.left, MONITOR.0 - PANEL.0); // 1620, right edge flush
    }

    #[test]
    fn clamp_past_top_edge() {
        let m = clamp_margins(100, -1000, PANEL, MONITOR);
        assert_eq!(m.top, 0);
    }

    #[test]
    fn clamp_past_bottom_edge() {
        let m = clamp_margins(100, 9999, PANEL, MONITOR);
        assert_eq!(m.top, MONITOR.1 - PANEL.1); // 930, bottom edge flush
    }

    #[test]
    fn clamp_keeps_an_oversized_panel_at_the_origin() {
        // Taller and wider than the monitor: the bounds cross, and the result
        // must still be the top-left corner rather than a positive offset.
        let huge = (3000, 2000);
        assert_eq!(
            clamp_margins(500, 500, huge, MONITOR),
            Margins { left: 0, top: 0 }
        );
    }

    #[test]
    fn clamp_leaves_an_on_screen_panel_alone() {
        assert_eq!(
            clamp_margins(500, 400, PANEL, MONITOR),
            Margins {
                left: 500,
                top: 400
            }
        );
    }

    // ---- snap ----

    #[test]
    fn snap_near_left_edge() {
        let m = snap_margins(20, 500, PANEL, MONITOR);
        assert_eq!(m.left, EDGE_GAP);
        assert_eq!(m.top, 500, "far from top and bottom, so unchanged");
    }

    #[test]
    fn snap_near_right_edge() {
        // right_gap = 1920 - (1600 + 300) = 20, within threshold.
        let m = snap_margins(1600, 500, PANEL, MONITOR);
        assert_eq!(m.left, MONITOR.0 - PANEL.0 - EDGE_GAP); // 1608
    }

    #[test]
    fn snap_near_top_edge() {
        let m = snap_margins(800, 30, PANEL, MONITOR);
        assert_eq!(m.top, EDGE_GAP);
        assert_eq!(m.left, 800);
    }

    #[test]
    fn snap_near_bottom_edge() {
        // bottom_gap = 1080 - (900 + 150) = 30, within threshold.
        let m = snap_margins(800, 900, PANEL, MONITOR);
        assert_eq!(m.top, MONITOR.1 - PANEL.1 - EDGE_GAP); // 918
    }

    #[test]
    fn snap_far_from_every_edge_does_nothing() {
        assert_eq!(
            snap_margins(800, 500, PANEL, MONITOR),
            Margins {
                left: 800,
                top: 500
            }
        );
    }

    #[test]
    fn snap_corner_snaps_both_axes() {
        // Near the bottom-right corner: both axes must move.
        let m = snap_margins(1600, 900, PANEL, MONITOR);
        assert_eq!(m.left, MONITOR.0 - PANEL.0 - EDGE_GAP);
        assert_eq!(m.top, MONITOR.1 - PANEL.1 - EDGE_GAP);
    }

    #[test]
    fn snap_top_left_corner_snaps_both_axes() {
        let m = snap_margins(10, 10, PANEL, MONITOR);
        assert_eq!(m, Margins { left: 12, top: 12 });
    }

    #[test]
    fn snap_threshold_is_inclusive_at_the_boundary() {
        assert_eq!(snap_margins(48, 500, PANEL, MONITOR).left, EDGE_GAP);
        assert_eq!(snap_margins(49, 500, PANEL, MONITOR).left, 49);
    }

    // ---- size ----

    // ---- responsive hysteresis ----

    #[test]
    fn compact_enters_below_the_entry_threshold() {
        assert!(responsive_state(239, 400, false, false).0);
        assert!(
            !responsive_state(240, 400, false, false).0,
            "the entry threshold itself is not below it"
        );
    }

    #[test]
    fn compact_leaves_only_above_the_exit_threshold() {
        // Already compact: 245 is past the entry threshold but inside the dead
        // band, so it must stay compact.
        assert!(responsive_state(245, 400, true, false).0);
        assert!(responsive_state(252, 400, true, false).0);
        assert!(!responsive_state(253, 400, true, false).0);
    }

    #[test]
    fn compact_dead_band_holds_both_states() {
        // The same width yields a different answer depending on where it came
        // from — that is the whole point of hysteresis.
        for width in [241, 246, 252] {
            assert!(responsive_state(width, 400, true, false).0);
            assert!(!responsive_state(width, 400, false, false).0);
        }
    }

    #[test]
    fn tight_enters_on_either_axis() {
        assert!(responsive_state(199, 400, false, false).1);
        assert!(responsive_state(400, 149, false, false).1);
        assert!(!responsive_state(400, 400, false, false).1);
    }

    #[test]
    fn tight_leaves_only_when_both_axes_clear() {
        // Width clear, height still inside: stays tight.
        assert!(responsive_state(300, 155, false, true).1);
        // Height clear, width still inside: stays tight.
        assert!(responsive_state(205, 300, false, true).1);
        // Both clear: leaves.
        assert!(!responsive_state(213, 163, false, true).1);
    }

    #[test]
    fn tight_dead_band_holds_both_states() {
        for (w, h) in [(205, 155), (212, 162)] {
            assert!(responsive_state(w, h, false, true).1);
            assert!(!responsive_state(w, h, false, false).1);
        }
    }

    #[test]
    fn unallocated_size_changes_nothing() {
        assert_eq!(responsive_state(0, 0, true, true), (true, true));
        assert_eq!(responsive_state(0, 0, false, false), (false, false));
        assert_eq!(responsive_state(300, 0, false, false), (false, false));
    }

    // ---- drag offsets must not accumulate ----
    //
    // GTK's gesture offset is cumulative from the press point, so every event
    // carries the WHOLE drag. Anything that folds an event's result back in as
    // the next event's base double-counts, and the panel accelerates away from
    // the cursor. These pin the property the defect violated.

    const ORIGIN: Margins = Margins { left: 500, top: 300 };
    /// One drag delivered as three cumulative events.
    const OFFSETS: [(i32, i32); 3] = [(10, 5), (20, 10), (30, 15)];

    #[test]
    fn move_depends_only_on_the_latest_offset() {
        let one_step = move_margins(ORIGIN, 30, 15, S0, MONITOR);

        // Replaying the sequence, always from the drag-begin origin.
        let mut last = ORIGIN;
        for (dx, dy) in OFFSETS {
            last = move_margins(ORIGIN, dx, dy, S0, MONITOR);
        }

        assert_eq!(last, one_step, "the count of events must not matter");
        assert_eq!(one_step, Margins { left: 530, top: 315 });
    }

    #[test]
    fn move_double_counts_if_fed_its_own_output() {
        // The 09fc6de defect, pinned. Chaining the result in as the next base
        // sums every offset: 500 + 10 + 20 + 30.
        let mut chained = ORIGIN;
        for (dx, dy) in OFFSETS {
            chained = move_margins(chained, dx, dy, S0, MONITOR);
        }

        assert_eq!(chained, Margins { left: 560, top: 330 });
        assert_ne!(
            chained,
            move_margins(ORIGIN, 30, 15, S0, MONITOR),
            "chaining must NOT agree with the single step — that is the bug"
        );
    }

    #[test]
    fn resize_right_depends_only_on_the_latest_offset() {
        // Press inside the right edge band.
        let grab = (295, 75);
        let point = |dx: i32| (ORIGIN.left + grab.0 + dx, ORIGIN.top + grab.1);

        let one_step = resize_from(Zone::Right, point(60), ORIGIN, S0, MONITOR);

        let mut last = (ORIGIN, S0);
        for dx in [20, 40, 60] {
            last = resize_from(Zone::Right, point(dx), ORIGIN, S0, MONITOR);
        }

        assert_eq!(last, one_step);
        assert_eq!(last.0, ORIGIN, "a right drag never moves the margins");
        assert_eq!(last.1 .0, 355, "width follows the pointer: 295 + 60");
    }

    #[test]
    fn resize_left_depends_only_on_the_latest_offset() {
        // Press inside the left edge band, dragging outward (negative dx).
        let grab = (4, 75);
        let point = |dx: i32| (ORIGIN.left + grab.0 + dx, ORIGIN.top + grab.1);

        let one_step = resize_from(Zone::Left, point(-60), ORIGIN, S0, MONITOR);

        let mut last = (ORIGIN, S0);
        for dx in [-20, -40, -60] {
            last = resize_from(Zone::Left, point(dx), ORIGIN, S0, MONITOR);
        }

        assert_eq!(last, one_step);
        assert_eq!(
            last.0.left + last.1 .0,
            ORIGIN.left + S0.0,
            "the right edge stays pinned across the whole sequence"
        );
    }

    #[test]
    fn resize_from_is_anchor_invariant() {
        // resize_from itself is immune: it always measures from the pinned
        // opposite edge, which chaining preserves. Feeding its own output back
        // in changes nothing. The resize defect therefore lived entirely in the
        // POINT computation, not here — which is what the next two tests cover.
        let grab = (295, 75);
        let point = |dx: i32| (ORIGIN.left + grab.0 + dx, ORIGIN.top + grab.1);

        let mut chained = (ORIGIN, S0);
        for dx in [20, 40, 60] {
            chained = resize_from(Zone::Right, point(dx), chained.0, chained.1, MONITOR);
        }

        assert_eq!(chained, resize_from(Zone::Right, point(60), ORIGIN, S0, MONITOR));
    }

    #[test]
    fn drag_point_depends_only_on_the_latest_offset() {
        let grab = (295, 75);
        let one_step = drag_point(ORIGIN, grab, 60, 0);

        let mut last = (0, 0);
        for dx in [20, 40, 60] {
            last = drag_point(ORIGIN, grab, dx, 0);
        }

        assert_eq!(last, one_step);
        assert_eq!(one_step, (500 + 295 + 60, 300 + 75));
    }

    #[test]
    fn drag_point_double_counts_if_based_on_current_margins() {
        // The 09fc6de defect in the resize path: recomputing the base from the
        // margins we just moved re-adds the offset the gesture already carries.
        let grab = (295, 75);
        let mut base = ORIGIN;
        let mut chained = (0, 0);

        for dx in [20, 40, 60] {
            chained = drag_point(base, grab, dx, 0);
            // Stand in for a resize having shifted the origin by the offset.
            base = Margins {
                left: base.left + dx,
                top: base.top,
            };
        }

        assert_ne!(
            chained,
            drag_point(ORIGIN, grab, 60, 0),
            "using moved margins as the base must NOT agree — that is the bug"
        );
    }

    // ---- zone classification ----

    const SIZE: (i32, i32) = (300, 150);

    #[test]
    fn zone_interior_is_none() {
        assert_eq!(zone_at(150, 75, SIZE), Zone::None);
    }

    #[test]
    fn zone_each_edge() {
        assert_eq!(zone_at(150, 2, SIZE), Zone::Top);
        assert_eq!(zone_at(150, 147, SIZE), Zone::Bottom);
        assert_eq!(zone_at(2, 75, SIZE), Zone::Left);
        assert_eq!(zone_at(297, 75, SIZE), Zone::Right);
    }

    #[test]
    fn zone_each_corner_beats_the_edges() {
        assert_eq!(zone_at(2, 2, SIZE), Zone::TopLeft);
        assert_eq!(zone_at(297, 2, SIZE), Zone::TopRight);
        assert_eq!(zone_at(2, 147, SIZE), Zone::BottomLeft);
        assert_eq!(zone_at(297, 147, SIZE), Zone::BottomRight);
    }

    #[test]
    fn zone_boundary_pixels_on_the_left() {
        // RESIZE_EDGE is 8, so 0..=7 are the band and 8 is interior.
        assert_eq!(zone_at(0, 75, SIZE), Zone::Left);
        assert_eq!(zone_at(7, 75, SIZE), Zone::Left);
        assert_eq!(zone_at(8, 75, SIZE), Zone::None);
    }

    #[test]
    fn zone_boundary_pixels_on_the_right() {
        // w - RESIZE_EDGE == 292 is the first pixel of the band.
        assert_eq!(zone_at(291, 75, SIZE), Zone::None);
        assert_eq!(zone_at(292, 75, SIZE), Zone::Right);
        assert_eq!(zone_at(299, 75, SIZE), Zone::Right);
    }

    #[test]
    fn zone_boundary_pixels_vertically() {
        assert_eq!(zone_at(150, 0, SIZE), Zone::Top);
        assert_eq!(zone_at(150, 7, SIZE), Zone::Top);
        assert_eq!(zone_at(150, 8, SIZE), Zone::None);
        assert_eq!(zone_at(150, 141, SIZE), Zone::None);
        assert_eq!(zone_at(150, 142, SIZE), Zone::Bottom);
        assert_eq!(zone_at(150, 149, SIZE), Zone::Bottom);
    }

    #[test]
    fn zone_outside_the_surface_is_none() {
        assert_eq!(zone_at(-1, 75, SIZE), Zone::None);
        assert_eq!(zone_at(300, 75, SIZE), Zone::None);
        assert_eq!(zone_at(150, -1, SIZE), Zone::None);
        assert_eq!(zone_at(150, 150, SIZE), Zone::None);
    }

    #[test]
    fn zone_cursor_names_are_the_css_ones() {
        assert_eq!(Zone::None.cursor_name(), None);
        assert_eq!(Zone::Top.cursor_name(), Some("n-resize"));
        assert_eq!(Zone::Bottom.cursor_name(), Some("s-resize"));
        assert_eq!(Zone::Left.cursor_name(), Some("w-resize"));
        assert_eq!(Zone::Right.cursor_name(), Some("e-resize"));
        assert_eq!(Zone::TopLeft.cursor_name(), Some("nw-resize"));
        assert_eq!(Zone::TopRight.cursor_name(), Some("ne-resize"));
        assert_eq!(Zone::BottomLeft.cursor_name(), Some("sw-resize"));
        assert_eq!(Zone::BottomRight.cursor_name(), Some("se-resize"));
    }

    // ---- resize maths ----

    const M0: Margins = Margins { left: 500, top: 300 };
    const S0: (i32, i32) = (300, 150);

    #[test]
    fn resize_right_edge_changes_size_only() {
        // Pointer at x=900: width becomes 900 - 500 = 400.
        let (m, s) = resize_from(Zone::Right, (900, 400), M0, S0, MONITOR);
        assert_eq!(m, M0, "the anchored corner must not move");
        assert_eq!(s, (400, 150));
    }

    #[test]
    fn resize_bottom_edge_changes_size_only() {
        let (m, s) = resize_from(Zone::Bottom, (600, 700), M0, S0, MONITOR);
        assert_eq!(m, M0);
        assert_eq!(s, (300, 400));
    }

    #[test]
    fn resize_left_edge_moves_the_margin_with_the_size() {
        // Right edge is pinned at 500 + 300 = 800. Pointer at x=400:
        // width becomes 400, and the margin must follow to 800 - 400.
        let (m, s) = resize_from(Zone::Left, (400, 400), M0, S0, MONITOR);
        assert_eq!(s, (400, 150));
        assert_eq!(m.left, 400);
        assert_eq!(m.left + s.0, 800, "the right edge stays put");
        assert_eq!(m.top, M0.top);
    }

    #[test]
    fn resize_top_edge_moves_the_margin_with_the_size() {
        // Bottom edge pinned at 300 + 150 = 450. Pointer at y=200:
        // height becomes 250.
        let (m, s) = resize_from(Zone::Top, (600, 200), M0, S0, MONITOR);
        assert_eq!(s, (300, 250));
        assert_eq!(m.top, 200);
        assert_eq!(m.top + s.1, 450, "the bottom edge stays put");
        assert_eq!(m.left, M0.left);
    }

    #[test]
    fn resize_bottom_right_corner_moves_both_axes() {
        let (m, s) = resize_from(Zone::BottomRight, (900, 700), M0, S0, MONITOR);
        assert_eq!(m, M0, "both anchored edges stay put");
        assert_eq!(s, (400, 400));
    }

    #[test]
    fn resize_top_left_corner_moves_both_margins() {
        let (m, s) = resize_from(Zone::TopLeft, (400, 200), M0, S0, MONITOR);
        assert_eq!(s, (400, 250));
        assert_eq!(m, Margins { left: 400, top: 200 });
        assert_eq!(m.left + s.0, 800);
        assert_eq!(m.top + s.1, 450);
    }

    #[test]
    fn resize_top_right_corner() {
        let (m, s) = resize_from(Zone::TopRight, (900, 200), M0, S0, MONITOR);
        assert_eq!(s, (400, 250));
        assert_eq!(m.left, M0.left, "left edge pinned");
        assert_eq!(m.top, 200);
    }

    #[test]
    fn resize_bottom_left_corner() {
        let (m, s) = resize_from(Zone::BottomLeft, (400, 700), M0, S0, MONITOR);
        assert_eq!(s, (400, 400));
        assert_eq!(m.left, 400);
        assert_eq!(m.top, M0.top, "top edge pinned");
    }

    #[test]
    fn resize_left_edge_stops_moving_at_the_minimum_width() {
        // Dragging the left edge far past the minimum. Width must clamp to
        // MIN_WIDTH and the margin must stop with it: if the margin kept
        // tracking the pointer the panel would slide while refusing to shrink.
        let (m, s) = resize_from(Zone::Left, (790, 400), M0, S0, MONITOR);
        assert_eq!(s.0, MIN_WIDTH);
        assert_eq!(
            m.left,
            800 - MIN_WIDTH,
            "the margin is derived from the clamped size, not the pointer"
        );
        assert_eq!(m.left + s.0, 800, "the right edge is still pinned");
    }

    #[test]
    fn resize_top_edge_stops_moving_at_the_minimum_height() {
        let (m, s) = resize_from(Zone::Top, (600, 445), M0, S0, MONITOR);
        assert_eq!(s.1, MIN_HEIGHT);
        assert_eq!(m.top, 450 - MIN_HEIGHT);
        assert_eq!(m.top + s.1, 450);
    }

    #[test]
    fn resize_right_edge_stops_at_the_monitor_edge() {
        let (m, s) = resize_from(Zone::Right, (9999, 400), M0, S0, MONITOR);
        assert_eq!(m, M0);
        assert_eq!(s.0, MONITOR.0 - M0.left, "cannot extend past the screen");
    }

    #[test]
    fn resize_none_zone_changes_nothing() {
        let (m, s) = resize_from(Zone::None, (900, 700), M0, S0, MONITOR);
        assert_eq!(m, M0);
        assert_eq!(s, S0);
    }

}
