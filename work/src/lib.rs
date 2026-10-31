//! Proof-of-work generation and threshold checks for block admission.
//!
//! Work adds a computational cost; its duration depends on the threshold and
//! hardware. It does not establish consensus or guarantee resistance to flooding.
//! Scheduling and network resource limits belong to the node.

pub mod error;
pub mod generator;
pub mod thresholds;
pub mod validator;

pub use error::WorkError;
pub use generator::WorkGenerator;
pub use thresholds::{WorkBlockKind, WorkThresholds};
pub use validator::{validate_work, work_value};

/// The result of PoW generation.
#[derive(Clone, Copy, Debug)]
pub struct WorkNonce(pub u64);
