use super::*;

#[test]
fn reverification_preserves_frozen_brn_and_outstanding_release_proofs() {
    for previous in [WalletState::Revoked, WalletState::Deactivated] {
        let wallet = address(20);
        let mut account = burst_store::account::AccountInfo::unopened(wallet.clone());
        account.state = previous;
        account.total_brn_burned = 20;
        account.total_brn_staked = 10;
        let mut brn = BrnEngine::with_rate(3, Timestamp::new(10));
        let mut state = burst_brn::BrnWalletState::new(Timestamp::new(10));
        state.total_burned = 20;
        state.total_staked = 10;
        state.stop_accrual(Timestamp::new(30));
        state.resume_accrual(Timestamp::new(40));
        state
            .record_release(
                BlockHash::new([1; 32]),
                5,
                &[BlockHash::new([1; 32]), BlockHash::new([2; 32])],
                Timestamp::new(50),
            )
            .unwrap();
        state.stop_accrual(Timestamp::new(100));
        let frozen = state
            .available_balance_checked(&brn.rate_history, Timestamp::new(100))
            .unwrap();
        let releases = bincode::serialize(&state.pending_releases).unwrap();
        let selected = state.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES);
        assert!(!selected.is_empty());
        assert!(state.has_more_releases(&[]));
        assert!(!state.has_more_releases(&selected));
        brn.track_wallet(wallet.clone(), state);
        mark_account_verified(&mut account, Timestamp::new(200), &mut brn).unwrap();
        let restored = brn.get_wallet(&wallet).unwrap();
        assert_eq!(account.state, WalletState::Verified);
        assert_eq!(restored.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES), selected);
        assert!(restored.has_more_releases(&[]));
        assert!(!restored.has_more_releases(&selected));
        assert_eq!(account.total_brn_burned, 20);
        assert_eq!(account.total_brn_staked, 10);
        assert_eq!(
            restored.verified_at,
            Timestamp::new(10),
            "{previous:?} lost earned BRN"
        );
        assert_eq!(restored.total_burned, 20);
        assert_eq!(restored.total_staked, 10);
        assert_eq!(
            bincode::serialize(&restored.pending_releases).unwrap(),
            releases
        );
        assert_eq!(
            restored.paused_intervals,
            vec![
                (Timestamp::new(30), Timestamp::new(40)),
                (Timestamp::new(100), Timestamp::new(200))
            ]
        );
        assert_eq!(
            restored.available_balance_checked(&brn.rate_history, Timestamp::new(200)),
            Some(frozen)
        );
        assert_eq!(
            restored.spendable_balance_checked(&brn.rate_history, Timestamp::new(210)),
            Some(frozen + 30 - 5)
        );
    }
}

#[test]
fn signed_reverification_revision_undo_survives_fork_abort_and_reopen() {
    signed_reverification_revision_case(8);
}

#[test]
#[ignore = "explicit capped-host signed revision undo diagnostic; not consensus throughput"]
fn signed_reverification_revision_scaling() {
    for unrelated in [100, 1_000, 10_000] { signed_reverification_revision_case(unrelated); }
}

fn signed_reverification_revision_case(unrelated: usize) {
    use std::time::Instant;
    let mut f = Fixture::new();
    f.add_account(10);
    let merge = TxHash::new([90; 32]);
    let inputs: Vec<_> = [(10, 40), (0, 60)].into_iter().map(|(seed, amount)| {
        f.trst.mint(TxHash::new([seed + 70; 32]), address(6), amount,
            address(seed), test_time(50)).unwrap()
    }).collect();
    let token = f.trst.merge(&inputs, address(6), merge, test_time(60)).unwrap();
    f.trst.track_token(token);
    // Synthetic independent graph/history population; affected votes below
    // still use signed production transitions. No background balance is spent.
    let background = TxHash::new([71; 32]);
    f.trst.mint(background, address(1), 1, address(1), test_time(50)).unwrap();
    for index in 0..unrelated {
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&(index as u64 + 1).to_be_bytes());
        f.trst.merger_graph.record_merge(burst_trst::merger_graph::MergeNode {
            merge_tx: TxHash::new(bytes),
            source_origins: vec![burst_trst::merger_graph::MergeSource { origin: background, amount: 1 }],
            total_amount: 1, revoked_contribs: Default::default(),
        });
    }
    f.trst.merger_graph.apply_revocation(background);
    f.trst.merger_graph.apply_restoration(&background);
    {
        // This is a retained revoked-state fixture, not proof of the preceding
        // fraud verdict. The signed re-verification and replacement run below.
        let mut batch = f.store.tx_begin_write().unwrap();
        super::super::appeals::revoke_wallet(&mut batch, &f.target, test_time(70),
            &mut f.brn, &mut f.trst).unwrap();
        crate::ledger_updater::persist_economics(&mut batch, &f.brn, &f.trst).unwrap();
        crate::graph_storage::stage_replayed(&mut batch, &f.trst.merger_graph).unwrap();
        crate::trst_storage::stage_replayed_stable(&mut batch, &f.trst).unwrap();
        assert!(batch.uses_graph_records().unwrap());
        crate::bootstrap_phase::initialize(&mut batch, &f.params).unwrap();
        let genesis = batch.get_account(&address(0)).unwrap();
        crate::confirmed_authority::ConfirmedAuthority::initialize(&mut batch, &genesis).unwrap();
        batch.commit().unwrap();
    }
    f.migrate_verification();
    f = f.reopen();
    assert_eq!(f.trst.transferable_balance(&address(6), test_time(1000)), Some(60));
    let mut before_final = None;
    let mut final_vote = None;
    for seed in [1, 2, 3] {
        let vote = f.vote(seed, 1, 1000 + u64::from(seed));
        if seed == 3 { before_final = Some(f.trst.clone()); }
        let mut batch = f.store.tx_begin_write().unwrap();
        crate::payment_transition::authenticate_contents(&vote, &f.params).unwrap();
        let state = crate::payment_transition::restore(&batch, &f.params).unwrap();
        let mut engines = crate::economic_candidate::StagedEngines::new(&state.brn, &state.trst);
        let economic = crate::economic_candidate::stage(&mut batch, &vote, &f.params, &mut engines).unwrap();
        let mut verification = crate::verification_storage::load(&batch).unwrap();
        crate::verification_transition::apply_staged(&mut batch, &vote, &economic.effect,
            &f.params, burst_types::NetworkId::Dev, &mut verification, &mut engines).unwrap();
        engines.persist(&mut batch).unwrap();
        batch.commit().unwrap();
        f = f.reopen();
        final_vote = Some(vote);
    }
    let before = before_final.unwrap();
    let losing = final_vote.unwrap();
    let restored = f.trst.clone();
    assert_eq!(f.store.account_store().get_account(&f.target).unwrap().state, WalletState::Verified);
    assert_eq!(restored.transferable_balance_snapshot(&address(6)), Some(100));
    assert_ne!(before.merger_graph.to_bytes(), restored.merger_graph.to_bytes());
    let plan = crate::rollback_plan::preflight(&f.store, losing.hash).unwrap();
    assert_eq!(plan.blocks(), &[losing.hash]);
    assert!(plan.changes().iter().any(|change| change.key.starts_with(b"trst_graph/v1/6/")),
        "the completing signed vote must journal a composition revision");
    let previous: StateBlock = bincode::deserialize(&f.store.block_store().get_block(&losing.previous).unwrap()).unwrap();
    let mut winner = StateBlock {
        block_type: BlockType::ChangeRepresentative,
        link: BlockHash::ZERO, origin: TxHash::ZERO, transaction: TxHash::ZERO,
        brn_spent: previous.brn_spent,
        ..losing.clone()
    };
    sign(&mut winner, 3);
    let mut invalid = winner.clone();
    invalid.signature = Signature([0; 64]);
    assert!(crate::fork_resolution::evaluate(&f.store, &invalid, &f.params, invalid.timestamp,
        burst_types::NetworkId::Dev, true, None).is_err());
    assert_eq!(crate::fork_resolution::evaluate(&f.store, &winner, &f.params, winner.timestamp,
        burst_types::NetworkId::Dev, true, None).unwrap(), Some(losing.hash));
    assert!(crate::rollback_plan::replace_with(&f.store, losing.hash, &winner, |batch, block| {
        crate::payment_transition::apply(batch, block, &f.params, block.timestamp)?;
        Err::<(), _>(StoreError::Backend("injected replacement abort".into()))
    }).is_err());
    f = f.reopen();
    assert_eq!(f.trst.save_wallets(), restored.save_wallets());
    assert_eq!(f.trst.merger_graph.to_bytes(), restored.merger_graph.to_bytes());
    assert_eq!(f.store.account_store().get_account(&f.target).unwrap().state, WalletState::Verified);
    assert!(f.store.block_store().get_block(&losing.hash).is_ok());
    assert!(f.store.block_store().get_block(&winner.hash).is_err());
    for sample in 0..3 {
        // Establish the retained writer cut without timing cold restoration.
        let batch = f.store.tx_begin_write().unwrap();
        crate::payment_transition::restore(&batch, &f.params).unwrap();
        batch.commit().unwrap();
        let started = Instant::now();
        let mut batch = f.store.tx_begin_write().unwrap();
        let writer_ns = started.elapsed().as_nanos();
        let started = Instant::now();
        crate::payment_transition::restore(&batch, &f.params).unwrap();
        let warm_restore_ns = started.elapsed().as_nanos();
        let started = Instant::now();
        let mut rewind_ns = 0;
        let mut apply_ns = 0;
        let (result, height) = crate::rollback_plan::stage_with(&mut batch, losing.hash, &winner, |batch, block| {
            rewind_ns = started.elapsed().as_nanos();
            let applying = Instant::now();
            let result = crate::payment_transition::apply(batch, block, &f.params, block.timestamp);
            apply_ns = applying.elapsed().as_nanos();
            result
        }).unwrap();
        let staged_ns = started.elapsed().as_nanos();
        assert_eq!(result.removed.len(), 1);
        assert_eq!(result.state.trst.save_wallets(), before.save_wallets());
        assert!(result.state.trst.merger_graph == before.merger_graph);
        drop(result);
        let finishing = Instant::now();
        if sample == 2 { batch.commit_block(&winner.hash, &winner.account, height).unwrap(); }
        else { drop(batch); }
        let finish_ns = finishing.elapsed().as_nanos();
        let batch = f.store.tx_begin_write().unwrap();
        let state = crate::payment_transition::restore(&batch, &f.params).unwrap();
        let expected = if sample == 2 { &before } else { &restored };
        assert_eq!(state.trst.save_wallets(), expected.save_wallets());
        assert!(state.trst.merger_graph == expected.merger_graph);
        assert_eq!(batch.get_account(&address(3)).unwrap().head,
            if sample == 2 { winner.hash } else { losing.hash });
        let memory: Vec<_> = std::fs::read_to_string("/proc/self/status").unwrap().lines()
            .filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:"))
            .map(str::to_owned).collect();
        println!("signed_revision_replacement {}", serde_json::json!({
            "unrelated_merges":unrelated, "unrelated_revisions":unrelated * 2,
            "sample":sample, "committed":sample == 2, "writer_ns":writer_ns,
            "warm_restore_ns":warm_restore_ns, "plan_rewind_ns":rewind_ns,
            "apply_ns":apply_ns, "staged_ns":staged_ns, "finish_ns":finish_ns, "memory":memory,
        }));
    }

    f = f.reopen();
    assert_eq!(f.trst.save_wallets(), before.save_wallets());
    assert_eq!(f.trst.merger_graph.to_bytes(), before.merger_graph.to_bytes());
    assert_eq!(f.store.account_store().get_account(&f.target).unwrap().state, WalletState::Revoked);
    assert_eq!(f.store.account_store().get_account(&address(3)).unwrap().head, winner.hash);
    assert!(f.store.block_store().get_block(&losing.hash).is_err());
}
