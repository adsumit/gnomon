//! Pure positioning maths for the layer surface.
//!
//! The surface is anchored Top+Left, so its margins *are* its absolute position
//! from the monitor's top-left corner. Nothing here touches GTK.

/// An edge closer than this to the monitor's edge snaps to it.
pub const SNAP_THRESHOLD: i32 = 48;
/// At least this much of the panel stays on screen on every side.
pub const MIN_VISIBLE: i32 = 48;
/// The gap a snapped edge settles at.
pub const EDGE_GAP: i32 = 12;

/// Smallest panel the resize grip will produce.
pub const MIN_WIDTH: i32 = 200;
pub const MIN_HEIGHT: i32 = 100;

/// Absolute position from the monitor's top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Margins {
    pub left: i32,
    pub top: i32,
}

/// Where the panel should sit initially: top-right, with a 12px gap.
pub fn initial_margins(panel: (i32, i32), monitor: (i32, i32)) -> Margins {
    Margins {
        left: monitor.0 - panel.0 - EDGE_GAP,
        top: EDGE_GAP,
    }
}

/// Keep at least [`MIN_VISIBLE`] pixels of the panel on screen on every side.
///
/// The lower bound is negative by design: the panel may hang off the left or
/// top, so long as a sliver remains reachable.
pub fn clamp_margins(left: i32, top: i32, panel: (i32, i32), monitor: (i32, i32)) -> Margins {
    let (panel_w, panel_h) = panel;
    let (mon_w, mon_h) = monitor;

    let min_left = MIN_VISIBLE - panel_w;
    let max_left = mon_w - MIN_VISIBLE;
    let min_top = MIN_VISIBLE - panel_h;
    let max_top = mon_h - MIN_VISIBLE;

    Margins {
        // `max` first: on a monitor narrower than the panel the bounds cross,
        // and clamping must not produce a value above `min_left`.
        left: left.clamp(min_left.min(max_left), max_left),
        top: top.clamp(min_top.min(max_top), max_top),
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

/// Clamp a grip-driven size to the allowed range.
pub fn clamp_size(width: i32, height: i32, monitor: (i32, i32)) -> (i32, i32) {
    (
        width.clamp(MIN_WIDTH, monitor.0.max(MIN_WIDTH)),
        height.clamp(MIN_HEIGHT, monitor.1.max(MIN_HEIGHT)),
    )
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

    // ---- clamp: one test per edge ----

    #[test]
    fn clamp_past_left_edge() {
        // Dragged far off the left; 48px of the panel must remain.
        let m = clamp_margins(-1000, 100, PANEL, MONITOR);
        assert_eq!(m.left, MIN_VISIBLE - PANEL.0); // -252
        assert_eq!(m.top, 100, "the other axis is untouched");
    }

    #[test]
    fn clamp_past_right_edge() {
        let m = clamp_margins(9999, 100, PANEL, MONITOR);
        assert_eq!(m.left, MONITOR.0 - MIN_VISIBLE); // 1872
    }

    #[test]
    fn clamp_past_top_edge() {
        let m = clamp_margins(100, -1000, PANEL, MONITOR);
        assert_eq!(m.top, MIN_VISIBLE - PANEL.1); // -102
    }

    #[test]
    fn clamp_past_bottom_edge() {
        let m = clamp_margins(100, 9999, PANEL, MONITOR);
        assert_eq!(m.top, MONITOR.1 - MIN_VISIBLE); // 1032
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

    #[test]
    fn clamp_size_respects_bounds() {
        assert_eq!(clamp_size(10, 10, MONITOR), (MIN_WIDTH, MIN_HEIGHT));
        assert_eq!(clamp_size(9999, 9999, MONITOR), MONITOR);
        assert_eq!(clamp_size(400, 300, MONITOR), (400, 300));
    }
}
