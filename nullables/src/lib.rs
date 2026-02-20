//! Nullable infrastructure for deterministic testing.
//!
//! Inspired by the "A-frame architecture" pattern from RsNano.
//! All external dependencies (clock, network, storage, random) are abstracted
//! behind traits. This crate provides test-friendly implementations that:
//! - Return deterministic values
//! - Can be controlled programmatically
//! - Never touch the filesystem or network
//!
//! Usage: swap real implementations for nullables in tests.

pub mod clock;
pub mod network;
pub mod random;
pub mod store;

pub use clock::NullClock;
pub use network::NullNetwork;
pub use random::NullRandom;
pub use store::{NullDelegationStore, NullStore};
