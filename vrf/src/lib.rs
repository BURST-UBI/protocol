//! Verifiable Random Function (VRF) for fair verifier selection.
//!
//! Phased approach:
//! - **Phase 1**: drand (external beacon from League of Entropy)
//! - **Phase 2**: Commit-reveal with representatives
//! - **Phase 3**: Threshold VRF (DVRF) — gold standard

pub mod commit_reveal;
pub mod drand;
pub mod error;
pub mod threshold;

pub use error::VrfError;

/// Trait for providing verifiable randomness.
pub trait VrfProvider: Send + Sync {
    /// Get randomness for a given context (e.g., verification request ID).
    fn get_randomness(&self, context: &[u8]) -> Result<RandomOutput, VrfError>;

    /// Verify that a randomness output was correctly generated.
    fn verify(&self, context: &[u8], output: &RandomOutput) -> Result<bool, VrfError>;

    /// Human-readable name of this VRF provider.
    fn name(&self) -> &str;
}

/// The output of a VRF — a random value with its proof.
#[derive(Clone, Debug)]
pub struct RandomOutput {
    /// The random bytes (32 bytes).
    pub value: [u8; 32],
    /// Proof that the value was correctly generated.
    pub proof: Vec<u8>,
    /// Round number or epoch (for drand).
    pub round: u64,
}
