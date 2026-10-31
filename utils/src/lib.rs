//! Shared utilities for the BURST protocol.

#[cfg(feature = "logging")]
pub mod logging;
pub mod time;

#[cfg(feature = "logging")]
pub use logging::init_tracing;
pub use time::format_duration;

mod shared;
mod shared_map;
mod shared_set;
pub use shared::Shared;
pub use shared_map::SharedMap;
pub use shared_set::SharedSet;

#[cfg(feature = "runtime")]
pub mod shutdown;
