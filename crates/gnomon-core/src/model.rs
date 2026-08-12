//! Normalized usage types.
//!
//! These are the types the rest of gnomon works with. They carry no serde
//! attributes — see [`crate::wire`] for the deserialize-only mirrors of the API.

use chrono::{DateTime, Utc};
use serde::Serialize;

/// Where a [`LimitWindow`] came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowSource {
    /// Mapped from an entry in the `limits[]` array.
    Api,
    /// Synthesized from the `five_hour` / `seven_day` fallback objects.
    LegacyFallback,
}

/// A single usage window.
#[derive(Debug, Clone, Serialize)]
pub struct LimitWindow {
    pub kind: String,
    pub group: String,
    pub percent: f64,
    /// `None` when synthesized from the legacy fallback.
    pub severity: Option<String>,
    pub resets_at: Option<DateTime<Utc>>,
    pub scope: Option<String>,
    pub is_active: Option<bool>,
    pub source: WindowSource,
}

/// A point-in-time view of every usage window.
#[derive(Debug, Clone, Serialize)]
pub struct UsageSnapshot {
    pub windows: Vec<LimitWindow>,
    pub captured_at: DateTime<Utc>,
}

impl LimitWindow {
    /// Render class for this window.
    ///
    /// A known severity passes through. An unknown severity fails loud as
    /// `"error"`. When severity is absent the class is derived from percent.
    pub fn severity_class(&self) -> &'static str {
        match self.severity.as_deref() {
            Some("normal") => "normal",
            Some("warning") => "warning",
            Some(_) => "error",
            None => {
                if self.percent < 75.0 {
                    "normal"
                } else if self.percent < 90.0 {
                    "warning"
                } else {
                    "error"
                }
            }
        }
    }

    /// Human-readable label for this window's kind.
    pub fn label(&self) -> String {
        match self.kind.as_str() {
            "session" => "Session".to_string(),
            "weekly_all" => "Weekly".to_string(),
            other => {
                let spaced = other.replace('_', " ");
                let mut chars = spaced.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => spaced,
                }
            }
        }
    }
}
