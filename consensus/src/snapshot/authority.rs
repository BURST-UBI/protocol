//! Incremental quorum-trust authority verification. No ledger or pruning authority.
use super::{CheckpointAnchor, CheckpointError, SnapshotAuthorityStep};

pub const MAX_PAGE_STEPS: usize = 32;

/// Initialize only from configured genesis or a previously authenticated anchor.
/// Transport must bound encoded bytes before decoding a page. This cursor keeps
/// no historical rosters and cannot be deserialized from a peer.
pub struct AuthorityCursor {
    anchor: CheckpointAnchor,
}

impl AuthorityCursor {
    pub fn new(anchor: CheckpointAnchor) -> Self {
        Self { anchor }
    }

    pub fn anchor(&self) -> &CheckpointAnchor {
        &self.anchor
    }

    /// A failed page leaves the cursor unchanged, including its electorate.
    pub fn advance(&mut self, page: &[SnapshotAuthorityStep]) -> Result<(), CheckpointError> {
        if page.is_empty() || page.len() > MAX_PAGE_STEPS {
            return Err(CheckpointError::InvalidCommitment);
        }
        let mut next = self.anchor.clone();
        for step in page {
            next = step.authenticate(&next)?;
        }
        self.anchor = next;
        Ok(())
    }

    pub fn into_anchor(self) -> CheckpointAnchor {
        self.anchor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::{
        Checkpoint, CheckpointCertificate, CheckpointSignature, HumanDelegation, HumanRoster,
    };
    use burst_crypto::{derive_address, keypair_from_seed, sign_message};
    use burst_types::{BlockHash, NetworkId, Signature};

    fn roster(seed: u8) -> Vec<HumanDelegation> {
        let address = derive_address(&keypair_from_seed(&[seed; 32]).public);
        vec![HumanDelegation {
            human: address.clone(),
            representative: address,
        }]
    }

    fn genesis() -> CheckpointAnchor {
        CheckpointAnchor::genesis(
            NetworkId::Dev,
            BlockHash::new([1; 32]),
            100,
            HumanRoster::new(roster(1)).unwrap(),
            6700,
        )
        .unwrap()
    }

    fn step(parent: &CheckpointAnchor, signer: u8, successor: u8) -> SnapshotAuthorityStep {
        let roster = roster(successor);
        let checkpoint = Checkpoint {
            network: parent.network(),
            genesis: parent.genesis_hash(),
            parent: parent.hash(),
            height: parent.height() + 1,
            cutoff: parent.cutoff() + 1,
            ledger_root: BlockHash::new([2; 32]),
            params_hash: BlockHash::new([3; 32]),
            next_roster: HumanRoster::new(roster.clone()).unwrap().commitment(),
            selection_root: BlockHash::new([4; 32]),
            next_quorum_bps: 6700,
        };
        let key = keypair_from_seed(&[signer; 32]);
        let payload = crate::checkpoint_round::Ballot::precommit(&checkpoint, 0).signing_payload();
        SnapshotAuthorityStep {
            roster,
            certificate: CheckpointCertificate {
                checkpoint,
                round: 0,
                signatures: vec![CheckpointSignature {
                    representative: derive_address(&key.public),
                    signature: sign_message(&payload, &key.private),
                }],
            },
        }
    }

    #[test]
    fn pages_cross_height_128_and_resume_only_from_authenticated_anchor() {
        let mut cursor = AuthorityCursor::new(genesis());
        let mut expected = genesis();
        let mut signer = 1;
        for _ in 0..9 {
            let mut page = Vec::new();
            for _ in 0..MAX_PAGE_STEPS {
                let successor = if signer == 1 { 2 } else { 1 };
                let next = step(&expected, signer, successor);
                expected = next.authenticate(&expected).unwrap();
                page.push(next);
                signer = successor;
            }
            cursor.advance(&page).unwrap();
            assert_eq!(cursor.anchor().hash(), expected.hash());
            cursor = AuthorityCursor::new(cursor.into_anchor());
        }
        assert_eq!(cursor.anchor().height(), 288);
    }

    #[test]
    fn bad_later_step_cannot_partially_change_authority() {
        let anchor = genesis();
        let first = step(&anchor, 1, 2);
        let second = step(&first.authenticate(&anchor).unwrap(), 2, 1);
        for mutation in 0..5 {
            let mut bad = second.clone();
            match mutation {
                0 => bad.certificate.signatures[0].signature = Signature([0; 64]),
                1 => bad.certificate.checkpoint.parent = anchor.hash(),
                2 => bad.certificate.checkpoint.height += 1,
                3 => bad.roster = roster(3),
                _ => bad = step(&first.authenticate(&anchor).unwrap(), 1, 1),
            }
            let mut cursor = AuthorityCursor::new(anchor.clone());
            assert!(cursor.advance(&[first.clone(), bad]).is_err());
            assert_eq!(cursor.anchor().hash(), anchor.hash());
            assert_eq!(cursor.anchor().roster(), anchor.roster());
            cursor.advance(&[first.clone(), second.clone()]).unwrap();
            assert_eq!(cursor.anchor().height(), 2);
            assert!(cursor.advance(std::slice::from_ref(&first)).is_err());
            assert_eq!(cursor.anchor().height(), 2);
        }
        let mut cursor = AuthorityCursor::new(anchor.clone());
        assert!(cursor.advance(&[]).is_err());
        assert!(cursor.advance(&vec![first; MAX_PAGE_STEPS + 1]).is_err());
        assert_eq!(cursor.anchor().hash(), anchor.hash());
    }
}
