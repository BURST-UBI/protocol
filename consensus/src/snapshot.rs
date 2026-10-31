//! On-demand state attestations under an independently authenticated epoch.
//! A snapshot never supplies its own voting authority.

use crate::checkpoint::{
    CheckpointAnchor, CheckpointCertificate, CheckpointError, CheckpointSignature, HumanDelegation,
    HumanRoster,
};
use burst_crypto::blake2b_256;
use burst_types::{BlockHash, NetworkId};
use serde::{Deserialize, Serialize};

pub const MAX_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;
pub const LEGACY_STATE_FORMAT: u16 = 1;
pub const PREVIOUS_STATE_FORMAT: u16 = 3;
pub const STATE_FORMAT: u16 = 4;

pub mod authority;

/// A roster witness has authority only when the preceding trusted electorate
/// certified its commitment. This authenticates the roster through quorum trust;
/// it does not independently replay the ledger that produced the roster.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotAuthorityStep {
    pub certificate: CheckpointCertificate,
    pub roster: Vec<HumanDelegation>,
}

impl SnapshotAuthorityStep {
    pub fn authenticate(
        &self,
        parent: &CheckpointAnchor,
    ) -> Result<CheckpointAnchor, CheckpointError> {
        // Verify the old electorate before constructing any advertised new one.
        parent.verify_certificate(&self.certificate)?;
        let roster = HumanRoster::new(self.roster.clone())?;
        parent.advance_certified_roster(&self.certificate, roster)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotManifest {
    pub network: NetworkId,
    pub genesis: BlockHash,
    pub epoch: BlockHash,
    pub height: u64,
    pub cutoff: u64,
    pub roster: BlockHash,
    pub quorum_bps: u16,
    pub state_format: u16,
    pub state_bytes: u64,
    pub state_hash: BlockHash,
}

impl SnapshotManifest {
    /// Call only with bytes derived from the exact certified state, not live
    /// provisional balances. Construction alone does not authorize signing.
    pub fn new(anchor: &CheckpointAnchor, state: &[u8]) -> Result<Self, CheckpointError> {
        Self::from_commitment(
            anchor,
            state.len() as u64,
            BlockHash::new(blake2b_256(state)),
        )
    }

    /// Bind a streamed state commitment. The caller must derive it from the exact
    /// certified state; this validates context and bounds, not the supplied hash.
    pub fn from_commitment(
        anchor: &CheckpointAnchor,
        state_bytes: u64,
        state_hash: BlockHash,
    ) -> Result<Self, CheckpointError> {
        let manifest = Self {
            network: anchor.network(),
            genesis: anchor.genesis_hash(),
            epoch: anchor.hash(),
            height: anchor.height(),
            cutoff: anchor.cutoff(),
            roster: anchor.roster().commitment(),
            quorum_bps: anchor.quorum_bps(),
            state_format: STATE_FORMAT,
            state_bytes,
            state_hash,
        };
        manifest.validate(anchor)?;
        Ok(manifest)
    }

    /// Check manifest context, supported format, length bounds and nonzero hash.
    /// Does not inspect state bytes or verify signatures.
    pub fn validate(&self, anchor: &CheckpointAnchor) -> Result<(), CheckpointError> {
        if self.network != anchor.network()
            || self.genesis != anchor.genesis_hash()
            || self.epoch != anchor.hash()
            || self.height != anchor.height()
            || self.cutoff != anchor.cutoff()
            || self.roster != anchor.roster().commitment()
            || self.quorum_bps != anchor.quorum_bps()
        {
            return Err(CheckpointError::WrongParent);
        }
        if !matches!(
            self.state_format,
            LEGACY_STATE_FORMAT | PREVIOUS_STATE_FORMAT | STATE_FORMAT
        ) || self.state_bytes == 0
            || self.state_bytes > MAX_SNAPSHOT_BYTES
            || self.state_hash.is_zero()
        {
            return Err(CheckpointError::InvalidCommitment);
        }
        Ok(())
    }

    pub fn signing_payload(&self) -> Vec<u8> {
        let mut bytes = b"BURST/economic-snapshot/v1\0".to_vec();
        bytes.push(match self.network {
            NetworkId::Live => 0,
            NetworkId::Test => 1,
            NetworkId::Dev => 2,
        });
        bytes.extend_from_slice(self.genesis.as_bytes());
        bytes.extend_from_slice(self.epoch.as_bytes());
        bytes.extend_from_slice(&self.height.to_be_bytes());
        bytes.extend_from_slice(&self.cutoff.to_be_bytes());
        bytes.extend_from_slice(self.roster.as_bytes());
        bytes.extend_from_slice(&self.quorum_bps.to_be_bytes());
        bytes.extend_from_slice(&self.state_format.to_be_bytes());
        bytes.extend_from_slice(&self.state_bytes.to_be_bytes());
        bytes.extend_from_slice(self.state_hash.as_bytes());
        bytes
    }

    /// Check format, byte length and hash. Does not decode economic state or
    /// establish that the manifest was certified.
    pub fn verify_state(&self, state: &[u8]) -> Result<(), CheckpointError> {
        if !matches!(
            self.state_format,
            LEGACY_STATE_FORMAT | PREVIOUS_STATE_FORMAT | STATE_FORMAT
        ) || self.state_bytes == 0
            || self.state_bytes > MAX_SNAPSHOT_BYTES
            || self.state_bytes != state.len() as u64
            || self.state_hash != BlockHash::new(blake2b_256(state))
        {
            return Err(CheckpointError::InvalidCommitment);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotCertificate {
    pub manifest: SnapshotManifest,
    pub signatures: Vec<CheckpointSignature>,
}

impl SnapshotCertificate {
    /// Verify the small certificate before hashing or decoding its state body.
    /// Peer attendance and account balances never change the denominator.
    pub fn verify(&self, anchor: &CheckpointAnchor) -> Result<(), CheckpointError> {
        self.manifest.validate(anchor)?;
        anchor.verify_signatures(&self.manifest.signing_payload(), &self.signatures)
    }

    /// Authenticate the manifest and check its bytes. State decoding and economic
    /// invariant checks remain the installer's responsibility.
    pub fn verify_state(
        &self,
        anchor: &CheckpointAnchor,
        state: &[u8],
    ) -> Result<(), CheckpointError> {
        self.verify(anchor)?;
        self.manifest.verify_state(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::{HumanDelegation, HumanRoster};
    use burst_crypto::{derive_address, keypair_from_seed, sign_message};

    fn anchor() -> CheckpointAnchor {
        let roster = HumanRoster::new(
            (1..=4)
                .map(|seed| HumanDelegation {
                    human: derive_address(&keypair_from_seed(&[seed; 32]).public),
                    representative: derive_address(
                        &keypair_from_seed(&[if seed == 4 { 1 } else { seed }; 32]).public,
                    ),
                })
                .collect(),
        )
        .unwrap();
        CheckpointAnchor::genesis(NetworkId::Dev, BlockHash::new([1; 32]), 100, roster, 6700)
            .unwrap()
    }

    fn certify(manifest: SnapshotManifest, seeds: &[u8]) -> SnapshotCertificate {
        let signatures = seeds
            .iter()
            .map(|seed| {
                let key = keypair_from_seed(&[*seed; 32]);
                CheckpointSignature {
                    representative: derive_address(&key.public),
                    signature: sign_message(&manifest.signing_payload(), &key.private),
                }
            })
            .collect();
        SnapshotCertificate {
            manifest,
            signatures,
        }
    }

    #[test]
    fn snapshot_requires_human_quorum_and_binds_every_context_field() {
        let anchor = anchor();
        let manifest = SnapshotManifest::new(&anchor, b"state").unwrap();
        assert_eq!(
            manifest,
            SnapshotManifest::from_commitment(&anchor, 5, BlockHash::new(blake2b_256(b"state")))
                .unwrap()
        );
        for (length, hash) in [
            (0, manifest.state_hash),
            (MAX_SNAPSHOT_BYTES + 1, manifest.state_hash),
            (5, BlockHash::ZERO),
        ] {
            assert!(SnapshotManifest::from_commitment(&anchor, length, hash).is_err());
        }
        assert!(certify(manifest.clone(), &[1, 2])
            .verify_state(&anchor, b"state")
            .is_ok());
        for seeds in [&[1][..], &[2, 3], &[1, 1], &[1, 99]] {
            assert!(certify(manifest.clone(), seeds).verify(&anchor).is_err());
        }
        for field in 0..10 {
            let mut changed = manifest.clone();
            match field {
                0 => changed.network = NetworkId::Test,
                1 => changed.genesis = BlockHash::new([2; 32]),
                2 => changed.epoch = BlockHash::new([2; 32]),
                3 => changed.height += 1,
                4 => changed.cutoff += 1,
                5 => changed.roster = BlockHash::new([2; 32]),
                6 => changed.quorum_bps = 6667,
                7 => changed.state_format += 1,
                8 => changed.state_bytes += 1,
                _ => changed.state_hash = BlockHash::new([2; 32]),
            }
            assert_ne!(changed.signing_payload(), manifest.signing_payload());
            assert!(certify(changed, &[1, 2])
                .verify_state(&anchor, b"state")
                .is_err());
        }
        assert!(SnapshotManifest::new(&anchor, b"").is_err());
        let mut oversized = manifest;
        oversized.state_bytes = MAX_SNAPSHOT_BYTES + 1;
        assert!(certify(oversized, &[1, 2]).verify(&anchor).is_err());
    }
}
