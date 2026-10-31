use super::*;

#[test]
fn verification_fork_candidates_validate_votes_and_challenges_without_publication() {
    for challenge in [false, true] {
        let mut fixture = Fixture::new();
        if challenge {
            fixture.verify_target();
        }
        {
            let mut batch = fixture.store.tx_begin_write().unwrap();
            crate::bootstrap_phase::initialize(&mut batch, &fixture.params).unwrap();
            batch.put_meta(
            crate::endorsement_commit::SNAPSHOT_KEY,
            &fixture.orchestrator.snapshot().encode().unwrap(),
        ).unwrap();
            let genesis = batch.get_account(&address(0)).unwrap();
            crate::confirmed_authority::ConfirmedAuthority::initialize(&mut batch, &genesis)
                .unwrap();
            batch.commit().unwrap();
        }
        let candidate = if challenge {
            fixture.challenge(burst_verification::ChallengeReason::Fraud)
        } else {
            fixture.vote(1, 1, 1001)
        };
        let seed = if challenge { 0 } else { 1 };
        let mut incumbent = candidate.clone();
        incumbent.block_type = BlockType::ChangeRepresentative;
        incumbent.link = BlockHash::ZERO;
        incumbent.origin = TxHash::ZERO;
        incumbent.transaction = TxHash::ZERO;
        let previous: StateBlock = bincode::deserialize(
            &fixture
                .store
                .block_store()
                .get_block(&candidate.previous)
                .unwrap(),
        )
        .unwrap();
        incumbent.brn_spent = previous.brn_spent;
        sign(&mut incumbent, seed);
        {
            let mut batch = fixture.store.tx_begin_write().unwrap();
            crate::payment_transition::apply(
                &mut batch,
                &incumbent,
                &fixture.params,
                incumbent.timestamp,
            )
            .unwrap();
            batch.commit().unwrap();
        }
        fixture = fixture.reopen();
        let before = fixture.orchestrator.snapshot().encode().unwrap();
        for enabled in [false, true] {
            let result = crate::fork_resolution::evaluate(
                &fixture.store,
                &candidate,
                &fixture.params,
                candidate.timestamp,
                burst_types::NetworkId::Dev,
                enabled,
                None,
            );
            if enabled {
                assert_eq!(result.unwrap(), Some(incumbent.hash));
            } else {
                assert!(result.is_err());
            }
        }
        assert_eq!(
            fixture
                .store
                .account_store()
                .get_account(&candidate.account)
                .unwrap()
                .head,
            incumbent.hash
        );
        assert!(fixture
            .store
            .block_store()
            .get_block(&candidate.hash)
            .is_err());
        fixture = fixture.reopen();
        assert_eq!(fixture.orchestrator.snapshot().encode().unwrap(), before);
        assert_eq!(
            fixture.brn.get_wallet(&address(seed)).unwrap().total_staked,
            0
        );
    }
}

#[test]
fn independent_endorsement_replacement_preserves_other_progress_after_reopen() {
    let mut fixture = Fixture::new();
    {
        let mut batch = fixture.store.tx_begin_write().unwrap();
        crate::bootstrap_phase::initialize(&mut batch, &fixture.params).unwrap();
        batch.put_meta(
            crate::endorsement_commit::SNAPSHOT_KEY,
            &fixture.orchestrator.snapshot().encode().unwrap(),
        ).unwrap();
        let genesis = batch.get_account(&address(0)).unwrap();
        crate::confirmed_authority::ConfirmedAuthority::initialize(&mut batch, &genesis).unwrap();
        batch.commit().unwrap();
    }
    let mut endorsements = Vec::new();
    for seed in [1, 2] {
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let account = batch.get_account(&address(seed)).unwrap();
        let previous: StateBlock =
            bincode::deserialize(&batch.get_block(&account.head).unwrap()).unwrap();
        let mut block = StateBlock {
            block_type: BlockType::Endorse,
            previous: previous.hash,
            link: BlockHash::new(
                burst_crypto::decode_address(address(seed + 10).as_str()).unwrap(),
            ),
            brn_spent: previous.brn_spent + fixture.params.endorsement_burn_amount,
            timestamp: test_time(1000),
            ..previous
        };
        sign(&mut block, seed);
        crate::payment_transition::authenticate_contents(&block, &fixture.params).unwrap();
        let mut engines =
            crate::economic_candidate::StagedEngines::new(&fixture.brn, &fixture.trst);
        let economic =
            crate::economic_candidate::stage(&mut batch, &block, &fixture.params, &mut engines)
                .unwrap();
        let EconomicResult::Endorse { burn_amount, .. } = economic.effect else {
            panic!("expected endorsement");
        };
        let mut verification = crate::endorsement_commit::restore(&batch).unwrap();
        crate::endorsement_commit::apply_staged(
            &mut batch,
            &block,
            burn_amount,
            burst_types::NetworkId::Dev,
            &fixture.params,
            &mut verification,
            &mut engines,
        )
        .unwrap();
        engines.persist(&mut batch).unwrap();
        batch.commit().unwrap();
        let (brn, trst) = engines.into_staged();
        fixture.brn = brn.unwrap();
        assert!(
            trst.is_none(),
            "incomplete endorsement must not prepare TRST"
        );
        fixture.orchestrator = verification;
        endorsements.push(block);
    }
    fixture = fixture.reopen();
    for (index, losing) in endorsements.iter().enumerate() {
        let seed = index as u8 + 1;
        let mut winner = losing.clone();
        winner.block_type = BlockType::ChangeRepresentative;
        winner.link = BlockHash::ZERO;
        winner.brn_spent = 0;
        sign(&mut winner, seed);
        assert!(crate::fork_resolution::evaluate(
            &fixture.store,
            &winner,
            &fixture.params,
            winner.timestamp,
            burst_types::NetworkId::Dev,
            false,
            None
        )
        .is_err());
        assert_eq!(
            crate::fork_resolution::evaluate(
                &fixture.store,
                &winner,
                &fixture.params,
                winner.timestamp,
                burst_types::NetworkId::Dev,
                true,
                None
            )
            .unwrap(),
            Some(losing.hash)
        );
        let plan = crate::rollback_plan::preflight(&fixture.store, losing.hash).unwrap();
        assert_eq!(plan.blocks(), &[losing.hash]);
        let original_snapshot = fixture
            .store
            .meta_store()
            .get_meta(crate::endorsement_commit::SNAPSHOT_KEY)
            .unwrap();
        assert!(crate::rollback_plan::replace_with(
            &fixture.store,
            losing.hash,
            &winner,
            |batch, block| {
                crate::payment_transition::apply(batch, block, &fixture.params, block.timestamp)?;
                Err::<(), _>(StoreError::Backend(
                    "injected failure after replacement staging".into(),
                ))
            }
        )
        .is_err());
        assert_eq!(
            fixture
                .store
                .meta_store()
                .get_meta(crate::endorsement_commit::SNAPSHOT_KEY)
                .unwrap(),
            original_snapshot
        );
        assert!(fixture.store.block_store().get_block(&losing.hash).is_ok());
        assert!(fixture.store.block_store().get_block(&winner.hash).is_err());
        crate::rollback_plan::replace_with(&fixture.store, losing.hash, &winner, |batch, block| {
            crate::payment_transition::apply(batch, block, &fixture.params, block.timestamp)
        })
        .unwrap();
        let batch = fixture.store.tx_begin_write().unwrap();
        let verification = crate::endorsement_commit::restore(&batch).unwrap();
        assert!(verification.get_state(&address(11)).is_none());
        assert_eq!(verification.get_state(&address(12)).is_some(), index == 0);
        assert!(verification.get_state(&fixture.target).is_some());
        let economics = crate::payment_transition::restore(&batch, &fixture.params).unwrap();
        assert_eq!(
            economics.brn.get_wallet(&address(1)).unwrap().total_burned,
            0
        );
        assert_eq!(
            economics.brn.get_wallet(&address(2)).unwrap().total_burned,
            if index == 0 {
                fixture.params.endorsement_burn_amount
            } else {
                0
            }
        );
        if index == 0 {
            assert!(batch.get_block(&endorsements[1].hash).is_ok());
        }
        drop(batch);
        fixture = fixture.reopen();
    }
}

#[test]
fn verifier_spending_returned_stake_waits_for_the_complete_outcome() {
    let mut fixture = Fixture::new();
    let mut batch = fixture.store.tx_begin_write().unwrap();
    let genesis = batch.get_account(&address(0)).unwrap();
    crate::confirmed_authority::ConfirmedAuthority::initialize(&mut batch, &genesis).unwrap();
    let mut authority = crate::confirmed_authority::ConfirmedAuthority::load(&batch).unwrap();
    for account in fixture.store.account_store().all_accounts().unwrap() {
        batch
            .advance_confirmation_height(&account.address, 1)
            .unwrap();
        authority.accounts.insert(
            account.address.to_string(),
            crate::confirmed_authority::Authority::from(&account),
        );
    }
    authority.persist(&mut batch).unwrap();
    batch.commit().unwrap();
    let votes: Vec<_> = (1..=3)
        .map(|seed| fixture.vote(seed, 1, 1000 + u64::from(seed)))
        .collect();
    for vote in &votes[..2] {
        fixture.apply(vote, true).unwrap();
        fixture
            .store
            .meta_store()
            .enqueue_confirmation(&vote.hash)
            .unwrap();
        assert!(
            crate::causal_cementation::cement(&fixture.store, vote.hash)
                .unwrap()
                .complete
        );
    }
    fixture.apply(&votes[2], true).unwrap();
    let spend_time = 1004;
    let available = fixture.brn.compute_balance(
        fixture.brn.get_wallet(&address(1)).unwrap(),
        test_time(spend_time),
    );
    let payment = fixture
        .payment(
            1,
            BlockType::Burn,
            available,
            (
                BlockHash::new(burst_crypto::decode_address(address(6).as_str()).unwrap()),
                TxHash::ZERO,
            ),
            spend_time,
        )
        .unwrap();
    fixture = fixture.reopen();
    let block: StateBlock =
        bincode::deserialize(&fixture.store.block_store().get_block(&payment).unwrap()).unwrap();
    let rollback = crate::rollback_plan::preflight(&fixture.store, votes[2].hash).unwrap();
    assert_eq!(rollback.blocks(), &[payment, votes[2].hash]);
    let mut batch = fixture.store.tx_begin_write().unwrap();
    assert!(
        !crate::causal_cementation::dependencies_confirmed(&batch, &block).unwrap(),
        "a verifier cannot finalize spending unlocked stake while its outcome is unconfirmed"
    );
    assert!(!crate::causal_cementation::handoff(&mut batch, payment).unwrap());
    drop(batch);
    fixture
        .store
        .meta_store()
        .enqueue_confirmation(&votes[2].hash)
        .unwrap();
    assert!(
        crate::causal_cementation::cement(&fixture.store, votes[2].hash)
            .unwrap()
            .complete
    );
    let mut batch = fixture.store.tx_begin_write().unwrap();
    assert!(crate::causal_cementation::handoff(&mut batch, payment).unwrap());
    batch.commit().unwrap();
    assert!(
        crate::causal_cementation::cement(&fixture.store, payment)
            .unwrap()
            .complete
    );
}

#[test]
fn ordinary_unlocked_spending_does_not_wait_for_or_confirm_a_stake_outcome() {
    let mut fixture = Fixture::new();
    let mut batch = fixture.store.tx_begin_write().unwrap();
    let genesis = batch.get_account(&address(0)).unwrap();
    crate::confirmed_authority::ConfirmedAuthority::initialize(&mut batch, &genesis).unwrap();
    let mut authority = crate::confirmed_authority::ConfirmedAuthority::load(&batch).unwrap();
    for account in fixture.store.account_store().all_accounts().unwrap() {
        batch
            .advance_confirmation_height(&account.address, 1)
            .unwrap();
        authority.accounts.insert(
            account.address.to_string(),
            crate::confirmed_authority::Authority::from(&account),
        );
    }
    authority.persist(&mut batch).unwrap();
    batch.commit().unwrap();
    let votes: Vec<_> = (1..=3)
        .map(|seed| fixture.vote(seed, 1, 1000 + u64::from(seed)))
        .collect();
    for vote in &votes[..2] {
        fixture.apply(vote, true).unwrap();
        fixture
            .store
            .meta_store()
            .enqueue_confirmation(&vote.hash)
            .unwrap();
        assert!(
            crate::causal_cementation::cement(&fixture.store, vote.hash)
                .unwrap()
                .complete
        );
    }
    fixture.apply(&votes[2], true).unwrap();
    let mut payment = StateBlock {
        block_type: BlockType::Burn,
        previous: votes[0].hash,
        brn_spent: votes[0].brn_spent + 1,
        link: BlockHash::new(burst_crypto::decode_address(address(6).as_str()).unwrap()),
        origin: TxHash::ZERO,
        transaction: TxHash::ZERO,
        release_sources: Vec::new(),
        timestamp: test_time(1004),
        ..votes[0].clone()
    };
    sign(&mut payment, 1);
    let mut batch = fixture.store.tx_begin_write().unwrap();
    crate::payment_transition::apply(&mut batch, &payment, &fixture.params, payment.timestamp)
        .unwrap();
    batch.commit().unwrap();
    let mut batch = fixture.store.tx_begin_write().unwrap();
    assert!(crate::causal_cementation::handoff(&mut batch, payment.hash).unwrap());
    batch.commit().unwrap();
    assert!(
        crate::causal_cementation::cement(&fixture.store, payment.hash)
            .unwrap()
            .complete
    );
    let voter = fixture
        .store
        .account_store()
        .get_account(&address(3))
        .unwrap();
    assert_eq!(
        voter.confirmation_height, 1,
        "ordinary spending must not finalize another voter's outcome"
    );
    fixture
        .store
        .meta_store()
        .enqueue_confirmation(&votes[2].hash)
        .unwrap();
    assert!(
        crate::causal_cementation::cement(&fixture.store, votes[2].hash)
            .unwrap()
            .complete
    );
    fixture = fixture.reopen();
    let account = fixture
        .store
        .account_store()
        .get_account(&address(1))
        .unwrap();
    assert_eq!(account.head, payment.hash);
    assert_eq!(account.total_brn_burned, 1);
    let wallet = fixture.brn.get_wallet(&address(1)).unwrap();
    assert_eq!(
        wallet
            .pending_releases
            .values()
            .map(|release| release.amount)
            .sum::<u128>(),
        20
    );
}

#[test]
fn verification_outcome_waits_for_a_targets_interleaved_representative_change() {
    let mut fixture = Fixture::new();
    fixture.add_account(10);
    let mut target = fixture
        .store
        .account_store()
        .get_account(&fixture.target)
        .unwrap();
    target.state = WalletState::Unverified;
    target.verified_at = None;
    fixture.store.account_store().put_account(&target).unwrap();
    let mut batch = fixture.store.tx_begin_write().unwrap();
    let genesis = batch.get_account(&address(0)).unwrap();
    crate::confirmed_authority::ConfirmedAuthority::initialize(&mut batch, &genesis).unwrap();
    let mut authority = crate::confirmed_authority::ConfirmedAuthority::load(&batch).unwrap();
    for account in fixture.store.account_store().all_accounts().unwrap() {
        batch
            .advance_confirmation_height(&account.address, 1)
            .unwrap();
        authority.accounts.insert(
            account.address.to_string(),
            crate::confirmed_authority::Authority::from(&account),
        );
    }
    authority.persist(&mut batch).unwrap();
    batch.commit().unwrap();
    let votes: Vec<_> = (1..=3)
        .map(|seed| fixture.vote(seed, 1, 1100 + u64::from(seed)))
        .collect();
    for vote in &votes[..2] {
        fixture.apply(vote, true).unwrap();
        fixture
            .store
            .meta_store()
            .enqueue_confirmation(&vote.hash)
            .unwrap();
        assert!(
            crate::causal_cementation::cement(&fixture.store, vote.hash)
                .unwrap()
                .complete
        );
    }
    let mut change: StateBlock =
        bincode::deserialize(&fixture.store.block_store().get_block(&target.head).unwrap())
            .unwrap();
    change.block_type = BlockType::ChangeRepresentative;
    change.previous = target.head;
    change.representative = address(6);
    change.timestamp = test_time(1102);
    sign(&mut change, 10);
    let mut batch = fixture.store.tx_begin_write().unwrap();
    let economics =
        crate::payment_transition::apply(&mut batch, &change, &fixture.params, change.timestamp)
            .unwrap();
    batch.commit().unwrap();
    fixture.brn = economics.brn;
    fixture.trst = economics.trst;
    fixture.apply(&votes[2], true).unwrap();
    fixture = fixture.reopen();
    let mut batch = fixture.store.tx_begin_write().unwrap();
    assert!(
        !crate::causal_cementation::dependencies_confirmed(&batch, &votes[2]).unwrap(),
        "an outcome cannot finalize an independently chosen target representative"
    );
    assert!(!crate::causal_cementation::handoff(&mut batch, votes[2].hash).unwrap());
    drop(batch);
    let plan = crate::rollback_plan::preflight(&fixture.store, change.hash).unwrap();
    assert_eq!(plan.blocks(), &[votes[2].hash, change.hash]);
    let mut batch = fixture.store.tx_begin_write().unwrap();
    for hash in plan.blocks() {
        batch.rewind_block(hash).unwrap();
    }
    let rewound = batch.get_account(&fixture.target).unwrap();
    assert_eq!(rewound.representative, target.representative);
    assert_eq!(rewound.state, WalletState::Unverified);
    drop(batch);
    fixture
        .store
        .meta_store()
        .enqueue_confirmation(&change.hash)
        .unwrap();
    assert!(
        crate::causal_cementation::cement(&fixture.store, change.hash)
            .unwrap()
            .complete
    );
    let mut batch = fixture.store.tx_begin_write().unwrap();
    assert!(crate::causal_cementation::handoff(&mut batch, votes[2].hash).unwrap());
    batch.commit().unwrap();
    assert!(
        crate::causal_cementation::cement(&fixture.store, votes[2].hash)
            .unwrap()
            .complete
    );
    let authority = crate::confirmed_authority::ConfirmedAuthority::read(&fixture.store).unwrap();
    assert_eq!(
        authority.accounts[&fixture.target.to_string()].representative,
        address(6)
    );
    assert_eq!(
        authority.accounts[&fixture.target.to_string()].state,
        WalletState::Verified
    );
}

#[test]
fn identity_binding_aborts_with_its_block_and_rollback_restores_the_binding_point() {
    let mut fixture = Fixture::new();
    let votes: Vec<_> = (1..=3)
        .map(|seed| fixture.vote(seed, 1, 1000 + u64::from(seed)))
        .collect();
    for vote in &votes {
        fixture.apply(vote, true).unwrap();
    }
    let mut open = votes[0].clone();
    open.block_type = BlockType::Open;
    open.account = fixture.target.clone();
    open.representative = fixture.target.clone();
    open.previous = BlockHash::ZERO;
    open.link = BlockHash::ZERO;
    open.origin = TxHash::ZERO;
    open.transaction = TxHash::ZERO;
    open.brn_spent = 0;
    open.timestamp = test_time(1010);
    sign(&mut open, 10);
    let mut expected: Vec<_> = votes.iter().map(|vote| vote.hash).collect();
    expected.sort_by_key(|hash| *hash.as_bytes());
    for commit in [false, true] {
        let mut batch = fixture.store.tx_begin_write().unwrap();
        crate::payment_transition::apply(&mut batch, &open, &fixture.params, open.timestamp)
            .unwrap();
        assert_eq!(
            crate::verification_commit::dependencies::read(&batch, &open).unwrap(),
            expected
        );
        if commit {
            batch.commit().unwrap();
        }
    }
    fixture = fixture.reopen();
    let later = fixture
        .payment(
            10,
            BlockType::ChangeRepresentative,
            0,
            (BlockHash::ZERO, TxHash::ZERO),
            1011,
        )
        .unwrap();
    let later_block: StateBlock =
        bincode::deserialize(&fixture.store.block_store().get_block(&later).unwrap()).unwrap();
    let batch = fixture.store.tx_begin_write().unwrap();
    assert_eq!(
        crate::verification_commit::dependencies::read(&batch, &open).unwrap(),
        expected
    );
    assert!(
        crate::verification_commit::dependencies::read(&batch, &later_block)
            .unwrap()
            .is_empty()
    );
    drop(batch);
    let key = format!("verification_identity_binding/v1/{}", open.hash);
    let original = fixture.store.meta_store().get_meta(&key).unwrap();
    for damaged in [
        None,
        Some(vec![0; 32]),
        Some(votes[0].hash.as_bytes().to_vec()),
    ] {
        match damaged {
            Some(bytes) => fixture.store.meta_store().put_meta(&key, &bytes).unwrap(),
            None => fixture.store.meta_store().delete_meta(&key).unwrap(),
        }
        let batch = fixture.store.tx_begin_write().unwrap();
        assert!(crate::verification_commit::dependencies::read(&batch, &open).is_err());
    }
    fixture
        .store
        .meta_store()
        .put_meta(&key, &original)
        .unwrap();
    let plan = crate::rollback_plan::preflight(&fixture.store, votes[2].hash).unwrap();
    assert_eq!(plan.blocks(), &[later, open.hash, votes[2].hash]);
    let mut batch = fixture.store.tx_begin_write().unwrap();
    for hash in plan.blocks() {
        batch.rewind_block(hash).unwrap();
    }
    batch.commit().unwrap();
    fixture = fixture.reopen();
    fixture.apply(&votes[2], true).unwrap();
    let mut batch = fixture.store.tx_begin_write().unwrap();
    crate::payment_transition::apply(&mut batch, &open, &fixture.params, open.timestamp).unwrap();
    assert_eq!(
        crate::verification_commit::dependencies::read(&batch, &open).unwrap(),
        expected
    );
    batch.commit().unwrap();
}

#[test]
fn first_account_block_after_verification_confirms_its_complete_identity_outcome() {
    for already_open in [false, true] {
        let mut fixture = Fixture::new();
        if already_open {
            fixture.add_account(10);
            let mut account = fixture
                .store
                .account_store()
                .get_account(&fixture.target)
                .unwrap();
            account.state = WalletState::Unverified;
            account.verified_at = None;
            fixture.store.account_store().put_account(&account).unwrap();
        }
        let mut batch = fixture.store.tx_begin_write().unwrap();
        let genesis = batch.get_account(&address(0)).unwrap();
        crate::confirmed_authority::ConfirmedAuthority::initialize(&mut batch, &genesis).unwrap();
        let mut authority = crate::confirmed_authority::ConfirmedAuthority::load(&batch).unwrap();
        for account in fixture.store.account_store().all_accounts().unwrap() {
            batch
                .advance_confirmation_height(&account.address, 1)
                .unwrap();
            authority.accounts.insert(
                account.address.to_string(),
                crate::confirmed_authority::Authority::from(&account),
            );
        }
        authority.persist(&mut batch).unwrap();
        batch.commit().unwrap();
        let votes: Vec<_> = (1..=3)
            .map(|seed| fixture.vote(seed, 1, 1000 + u64::from(seed)))
            .collect();
        for index in [2, 0, 1] {
            fixture.apply(&votes[index], true).unwrap();
        }
        let first = fixture
            .payment(
                10,
                if already_open {
                    BlockType::ChangeRepresentative
                } else {
                    BlockType::Open
                },
                0,
                (BlockHash::ZERO, TxHash::ZERO),
                1010,
            )
            .unwrap();
        fixture = fixture.reopen();
        let mut candidate: StateBlock =
            bincode::deserialize(&fixture.store.block_store().get_block(&first).unwrap()).unwrap();
        candidate.representative = address(6);
        sign(&mut candidate, 10);
        let batch = fixture.store.tx_begin_write().unwrap();
        assert!(!crate::causal_cementation::dependencies_confirmed(&batch, &candidate).unwrap());
        let mut expected: Vec<_> = votes.iter().map(|vote| vote.hash).collect();
        expected.sort_by_key(|hash| *hash.as_bytes());
        let mut candidate_sources =
            crate::verification_commit::dependencies::voting_sources(&batch, &candidate).unwrap();
        candidate_sources.sort_by_key(|hash| *hash.as_bytes());
        assert_eq!(candidate_sources, expected);
        drop(batch);
        let waiting = crate::causal_cementation::cement(&fixture.store, first).unwrap();
        assert!(!waiting.complete);
        assert!(waiting.hashes.is_empty());
        for (index, vote) in votes.iter().enumerate() {
            fixture
                .store
                .meta_store()
                .enqueue_confirmation(&vote.hash)
                .unwrap();
            if index < votes.len() - 1 {
                let waiting = crate::causal_cementation::cement(&fixture.store, first).unwrap();
                assert!(!waiting.complete);
                assert!(waiting.hashes.is_empty());
            }
        }
        let result = crate::causal_cementation::cement(&fixture.store, first).unwrap();
        assert!(result.complete);
        assert_eq!(result.hashes.last(), Some(&first));
        for vote in &votes {
            assert!(result.hashes.contains(&vote.hash));
            assert_eq!(
                fixture
                    .store
                    .account_store()
                    .get_account(&vote.account)
                    .unwrap()
                    .confirmation_height,
                2
            );
        }
        let authority =
            crate::confirmed_authority::ConfirmedAuthority::read(&fixture.store).unwrap();
        assert!(authority.accounts[&fixture.target.to_string()]
            .state
            .is_verified());
    }
}

#[test]
fn outcome_rollback_includes_reward_claims_even_when_their_source_is_an_earlier_vote() {
    let mut fixture = Fixture::new();
    let votes: Vec<_> = (1..=3)
        .map(|seed| fixture.vote(seed, if seed == 3 { 2 } else { 1 }, 1000 + u64::from(seed)))
        .collect();
    for index in [2, 0, 1] {
        fixture.apply(&votes[index], true).unwrap();
    }
    let claim = fixture
        .payment(
            1,
            BlockType::Receive,
            10,
            (votes[2].hash, TxHash::ZERO),
            1004,
        )
        .unwrap();
    let plan = crate::rollback_plan::preflight(&fixture.store, votes[1].hash).unwrap();
    assert_eq!(plan.blocks(), &[claim, votes[1].hash]);
    {
        let mut batch = fixture.store.tx_begin_write().unwrap();
        for hash in plan.blocks() {
            batch.rewind_block(hash).unwrap();
        }
        assert!(batch.get_account(&fixture.target).is_err());
        let restored = crate::payment_transition::restore(&batch, &fixture.params).unwrap();
        assert_eq!(
            restored.brn.get_wallet(&address(1)).unwrap().total_staked,
            20
        );
        assert_eq!(
            restored.brn.get_wallet(&address(2)).unwrap().total_staked,
            0
        );
        assert_eq!(
            restored.brn.get_wallet(&address(3)).unwrap().total_staked,
            20
        );
    }
    assert!(fixture.store.block_store().exists(&claim).unwrap());
    assert!(fixture
        .store
        .account_store()
        .get_account(&fixture.target)
        .unwrap()
        .state
        .is_verified());
    let mut batch = fixture.store.tx_begin_write().unwrap();
    for hash in plan.blocks() {
        batch.rewind_block(hash).unwrap();
    }
    batch.commit().unwrap();
    fixture = fixture.reopen();
    assert!(!fixture.store.block_store().exists(&claim).unwrap());
    assert!(!fixture.store.block_store().exists(&votes[1].hash).unwrap());
    assert!(fixture.store.block_store().exists(&votes[0].hash).unwrap());
    assert!(fixture.store.block_store().exists(&votes[2].hash).unwrap());
    assert_eq!(
        fixture
            .orchestrator
            .get_state(&fixture.target)
            .unwrap()
            .votes
            .len(),
        2
    );
    assert_eq!(fixture.store.pending_store().pending_count().unwrap(), 0);
}

#[test]
fn outcome_dependencies_commit_atomically_and_do_not_depend_on_arrival_order() {
    for order in super::order_tests::ORDERS {
        let mut fixture = Fixture::new();
        let votes: Vec<_> = (1..=3)
            .map(|seed| fixture.vote(seed, if seed == 3 { 2 } else { 1 }, 1000 + u64::from(seed)))
            .collect();
        for seed in &order[..2] {
            fixture.apply(&votes[seed - 1], true).unwrap();
        }
        let transition = &votes[order[2] - 1];
        fixture.apply(transition, false).unwrap();
        let batch = fixture.store.tx_begin_write().unwrap();
        assert!(
            crate::verification_commit::dependencies::read(&batch, transition)
                .unwrap()
                .is_empty()
        );
        let mut claim = votes[0].clone();
        claim.block_type = BlockType::Receive;
        claim.link = votes[2].hash;
        claim.hash = claim.compute_hash();
        assert!(crate::verification_commit::dependencies::read(&batch, &claim).is_err());
        drop(batch);
        fixture.apply(transition, true).unwrap();
        fixture = fixture.reopen();
        let batch = fixture.store.tx_begin_write().unwrap();
        let mut expected: Vec<_> = votes.iter().map(|vote| vote.hash).collect();
        expected.sort_by_key(|hash| *hash.as_bytes());
        assert_eq!(
            crate::verification_commit::dependencies::read(&batch, &claim).unwrap(),
            expected
        );
        expected.retain(|hash| *hash != transition.hash);
        assert_eq!(
            crate::verification_commit::dependencies::read(&batch, transition).unwrap(),
            expected
        );
        for vote in votes.iter().filter(|vote| vote.hash != transition.hash) {
            assert!(crate::verification_commit::dependencies::read(&batch, vote)
                .unwrap()
                .is_empty());
        }
    }
}

#[test]
fn missing_or_changed_transition_dependencies_cannot_be_confirmed() {
    let mut fixture = Fixture::new();
    fixture.apply(&fixture.vote(1, 1, 1000), true).unwrap();
    fixture.apply(&fixture.vote(2, 1, 1001), true).unwrap();
    let last = fixture.vote(3, 2, 1002);
    fixture.apply(&last, true).unwrap();
    let key = format!("verification_transition_sources/v1/{}", last.hash);
    let original = fixture.store.meta_store().get_meta(&key).unwrap();
    fixture.store.meta_store().delete_meta(&key).unwrap();
    let batch = fixture.store.tx_begin_write().unwrap();
    assert!(
        crate::verification_commit::dependencies::read(&batch, &last)
            .unwrap_err()
            .to_string()
            .contains("differ from the committed transition")
    );
    drop(batch);
    fixture
        .store
        .meta_store()
        .put_meta(&key, &bincode::serialize(&vec![last.hash]).unwrap())
        .unwrap();
    let batch = fixture.store.tx_begin_write().unwrap();
    assert!(crate::verification_commit::dependencies::read(&batch, &last).is_err());
    drop(batch);
    fixture
        .store
        .meta_store()
        .put_meta(&key, &original)
        .unwrap();
    let batch = fixture.store.tx_begin_write().unwrap();
    assert_eq!(
        crate::verification_commit::dependencies::read(&batch, &last)
            .unwrap()
            .len(),
        2
    );
}
