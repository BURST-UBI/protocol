use super::*;
use crate::checkpoint_store::CertifiedCheckpoint;
use burst_consensus::checkpoint::{Checkpoint, CheckpointCertificate, CheckpointSignature};
use burst_consensus::checkpoint_round::Ballot;

const CUTOFF: u64 = 1_595_434_050;

mod activation;
mod boundaries;
mod challenges;
mod installation;
mod revotes;

pub(super) fn certify(checkpoint: &Checkpoint, seeds: &[u8]) -> CheckpointCertificate {
    CheckpointCertificate {
        checkpoint: checkpoint.clone(),
        round: 0,
        signatures: seeds
            .iter()
            .map(|seed| CheckpointSignature {
                representative: address(*seed),
                signature: burst_crypto::sign_message(
                    &Ballot::precommit(checkpoint, 0).signing_payload(),
                    &burst_crypto::keypair_from_seed(&[*seed; 32]).private,
                ),
            })
            .collect(),
    }
}

fn certificate(fixture: &Fixture, beacon: bool) -> CertifiedCheckpoint {
    let (_dir, store, heads) = fixture.store();
    let replay = CertifiedCheckpoint::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF,
        &heads,
        &address(20),
    )
    .unwrap();
    let mut record = CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certify(replay.checkpoint(), &[0]),
        heads,
        address(20),
    )
    .unwrap();
    if beacon {
        let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
            "../../../tests/fixtures/selection-beacons.json"
        ))
        .unwrap();
        record
            .attach_beacon(
                &store,
                &fixture.params,
                burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap(),
            )
            .unwrap();
    }
    record
}

pub(super) fn panel(record: &CertifiedCheckpoint) -> Vec<u8> {
    record
        .replayed()
        .request()
        .select(
            record.parent(),
            record.certificate(),
            record.beacon().unwrap(),
        )
        .unwrap()
        .iter()
        .map(|voter| (0..=8).find(|seed| address(*seed) == *voter).unwrap())
        .collect()
}

fn vote(fixture: &mut Fixture, record: &CertifiedCheckpoint, seed: u8, time: u64) -> BlockHash {
    vote_choice(fixture, record, seed, time, 1)
}

pub(super) fn vote_choice(
    fixture: &mut Fixture,
    record: &CertifiedCheckpoint,
    seed: u8,
    time: u64,
    choice: u8,
) -> BlockHash {
    vote_choice_in_epoch(
        fixture,
        record,
        seed,
        time,
        choice,
        record.certificate().checkpoint.hash(),
    )
}

fn vote_choice_in_epoch(
    fixture: &mut Fixture,
    record: &CertifiedCheckpoint,
    seed: u8,
    time: u64,
    choice: u8,
    epoch: BlockHash,
) -> BlockHash {
    let context = record
        .replayed()
        .request()
        .vote_context(&record.certificate().checkpoint)
        .unwrap();
    let stake = if choice == 3 {
        0
    } else {
        fixture.params.verifier_stake_amount
    };
    fixture.push(seed, BlockType::VerificationVote, time, |block| {
        block.payment_epoch = epoch;
        block.link = BlockHash::new(
            burst_crypto::decode_address(record.replayed().request().target().as_str()).unwrap(),
        );
        block.origin = context.into_tx_hash();
        let mut bytes = [0; 32];
        bytes[0] = choice;
        block.transaction = TxHash::new(bytes);
        block.brn_balance += stake;
    })
}

#[test]
fn replay_installation_checks_the_certified_lifecycle_before_freezing() {
    let mut fixture = Fixture::selection();
    let record = certificate(&fixture, true);
    let original = fixture.replay(CUTOFF).unwrap().verification;
    let voter = panel(&record)[0];
    let hash = vote(&mut fixture, &record, voter, CUTOFF + 31);
    let block = fixture
        .blocks
        .iter()
        .find(|block| block.hash == hash)
        .unwrap();
    let records = [record];
    let record_refs = records.iter().collect::<Vec<_>>();
    for mutation in 0..3 {
        let mut snapshot = original.snapshot();
        let state = snapshot.states.get_mut(&address(20)).unwrap();
        match mutation {
            0 => state.endorsements[0].burn_amount += 1,
            1 => state.endorsements[0].timestamp = Timestamp::new(CUTOFF - 1),
            _ => {
                assert!(state.excluded_verifiers.insert(address(8)));
            }
        }
        let mut changed = VerificationOrchestrator::restore(snapshot);
        let before = changed.selection_history_commitment(&address(20)).unwrap();
        let mut selections = selections::Selections::new(&record_refs, &fixture.blocks).unwrap();
        let error = selections
            .install_for(block, &mut changed, &fixture.params)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("history differs from the certified cut"),
            "{error}"
        );
        assert!(changed.selection_request(&address(20)).is_none());
        assert_eq!(
            changed.selection_history_commitment(&address(20)).unwrap(),
            before
        );
    }
    let mut matching = original;
    let mut selections = selections::Selections::new(&record_refs, &fixture.blocks).unwrap();
    selections
        .install_for(block, &mut matching, &fixture.params)
        .unwrap();
    assert!(matching.selection_context(&address(20)).is_some());
}

#[test]
fn certified_minority_penalty_funds_claimable_rewards_with_all_panel_dependencies() {
    let mut fixture = Fixture::selection_with(|params| {
        params.num_verifiers = 3;
        params.verification_threshold_bps = 6000;
    });
    let record = certificate(&fixture, true);
    let selected = panel(&record);
    let mut votes = Vec::new();
    for (index, seed) in selected.iter().enumerate() {
        votes.push(vote_choice(
            &mut fixture,
            &record,
            *seed,
            CUTOFF + 31 + index as u64,
            if index == 2 { 2 } else { 1 },
        ));
    }
    let unclaimed = replay(&fixture, std::slice::from_ref(&record)).unwrap();
    let amount = fixture.params.verifier_stake_amount / 2;
    assert_eq!(unclaimed.pending.len(), 2);
    for seed in &selected[..2] {
        let reward = &unclaimed.pending[&(address(*seed), votes[2].into_tx_hash())];
        assert_eq!(reward.amount, amount);
        assert_eq!(reward.timestamp, Timestamp::new(CUTOFF + 33));
        fixture.push(*seed, BlockType::Receive, CUTOFF + 34, |block| {
            block.link = votes[2];
            block.trst_balance += amount;
        });
    }
    let claimed = replay(&fixture, std::slice::from_ref(&record)).unwrap();
    assert!(claimed.pending.is_empty());
    assert!(claimed.account(&address(20)).unwrap().state.is_verified());
    for seed in &selected[..2] {
        // Expiry does not rewrite a signed account balance. Only the reward is
        // transferable; the old raw token remains in the expired counter.
        let account = claimed.account(&address(*seed)).unwrap();
        assert_eq!(account.trst_balance, amount + 1);
        assert_eq!(account.expired_trst, 1);
        assert_eq!(
            claimed
                .portfolio(&address(*seed))
                .unwrap()
                .cached_transferable,
            amount
        );
    }
    let dissent = claimed.brn.get_wallet(&address(selected[2])).unwrap();
    assert_eq!(dissent.total_staked, 0);
    let prior = if [1, 2].contains(&selected[2]) { 1 } else { 0 };
    assert_eq!(
        dissent.total_burned,
        fixture.params.verifier_stake_amount + prior
    );

    // A signed claim naming one vote is not proof that the other votes exist.
    fixture.blocks.retain(|block| block.hash != votes[1]);
    fixture.blocks.retain(|block| {
        block.account != address(selected[1])
            || block.block_type != BlockType::Receive
            || block.timestamp.as_secs() < CUTOFF
    });
    assert!(replay(&fixture, &[record])
        .err()
        .unwrap()
        .to_string()
        .contains("complete certified panel"));
}

fn replay(
    fixture: &Fixture,
    records: &[CertifiedCheckpoint],
) -> Result<ConfirmedReplay, ReplayError> {
    ConfirmedReplay::replay_with_certificates(
        &fixture.history(CUTOFF + 60),
        NetworkId::Dev,
        &fixture.params,
        records,
    )
}

#[test]
fn certified_votes_reconstruct_verification_and_unlock_from_signed_history() {
    let mut fixture = Fixture::selection();
    let record = certificate(&fixture, true);
    let selected = panel(&record);
    for (index, seed) in selected.iter().enumerate() {
        vote(&mut fixture, &record, *seed, CUTOFF + 31 + index as u64);
    }
    fixture.push(20, BlockType::Open, CUTOFF + 33, |_| {});
    let burn = fixture.push(20, BlockType::Burn, CUTOFF + 45, |block| {
        block.brn_balance = 10;
        block.link = destination(21);
    });
    fixture.push(21, BlockType::Open, CUTOFF + 46, |block| {
        block.link = burn;
        block.trst_balance = 10;
    });
    assert!(replay(&fixture, &[])
        .err()
        .unwrap()
        .to_string()
        .contains("no certified ancestry"));
    let state = replay(&fixture, &[record]).unwrap();
    let target = state.account(&address(20)).unwrap();
    assert!(target.state.is_verified());
    assert_eq!(target.verified_at, Some(Timestamp::new(CUTOFF + 32)));
    assert_eq!(target.total_brn_burned, 10);
    assert_eq!(state.account(&address(21)).unwrap().trst_balance, 10);
    assert_eq!(state.human_roster().unwrap().total_humans(), 10);
    for seed in selected {
        assert_eq!(state.account(&address(seed)).unwrap().total_brn_staked, 0);
        assert_eq!(
            state.brn.get_wallet(&address(seed)).unwrap().total_staked,
            0
        );
    }
}

#[test]
fn staged_panels_preserve_vote_and_verified_spending_dependencies() {
    struct Reloaded {
        record: CertifiedCheckpoint,
        unreadable: std::cell::Cell<bool>,
        reads: std::cell::Cell<usize>,
    }
    impl selections::source::SelectionSource for Reloaded {
        fn len(&self) -> usize {
            1
        }
        fn read_selection(
            &self,
            position: usize,
        ) -> Result<std::borrow::Cow<'_, CertifiedCheckpoint>, ReplayError> {
            self.reads.set(self.reads.get() + 1);
            if position != 0 || self.unreadable.get() {
                return Err(invalid("selection source read failed"));
            }
            Ok(std::borrow::Cow::Owned(self.record.clone()))
        }
    }
    let mut fixture = Fixture::selection();
    let record = certificate(&fixture, true);
    for (index, seed) in panel(&record).into_iter().enumerate() {
        vote(&mut fixture, &record, seed, CUTOFF + 31 + index as u64);
    }
    fixture.push(20, BlockType::Burn, CUTOFF + 60, |block| {
        block.brn_balance = 10;
        block.link = destination(21);
    });
    let directory = tempfile::tempdir().unwrap();
    let records = [&record];
    let memory = selections::Selections::new(&records, &fixture.blocks).unwrap();
    let source =
        super::super::blocks::source::Source::new(&fixture.blocks, Some(directory.path())).unwrap();
    let staged = selections::Selections::for_certified_epoch(&records, &source).unwrap();
    let reloaded = Reloaded {
        record: record.clone(),
        unreadable: std::cell::Cell::new(false),
        reads: std::cell::Cell::new(0),
    };
    let empty = selections::Selections::new(&reloaded, &[]).unwrap();
    assert_eq!(
        reloaded.reads.get(),
        1,
        "panel insertion must reuse the streamed certificate"
    );
    drop(empty);
    let loaded = selections::Selections::for_certified_epoch(&reloaded, &source).unwrap();
    for block in fixture.blocks.iter().filter(|block| {
        block.block_type == BlockType::VerificationVote || block.account == address(20)
    }) {
        assert_eq!(
            staged.dependencies(block, usize::MAX).unwrap(),
            memory.dependencies(block, usize::MAX).unwrap()
        );
        assert_eq!(
            loaded.dependencies(block, usize::MAX).unwrap(),
            memory.dependencies(block, usize::MAX).unwrap()
        );
    }
    assert!(!staged
        .dependencies(fixture.blocks.last().unwrap(), usize::MAX)
        .unwrap()
        .is_empty());
    let expected = memory
        .dependencies(fixture.blocks.last().unwrap(), usize::MAX)
        .unwrap();
    let reads = reloaded.reads.get();
    reloaded.unreadable.set(true);
    assert_eq!(
        loaded
            .dependencies(fixture.blocks.last().unwrap(), usize::MAX)
            .unwrap(),
        expected
    );
    assert_eq!(
        reloaded.reads.get(),
        reads,
        "authenticated dependencies should reuse the replay-scoped certificate"
    );
    drop(loaded);
    assert!(selections::Selections::for_certified_epoch(&reloaded, &source).is_err());
}

#[test]
fn partial_panel_locks_only_its_signed_vote_and_cannot_authorize_target_spending() {
    let mut fixture = Fixture::selection();
    let record = certificate(&fixture, true);
    let selected = panel(&record);
    vote(&mut fixture, &record, selected[0], CUTOFF + 31);
    let records = [&record];
    let view = selections::Selections::new(&records, &fixture.blocks).unwrap();
    let signed_vote = fixture.blocks.last().unwrap();
    assert!(view
        .dependencies(signed_vote, record.heads().len() - 1)
        .is_err());
    assert_eq!(
        view.dependencies(signed_vote, record.heads().len())
            .unwrap()
            .len(),
        record.heads().len()
    );
    let state = replay(&fixture, std::slice::from_ref(&record)).unwrap();
    assert!(!state
        .account(&address(20))
        .is_some_and(|account| account.state.is_verified()));
    assert_eq!(
        state
            .account(&address(selected[0]))
            .unwrap()
            .total_brn_staked,
        fixture.params.verifier_stake_amount
    );
    fixture.push(20, BlockType::Open, CUTOFF + 33, |_| {});
    fixture.push(20, BlockType::Burn, CUTOFF + 45, |block| {
        block.brn_balance = 10;
        block.link = destination(21);
    });
    assert!(replay(&fixture, &[record])
        .err()
        .unwrap()
        .to_string()
        .contains("complete certified panel"));
}

#[test]
fn certified_vote_replay_rejects_missing_beacon_wrong_context_and_duplicate_voter() {
    let base = Fixture::selection();
    let record = certificate(&base, true);
    let without_beacon = certificate(&base, false);
    let selected = panel(&record);
    for case in [
        "beacon",
        "context",
        "duplicate",
        "outsider",
        "signature",
        "stake",
        "pre-beacon",
    ] {
        let mut fixture = Fixture {
            params: base.params.clone(),
            blocks: base.blocks.clone(),
        };
        vote(&mut fixture, &record, selected[0], CUTOFF + 31);
        let block = fixture.blocks.last_mut().unwrap();
        match case {
            "context" => {
                block.origin = TxHash::ZERO;
                sign(selected[0], block);
            }
            "signature" => block.signature = Signature([0; 64]),
            "stake" => {
                block.brn_balance -= 1;
                sign(selected[0], block);
            }
            "pre-beacon" => {
                block.timestamp = Timestamp::new(CUTOFF);
                sign(selected[0], block);
            }
            "duplicate" => {
                vote(&mut fixture, &record, selected[0], CUTOFF + 32);
            }
            "outsider" => {
                fixture.blocks.pop();
                let outsider = (1..=8).find(|seed| !selected.contains(seed)).unwrap();
                vote(&mut fixture, &record, outsider, CUTOFF + 31);
            }
            _ => {}
        }
        let accepted = replay(
            &fixture,
            std::slice::from_ref(if case == "beacon" {
                &without_beacon
            } else {
                &record
            }),
        );
        assert!(accepted.is_err(), "accepted malformed {case} history");
    }
}

#[test]
fn successor_replays_completed_panel_and_recovers_new_human_roster_after_restart() {
    let mut fixture = Fixture::selection();
    let record = certificate(&fixture, true);
    for (index, seed) in panel(&record).iter().enumerate() {
        vote(&mut fixture, &record, *seed, CUTOFF + 31 + index as u64);
    }
    for seed in [1, 2] {
        fixture.push(seed, BlockType::Endorse, CUTOFF + 35, |block| {
            block.brn_balance += 1;
            block.link = destination(21);
        });
    }
    let (dir, store, heads) = fixture.store();
    let mut first = CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        record.certificate().clone(),
        record.heads().to_vec(),
        address(20),
    )
    .unwrap();
    first
        .attach_beacon(&store, &fixture.params, record.beacon().unwrap().clone())
        .unwrap();
    let next = CertifiedCheckpoint::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 60,
        &heads,
        &address(21),
    )
    .unwrap();
    assert_eq!(next.roster().total_humans(), 10);
    assert!(
        CertifiedCheckpoint::accept(
            &store,
            NetworkId::Dev,
            &fixture.params,
            certify(next.checkpoint(), &[0, 1, 2, 3, 4, 5, 20]),
            heads.clone(),
            address(21),
        )
        .is_err(),
        "a newly verified human cannot authorize its own roster admission"
    );
    let next_certificate = certify(next.checkpoint(), &[0, 1, 2, 3, 4, 5, 6]);
    let accepted = CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        next_certificate.clone(),
        heads,
        address(21),
    )
    .unwrap();
    assert_eq!(accepted.parent().roster().total_humans(), 9);
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let recovered = CertifiedCheckpoint::load(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.certificate(), &next_certificate);
    assert_eq!(recovered.replayed().roster().total_humans(), 10);
    assert!(recovered.beacon().is_none());
    let (_observer_dir, observer, _) = fixture.store();
    let first_bytes =
        CertifiedCheckpoint::export_record(&store, NetworkId::Dev, &fixture.params, 1)
            .unwrap()
            .unwrap();
    let second_bytes =
        CertifiedCheckpoint::export_record(&store, NetworkId::Dev, &fixture.params, 2)
            .unwrap()
            .unwrap();
    assert!(CertifiedCheckpoint::import_record(
        &observer,
        NetworkId::Dev,
        &fixture.params,
        2,
        &second_bytes
    )
    .is_err());
    CertifiedCheckpoint::import_record(&observer, NetworkId::Dev, &fixture.params, 1, &first_bytes)
        .unwrap();
    let imported = CertifiedCheckpoint::import_record(
        &observer,
        NetworkId::Dev,
        &fixture.params,
        2,
        &second_bytes,
    )
    .unwrap();
    assert_eq!(imported.certificate(), &next_certificate);
    assert_eq!(imported.replayed().roster().total_humans(), 10);
}
