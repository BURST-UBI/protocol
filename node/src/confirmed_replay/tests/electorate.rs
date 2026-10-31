use super::*;
use crate::{
    checkpoint_agreement::ReplayManifest, checkpoint_roster::PreparedRoster,
    checkpoint_store::chain, confirmed_replay::installation::PreparedInstallation,
};

pub(super) use crate::verification_test_support::indexed_seed as seed;

impl Fixture {
    /// Synthetic bootstrap history, not production-parameter load evidence.
    /// Verification comes from signed endorsements; storage projections remain
    /// deliberately invalid until authenticated replay reconstructs them.
    pub(super) fn electorate(humans: u32) -> (Self, u64) {
        assert!(humans >= 2);
        let mut fixture = Self::new();
        fixture.params.bootstrap_exit_threshold = humans;
        fixture.blocks[0] = crate::genesis_state::block(NetworkId::Dev, &fixture.params);
        let cutoff = fixture.extend_electorate(1..humans, fixture.blocks[0].hash, |index| {
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&seed(index)).public)
        });
        (fixture, cutoff)
    }

    pub(super) fn extend_electorate(
        &mut self,
        indices: std::ops::Range<u32>,
        epoch: BlockHash,
        representative: impl Fn(u32) -> WalletAddress,
    ) -> u64 {
        assert!(indices.start > 0 && indices.end <= self.params.bootstrap_exit_threshold);
        for index in indices {
            let key = burst_crypto::keypair_from_seed(&seed(index));
            let account = burst_crypto::derive_address(&key.public);
            let time = (100 + 2 * u64::from(index))
                .max(self.blocks.last().unwrap().timestamp.as_secs() + 2);
            self.push(0, BlockType::Endorse, time, |block| {
                block.payment_epoch = epoch;
                block.brn_spent += 1;
                block.link = BlockHash::new(key.public.0);
            });
            let mut open = self.blocks[0].clone();
            open.payment_epoch = epoch;
            open.account = account;
            open.representative = representative(index);
            open.timestamp = Timestamp::new(time + 1);
            open.hash = open.compute_hash();
            open.signature = burst_crypto::sign_message(open.hash.as_bytes(), &key.private);
            self.blocks.push(open);
        }
        self.blocks.last().unwrap().timestamp.as_secs() + 1
    }
}

#[test]
fn signed_electorate_crosses_byte_key_boundary_and_installs_exact_membership() {
    let humans = 257;
    let (fixture, cutoff) = Fixture::electorate(humans);
    let (_dir, store, _) = fixture.store();
    let candidate = PreparedRoster::prepare_next(&store, NetworkId::Dev, &fixture.params, cutoff)
        .unwrap()
        .unwrap();
    assert_eq!(candidate.parent().roster().total_humans(), 1);
    chain::accept_manifest(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certified_votes::certify(&candidate.manifest().checkpoint, &[0]),
        &ReplayManifest::Roster(candidate.manifest().clone()),
    )
    .unwrap();
    let installed = PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    let roster = installed.anchor.roster();
    assert_eq!(roster.total_humans(), u64::from(humans));
    for index in 0..humans {
        let key = burst_crypto::keypair_from_seed(&seed(index));
        assert_eq!(roster.weight(&burst_crypto::derive_address(&key.public)), 1);
    }
    assert!(
        PreparedRoster::prepare_next(&store, NetworkId::Dev, &fixture.params, cutoff + 1)
            .unwrap()
            .is_none()
    );
}

#[test]
fn signed_open_without_endorsement_does_not_join_electorate() {
    let (mut fixture, cutoff) = Fixture::electorate(257);
    let last_endorsement = fixture.blocks.len() - 2;
    assert_eq!(
        fixture.blocks[last_endorsement].block_type,
        BlockType::Endorse
    );
    fixture.blocks.remove(last_endorsement);
    let roster = fixture.replay(cutoff).unwrap().human_roster().unwrap();
    assert_eq!(roster.total_humans(), 256);
    let key = burst_crypto::keypair_from_seed(&seed(256));
    assert_eq!(roster.weight(&burst_crypto::derive_address(&key.public)), 0);
}
