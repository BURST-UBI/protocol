use super::*;

pub(super) const ORDERS: [[usize; 3]; 6] = [
    [1, 2, 3],
    [1, 3, 2],
    [2, 1, 3],
    [2, 3, 1],
    [3, 1, 2],
    [3, 2, 1],
];

#[test]
fn vote_arrival_order_does_not_change_verification_time_or_reward_identity() {
    let mut expected = None;
    for order in ORDERS {
        let mut fixture = Fixture::new();
        let votes: Vec<_> = (1..=3)
            .map(|seed| fixture.vote(seed, if seed == 3 { 2 } else { 1 }, 1000 + u64::from(seed)))
            .collect();
        for seed in order {
            fixture.apply(&votes[seed - 1], true).unwrap();
        }
        fixture = fixture.reopen();
        let account = fixture
            .store
            .account_store()
            .get_account(&fixture.target)
            .unwrap();
        assert_eq!(account.verified_at, Some(test_time(1003)));
        let mut pending = fixture.store.pending_store().get_all_pending().unwrap();
        pending.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        assert_eq!(pending.len(), 2);
        for (_, source, info) in &pending {
            assert_eq!(*source, votes[2].hash.into_tx_hash());
            assert_eq!(info.timestamp, test_time(1003));
            assert_eq!(info.amount, 10);
        }
        let encoded = bincode::serialize(&pending).unwrap();
        if let Some(expected) = &expected {
            assert_eq!(&encoded, expected, "arrival order {order:?}");
        } else {
            expected = Some(encoded);
        }
    }
}

#[test]
fn tied_vote_times_have_one_reward_identity_for_every_arrival_order() {
    let mut expected = None;
    for order in ORDERS {
        let mut fixture = Fixture::new();
        let votes: Vec<_> = (1..=3)
            .map(|seed| fixture.vote(seed, if seed == 3 { 2 } else { 1 }, 1000))
            .collect();
        for seed in order {
            fixture.apply(&votes[seed - 1], true).unwrap();
        }
        fixture = fixture.reopen();
        let mut pending = fixture.store.pending_store().get_all_pending().unwrap();
        pending.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        assert_eq!(pending.len(), 2);
        assert!(votes
            .iter()
            .any(|vote| vote.hash.into_tx_hash() == pending[0].1));
        assert_eq!(pending[0].1, pending[1].1);
        let encoded = bincode::serialize(&pending).unwrap();
        if let Some(expected) = &expected {
            assert_eq!(&encoded, expected);
        } else {
            expected = Some(encoded);
        }
    }
}

#[test]
fn challenge_arrival_order_preserves_rewards_penalties_and_revocation() {
    for fraud in [false, true] {
        let mut expected = None;
        for order in ORDERS {
            let mut fixture = Fixture::new();
            fixture.verified_target();
            fixture.seed_target_origin_at_holder();
            fixture
                .apply(
                    &fixture.challenge(burst_verification::ChallengeReason::Fraud),
                    true,
                )
                .unwrap();
            fixture.select(&[3, 4, 5]);
            let votes: Vec<_> = (3..=5)
                .map(|seed| {
                    let legitimate = (seed != 5) != fraud;
                    fixture.vote(seed, if legitimate { 1 } else { 2 }, 1100 + u64::from(seed))
                })
                .collect();
            for seed in order {
                fixture.apply(&votes[seed - 1], true).unwrap();
            }
            fixture = fixture.reopen();
            let account = fixture
                .store
                .account_store()
                .get_account(&fixture.target)
                .unwrap();
            assert_eq!(
                account.state,
                if fraud {
                    WalletState::Revoked
                } else {
                    WalletState::Verified
                }
            );
            let challenger = fixture.brn.get_wallet(&address(0)).unwrap();
            assert_eq!(challenger.total_staked, 0);
            assert_eq!(challenger.total_burned, if fraud { 0 } else { 100 });
            let mut pending = fixture.store.pending_store().get_all_pending().unwrap();
            pending.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
            assert_eq!(pending.len(), if fraud { 3 } else { 2 });
            for (_, source, info) in &pending {
                assert_eq!(*source, votes[2].hash.into_tx_hash());
                assert_eq!(info.timestamp, test_time(1105));
            }
            let encoded = bincode::serialize(&pending).unwrap();
            if let Some(expected) = &expected {
                assert_eq!(&encoded, expected);
            } else {
                expected = Some(encoded);
            }
        }
    }
}
