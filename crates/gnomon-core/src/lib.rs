//! gnomon-core — core logic for the gnomon usage meter.
//!
//! Parses the Claude usage payload into normalized types. No network code here.

pub mod model;
pub mod parse;
mod wire;

pub use model::{LimitWindow, UsageSnapshot, WindowSource};
pub use parse::parse_snapshot;
