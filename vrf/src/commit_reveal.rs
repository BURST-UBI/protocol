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

    /// Record a reveal and verify it matches the corresponding commitment.
    ///
    /// The reveal's value is hashed with Blake2b-256 and compared against the
    /// commitment hash from the same representative. Rejects if no matching
    /// commitment exists or if the hash doesn't match.
    pub fn record_reveal(&mut self, reveal: Reveal) -> Result<(), VrfError> {
        let matching_commitment = self
            .commitments
            .iter()
            .find(|c| c.representative == reveal.representative);

        match matching_commitment {
            None => Err(VrfError::CommitReveal(format!(
                "no commitment found for representative {}",
                reveal.representative
            ))),
            Some(commitment) => {
                let reveal_hash = burst_crypto::blake2b_256(&reveal.value);
                if reveal_hash != commitment.hash {
                    return Err(VrfError::CommitReveal(
                        "reveal hash does not match commitment".into(),
                    ));
                }
                self.reveals.push(reveal);
                Ok(())
            }
        }
    }

    /// Combine all reveals into a single random seed by XOR-ing their values.
    ///
    /// Requires at least one reveal. The XOR combination ensures that as long as
    /// at least one participant's value is truly random, the output is random.
    pub fn combine_reveals(&self) -> Result<[u8; 32], VrfError> {
        if self.reveals.is_empty() {
            return Err(VrfError::CommitReveal("no reveals to combine".into()));
        }

        let mut combined = [0u8; 32];
        for reveal in &self.reveals {
            for (i, byte) in reveal.value.iter().enumerate() {
                combined[i] ^= byte;
            }
        }
        Ok(combined)
    }
}

impl Default for CommitRevealVrf {
    fn default() -> Self {
        Self::new()
    }
}

impl VrfProvider for CommitRevealVrf {
    /// Combine all reveals with the provided context to produce deterministic randomness.
    ///
    /// The output is `Blake2b(combined_reveals || context)`, ensuring the same
    /// set of reveals with the same context always produces the same output.
    fn get_randomness(&self, context: &[u8]) -> Result<RandomOutput, VrfError> {
        let combined = self.combine_reveals()?;
        let value = burst_crypto::blake2b_256_multi(&[&combined, context]);
        Ok(RandomOutput {
            value,
            proof: combined.to_vec(),
            round: 0,
        })
    }

    /// Verify that all reveals match their commitments and that the output
    /// was correctly derived from the combined reveals.
    fn verify(&self, context: &[u8], output: &RandomOutput) -> Result<bool, VrfError> {
        // Verify every reveal has a matching commitment
        for reveal in &self.reveals {
            let matching = self
                .commitments
                .iter()
                .find(|c| c.representative == reveal.representative);

            match matching {
                None => return Ok(false),
                Some(commitment) => {
                    let reveal_hash = burst_crypto::blake2b_256(&reveal.value);
                    if reveal_hash != commitment.hash {
                        return Ok(false);
                    }
                }
            }
        }

        // Verify the output value matches the combined reveals + context
        let combined = self.combine_reveals()?;
        let expected = burst_crypto::blake2b_256_multi(&[&combined, context]);
        Ok(expected == output.value)
    }

    fn name(&self) -> &str {
        "commit-reveal"
    }
}
