use super::*;
use crate::verification_commit::receipts as records;

fn latest(f: &Fixture) -> records::Receipt {
    let bytes = f
        .store
        .meta_store()
        .get_meta(&records::latest_key(&f.target))
        .unwrap();
    let source = BlockHash::new(bytes.as_slice().try_into().unwrap());
    records::decode(
        &f.store
            .meta_store()
            .get_meta(&records::key(source))
            .unwrap(),
    )
    .unwrap()
}

fn mark_component_receipt(f: &Fixture) {
    let receipt = latest(f);
    // This fixture exercises settlement, not quorum authentication. The live
    // replay test must obtain this marker from a real certified boundary.
    f.store
        .meta_store()
        .put_meta(&records::certified_key(receipt.source), &[90; 32])
        .unwrap();
}

fn settle_candidate(f: &mut Fixture, cutoff: u64, commit: bool) -> Result<(), String> {
    let mut brn = f.brn.clone();
    let mut trst = f.trst.clone();
    let mut orchestrator = f.orchestrator.clone();
    let mut batch = f.store.tx_begin_write().unwrap();
    settle_upheld_candidate(
        &mut batch,
        &f.target,
        test_time(cutoff),
        &f.params,
        &mut orchestrator,
        (&mut brn, &mut trst),
        burst_types::NetworkId::Dev,
    )?;
    crate::ledger_updater::persist_economics(&mut batch, &brn, &trst)?;
    if commit {
        batch.commit().unwrap();
        f.brn = brn;
        f.trst = trst;
        f.orchestrator = orchestrator;
    }
    Ok(())
}

fn review(
    f: &mut Fixture,
    reason: burst_types::ChallengeReason,
    legitimate: bool,
    at: u64,
) -> records::Receipt {
    let previous = latest(f);
    mark_component_receipt(f);
    let mut appeal = f.challenge(reason);
    appeal.target_head = previous.source;
    appeal.timestamp = test_time(at);
    sign(&mut appeal, 0);
    f.apply_with_policy(&appeal, true, true).unwrap();
    let before_state = f
        .store
        .account_store()
        .get_account(&f.target)
        .unwrap()
        .state;
    f.select(&[3, 4, 5]);
    for seed in [3, 4, 5] {
        let vote = if (seed != 5) == legitimate { 1 } else { 2 };
        f.apply_with_policy(&f.vote(seed, vote, at + u64::from(seed)), true, true)
            .unwrap();
    }
    assert_eq!(
        f.store
            .account_store()
            .get_account(&f.target)
            .unwrap()
            .state,
        before_state
    );
    assert_eq!(latest(f), previous);
    assert_eq!(
        f.brn.get_wallet(&address(0)).unwrap().total_staked,
        f.params.challenge_stake_amount
    );
    settle_candidate(f, at + 10, false).unwrap();
    assert_eq!(latest(f), previous);
    assert_eq!(
        f.store
            .account_store()
            .get_account(&f.target)
            .unwrap()
            .state,
        before_state
    );
    settle_candidate(f, at + 10, true).unwrap();
    let receipt = latest(f);
    assert_eq!(receipt.previous, Some(previous.source));
    assert!(
        matches!(receipt.verdict, records::Verdict::Appeal { reviewed, .. } if reviewed == previous.source)
    );
    assert!(settle_candidate(f, at + 10, true).is_err());
    receipt
}

fn original(reason: burst_types::ChallengeReason, legitimate: bool) -> Fixture {
    let mut f = Fixture::new();
    f.verify_target();
    f.seed_target_origin_at_holder();
    let challenge = f.challenge(reason);
    f.apply(&challenge, true).unwrap();
    f.select(&[3, 4, 5]);
    for seed in [3, 4, 5] {
        let vote = if (seed != 5) == legitimate { 1 } else { 2 };
        f.apply(&f.vote(seed, vote, 1100 + u64::from(seed)), true)
            .unwrap();
    }
    f
}

fn rewards_revoked(f: &Fixture, receipt: &records::Receipt, expected: bool) {
    for reward in &receipt.rewards {
        assert_eq!(
            f.trst.merger_graph.is_origin_revoked(&reward.origin),
            expected
        );
        let pending = f
            .store
            .pending_store()
            .get_pending(&reward.recipient, &receipt.source.into_tx_hash())
            .unwrap();
        assert_eq!(
            pending.amount, reward.amount,
            "nominal reward was rewritten"
        );
    }
}

#[test]
fn appeal_reversal_follows_the_root_through_failed_and_repeated_reviews_without_duplicate_bounty() {
    for reason in [
        burst_types::ChallengeReason::Fraud,
        burst_types::ChallengeReason::Inactivity,
    ] {
        let mut f = original(reason, false).reopen();
        let root = latest(&f);
        let records::Verdict::Challenge { admission, .. } = root.verdict else {
            panic!("missing root")
        };
        let selection = f.orchestrator.selection_context(&f.target).unwrap();
        let original_stakes: Vec<_> = [0, 3, 4, 5]
            .into_iter()
            .map(|seed| {
                let key = stake_key(
                    &f.target,
                    &address(seed),
                    seed == 0,
                    if seed == 0 { admission } else { selection },
                );
                let bytes = f.store.meta_store().get_meta(&key).unwrap();
                (key, bytes)
            })
            .collect();
        let nominal = f
            .store
            .account_store()
            .get_account(&address(6))
            .unwrap()
            .trst_balance;
        let first = review(&mut f, reason, true, 1200);
        assert_eq!(
            f.store
                .account_store()
                .get_account(&f.target)
                .unwrap()
                .state,
            WalletState::Verified
        );
        assert_eq!(
            f.trst.transferable_balance(&address(6), test_time(1210)),
            Some(10_000)
        );
        rewards_revoked(&f, &root, true);
        assert!(!first
            .rewards
            .iter()
            .any(|reward| reward.recipient == address(0)));
        f = f.reopen();
        let burned = f.brn.get_wallet(&address(0)).unwrap().total_burned;
        let failed = review(&mut f, reason, true, 1300);
        assert_eq!(
            f.brn.get_wallet(&address(0)).unwrap().total_burned,
            burned + f.params.challenge_stake_amount
        );
        rewards_revoked(&f, &root, true);
        f = f.reopen();
        let overturned = review(&mut f, reason, false, 1400);
        rewards_revoked(&f, &root, false);
        rewards_revoked(&f, &first, true);
        rewards_revoked(&f, &failed, true);
        assert!(
            !overturned
                .rewards
                .iter()
                .any(|reward| reward.recipient == address(0)),
            "repeated fraud paid another bounty"
        );
        f = f.reopen();
        let final_review = review(&mut f, reason, true, 1500);
        rewards_revoked(&f, &root, true);
        rewards_revoked(&f, &first, false);
        rewards_revoked(&f, &failed, false);
        rewards_revoked(&f, &overturned, true);
        assert!(!final_review
            .rewards
            .iter()
            .any(|reward| reward.recipient == address(0)));
        assert_eq!(
            f.store
                .account_store()
                .get_account(&address(6))
                .unwrap()
                .trst_balance,
            nominal
        );
        assert_eq!(
            f.brn.get_wallet(&address(0)).unwrap().total_burned,
            burned + f.params.challenge_stake_amount
        );
        assert_eq!(f.brn.get_wallet(&address(0)).unwrap().total_staked, 0);
        assert_eq!(
            records::decode(
                &f.store
                    .meta_store()
                    .get_meta(&records::key(root.source))
                    .unwrap()
            )
            .unwrap(),
            root
        );
        for (key, bytes) in original_stakes {
            assert_eq!(
                f.store.meta_store().get_meta(&key).unwrap(),
                bytes,
                "appeal rewrote a historical stake settlement"
            );
        }
    }
}

#[test]
fn appeal_of_a_rejected_challenge_pays_the_first_fraud_bounty_only_once() {
    let reason = burst_types::ChallengeReason::Fraud;
    let mut f = original(reason, true);
    let initial = latest(&f);
    let first_fraud = review(&mut f, reason, false, 1200);
    let bounty = first_fraud
        .rewards
        .iter()
        .find(|reward| reward.recipient == address(0))
        .unwrap();
    assert_eq!(
        bounty.amount,
        (10_000 * u128::from(f.params.challenge_reward_bps) / 10_000)
            .min(f.params.challenge_reward_cap)
    );
    rewards_revoked(&f, &initial, true);
    let _ = review(&mut f, reason, true, 1300);
    rewards_revoked(&f, &first_fraud, true);
    let second_fraud = review(&mut f, reason, false, 1400);
    rewards_revoked(&f, &first_fraud, false);
    assert!(!second_fraud
        .rewards
        .iter()
        .any(|reward| reward.recipient == address(0)));
}
