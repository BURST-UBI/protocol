//! Anti-spam proof-of-work.
//!
//! Not mining — a lightweight computational cost (fractions of a second) that
//! makes flooding the network prohibitively expensive while keeping legitimate use free.
//! Transactions are prioritized by account balance and PoW difficulty.

pub mod difficulty;
pub mod error;
pub mod generator;
pub mod precompute;
pub mod thresholds;
pub mod validator;

pub use difficulty::DifficultyAdjuster;
pub use error::WorkError;
pub use generator::WorkGenerator;
pub use precompute::{PriorityBlock, WorkCache, WorkPriorityQueue};
pub use thresholds::{WorkBlockKind, WorkThresholds};
pub use validator::validate_work;

/// The result of PoW generation.
#[derive(Clone, Copy, Debug)]
pub struct WorkNonce(pub u64);
