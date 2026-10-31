use super::*;
use crate::verification_commit::receipts as records;

fn latest(fixture: &Fixture) -> records::Receipt {
    let reference = fixture
        .store
        .meta_store()
        .get_meta(&records::latest_key(&fixture.target))
        .unwrap();
    let source = BlockHash::new(reference.as_slice().try_into().unwrap());
    records::decode(
        &fixture
            .store
            .meta_store()
            .get_meta(&records::key(source))
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn appeal_reference_cannot_be_silently_processed_as_an_ordinary_challenge() {
    let mut f = Fixture::new();
    f.verify_target();
    let original = latest(&f);
    let mut appeal = f.challenge(burst_types::ChallengeReason::Fraud);
    appeal.target_head = original.source;
    sign(&mut appeal, 0);
    let before = bincode::serialize(f.brn.get_wallet(&address(0)).unwrap()).unwrap();
    assert_eq!(
        f.apply(&appeal, true).unwrap_err(),
        "appeal requires a completed challenge"
    );
    assert_eq!(
        bincode::serialize(f.brn.get_wallet(&address(0)).unwrap()).unwrap(),
        before
    );
    assert_eq!(latest(&f), original);
}

#[test]
fn appeal_context_requires_certified_latest_matching_challenge_history() {
    for reason in [
        burst_types::ChallengeReason::Fraud,
        burst_types::ChallengeReason::Inactivity,
    ] {
        for legitimate in [false, true] {
            let mut f = Fixture::new();
            f.verify_target();
            let initial = latest(&f);
            let challenge = f.challenge(reason);
            f.apply(&challenge, true).unwrap();
            f.select(&[3, 4, 5]);
            for seed in [3, 4, 5] {
                f.apply(
                    &f.vote(seed, if legitimate { 1 } else { 2 }, 1100 + u64::from(seed)),
                    true,
                )
                .unwrap();
            }
            f = f.reopen();
            let outcome = latest(&f);
            let mut appeal = f.challenge(reason);
            appeal.timestamp = test_time(1200);
            appeal.target_head = outcome.source;
            sign(&mut appeal, 0);
            let before = bincode::serialize(f.brn.get_wallet(&address(0)).unwrap()).unwrap();
            assert_eq!(
                f.apply(&appeal, true).unwrap_err(),
                "appeal outcome is not certified"
            );
            assert_eq!(
                bincode::serialize(f.brn.get_wallet(&address(0)).unwrap()).unwrap(),
                before
            );
            assert_eq!(latest(&f), outcome);
            let mut batch = f.store.tx_begin_write().unwrap();
            // Component receipt validation only. Live replay creates this marker
            // after validating the checkpoint, never from a wallet request.
            batch
                .put_meta(&records::certified_key(outcome.source), &[90; 32])
                .unwrap();
            assert_eq!(
                records::appeal_context(&batch, &appeal).unwrap(),
                burst_verification::AppealContext {
                    reviewed_outcome: outcome.source,
                    previously_legitimate: legitimate,
                }
            );
            let mut invalid = appeal.clone();
            invalid.target_head = initial.source;
            assert_eq!(
                records::appeal_context(&batch, &invalid).unwrap_err(),
                "appeal does not reference the latest target outcome"
            );
            invalid = appeal.clone();
            invalid.timestamp = test_time(1100);
            assert_eq!(
                records::appeal_context(&batch, &invalid).unwrap_err(),
                "appeal outcome history mismatch"
            );
            invalid = appeal.clone();
            invalid.origin = TxHash::new(if reason == burst_types::ChallengeReason::Fraud {
                burst_types::ChallengeReason::Inactivity.to_origin()
            } else {
                burst_types::ChallengeReason::Fraud.to_origin()
            });
            assert_eq!(
                records::appeal_context(&batch, &invalid).unwrap_err(),
                "appeal cannot change the challenged reason"
            );
            drop(batch);
            assert_eq!(latest(&f), outcome);
        }
    }
}

#[test]
fn checkpoint_replacement_clears_outcome_history_atomically_only() {
    let mut f = Fixture::new();
    f.verify_target();
    let receipt = latest(&f);
    let receipt_key = records::key(receipt.source);
    let latest_key = records::latest_key(&f.target);
    let certified_key = records::certified_key(receipt.source);
    f.store
        .meta_store()
        .put_meta(&certified_key, &[90; 32])
        .unwrap();
    let epoch = f.store.meta_store().payment_epoch().unwrap();
    let journal = "checkpoint_round/v1/fixture-finality";
    f.store.meta_store().put_meta(journal, b"preserve").unwrap();
    for commit in [false, true] {
        let mut batch = f.store.tx_begin_write().unwrap();
        batch.clear_checkpoint_lifecycle().unwrap();
        assert!(batch.get_meta(&receipt_key).unwrap().is_none());
        assert!(batch.get_meta(&latest_key).unwrap().is_none());
        assert!(batch.get_meta(&certified_key).unwrap().is_none());
        assert_eq!(batch.get_meta(journal).unwrap().unwrap(), b"preserve");
        if commit {
            batch.commit().unwrap();
        } else {
            drop(batch);
        }
        assert_eq!(f.store.meta_store().get_meta(&receipt_key).is_ok(), !commit);
        assert_eq!(f.store.meta_store().get_meta(&latest_key).is_ok(), !commit);
        assert_eq!(
            f.store.meta_store().get_meta(&certified_key).is_ok(),
            !commit
        );
        assert_eq!(f.store.meta_store().payment_epoch().unwrap(), epoch);
        assert_eq!(f.store.meta_store().get_meta(journal).unwrap(), b"preserve");
    }
}

#[test]
fn pending_reward_invalidation_survives_recipient_reverification_and_restart() {
    let mut f = Fixture::new();
    for seed in [1, 2, 3] {
        f.apply(
            &f.vote(seed, if seed == 3 { 2 } else { 1 }, 1000 + u64::from(seed)),
            true,
        )
        .unwrap();
    }
    let receipt = latest(&f);
    let reward = receipt
        .rewards
        .iter()
        .find(|reward| reward.recipient == address(1))
        .unwrap();
    let origins = [(reward.recipient.clone(), reward.origin)];
    let cause = BlockHash::new([99; 32]);
    let ordinary = f
        .trst
        .mint(
            TxHash::new([98; 32]),
            address(1),
            50,
            address(1),
            test_time(1050),
        )
        .unwrap();
    f.trst.track_token(ordinary);
    let (held, effects) = f.trst.invalidate_outcome_origins(cause, &origins).unwrap();
    assert!(held.is_empty());
    assert_eq!(
        f.trst.transferable_balance(&address(1), test_time(1100)),
        Some(50)
    );
    let mut batch = f.store.tx_begin_write().unwrap();
    assert_eq!(
        revoke_pending(&mut batch, &effects, &mut f.trst).unwrap(),
        reward.amount
    );
    f.trst.revoke_by_originator(&address(1));
    f.brn
        .deactivate_wallet(&address(1), test_time(1100))
        .unwrap();
    let mut account = batch.get_account(&address(1)).unwrap();
    account.state = WalletState::Revoked;
    batch.put_account_info(&account, false).unwrap();
    crate::ledger_updater::persist_economics(&mut batch, &f.brn, &f.trst).unwrap();
    batch.commit().unwrap();
    f = f.reopen();

    let mut batch = f.store.tx_begin_write().unwrap();
    mark_wallet_verified(
        &mut batch,
        &address(1),
        test_time(1200),
        &mut f.brn,
        &mut f.trst,
    )
    .unwrap();
    let pending = batch
        .get_pending(&address(1), &receipt.source.into_tx_hash())
        .unwrap();
    let chunk = &pending.provenance[0].revoked_amounts[reward.origin.as_bytes()];
    assert_eq!(chunk.amount, reward.amount);
    assert!(!chunk.restored);
    assert_eq!(
        f.trst.transferable_balance(&address(1), test_time(1200)),
        Some(50)
    );
    crate::ledger_updater::persist_economics(&mut batch, &f.brn, &f.trst).unwrap();
    batch.commit().unwrap();
    f = f.reopen();

    let mut batch = f.store.tx_begin_write().unwrap();
    assert!(f
        .trst
        .restore_outcome_origins(cause, &origins, test_time(1300))
        .unwrap()
        .is_empty());
    restore_pending(&mut batch, &f.trst).unwrap();
    let pending = batch
        .get_pending(&address(1), &receipt.source.into_tx_hash())
        .unwrap();
    assert!(pending.provenance[0].revoked_amounts[reward.origin.as_bytes()].restored);
    assert_eq!(pending.amount, reward.amount);
    assert_eq!(
        f.trst.transferable_balance(&address(1), test_time(1300)),
        Some(50)
    );
    crate::ledger_updater::persist_economics(&mut batch, &f.brn, &f.trst).unwrap();
    batch.commit().unwrap();
    f = f.reopen();
    assert_eq!(latest(&f), receipt);
    assert!(!f.trst.merger_graph.is_origin_revoked(&reward.origin));
}

#[test]
fn outcome_receipts_bind_rewards_preserve_history_and_commit_atomically() {
    for reason in [
        burst_types::ChallengeReason::Fraud,
        burst_types::ChallengeReason::Inactivity,
    ] {
        let mut f = Fixture::new();
        f.verify_target();
        let first = latest(&f);
        assert_eq!(first.verdict, records::Verdict::Verified);
        assert_eq!(first.previous, None);
        assert!(first.rewards.is_empty());
        f.seed_target_origin_at_holder();
        let challenge = f.challenge(reason);
        f.apply(&challenge, true).unwrap();
        f.select(&[3, 4, 5]);
        f.apply(&f.vote(3, 2, 1101), true).unwrap();
        f.apply(&f.vote(4, 2, 1102), true).unwrap();
        let last = f.vote(5, 1, 1103);
        f.apply(&last, false).unwrap();
        assert_eq!(latest(&f), first);
        assert!(f
            .store
            .meta_store()
            .get_meta(&records::key(last.hash))
            .is_err());
        f.apply(&last, true).unwrap();
        f = f.reopen();
        let receipt = latest(&f);
        assert_eq!(receipt.source, last.hash);
        assert_eq!(receipt.previous, Some(first.source));
        assert_eq!(receipt.completed_at, last.timestamp);
        assert_eq!(receipt.target, f.target);
        assert_eq!(
            receipt.verdict,
            records::Verdict::Challenge {
                admission: challenge.hash,
                reason,
                result: records::ChallengeVerdict::Upheld,
            }
        );
        assert_eq!(
            receipt.rewards.len(),
            if reason == burst_types::ChallengeReason::Fraud {
                3
            } else {
                2
            }
        );
        for reward in &receipt.rewards {
            let pending = f
                .store
                .pending_store()
                .get_pending(&reward.recipient, &receipt.source.into_tx_hash())
                .unwrap();
            assert_eq!(pending.amount, reward.amount);
            assert_eq!(pending.provenance[0].origin, reward.origin);
        }
        assert_eq!(
            records::decode(
                &f.store
                    .meta_store()
                    .get_meta(&records::key(first.source))
                    .unwrap()
            )
            .unwrap(),
            first
        );
        assert!(f.apply(&last, true).is_err());
        assert_eq!(latest(&f), receipt);

        let mut corrupt = receipt.clone();
        corrupt.rewards[0].origin = TxHash::ZERO;
        assert!(records::decode(&bincode::serialize(&corrupt).unwrap()).is_err());
        corrupt = receipt.clone();
        corrupt.previous = Some(corrupt.source);
        assert!(records::decode(&bincode::serialize(&corrupt).unwrap()).is_err());
        let mut bytes = bincode::serialize(&receipt).unwrap();
        bytes.push(0);
        assert!(records::decode(&bytes).is_err());
    }
}
