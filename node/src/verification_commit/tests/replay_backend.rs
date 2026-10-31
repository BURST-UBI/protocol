use super::*;
use crate::verification_commit::ledger::ReplayedLedger;
use std::collections::HashMap;

#[test]
fn replayed_pending_claim_and_return_match_the_storage_adapter() {
    use crate::verification_commit::ledger::Ledger;
    let fixture = Fixture::new();
    let block = fixture.challenge(burst_verification::ChallengeReason::Fraud);
    let destination = address(1);
    let source = block.hash.into_tx_hash();
    let returned = BlockHash::new([91; 32]).into_tx_hash();
    let pending = burst_store::pending::PendingInfo {
        source: address(2),
        amount: 17,
        timestamp: Timestamp::new(1000),
        provenance: Vec::new(),
        return_credit: true,
    };
    let mut accounts = HashMap::new();
    let mut claims = HashMap::new();
    let mut metadata = HashMap::new();
    let applied = HashMap::<BlockHash, &StateBlock>::new();
    let mut mutations = crate::confirmed_replay::mutations::Mutations::default();
    let mut replay = ReplayedLedger {
        bootstrap: &mut burst_verification::bootstrap::BootstrapPhase::Active,
        bootstrap_threshold: 50,
        mutations: Some(&mut mutations),
        accounts: &mut accounts,
        pending: &mut claims,
        metadata: &mut metadata,
        applied: &applied,
        current: &block,
    };
    let mut batch = fixture.store.tx_begin_write().unwrap();
    Ledger::put_pending(&mut batch, &destination, &source, &pending).unwrap();
    replay.put_pending(&destination, &source, &pending).unwrap();
    let stored = Ledger::take_pending(&mut batch, &destination, &source).unwrap();
    let derived = replay.take_pending(&destination, &source).unwrap();
    assert_eq!(
        bincode::serialize(&stored).unwrap(),
        bincode::serialize(&derived).unwrap()
    );
    assert!(Ledger::take_pending(&mut batch, &destination, &source).is_err());
    assert!(replay.take_pending(&destination, &source).is_err());
    Ledger::put_pending(&mut batch, &stored.source, &returned, &stored).unwrap();
    replay
        .put_pending(&derived.source, &returned, &derived)
        .unwrap();
    let stored = Ledger::get_pending(&batch, &pending.source, &returned).unwrap();
    let derived = replay.get_pending(&pending.source, &returned).unwrap();
    assert!(derived.return_credit);
    assert_eq!(
        bincode::serialize(&stored).unwrap(),
        bincode::serialize(&derived).unwrap()
    );
    assert_eq!(replay.all_pending().unwrap().len(), 1);
    replay
        .put_meta("verification_receipt/test", b"first")
        .unwrap();
    replay
        .put_meta("verification_receipt/test", b"last")
        .unwrap();
    let mut account = burst_store::account::AccountInfo::unopened(destination.clone());
    replay.put_account(&account).unwrap();
    account.trst_balance = 17;
    replay.put_account(&account).unwrap();
    drop(replay);
    let changes: Vec<_> = mutations.changes().collect();
    assert_eq!(changes.len(), 3);
    assert!(changes.iter().all(|row| row.before.is_none()));
    let account_row = changes
        .iter()
        .find(|row| row.table == burst_store_lmdb::block_undo::UndoTable::Accounts)
        .unwrap();
    assert_eq!(
        account_row.after.as_ref().unwrap(),
        &bincode::serialize(&account).unwrap()
    );
    let pending_row = changes
        .iter()
        .find(|row| row.table == burst_store_lmdb::block_undo::UndoTable::Pending)
        .unwrap();
    assert_eq!(
        pending_row.after.as_ref().unwrap(),
        &bincode::serialize(&pending).unwrap()
    );
    let meta_row = changes
        .iter()
        .find(|row| row.table == burst_store_lmdb::block_undo::UndoTable::Meta)
        .unwrap();
    assert_eq!(meta_row.after.as_deref(), Some(b"last".as_slice()));
    // Candidate writes stay private until commit.
    drop(batch);
    assert!(fixture
        .store
        .tx_begin_write()
        .unwrap()
        .get_pending(&pending.source, &returned)
        .is_err());
}

#[test]
fn replayed_expiry_matches_storage_without_granting_spendable_deadline_credit() {
    // Backend parity for the gated legacy expiry rule, not approval of that rule
    // as the agreed missing-vote policy required by decision 15.16.
    for partial_panel in [false, true] {
        let mut fixture = Fixture::new();
        fixture.verify_target();
        let challenge = fixture.challenge(burst_verification::ChallengeReason::Fraud);
        fixture.apply(&challenge, true).unwrap();
        let mut keys = vec![stake_key(
            &fixture.target,
            &address(0),
            true,
            challenge.hash,
        )];
        if partial_panel {
            fixture.select(&[3, 4, 5]);
            let context = fixture
                .orchestrator
                .selection_context(&fixture.target)
                .unwrap();
            fixture.apply(&fixture.vote(3, 2, 1101), true).unwrap();
            keys.push(stake_key(&fixture.target, &address(3), false, context));
        }
        let deadline =
            Timestamp::new(challenge.timestamp.as_secs() + fixture.params.challenge_duration_secs);
        let expected_id = BlockHash::new(burst_crypto::blake2b_256_multi(&[
            b"BURST/challenge-expiry/v1\0",
            challenge.hash.as_bytes(),
            &deadline.as_secs().to_be_bytes(),
        ]));
        let initial_accounts: HashMap<_, _> = (0..=6)
            .chain([10])
            .map(|seed| {
                let account = fixture
                    .store
                    .account_store()
                    .get_account(&address(seed))
                    .unwrap();
                (account.address.clone(), account)
            })
            .collect();
        let initial_metadata: HashMap<_, _> = keys
            .iter()
            .map(|key| {
                (
                    key.clone(),
                    fixture.store.meta_store().get_meta(key).unwrap(),
                )
            })
            .collect();
        let initial_pending: HashMap<_, _> = fixture
            .store
            .pending_store()
            .get_all_pending()
            .unwrap()
            .into_iter()
            .map(|(address, hash, pending)| ((address, hash), pending))
            .collect();
        let original_brn = fixture.brn.clone();
        let original_trst = fixture.trst.clone();
        let original_verification = fixture.orchestrator.clone();
        expire(
            &fixture.store,
            deadline,
            &fixture.params,
            (
                &mut fixture.brn,
                &mut fixture.trst,
                &mut fixture.orchestrator,
            ),
        )
        .unwrap();
        fixture = fixture.reopen();

        // Neither delayed execution nor a storage backend chooses the outcome ID.
        for at in [deadline, Timestamp::new(deadline.as_secs() + 100)] {
            let mut accounts = initial_accounts.clone();
            let mut pending = initial_pending.clone();
            let mut metadata = initial_metadata.clone();
            let mut brn = original_brn.clone();
            let mut trst = original_trst.clone();
            let mut verification = original_verification.clone();
            let applied = HashMap::from([(challenge.hash, &challenge)]);
            let mut ledger = ReplayedLedger {
                bootstrap: &mut burst_verification::bootstrap::BootstrapPhase::Active,
                bootstrap_threshold: 50,
                mutations: None,
                accounts: &mut accounts,
                pending: &mut pending,
                metadata: &mut metadata,
                applied: &applied,
                current: &challenge,
            };
            assert!(expire_candidate(
                &mut ledger,
                Timestamp::new(deadline.as_secs() - 1),
                &fixture.params,
                &mut verification,
                (&mut brn, &mut trst)
            )
            .unwrap()
            .is_empty());
            assert_eq!(brn.get_wallet(&address(0)).unwrap().total_staked, 100);
            assert_eq!(
                expire_candidate(
                    &mut ledger,
                    at,
                    &fixture.params,
                    &mut verification,
                    (&mut brn, &mut trst)
                )
                .unwrap(),
                vec![expected_id]
            );
            assert!(expire_candidate(
                &mut ledger,
                at,
                &fixture.params,
                &mut verification,
                (&mut brn, &mut trst)
            )
            .unwrap()
            .is_empty());
            assert!(verification.snapshot().active_challenges.is_empty());
            assert_eq!(
                verification.get_state(&fixture.target).unwrap().phase,
                burst_verification::state::VerificationPhase::Verified
            );
            for account in accounts.values() {
                let stored = fixture
                    .store
                    .account_store()
                    .get_account(&account.address)
                    .unwrap();
                assert_eq!(
                    bincode::serialize(account).unwrap(),
                    bincode::serialize(&stored).unwrap()
                );
            }
            for seed in 0..=6 {
                let actual = brn.get_wallet(&address(seed)).unwrap();
                let expected = fixture.brn.get_wallet(&address(seed)).unwrap();
                assert_eq!(
                    bincode::serialize(actual).unwrap(),
                    bincode::serialize(expected).unwrap()
                );
            }
            for key in &keys {
                assert_eq!(
                    metadata[key],
                    fixture.store.meta_store().get_meta(key).unwrap()
                );
            }
            assert_eq!(pending.len(), initial_pending.len());
            assert_eq!(trst.save_wallets(), original_trst.save_wallets());
            let challenger = brn.get_wallet_mut(&address(0)).unwrap();
            let release = &challenger.pending_releases[&expected_id];
            assert_eq!((release.amount, release.completed_at), (50, deadline));
            assert!(release.sources.is_empty());
            assert!(challenger.acknowledge_releases(&[expected_id], at).is_err());
            assert_eq!(challenger.total_burned, 50);
        }
    }
}

#[test]
fn replayed_candidate_and_lmdb_share_stake_and_reward_transitions() {
    // Both paths start from this component fixture's explicit state. This is
    // backend parity, not evidence of certified history admission.
    let mut fixture = Fixture::new();
    let mut accounts: HashMap<_, _> = (0..=6)
        .map(|seed| {
            let account = fixture
                .store
                .account_store()
                .get_account(&address(seed))
                .unwrap();
            (account.address.clone(), account)
        })
        .collect();
    let initial: Vec<StateBlock> = accounts
        .values()
        .map(|account| {
            bincode::deserialize(
                &fixture
                    .store
                    .block_store()
                    .get_block(&account.head)
                    .unwrap(),
            )
            .unwrap()
        })
        .collect();
    let mut applied: HashMap<_, _> = initial.iter().map(|block| (block.hash, block)).collect();
    let mut pending = HashMap::new();
    let mut metadata = HashMap::new();
    let mut brn = fixture.brn.clone();
    let mut trst = fixture.trst.clone();
    let mut orchestrator = fixture.orchestrator.clone();
    let votes: Vec<_> = (1..=3)
        .map(|seed| fixture.vote(seed, if seed == 3 { 2 } else { 1 }, 1000 + u64::from(seed)))
        .collect();
    for index in [2, 0, 1] {
        let block = &votes[index];
        let previous = applied[&block.previous];
        let account = accounts.get(&block.account);
        let economics = crate::economic_candidate::apply(
            block,
            crate::economic_candidate::EconomicContext {
                account,
                previous_brn_spent: previous.brn_spent,
                pending: None,
                params: &fixture.params,
            },
            &mut brn,
            &mut trst,
        )
        .unwrap();
        let updated =
            crate::ledger_updater::account_after_block(block, account, previous.brn_spent)
                .unwrap();
        accounts.insert(block.account.clone(), updated);
        apply(
            &mut ReplayedLedger {
                bootstrap: &mut burst_verification::bootstrap::BootstrapPhase::Active,
                bootstrap_threshold: 50,
                mutations: None,
                accounts: &mut accounts,
                pending: &mut pending,
                metadata: &mut metadata,
                applied: &applied,
                current: block,
            },
            block,
            &economics.effect,
            &fixture.params,
            &mut orchestrator,
            (&mut brn, &mut trst),
            burst_types::NetworkId::Dev,
        )
        .unwrap();
        applied.insert(block.hash, block);
        fixture.apply(block, true).unwrap();
        for account in accounts.values() {
            let stored = fixture
                .store
                .account_store()
                .get_account(&account.address)
                .unwrap();
            assert_eq!(
                bincode::serialize(account).unwrap(),
                bincode::serialize(&stored).unwrap()
            );
        }
        let mut expected = fixture.store.pending_store().get_all_pending().unwrap();
        expected.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        let mut actual: Vec<_> = pending
            .iter()
            .map(|((address, hash), info)| (address.clone(), *hash, info.clone()))
            .collect();
        actual.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        assert_eq!(
            bincode::serialize(&actual).unwrap(),
            bincode::serialize(&expected).unwrap()
        );
        for seed in 1..=3 {
            let actual = brn.get_wallet(&address(seed)).unwrap();
            let expected = fixture.brn.get_wallet(&address(seed)).unwrap();
            assert_eq!(actual.total_staked, expected.total_staked);
            assert_eq!(actual.total_burned, expected.total_burned);
        }
        for (key, bytes) in &metadata {
            if key.starts_with("verification_stake/") {
                assert_eq!(fixture.store.meta_store().get_meta(key).unwrap(), *bytes);
            }
        }
    }
    assert_eq!(accounts[&fixture.target].state, WalletState::Verified);
    assert_eq!(pending.len(), 2);
}

#[test]
fn ordinary_verification_replay_retires_creator_authority_before_a_later_revocation() {
    use crate::verification_commit::ledger::Ledger;
    use burst_verification::bootstrap::BootstrapPhase;
    let fixture = Fixture::new();
    let block = fixture.challenge(burst_verification::ChallengeReason::Fraud);
    let mut accounts = HashMap::new();
    for seed in 0..49 {
        let mut account = burst_store::account::AccountInfo::unopened(address(seed));
        account.state = WalletState::Verified;
        accounts.insert(account.address.clone(), account);
    }
    let mut phase = BootstrapPhase::Active;
    let mut pending = HashMap::new();
    let mut metadata = HashMap::new();
    let applied = HashMap::<BlockHash, &StateBlock>::new();
    let mut replay = ReplayedLedger {
        bootstrap: &mut phase, bootstrap_threshold: 1000,
        mutations: None, accounts: &mut accounts, pending: &mut pending,
        metadata: &mut metadata, applied: &applied, current: &block,
    };
    let mut fiftieth = burst_store::account::AccountInfo::unopened(address(49));
    replay.put_account(&fiftieth).unwrap();
    assert_eq!(*replay.bootstrap, BootstrapPhase::Active);
    fiftieth.state = WalletState::Verified;
    replay.put_account(&fiftieth).unwrap();
    assert_eq!(*replay.bootstrap, BootstrapPhase::Complete);
    fiftieth.state = WalletState::Revoked;
    replay.put_account(&fiftieth).unwrap();
    let restored = BootstrapPhase::decode(Some(&replay.bootstrap.encoding())).unwrap();
    assert!(!restored.permits_creator(49, 1000));
}
