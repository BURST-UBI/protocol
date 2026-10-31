use super::*;
use crate::checkpoint_deadline::PreparedDeadline;

fn install(store: &LmdbStore, params: &ProtocolParams, manifest: ReplayManifest, signers: &[u8]) {
    chain::accept_manifest(
        store,
        NetworkId::Dev,
        params,
        certified_votes::certify(manifest.checkpoint(), signers),
        &manifest,
    )
    .unwrap();
    PreparedInstallation::next(store, NetworkId::Dev, params)
        .unwrap()
        .unwrap()
        .commit(store)
        .unwrap();
}

#[test]
fn compact_checkpoint_counts_governance_then_activates_the_governed_rate() {
    let (mut fixture, _) = fixture_with_cooldown(10);
    fixture
        .blocks
        .retain(|block| block.timestamp.as_secs() <= 100);
    let (_dir, store, _) = fixture.store();
    let roster = PreparedRoster::prepare_next(&store, NetworkId::Dev, &fixture.params, 105)
        .unwrap()
        .unwrap();
    let epoch = roster.manifest().checkpoint.hash();
    install(
        &store,
        &fixture.params,
        ReplayManifest::Roster(roster.manifest().clone()),
        &[0],
    );
    let content = ProposalContent::ParameterChange {
        param: GovernableParam::BrnRate,
        new_value: 42,
    };
    let proposal = fixture.push(0, BlockType::GovernanceProposal, 110, |block| {
        block.payment_epoch = epoch;
        block.link = BlockHash::new(content.encode_compact().unwrap());
    });
    fixture.push(0, BlockType::GovernanceEndorse, 111, |block| {
        block.link = proposal;
        block.brn_spent += 1;
    });
    for seed in [0, 2] {
        fixture.push(seed, BlockType::GovernanceVote, 122, |block| {
            block.link = proposal;
        });
    }
    fixture.append_confirmed_prefix(&store, CUT - 1);
    let counting = PreparedGovernance::prepare_next(&store, NetworkId::Dev, &fixture.params, CUT)
        .unwrap()
        .unwrap();
    install(
        &store,
        &fixture.params,
        ReplayManifest::Governance(counting.manifest().clone()),
        &[0, 1, 2],
    );
    let activation =
        PreparedGovernance::prepare_next(&store, NetworkId::Dev, &fixture.params, CUT + 11)
            .unwrap()
            .unwrap();
    install(
        &store,
        &fixture.params,
        ReplayManifest::Governance(activation.manifest().clone()),
        &[0, 1, 2],
    );
    crate::confirmed_replay::tests::snapshot_history::boundaries::assert_chain(&fixture, &store);
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let state = ConfirmedReplay::replay_with_events(
        &fixture.history(CUT + 11),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    assert_eq!(state.params().brn_rate, 42);
    // The signed effect root does not excuse an altered manifest inventory.
    use crate::confirmed_replay::tests::snapshot_history::{
        authenticate, compact::retained, suffix_store,
    };
    let snapshot = authenticate(
        &store,
        &fixture.params,
        events[1].certificate().checkpoint.hash(),
    );
    let witnesses = retained(&snapshot, &fixture, &store, &[]);
    let base = snapshot.into_continuation();
    let (_tail_dir, suffix) = suffix_store(&[]);
    let event = &events[2];
    let tail = base
        .read_tail(
            &suffix,
            event.certificate().checkpoint.cutoff,
            event.heads(),
            witnesses,
        )
        .unwrap();
    let ReplayManifest::Governance(mut forged) = event.manifest() else {
        unreachable!()
    };
    assert!(!forged.proposals.is_empty());
    forged.proposals.clear();
    assert!(base
        .replay_manifest(
            tail,
            event.certificate(),
            &ReplayManifest::Governance(forged),
            &[],
        )
        .is_err());
}

#[test]
fn counting_schedule_reauthenticates_confirmed_tail_before_its_first_certificate() {
    use burst_store::meta::{encode_payment_vote_limit, MetaStore, GOVERNANCE_COUNTING_LIMIT_KEY};
    let (mut fixture, _) = fixture_with_cooldown(10);
    let blocks = fixture.blocks.clone();
    fixture.blocks.truncate(1);
    let (dir, store, _) = fixture.store();
    crate::payment_authority::initialize(&store, NetworkId::Dev, &fixture.params).unwrap();
    fixture.blocks = blocks;
    fixture.append_confirmed_prefix(&store, CUT - 1);
    let mut batch = store.tx_begin_write().unwrap();
    let mut schedule = crate::confirmed_governance::ConfirmedGovernance::load(&batch)
        .unwrap()
        .unwrap();
    for block in &fixture.blocks {
        schedule.promote(&batch, block).unwrap();
    }
    schedule.persist(&mut batch).unwrap();
    batch.commit().unwrap();
    let epoch = store.meta_store().payment_epoch().unwrap();
    let correct = encode_payment_vote_limit(epoch, Some(CUT));
    assert_eq!(
        store
            .meta_store()
            .get_meta(GOVERNANCE_COUNTING_LIMIT_KEY)
            .unwrap(),
        correct
    );
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    crate::payment_authority::initialize(&store, NetworkId::Dev, &fixture.params).unwrap();
    let bad = encode_payment_vote_limit(epoch, None);
    store
        .meta_store()
        .put_meta(GOVERNANCE_COUNTING_LIMIT_KEY, &bad)
        .unwrap();
    assert!(crate::payment_authority::initialize(&store, NetworkId::Dev, &fixture.params).is_err());
    assert_eq!(
        store
            .meta_store()
            .get_meta(GOVERNANCE_COUNTING_LIMIT_KEY)
            .unwrap(),
        bad
    );
}

#[test]
fn legacy_confirmed_governance_cannot_partially_initialize_certified_authority() {
    use burst_store::{meta::MetaStore, StoreError};
    let (fixture, _) = fixture_with_cooldown(10);
    let (_dir, store, _) = fixture.store();
    assert!(crate::payment_authority::initialize(&store, NetworkId::Dev, &fixture.params).is_err());
    for key in [
        crate::confirmed_replay::installation::INSTALLED_KEY,
        crate::confirmed_replay::installation::CUTOFF_KEY,
        burst_store::meta::PAYMENT_VOTE_LIMIT_KEY,
        burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY,
        crate::confirmed_governance::KEY,
        crate::confirmed_governance::HEADER,
    ] {
        assert!(
            matches!(
                store.meta_store().get_meta(key),
                Err(StoreError::NotFound(_))
            ),
            "{key}"
        );
    }
}

#[test]
fn certified_activation_fences_later_votes_and_reauthenticates_its_limit_on_restart() {
    use burst_store::meta::{
        decode_payment_vote_limit, encode_payment_vote_limit, MetaStore, PAYMENT_VOTE_LIMIT_KEY,
    };
    let (mut fixture, _) = fixture_with_cooldown(10);
    let (dir, store, _) = fixture.store();
    let ballot = PreparedGovernance::prepare_next(&store, NetworkId::Dev, &fixture.params, CUT)
        .unwrap()
        .unwrap();
    install(
        &store,
        &fixture.params,
        ReplayManifest::Governance(ballot.manifest().clone()),
        &[0],
    );
    let epoch = store.meta_store().payment_epoch().unwrap();
    let limit = decode_payment_vote_limit(
        &store.meta_store().get_meta(PAYMENT_VOTE_LIMIT_KEY).unwrap(),
        epoch,
    )
    .unwrap()
    .unwrap();
    assert_eq!(limit, CUT + 11);
    let late = fixture.push(0, BlockType::Burn, limit + 1, |block| {
        block.brn_spent += 1;
        block.link = destination(1);
        block.payment_epoch = epoch;
    });
    store
        .meta_store()
        .put_fork_candidate(
            &late,
            &bincode::serialize(fixture.blocks.last().unwrap()).unwrap(),
        )
        .unwrap();
    let keys = burst_crypto::keypair_from_seed(&[0; 32]);
    let mut generator = burst_consensus::VoteGenerator::new(address(0), keys.private.0);
    for final_vote in [false, true] {
        let error = crate::durable_votes::generate_vote(&store, &mut generator, late, final_vote)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("crosses certified governance activation"),
            "{error}"
        );
    }
    assert_eq!(generator.sequence(), 0);
    fixture.blocks.pop();
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    crate::payment_authority::initialize(&store, NetworkId::Dev, &fixture.params).unwrap();
    let saved = store.meta_store().get_meta(PAYMENT_VOTE_LIMIT_KEY).unwrap();
    for bad in [
        encode_payment_vote_limit(epoch, None),
        encode_payment_vote_limit(epoch, Some(limit + 1)),
    ] {
        store
            .meta_store()
            .put_meta(PAYMENT_VOTE_LIMIT_KEY, &bad)
            .unwrap();
        assert!(
            crate::payment_authority::initialize(&store, NetworkId::Dev, &fixture.params).is_err()
        );
        assert_eq!(
            store.meta_store().get_meta(PAYMENT_VOTE_LIMIT_KEY).unwrap(),
            bad
        );
    }
    store
        .meta_store()
        .put_meta(PAYMENT_VOTE_LIMIT_KEY, &saved)
        .unwrap();
    let before = fixture.push(0, BlockType::Burn, limit - 1, |block| {
        block.brn_spent += 1;
        block.link = destination(1);
        block.payment_epoch = epoch;
    });
    fixture.append_confirmed_prefix(&store, limit - 1);
    assert!(crate::durable_votes::generate_vote(&store, &mut generator, before, true).is_ok());
    let activation =
        PreparedGovernance::prepare_next(&store, NetworkId::Dev, &fixture.params, limit)
            .unwrap()
            .unwrap();
    install(
        &store,
        &fixture.params,
        ReplayManifest::Governance(activation.manifest().clone()),
        &[0, 1, 2],
    );
    let next = store.meta_store().payment_epoch().unwrap();
    crate::confirmed_replay::tests::snapshot_history::boundaries::assert_chain(&fixture, &store);
    assert_eq!(
        decode_payment_vote_limit(
            &store.meta_store().get_meta(PAYMENT_VOTE_LIMIT_KEY).unwrap(),
            next
        )
        .unwrap(),
        None
    );
    crate::payment_authority::initialize(&store, NetworkId::Dev, &fixture.params).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn governance_expiry_boundaries_preserve_pending_ownership_and_retroactive_period_changes() {
    // Expiry at activation uses the resulting period. A return at an earlier
    // certified cut remains owned by its sender even when an extension revives it.
    for (old_period, new_period, minted, stays_pending, sender_active, sender_expired) in [
        (10, 20, 132, true, 0, 0),
        (10, u64::MAX as u128, 132, true, 0, 0),
        (10, 0, 132, false, 0, 40),
        (20, 5, 132, false, 0, 40),
        (10, 20, 131, false, 40, 0),
        (10, 20, 130, false, 40, 0),
    ] {
        let (mut fixture, proposals) = fixture_with_settings(
            0,
            1,
            |params| params.trst_expiry_secs = old_period,
            vec![ProposalContent::ParameterChange {
                param: GovernableParam::TrstExpirySecs,
                new_value: new_period,
            }],
        );
        for offset in 0..4 {
            fixture.push(0, BlockType::ChangeRepresentative, 124 + offset, |_| {});
        }
        let burn = fixture.push(0, BlockType::Burn, minted, |block| {
            block.brn_spent += 40;
            block.link = destination(1);
        });
        let (_dir, store, _) = fixture.store();
        let mut ballot_signers = vec![0];
        if minted + old_period < CUT {
            let error =
                PreparedGovernance::prepare_next(&store, NetworkId::Dev, &fixture.params, CUT)
                    .err()
                    .expect("an older pending expiry was skipped");
            assert!(
                error.to_string().contains("earlier pending expiry"),
                "{error}"
            );
            let deadline =
                PreparedDeadline::prepare_next(&store, NetworkId::Dev, &fixture.params, CUT - 1)
                    .unwrap()
                    .unwrap();
            install(
                &store,
                &fixture.params,
                ReplayManifest::Deadline(deadline.manifest().clone()),
                &[0],
            );
            ballot_signers = vec![0, 1, 2];
        }
        let ballot = PreparedGovernance::prepare_next(&store, NetworkId::Dev, &fixture.params, CUT)
            .unwrap()
            .unwrap();
        install(
            &store,
            &fixture.params,
            ReplayManifest::Governance(ballot.manifest().clone()),
            &ballot_signers,
        );
        let activation =
            PreparedGovernance::prepare_next(&store, NetworkId::Dev, &fixture.params, CUT + 1)
                .unwrap()
                .unwrap();
        install(
            &store,
            &fixture.params,
            ReplayManifest::Governance(activation.manifest().clone()),
            &[0, 1, 2],
        );
        let claim_start = fixture.blocks.len();
        let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
        let state = ConfirmedReplay::replay_with_events(
            &fixture.history(CUT + 2),
            NetworkId::Dev,
            &fixture.params,
            &events,
        )
        .unwrap();
        assert_eq!(state.params().trst_expiry_secs, new_period as u64);
        assert_eq!(
            state.pending(&address(1), burn.into_tx_hash()).is_some(),
            stays_pending,
            "expiry change {old_period} -> {new_period}, minted at {minted}"
        );
        assert_eq!(
            (
                state
                    .portfolio(&address(0))
                    .map_or(0, |portfolio| portfolio.cached_transferable),
                state.account(&address(0)).unwrap().expired_trst
            ),
            (sender_active, sender_expired),
            "expiry change {old_period} -> {new_period}, minted at {minted}"
        );
        assert_eq!(
            state
                .governance
                .as_ref()
                .unwrap()
                .governance
                .get_proposal(&proposals[0])
                .unwrap()
                .phase,
            GovernancePhase::Activated
        );

        let fresh_dir = tempfile::tempdir().unwrap();
        let fresh = LmdbStore::open(fresh_dir.path(), 32, 32 << 20).unwrap();
        let mut batch = fresh.tx_begin_write().unwrap();
        crate::payment_epoch::initialize(&mut batch, fixture.blocks[0].hash).unwrap();
        batch.commit().unwrap();
        for height in 1..=events.len() as u64 {
            let bytes = CertifiedCheckpoint::export_transfer(
                &store,
                NetworkId::Dev,
                &fixture.params,
                height,
            )
            .unwrap()
            .unwrap();
            CertifiedCheckpoint::import_transfer(
                &fresh,
                NetworkId::Dev,
                &fixture.params,
                height,
                &bytes,
                &HistoricalAdmissions::default(),
            )
            .unwrap();
            PreparedInstallation::next(&fresh, NetworkId::Dev, &fixture.params)
                .unwrap()
                .unwrap()
                .commit(&fresh)
                .unwrap();
        }
        let sender = fresh.account_store().get_account(&address(0)).unwrap();
        let restored = crate::trst_storage::load(
            &fresh.tx_begin_write().unwrap(), new_period as u64).unwrap();
        assert_eq!(
            (
                restored
                    .get_portfolio(&address(0))
                    .map_or(0, |portfolio| portfolio.cached_transferable),
                sender.expired_trst
            ),
            (sender_active, sender_expired)
        );
        assert_eq!(
            fresh
                .pending_store()
                .get_pending(&address(1), &burn.into_tx_hash())
                .is_ok(),
            stays_pending
        );

        fixture.push(1, BlockType::Receive, CUT + 2, |block| {
            block.link = burn;
            block.trst_balance += 40;
            block.payment_epoch = activation.manifest().checkpoint.hash();
            block.params_hash = state.params().params_hash();
        });
        let received = ConfirmedReplay::replay_with_events(
            &fixture.history(CUT + 2),
            NetworkId::Dev,
            &fixture.params,
            &events,
        );
        if stays_pending {
            assert_eq!(
                received.unwrap().account(&address(1)).unwrap().trst_balance,
                40
            );
        } else {
            assert!(received
                .err()
                .unwrap()
                .to_string()
                .contains("missing replayed pending"));
            fixture.blocks.pop();
            let receipt = state.pending(&address(0), burn.into_tx_hash()).unwrap();
            assert!(receipt.return_credit);
            assert_eq!(receipt.amount, 40);
            for (kind, credit) in [(BlockType::RejectReceive, 0), (BlockType::Receive, 39)] {
                fixture.push(0, kind, CUT + 2, |block| {
                    block.link = burn;
                    block.trst_balance += credit;
                    block.payment_epoch = activation.manifest().checkpoint.hash();
                    block.params_hash = state.params().params_hash();
                });
                assert!(ConfirmedReplay::replay_with_events(
                    &fixture.history(CUT + 2),
                    NetworkId::Dev,
                    &fixture.params,
                    &events
                )
                .is_err());
                fixture.blocks.pop();
            }
            fixture.push(0, BlockType::Receive, CUT + 2, |block| {
                block.link = burn;
                block.trst_balance += 40;
                block.payment_epoch = activation.manifest().checkpoint.hash();
                block.params_hash = state.params().params_hash();
            });
            let claimed = ConfirmedReplay::replay_with_events(
                &fixture.history(CUT + 2),
                NetworkId::Dev,
                &fixture.params,
                &events,
            )
            .unwrap();
            assert!(claimed.pending(&address(0), burn.into_tx_hash()).is_none());
            assert_eq!(claimed.account(&address(0)).unwrap().trst_balance, 40);
            assert_eq!(
                claimed
                    .portfolio(&address(0))
                    .unwrap()
                    .tokens
                    .iter()
                    .map(|token| token.amount)
                    .sum::<u128>(),
                40
            );
            fixture.push(0, BlockType::Receive, CUT + 3, |block| {
                block.link = burn;
                block.trst_balance += 40;
                block.params_hash = state.params().params_hash();
            });
            assert!(ConfirmedReplay::replay_with_events(
                &fixture.history(CUT + 3),
                NetworkId::Dev,
                &fixture.params,
                &events
            )
            .is_err());
            fixture.blocks.pop();
            let send = fixture.push(0, BlockType::Send, CUT + 3, |block| {
                block.link = destination(1);
                block.origin = burn.into_tx_hash();
                block.trst_balance -= 40;
                block.params_hash = state.params().params_hash();
            });
            let sent = ConfirmedReplay::replay_with_events(
                &fixture.history(CUT + 3),
                NetworkId::Dev,
                &fixture.params,
                &events,
            );
            if sender_active == 0 {
                assert!(
                    sent.is_err(),
                    "claiming nominal credit cannot revive expired tokens"
                );
            } else {
                assert_eq!(
                    sent.unwrap()
                        .pending(&address(1), send.into_tx_hash())
                        .unwrap()
                        .amount,
                    40
                );
                fixture.push(1, BlockType::Receive, CUT + 4, |block| {
                    block.link = send;
                    block.trst_balance += 40;
                    block.params_hash = state.params().params_hash();
                });
                let delivered = ConfirmedReplay::replay_with_events(
                    &fixture.history(CUT + 4),
                    NetworkId::Dev,
                    &fixture.params,
                    &events,
                )
                .unwrap();
                assert_eq!(delivered.account(&address(1)).unwrap().trst_balance, 40);
                assert_eq!(
                    delivered
                        .portfolio(&address(1))
                        .unwrap()
                        .cached_transferable,
                    40
                );
                crate::confirmed_replay::tests::snapshot_history::installed_recovery::body_retirement::assert_governed_restoration(
                    &fixture, std::sync::Arc::new(store),
                    ballot.manifest().checkpoint.hash(),
                    activation.manifest().checkpoint.hash(),
                    &fixture.blocks[claim_start..], &delivered,
                ).await;
            }
        }
    }
}

#[test]
fn certified_bootstrap_veto_survives_replay_while_other_approval_activates() {
    let (mut fixture, proposals) = fixture_with_cooldown(10);
    let (_dir, store, _) = fixture.store();
    let ballot = PreparedGovernance::prepare_next(&store, NetworkId::Dev, &fixture.params, CUT)
        .unwrap().unwrap();
    let epoch = ballot.manifest().checkpoint.hash();
    install(&store, &fixture.params, ReplayManifest::Governance(ballot.manifest().clone()), &[0]);
    fixture.push(0, BlockType::GovernanceVeto, CUT + 1, |block| {
        block.payment_epoch = epoch;
        block.link = BlockHash::new(*proposals[0].as_bytes());
        block.transaction = TxHash::ZERO;
    });
    fixture.append_confirmed_prefix(&store, CUT + 2);
    let activation = PreparedGovernance::prepare_next(&store, NetworkId::Dev, &fixture.params, CUT + 11)
        .unwrap().unwrap();
    assert_eq!(activation.manifest().proposals, vec![proposals[1]]);
    install(&store, &fixture.params, ReplayManifest::Governance(activation.manifest().clone()), &[0, 1, 2]);
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let restored = ConfirmedReplay::replay_with_events(&fixture.history(CUT + 11),
        NetworkId::Dev, &fixture.params, &events).unwrap();
    let governance = &restored.governance.as_ref().unwrap().governance;
    assert_eq!(governance.get_proposal(&proposals[0]).unwrap().phase, GovernancePhase::Vetoed);
    assert_eq!(governance.get_proposal(&proposals[1]).unwrap().phase, GovernancePhase::Activated);
    assert_eq!(restored.params().brn_rate, 3);
    crate::confirmed_replay::tests::snapshot_history::boundaries::assert_chain(&fixture, &store);
}
