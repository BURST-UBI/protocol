//! Phase 2: Commit-reveal with representatives.
//!
//! When verification is needed, representatives commit hashed random values,
//! then reveal them. Combined outputs produce the seed. Slashing penalties
//! for non-reveal mitigate the last-revealer problem.

use crate::{RandomOutput, VrfError, VrfProvider};
use burst_types::WalletAddress;

/// A commitment from a representative.
pub struct Commitment {
    pub representative: WalletAddress,
    /// Hash of the random value (commitment).
    pub hash: [u8; 32],
}

/// A reveal from a representative.
pub struct Reveal {
    pub representative: WalletAddress,
    /// The actual random value.
    pub value: [u8; 32],
}

/// Commit-reveal VRF provider — self-sovereign randomness.
pub struct CommitRevealVrf {
    /// Commitments received so far.
    pub commitments: Vec<Commitment>,
    /// Reveals received so far.
    pub reveals: Vec<Reveal>,
}

impl CommitRevealVrf {
    pub fn new() -> Self {
        Self {
            commitments: Vec::new(),
            reveals: Vec::new(),
        }
    }

    /// Record a commitment from a representative.
    pub fn record_commitment(&mut self, commitment: Commitment) {
        self.commitments.push(commitment);
    }

    /// Record a reveal and check it matches the commitment.
    pub fn record_reveal(&mut self, _reveal: Reveal) -> Result<(), VrfError> {
        todo!("verify hash(reveal.value) matches commitment, add to reveals")
    }

    /// Combine all reveals into a single random seed.
    pub fn combine_reveals(&self) -> Result<[u8; 32], VrfError> {
        todo!("XOR or hash all reveal values together")
    }
}

impl Default for CommitRevealVrf {
    fn default() -> Self {
        Self::new()
    }
}

impl VrfProvider for CommitRevealVrf {
    fn get_randomness(&self, _context: &[u8]) -> Result<RandomOutput, VrfError> {
        todo!("combine reveals with context")
    }

    fn verify(&self, _context: &[u8], _output: &RandomOutput) -> Result<bool, VrfError> {
        todo!("verify all commitments match reveals, then verify combined output")
    }

    fn name(&self) -> &str {
        "commit-reveal"
    }
}
