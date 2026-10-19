//! Phase 2: Commit-reveal with representatives.
//!
//! When verification is needed, representatives commit hashed random values,
//! then reveal them. This experimental local transcript waits for every sealed
//! commitment: a missing revealer stalls the round instead of choosing an output.
//! Authenticated membership, agreed deadlines and recovery are not implemented.
//! This is not a production alternative or an automatic fallback to drand.

use crate::{RandomOutput, VrfError, VrfProvider};
use burst_types::WalletAddress;
use std::collections::HashMap;

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
    /// Commitments indexed by representative for O(1) lookup.
    commitment_index: HashMap<WalletAddress, [u8; 32]>,
    /// Reveals received so far.
    reveals: Vec<Reveal>,
    sealed: bool,
}

impl CommitRevealVrf {
    pub fn new() -> Self {
        Self {
            commitment_index: HashMap::new(),
            reveals: Vec::new(),
            sealed: false,
        }
    }

    /// Record a commitment from a representative.
    pub fn record_commitment(&mut self, commitment: Commitment) -> Result<(), VrfError> {
        if self.sealed
            || self
                .commitment_index
                .contains_key(&commitment.representative)
        {
            return Err(VrfError::CommitReveal(
                "commitment is duplicate or round is sealed".into(),
            ));
        }
        self.commitment_index
            .insert(commitment.representative, commitment.hash);
        Ok(())
    }

    pub fn seal_commitments(&mut self) -> Result<(), VrfError> {
        if self.commitment_index.is_empty() {
            return Err(VrfError::CommitReveal("cannot seal an empty round".into()));
        }
        self.sealed = true;
        Ok(())
    }

    /// Access commitments (for compatibility / inspection).
    pub fn commitments_iter(&self) -> impl Iterator<Item = (&WalletAddress, &[u8; 32])> {
        self.commitment_index.iter()
    }

    /// Record a reveal and verify it matches the corresponding commitment.
    ///
    /// The reveal's value is hashed with Blake2b-256 and compared against the
    /// commitment hash from the same representative. Rejects if no matching
    /// commitment exists or if the hash doesn't match.
    pub fn record_reveal(&mut self, reveal: Reveal) -> Result<(), VrfError> {
        if !self.sealed {
            return Err(VrfError::CommitReveal(
                "seal commitments before revealing".into(),
            ));
        }
        if self
            .reveals
            .iter()
            .any(|existing| existing.representative == reveal.representative)
        {
            return Err(VrfError::CommitReveal("duplicate reveal".into()));
        }
        match self.commitment_index.get(&reveal.representative) {
            None => Err(VrfError::CommitReveal(format!(
                "no commitment found for representative {}",
                reveal.representative
            ))),
            Some(commitment_hash) => {
                let reveal_hash = burst_crypto::blake2b_256(&reveal.value);
                if reveal_hash != *commitment_hash {
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
    /// Requires every sealed commitment to be revealed. Skipping a missing
    /// contribution would let its owner choose between including and omitting it.
    pub fn combine_reveals(&self) -> Result<[u8; 32], VrfError> {
        if !self.sealed
            || self.reveals.is_empty()
            || self.reveals.len() != self.commitment_index.len()
        {
            return Err(VrfError::Unavailable(
                "waiting for all sealed commitments to reveal".into(),
            ));
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
        // Verify every reveal has a matching commitment (O(r) with HashMap)
        for reveal in &self.reveals {
            match self.commitment_index.get(&reveal.representative) {
                None => return Ok(false),
                Some(commitment_hash) => {
                    let reveal_hash = burst_crypto::blake2b_256(&reveal.value);
                    if reveal_hash != *commitment_hash {
                        return Ok(false);
                    }
                }
            }
        }

        // Verify the output value matches the combined reveals + context
        let combined = self.combine_reveals()?;
        let expected = burst_crypto::blake2b_256_multi(&[&combined, context]);
        Ok(expected == output.value && output.proof == combined && output.round == 0)
    }

    fn name(&self) -> &str {
        "commit-reveal"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{id}"))
    }

    fn commit(round: &mut CommitRevealVrf, id: &str, value: [u8; 32]) {
        round
            .record_commitment(Commitment {
                representative: member(id),
                hash: burst_crypto::blake2b_256(&value),
            })
            .unwrap();
    }

    fn reveal(id: &str, value: [u8; 32]) -> Reveal {
        Reveal {
            representative: member(id),
            value,
        }
    }

    #[test]
    fn sealed_membership_rejects_rewrites_late_commits_and_duplicate_reveals() {
        let mut round = CommitRevealVrf::new();
        assert!(round.seal_commitments().is_err());
        commit(&mut round, "alice", [1; 32]);
        assert!(round
            .record_commitment(Commitment {
                representative: member("alice"),
                hash: [0; 32]
            })
            .is_err());
        assert!(round.record_reveal(reveal("alice", [1; 32])).is_err());
        round.seal_commitments().unwrap();
        assert!(round
            .record_commitment(Commitment {
                representative: member("bob"),
                hash: [0; 32]
            })
            .is_err());
        assert!(round.record_reveal(reveal("bob", [1; 32])).is_err());
        assert!(round.record_reveal(reveal("alice", [2; 32])).is_err());
        round.record_reveal(reveal("alice", [1; 32])).unwrap();
        assert!(round.record_reveal(reveal("alice", [1; 32])).is_err());
        assert_eq!(round.combine_reveals().unwrap(), [1; 32]);
    }

    #[test]
    fn withholding_cannot_produce_a_partial_output_and_proof_is_checked() {
        let mut round = CommitRevealVrf::new();
        commit(&mut round, "alice", [1; 32]);
        commit(&mut round, "bob", [2; 32]);
        round.seal_commitments().unwrap();
        round.record_reveal(reveal("alice", [1; 32])).unwrap();
        assert!(round.get_randomness(b"request-a").is_err());
        round.record_reveal(reveal("bob", [2; 32])).unwrap();
        let mut output = round.get_randomness(b"request-a").unwrap();
        assert!(round.verify(b"request-a", &output).unwrap());
        assert!(!round.verify(b"request-b", &output).unwrap());
        output.proof[0] ^= 1;
        assert!(!round.verify(b"request-a", &output).unwrap());
        output.proof[0] ^= 1;
        output.round = 1;
        assert!(!round.verify(b"request-a", &output).unwrap());
    }
}
