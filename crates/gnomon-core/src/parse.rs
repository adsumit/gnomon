//! Tolerant normalization from the wire payload to [`UsageSnapshot`].

use chrono::{DateTime, Utc};

use crate::model::{LimitWindow, UsageSnapshot, WindowSource};
use crate::wire::{WireUsage, WireWindow};

/// Parse an API payload into a normalized snapshot.
///
/// A malformed document is the only case that returns `Err`. Missing, absent,
/// or unparseable values are normalized rather than rejected, and an empty
/// `windows` vec is a valid successful result.
pub fn parse_snapshot(json: &str) -> Result<UsageSnapshot, serde_json::Error> {
    let wire: WireUsage = serde_json::from_str(json)?;

    let mut windows = Vec::new();

    match wire.limits {
        Some(limits) if !limits.is_empty() => {
            for limit in limits {
                windows.push(LimitWindow {
                    kind: limit.kind.unwrap_or_else(|| "unknown".to_string()),
                    group: limit.group.unwrap_or_else(|| "unknown".to_string()),
                    percent: limit.percent.unwrap_or(0.0),
                    severity: limit.severity,
                    resets_at: parse_rfc3339(limit.resets_at.as_deref()),
                    scope: limit.scope,
                    is_active: limit.is_active,
                    source: WindowSource::Api,
                });
            }
        }
        _ => {
            if let Some(five_hour) = wire.five_hour {
                windows.push(legacy_window("session", "session", five_hour));
            }
            if let Some(seven_day) = wire.seven_day {
                windows.push(legacy_window("weekly_all", "weekly", seven_day));
            }
        }
    }

    Ok(UsageSnapshot {
        windows,
        captured_at: Utc::now(),
    })
}

/// Synthesize a window from a legacy fallback object.
fn legacy_window(kind: &str, group: &str, window: WireWindow) -> LimitWindow {
    LimitWindow {
        kind: kind.to_string(),
        group: group.to_string(),
        percent: window.utilization.unwrap_or(0.0),
        severity: None,
        resets_at: parse_rfc3339(window.resets_at.as_deref()),
        scope: None,
        is_active: None,
        source: WindowSource::LegacyFallback,
    }
}

/// Absent or unparseable timestamps become `None`, never an error.
fn parse_rfc3339(raw: Option<&str>) -> Option<DateTime<Utc>> {
    let raw = raw?;
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}
