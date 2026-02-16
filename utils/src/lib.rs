//! Shared utilities for the BURST protocol.

pub mod logging;
pub mod stats;
pub mod time;

pub use logging::init_tracing;
pub use time::format_duration;
