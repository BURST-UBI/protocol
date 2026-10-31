//! Randomness providers for verifier selection.
//!
//! Drand verifies beacons against a pinned BLS key. Commit-reveal is an
//! experimental local transcript; the threshold provider is unavailable.
//! Callers must enforce the selection round and eligible population.

pub mod commit_reveal;
pub mod drand;
pub mod error;
pub mod threshold;

pub use drand::{ChainInfo, DrandBeacon, DrandClient, DrandPolicy, DrandScheme, DrandVerifier};
pub use error::VrfError;

/// Trait for providing verifiable randomness.
pub trait VrfProvider: Send + Sync {
    /// Get randomness for a context, where supported by the provider.
    /// Drand requires its asynchronous fetch methods instead.
    fn get_randomness(&self, context: &[u8]) -> Result<RandomOutput, VrfError>;

    /// Verify an output under this provider's policy. Drand ignores context;
    /// callers must separately bind its round to the selection request.
    fn verify(&self, context: &[u8], output: &RandomOutput) -> Result<bool, VrfError>;

    /// Human-readable name of this VRF provider.
    fn name(&self) -> &str;
}

/// The output of a VRF — a random value with its proof.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RandomOutput {
    /// The random bytes (32 bytes).
    pub value: [u8; 32],
    /// Provider-specific proof. Commit-reveal also requires the local transcript.
    pub proof: Vec<u8>,
    /// Round number or epoch (for drand).
    pub round: u64,
}
