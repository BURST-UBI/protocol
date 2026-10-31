use super::*;
use crate::checkpoint_outcome::CertifiedChallengeOutcome;
use crate::checkpoint_store::chain::{self, CertifiedEvent};

mod epochs;
mod transfer;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_outcome_service_agrees_and_recovers_over_tcp() {
    let (fixture, selections) = completed(ChallengeReason::Fraud);
    let (_dir, source, heads) = restore_chain(&fixture, &selections);
    let prepare = |cutoff| {
        PreparedChallengeOutcome::prepare(
            &source,
            NetworkId::Dev,
            &fixture.params,
            cutoff,
            &heads,
            &address(20),
        )
        .unwrap()
        .manifest()
        .clone()
    };
    let manifest = prepare(CUTOFF + 70);
    let alternative = prepare(CUTOFF + 71);
    crate::confirmed_replay::tests::snapshot_history::selection::compact_service::assert_service(
        &fixture,
        std::sync::Arc::new(source),
        crate::checkpoint_agreement::ReplayManifest::ChallengeOutcome(manifest),
        crate::checkpoint_agreement::ReplayManifest::ChallengeOutcome(alternative),
        CUTOFF + 74,
    )
    .await;
}

#[test]
fn checkpoint_driver_recovers_authority_after_certified_outcome() {
    let (mut fixture, selections) = completed(ChallengeReason::Fraud);
    pending_successor(&mut fixture);
    let (_dir, store, heads) = restore_chain(&fixture, &selections);
    let outcome = persist_outcome(&fixture, &store, &heads);
    crate::confirmed_replay::tests::snapshot_history::boundaries::assert_chain(&fixture, &store);
    let next = CertifiedCheckpoint::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 71,
        &heads,
        &address(21),
    )
    .unwrap();
    let driver = crate::checkpoint_driver::CheckpointDriver::new(
        std::sync::Arc::new(store),
        fixture.params.clone(),
        burst_crypto::keypair_from_seed(&[0; 32]).private,
        next,
    )
    .unwrap();
    assert_eq!(
        driver.anchor().hash(),
        outcome.certificate().checkpoint.hash()
    );
    assert_eq!(driver.anchor().roster().weight(&address(20)), 0);
}

fn pending_successor(fixture: &mut Fixture) {
    for seed in [1, 2] {
        fixture.push(seed, BlockType::Endorse, CUTOFF + 67, |block| {
            block.link = destination(21);
            block.brn_spent += 1;
        });
    }
}

fn persist_outcome(
    fixture: &Fixture,
    store: &LmdbStore,
    heads: &[HistoryHead],
) -> CertifiedChallengeOutcome {
    let candidate = PreparedChallengeOutcome::prepare(
        store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 70,
        heads,
        &address(20),
    )
    .unwrap();
    let certificate = certify(&candidate.manifest().checkpoint, &[0, 1, 2, 3, 4, 5, 20]);
    candidate
        .certify(certificate)
        .unwrap()
        .persist(store, NetworkId::Dev, &fixture.params)
        .unwrap()
}

#[test]
fn mixed_chain_restarts_with_revoked_authority_and_no_duplicate_settlement() {
    let (mut fixture, selections) = completed(ChallengeReason::Fraud);
    merged_payments(&mut fixture);
    pending_successor(&mut fixture);
    let (dir, store, heads) = restore_chain(&fixture, &selections);
    let before = store.account_store().get_account(&address(20)).unwrap();
    let outcome = persist_outcome(&fixture, &store, &heads);
    {
        let mut batch = store.tx_begin_write().unwrap();
        assert!(!selections[0].live_epoch_ready(&batch).unwrap());
        assert!(crate::checkpoint_selection::stage(
            &mut batch,
            &selections[0],
            &fixture.params,
            &burst_verification::VerificationOrchestrator::new(),
        )
        .unwrap()
        .is_none());
    }
    let manifest = outcome.manifest().clone();
    let expected = bincode::serialize(&outcome.state().account(&address(20))).unwrap();
    let settlement_source = fixture
        .blocks
        .iter()
        .filter(|block| {
            block.block_type == BlockType::VerificationVote
                && block.timestamp == Timestamp::new(CUTOFF + 60)
        })
        .max_by_key(|block| *block.hash.as_bytes())
        .unwrap()
        .hash;
    let reward = bincode::serialize(
        &outcome
            .state()
            .pending(&address(8), settlement_source.into_tx_hash()),
    )
    .unwrap();
    let release = bincode::serialize(
        &outcome
            .state()
            .brn
            .get_wallet(&address(8))
            .unwrap()
            .pending_releases[&settlement_source],
    )
    .unwrap();
    assert_eq!(outcome.next_roster().total_humans(), 9);
    assert_eq!(outcome.next_roster().weight(&address(20)), 0);
    assert_eq!(
        bincode::serialize(&store.account_store().get_account(&address(20)).unwrap()).unwrap(),
        bincode::serialize(&before).unwrap(),
        "certificate persistence must not install live balances"
    );
    assert!(CertifiedCheckpoint::load_chain(&store, NetworkId::Dev, &fixture.params).is_err());
    let proposal = CertifiedCheckpoint::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 71,
        &heads,
        &address(21),
    )
    .unwrap();
    assert_eq!(proposal.checkpoint().height, 4);
    assert_eq!(proposal.checkpoint().parent, manifest.checkpoint.hash());
    for voters in [&[0, 1, 2, 3, 4, 5, 20][..], &[0, 1, 2, 3, 4, 5][..]] {
        assert!(CertifiedCheckpoint::accept(
            &store,
            NetworkId::Dev,
            &fixture.params,
            certify(proposal.checkpoint(), voters),
            heads.clone(),
            address(21)
        )
        .is_err());
    }
    let fourth = CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certify(proposal.checkpoint(), &[0, 1, 2, 3, 4, 5, 6]),
        heads.clone(),
        address(21),
    )
    .unwrap();
    let fourth_manifest = fourth.replayed().manifest();
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let restored = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    assert_eq!(restored.len(), 4);
    assert!(matches!(restored[2], CertifiedEvent::Outcome(_)));
    let anchor = chain::next_anchor(&restored, NetworkId::Dev, &fixture.params).unwrap();
    assert_eq!(anchor.roster().weight(&address(20)), 0);
    let history = fixture.history(CUTOFF + 71);
    let replay =
        ConfirmedReplay::replay_with_events(&history, NetworkId::Dev, &fixture.params, &restored)
            .unwrap();
    assert_eq!(
        bincode::serialize(&replay.account(&address(20))).unwrap(),
        expected
    );
    assert_eq!(
        bincode::serialize(&replay.pending(&address(8), settlement_source.into_tx_hash())).unwrap(),
        reward
    );
    assert_eq!(
        bincode::serialize(
            &replay.brn.get_wallet(&address(8)).unwrap().pending_releases[&settlement_source]
        )
        .unwrap(),
        release
    );
    for holder in [1, 8] {
        let revoked: u128 = replay
            .portfolio(&address(holder))
            .unwrap()
            .tokens
            .iter()
            .filter(|token| token.state == burst_types::TrstState::Revoked)
            .map(|token| token.amount)
            .sum();
        assert_eq!(revoked, 5_000);
    }
    assert_eq!(
        PreparedChallengeOutcome::replay_manifest(&store, &fixture.params, &manifest)
            .unwrap()
            .manifest(),
        &manifest
    );
    assert_eq!(
        CertifiedCheckpoint::replay_manifest(&store, &fixture.params, &fourth_manifest)
            .unwrap()
            .manifest(),
        fourth_manifest
    );
    assert!(CertifiedCheckpoint::prepare_next(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 71
    )
    .unwrap()
    .is_none());
    assert!(PreparedChallengeOutcome::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 72,
        &heads,
        &address(20)
    )
    .is_err());
    assert_eq!(
        CertifiedCheckpoint::stored_tip(&store, NetworkId::Dev, &fixture.params).unwrap(),
        4
    );
    assert_eq!(
        crate::checkpoint_beacon::selection_checkpoints(restored).len(),
        3
    );
    let store = std::sync::Arc::new(store);
    let mut runtime =
        crate::checkpoint_service::Runtime::new(crate::checkpoint_service::Settings {
            store: store.clone(),
            network: NetworkId::Dev,
            initial_params: fixture.params.clone(),
            key: Some(std::sync::Arc::new(
                burst_crypto::keypair_from_seed(&[0; 32]).private,
            )),
        })
        .unwrap();
    let clock: crate::checkpoint_service::Clock = std::sync::Arc::new(|| {
        Some(crate::checkpoint_replay::SigningClock {
            now: CUTOFF + 100,
            error_bound: 0,
        })
    });
    runtime
        .poll(&[], std::time::Instant::now(), &clock)
        .unwrap();
    assert!(
        crate::checkpoint_agreement::read_state(&store, &anchor, &address(0))
            .unwrap()
            .is_none()
    );
}

#[test]
fn mixed_chain_raw_import_checks_kind_domain_and_exact_parent_before_writing() {
    let (mut fixture, selections) = completed(ChallengeReason::Fraud);
    pending_successor(&mut fixture);
    let (_dir, source, heads) = restore_chain(&fixture, &selections);
    persist_outcome(&fixture, &source, &heads);
    let next = CertifiedCheckpoint::prepare(
        &source,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 71,
        &heads,
        &address(21),
    )
    .unwrap();
    CertifiedCheckpoint::accept(
        &source,
        NetworkId::Dev,
        &fixture.params,
        certify(next.checkpoint(), &[0, 1, 2, 3, 4, 5, 6]),
        heads,
        address(21),
    )
    .unwrap();
    let records: Vec<_> = (1..=4)
        .map(|height| {
            CertifiedCheckpoint::export_record(&source, NetworkId::Dev, &fixture.params, height)
                .unwrap()
                .unwrap()
        })
        .collect();
    let (_dir, store, _) = fixture.store();
    assert!(CertifiedChallengeOutcome::import_record(
        &store,
        NetworkId::Dev,
        &fixture.params,
        3,
        &records[2]
    )
    .is_err());
    for height in 1..=2 {
        CertifiedCheckpoint::import_record(
            &store,
            NetworkId::Dev,
            &fixture.params,
            height,
            &records[height as usize - 1],
        )
        .unwrap();
    }
    assert!(CertifiedCheckpoint::import_record(
        &store,
        NetworkId::Dev,
        &fixture.params,
        3,
        &records[2]
    )
    .is_err());
    assert!(CertifiedChallengeOutcome::import_record(
        &store,
        NetworkId::Dev,
        &fixture.params,
        2,
        &records[1]
    )
    .is_err());
    assert!(CertifiedChallengeOutcome::import_record(
        &store,
        NetworkId::Dev,
        &fixture.params,
        4,
        &records[2]
    )
    .is_err());
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Record {
        version: u8,
        certificate: CheckpointCertificate,
        heads: Vec<HistoryHead>,
        target: WalletAddress,
    }
    for mutation in 0..6 {
        let mut bad: Record = bincode::deserialize(&records[2]).unwrap();
        match mutation {
            0 => bad.target = address(8),
            1 => bad.heads.pop().map(|_| ()).unwrap(),
            2 => {
                bad.certificate.signatures.truncate(6);
            }
            3 => bad.version = 3,
            4 => bad.certificate.checkpoint.next_roster = BlockHash::ZERO,
            _ => {
                bad.certificate.checkpoint.selection_root =
                    selections[1].certificate().checkpoint.selection_root
            }
        }
        if mutation >= 4 {
            bad.certificate = certify(&bad.certificate.checkpoint, &[0, 1, 2, 3, 4, 5, 20]);
        }
        assert!(
            CertifiedChallengeOutcome::import_record(
                &store,
                NetworkId::Dev,
                &fixture.params,
                3,
                &bincode::serialize(&bad).unwrap()
            )
            .is_err(),
            "mutation {mutation}"
        );
        assert_eq!(
            CertifiedCheckpoint::stored_tip(&store, NetworkId::Dev, &fixture.params).unwrap(),
            2
        );
    }
    let mut trailing = records[2].clone();
    trailing.push(0);
    assert!(CertifiedChallengeOutcome::import_record(
        &store,
        NetworkId::Dev,
        &fixture.params,
        3,
        &trailing
    )
    .is_err());
    let mut relabeled = records[2].clone();
    relabeled[0] = 1;
    relabeled.push(0); // selection record's absent beacon
    assert!(CertifiedCheckpoint::import_record(
        &store,
        NetworkId::Dev,
        &fixture.params,
        3,
        &relabeled
    )
    .is_err());
    for _ in 0..2 {
        CertifiedChallengeOutcome::import_record(
            &store,
            NetworkId::Dev,
            &fixture.params,
            3,
            &records[2],
        )
        .unwrap();
    }
    let mut alternate: Record = bincode::deserialize(&records[2]).unwrap();
    alternate.certificate = certify(&alternate.certificate.checkpoint, &[0, 1, 2, 3, 4, 5, 6]);
    CertifiedChallengeOutcome::import_record(
        &store,
        NetworkId::Dev,
        &fixture.params,
        3,
        &bincode::serialize(&alternate).unwrap(),
    )
    .unwrap();
    CertifiedCheckpoint::import_record(&store, NetworkId::Dev, &fixture.params, 4, &records[3])
        .unwrap();
    CertifiedChallengeOutcome::import_record(
        &store,
        NetworkId::Dev,
        &fixture.params,
        3,
        &records[2],
    )
    .unwrap();
    CertifiedCheckpoint::import_record(&store, NetworkId::Dev, &fixture.params, 1, &records[0])
        .unwrap();
    let loaded = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    assert_eq!(loaded.len(), 4);
    assert_eq!(loaded[3].next_roster().weight(&address(20)), 0);
    for height in 1..=4 {
        assert_eq!(
            CertifiedCheckpoint::export_record(&store, NetworkId::Dev, &fixture.params, height)
                .unwrap()
                .unwrap(),
            records[height as usize - 1]
        );
    }
}

#[test]
fn post_outcome_legacy_block_tail_is_rejected_without_advancing_chain() {
    let (mut fixture, selections) = completed(ChallengeReason::Inactivity);
    pending_successor(&mut fixture);
    let (_dir, store, heads) = restore_chain(&fixture, &selections);
    persist_outcome(&fixture, &store, &heads);
    let records: Vec<_> = (1..=3)
        .map(|height| {
            CertifiedCheckpoint::export_record(&store, NetworkId::Dev, &fixture.params, height)
                .unwrap()
                .unwrap()
        })
        .collect();
    fixture.push(1, BlockType::ChangeRepresentative, CUTOFF + 72, |block| {
        block.representative = address(2)
    });
    let (_dir, receiver, new_heads) = fixture.store();
    for height in 1..=2 {
        CertifiedCheckpoint::import_record(
            &receiver,
            NetworkId::Dev,
            &fixture.params,
            height,
            &records[height as usize - 1],
        )
        .unwrap();
    }
    CertifiedChallengeOutcome::import_record(
        &receiver,
        NetworkId::Dev,
        &fixture.params,
        3,
        &records[2],
    )
    .unwrap();
    let error = CertifiedCheckpoint::prepare(
        &receiver,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 73,
        &new_heads,
        &address(21),
    )
    .err()
    .expect("legacy tail must fail closed");
    assert!(
        error.to_string().contains("payment epoch differs"),
        "{error}"
    );
    assert_eq!(
        CertifiedCheckpoint::stored_tip(&receiver, NetworkId::Dev, &fixture.params).unwrap(),
        3
    );
}

#[test]
fn consecutive_outcomes_replay_in_order_without_reopening_creator_bootstrap() {
    let (mut fixture, first, _) = pending_reason(ChallengeReason::Fraud);
    let (_dir, _store, second) = certify_challenge(&fixture, &first);
    challenge(&mut fixture, 0, ChallengeReason::Fraud, CUTOFF + 35);
    let block = fixture.blocks.last_mut().unwrap();
    block.payment_epoch = second.certificate().checkpoint.hash();
    sign(8, block);
    let mut selections = vec![first, second];
    let (_dir, store, heads) = restore_chain(&fixture, &selections);
    let request = CertifiedCheckpoint::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 36,
        &heads,
        &address(0),
    )
    .unwrap();
    let mut third = CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certify(request.checkpoint(), &[0, 1, 2, 3, 4, 5, 6]),
        heads,
        address(0),
    )
    .unwrap();
    third
        .attach_beacon(
            &store,
            &fixture.params,
            selections[1].beacon().unwrap().clone(),
        )
        .unwrap();
    selections.push(third);
    for index in [1, 2] {
        for seed in panel(&selections[index]) {
            vote_choice_in_epoch(
                &mut fixture,
                &selections[index],
                seed,
                CUTOFF + 59 + index as u64,
                2,
                selections[2].certificate().checkpoint.hash(),
            );
        }
    }
    let (dir, store, heads) = restore_chain(&fixture, &selections);
    let first_outcome = persist_outcome(&fixture, &store, &heads);
    assert_eq!(first_outcome.certificate().checkpoint.height, 4);
    assert!(
        first_outcome
            .state()
            .account(&address(8))
            .unwrap()
            .total_brn_staked
            > 0
    );
    let second_outcome = PreparedChallengeOutcome::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 71,
        &heads,
        &address(0),
    )
    .unwrap();
    assert_eq!(second_outcome.parent().roster().total_humans(), 9);
    assert_eq!(second_outcome.parent().roster().weight(&address(20)), 0);
    assert_eq!(second_outcome.parent().roster().weight(&address(0)), 1);
    let certificate = certify(
        &second_outcome.manifest().checkpoint,
        &[0, 1, 2, 3, 4, 5, 6],
    );
    let certified = second_outcome
        .certify(certificate)
        .unwrap()
        .persist(&store, NetworkId::Dev, &fixture.params)
        .unwrap();
    assert_eq!(certified.certificate().checkpoint.height, 5);
    assert_eq!(certified.next_roster().total_humans(), 8);
    assert!(certified.state().bootstrap_complete());
    assert_eq!(
        certified
            .state()
            .account(&address(8))
            .unwrap()
            .total_brn_staked,
        0
    );
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    assert_eq!(events.len(), 5);
    let replay = ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 72),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    assert!(replay.bootstrap_complete());
    assert_eq!(replay.human_roster().unwrap().total_humans(), 8);
    for seed in [0, 20] {
        assert_eq!(
            replay.account(&address(seed)).unwrap().state,
            WalletState::Revoked
        );
        assert_eq!(replay.human_roster().unwrap().weight(&address(seed)), 0);
    }
    assert_eq!(
        replay
            .brn
            .get_wallet(&address(20))
            .unwrap()
            .accrual_stopped_at,
        Some(Timestamp::new(CUTOFF + 70))
    );
    assert_eq!(
        replay
            .brn
            .get_wallet(&address(0))
            .unwrap()
            .accrual_stopped_at,
        Some(Timestamp::new(CUTOFF + 71))
    );
    for seed in 1..=8 {
        let portfolio = replay.portfolio(&address(seed)).unwrap();
        assert!(portfolio
            .tokens
            .iter()
            .any(|token| token.origin_wallet == address(0)));
        assert!(portfolio
            .tokens
            .iter()
            .filter(|token| token.origin_wallet == address(0))
            .all(|token| token.state == burst_types::TrstState::Revoked));
    }
}
