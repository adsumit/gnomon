//! gnomon-core — core logic for the gnomon usage meter.
//!
//! Parses the Claude usage payload into normalized types and fetches it over
//! the OAuth transport. No UI, no poll loop.

pub mod creds;
pub mod error;
pub mod ipc;
pub mod model;
pub mod nm;
pub mod oauth;
pub mod parse;
mod wire;

/// Re-exported so dependent crates can format timestamps without taking their
/// own chrono dependency.
pub use chrono;

pub use error::SourceError;
pub use model::{LimitWindow, UsageSnapshot, WindowSource};
pub use parse::parse_snapshot;
