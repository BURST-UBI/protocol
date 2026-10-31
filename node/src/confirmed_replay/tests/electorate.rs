use super::snapshot_history::{
    cemented,
    reconciliation::rollover::{electorate::prepared, install},
};
use super::*;
use crate::{checkpoint_agreement::CompactReplayCandidate, checkpoint_replay::SigningClock};

pub(super) use crate::verification_test_support::indexed_seed as seed;

impl Fixture {
    pub(super) fn extend_bootstrap_electorate(&mut self, indices: std::ops::Range<u32>) -> u64 {
        assert!(indices.start > 0 && indices.end <= self.params.bootstrap_exit_threshold);
        assert!(indices.end <= burst_verification::bootstrap::MAX_BOOTSTRAP_HUMANS);
        for index in indices {
            let key = burst_crypto::keypair_from_seed(&seed(index));
            let time = 100 + 2 * u64::from(index);
            self.push(0, BlockType::Endorse, time, |block| {
                block.brn_spent += 1;
                block.link = BlockHash::new(key.public.0);
            });
            self.open_human(
                index,
                self.blocks[0].hash,
                time + 1,
                burst_crypto::derive_address(&key.public),
            );
        }
        self.blocks.last().unwrap().timestamp.as_secs() + 1
    }

    pub(super) fn extend_electorate(
        &mut self,
        store: &LmdbStore,
        indices: std::ops::Range<u32>,
        epoch: BlockHash,
        representative: impl Fn(u32) -> WalletAddress,
    ) -> u64 {
        let base = crate::confirmed_replay::snapshot::installed::load_base(
            store,
            NetworkId::Dev,
            &self.params,
        )
        .unwrap()
        .unwrap()
        .into_continuation();
        assert_eq!(base.anchor().hash(), epoch);
        let mut time = base.anchor().cutoff() + 1;
        for index in indices {
            let key = burst_crypto::keypair_from_seed(&seed(index));
            let account = burst_crypto::derive_address(&key.public);
            let verification = &base.state().verification;
            let state = verification.get_state(&account).unwrap();
            assert_eq!(state.selected_verifiers.as_slice(), &[address(1)]);
            let context = verification.selection_context(&account).unwrap();
            assert!(time < state.started_at.as_secs() + self.params.verification_timeout_secs);
            let stake = self.params.verifier_stake_amount;
            self.push(1, BlockType::VerificationVote, time, |block| {
                block.payment_epoch = epoch;
                block.link = BlockHash::new(key.public.0);
                block.origin = context.into_tx_hash();
                let mut choice = [0; 32];
                choice[0] = 1;
                block.transaction = TxHash::new(choice);
                block.brn_spent += stake;
            });
            self.open_human(index, epoch, time + 1, representative(index));
            time += 2;
        }
        time
    }

    pub(super) fn open_human(
        &mut self,
        index: u32,
        epoch: BlockHash,
        time: u64,
        representative: WalletAddress,
    ) {
        let key = burst_crypto::keypair_from_seed(&seed(index));
        let mut open = self.blocks[0].clone();
        open.payment_epoch = epoch;
        open.account = burst_crypto::derive_address(&key.public);
        open.representative = representative;
        open.timestamp = Timestamp::new(time);
        open.hash = open.compute_hash();
        open.signature = burst_crypto::sign_message(open.hash.as_bytes(), &key.private);
        self.blocks.push(open);
    }
}

fn admitted(humans: u32, unendorsed: bool) {
    let prepared_humans = humans + u32::from(unendorsed);
    let (_dir, store, mut fixture, epoch, _) =
        prepared(prepared_humans, Fixture::new().params, 256 << 20);
    let recovered = crate::confirmed_replay::snapshot::installed::load_base(
        &store,
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap()
    .unwrap()
    .recover_stored_successors(&store, |base, plan, _| {
        crate::checkpoint_sync::witness::read_plan(&store, base, plan)
    })
    .unwrap();
    let base = &recovered.continuation;
    let first = fixture.blocks.len();
    let mut cutoff = fixture.extend_electorate(&store, 3..humans, epoch, |index| {
        burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&seed(index)).public)
    });
    if unendorsed {
        fixture.open_human(prepared_humans, epoch, cutoff, address(0));
        cutoff += 1;
    }
    cemented::append_batch(&store, &fixture.blocks[first..], true);
    let tail = cemented::finish(base, &store, &store);
    let candidate = base
        .prepare_next_cemented_candidate(
            &store,
            &tail,
            &recovered.selections,
            SigningClock {
                now: cutoff + 1,
                error_bound: 0,
            },
        )
        .unwrap()
        .unwrap();
    assert!(matches!(&candidate, CompactReplayCandidate::Roster(_)));
    assert_eq!(base.anchor().roster().total_humans(), 3);
    let certificate =
        certified_votes::certify(candidate.input().manifest().checkpoint(), &[0, 1, 2]);
    crate::checkpoint_store::compact::accept_candidate(
        &store,
        base.anchor(),
        candidate.input(),
        &certificate,
    )
    .unwrap();
    let signers: Vec<_> = (0..=255).collect();
    install(&fixture, &store, certificate.checkpoint.hash(), &signers);
    let installed = crate::confirmed_replay::snapshot::installed::load_base(
        &store,
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap()
    .unwrap()
    .into_continuation();
    let roster = installed.anchor().roster();
    assert_eq!(roster.total_humans(), u64::from(humans));
    for index in 0..humans {
        let key = burst_crypto::keypair_from_seed(&seed(index));
        assert_eq!(roster.weight(&burst_crypto::derive_address(&key.public)), 1);
    }
    if unendorsed {
        let key = burst_crypto::keypair_from_seed(&seed(prepared_humans));
        assert_eq!(roster.weight(&burst_crypto::derive_address(&key.public)), 0);
    }
    let tail = cemented::finish(&installed, &store, &store);
    assert!(installed
        .prepare_next_cemented_candidate(
            &store,
            &tail,
            &recovered.selections,
            SigningClock {
                now: cutoff + 2,
                error_bound: 0,
            }
        )
        .unwrap()
        .is_none());
}

#[test]
fn signed_electorate_crosses_byte_key_boundary_and_installs_exact_membership() {
    admitted(257, false);
}

#[test]
fn signed_open_without_endorsement_does_not_join_electorate() {
    admitted(256, true);
}
