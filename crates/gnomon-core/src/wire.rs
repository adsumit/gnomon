//! Deserialize-only mirrors of the API payload.
//!
//! Every field is optional and unknown keys are ignored implicitly. These types
//! are crate-private; nothing here appears in gnomon-core's public API.
//!
//! `deny_unknown_fields` is forbidden — the API emits unreleased codenamed keys
//! that must never cause a parse failure.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct WireUsage {
    pub(crate) limits: Option<Vec<WireLimit>>,
    pub(crate) five_hour: Option<WireWindow>,
    pub(crate) seven_day: Option<WireWindow>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct WireLimit {
    pub(crate) kind: Option<String>,
    pub(crate) group: Option<String>,
    pub(crate) percent: Option<f64>,
    pub(crate) severity: Option<String>,
    pub(crate) resets_at: Option<String>,
    pub(crate) scope: Option<String>,
    pub(crate) is_active: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct WireWindow {
    pub(crate) utilization: Option<f64>,
    pub(crate) resets_at: Option<String>,
}
