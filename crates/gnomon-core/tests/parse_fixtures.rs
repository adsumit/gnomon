//! Fixture-driven parser tests.
//!
//! The `live_oauth` assertions track the values in the committed capture
//! (`session` 12.0 / `normal`, `weekly_all` 22.0 / `normal`). That fixture is
//! real captured data, so the expected values are read from it rather than
//! chosen. Explicit-`warning` passthrough is covered by `explicit_warning.json`,
//! and derived thresholds by `severity_class_thresholds`.

use chrono::Datelike;
use gnomon_core::{parse_snapshot, LimitWindow, WindowSource};

#[test]
fn live_oauth_parses() {
    let snapshot = parse_snapshot(include_str!("fixtures/live_oauth.json"))
        .expect("live_oauth.json must parse");

    assert_eq!(snapshot.windows.len(), 2, "expected exactly 2 windows");
    assert!(
        snapshot
            .windows
            .iter()
            .all(|w| w.source == WindowSource::Api),
        "all windows must come from limits[]"
    );

    let session = &snapshot.windows[0];
    assert_eq!(session.kind, "session");
    assert_eq!(session.percent, 12.0);
    assert_eq!(session.severity.as_deref(), Some("normal"));
    assert_eq!(session.severity_class(), "normal");
    assert_eq!(session.label(), "Session");
    let resets_at = session.resets_at.expect("session resets_at must be Some");
    assert_eq!(resets_at.year(), 2026);

    let weekly = &snapshot.windows[1];
    assert_eq!(weekly.kind, "weekly_all");
    assert_eq!(weekly.percent, 22.0);
    assert_eq!(weekly.severity.as_deref(), Some("normal"));
    assert_eq!(weekly.severity_class(), "normal");
    assert_eq!(weekly.label(), "Weekly");
}

#[test]
fn explicit_warning_passthrough() {
    let snapshot = parse_snapshot(include_str!("fixtures/explicit_warning.json"))
        .expect("explicit_warning.json must parse");

    assert_eq!(snapshot.windows.len(), 2, "expected exactly 2 windows");
    assert!(
        snapshot
            .windows
            .iter()
            .all(|w| w.source == WindowSource::Api),
        "all windows must come from limits[]"
    );

    let session = &snapshot.windows[0];
    assert_eq!(session.severity.as_deref(), Some("warning"));
    assert_eq!(session.severity_class(), "warning");
    assert_eq!(session.percent, 88.0);
    assert!(session.resets_at.is_some());

    // 88.0 would also derive to "warning", so the window above cannot on its own
    // distinguish passthrough from derivation. 41.0 derives to "normal" and its
    // severity is "normal", so this pair is likewise consistent with both — see
    // severity_passthrough_beats_derivation for the assertion that separates them.
    let weekly = &snapshot.windows[1];
    assert_eq!(weekly.severity.as_deref(), Some("normal"));
    assert_eq!(weekly.percent, 41.0);
}

#[test]
fn severity_passthrough_beats_derivation() {
    // percent 95.0 would derive to "error"; explicit Some("normal") must win.
    let window = LimitWindow {
        kind: "session".to_string(),
        group: "session".to_string(),
        percent: 95.0,
        severity: Some("normal".to_string()),
        resets_at: None,
        scope: None,
        is_active: None,
        source: WindowSource::Api,
    };

    assert_eq!(window.severity_class(), "normal");
}

#[test]
fn legacy_no_limits_falls_back() {
    let snapshot = parse_snapshot(include_str!("fixtures/legacy_no_limits.json"))
        .expect("legacy_no_limits.json must parse");

    assert_eq!(snapshot.windows.len(), 2, "expected exactly 2 windows");
    assert!(
        snapshot
            .windows
            .iter()
            .all(|w| w.source == WindowSource::LegacyFallback),
        "all windows must be synthesized"
    );
    assert!(
        snapshot.windows.iter().all(|w| w.severity.is_none()),
        "synthesized windows carry no severity"
    );

    assert_eq!(snapshot.windows[0].kind, "session");
    assert_eq!(snapshot.windows[1].kind, "weekly_all");

    // Derived from percent (47.0 and 81.5), not defaulted to "error".
    assert_eq!(snapshot.windows[0].severity_class(), "normal");
    assert_eq!(snapshot.windows[1].severity_class(), "warning");
}

#[test]
fn unknown_shape_parses() {
    let snapshot = parse_snapshot(include_str!("fixtures/unknown_shape.json"))
        .expect("unrecognised top-level keys must not cause an error");

    assert_eq!(snapshot.windows.len(), 1, "expected exactly 1 window");

    let window = &snapshot.windows[0];
    assert_eq!(window.kind, "quarterly_thing");
    assert_eq!(window.severity.as_deref(), Some("critical"));
    assert_eq!(window.severity_class(), "error");
    assert_eq!(window.resets_at, None);
    assert_eq!(window.label(), "Quarterly thing");
}

#[test]
fn empty_parses_to_no_windows() {
    let snapshot =
        parse_snapshot(include_str!("fixtures/empty.json")).expect("empty.json must parse");
    assert!(snapshot.windows.is_empty());
}

#[test]
fn parse_garbage_is_err() {
    assert!(parse_snapshot("{ not json").is_err());
}

#[test]
fn severity_class_thresholds() {
    fn window(percent: f64) -> LimitWindow {
        LimitWindow {
            kind: "session".to_string(),
            group: "session".to_string(),
            percent,
            severity: None,
            resets_at: None,
            scope: None,
            is_active: None,
            source: WindowSource::LegacyFallback,
        }
    }

    assert_eq!(window(74.9).severity_class(), "normal");
    assert_eq!(window(75.0).severity_class(), "warning");
    assert_eq!(window(89.9).severity_class(), "warning");
    assert_eq!(window(90.0).severity_class(), "error");
}
