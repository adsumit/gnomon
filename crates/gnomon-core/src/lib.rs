//! gnomon-core — core logic for the gnomon usage meter.
//!
//! Parses the Claude usage payload into normalized types and fetches it over
//! the OAuth transport. No UI, no poll loop.

pub mod creds;
pub mod error;
pub mod model;
pub mod oauth;
pub mod parse;
mod wire;

pub use error::SourceError;
pub use model::{LimitWindow, UsageSnapshot, WindowSource};
pub use parse::parse_snapshot;
