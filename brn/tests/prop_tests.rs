use proptest::prelude::*;

use burst_brn::state::BrnWalletState;
use burst_brn::BrnEngine;
use burst_types::Timestamp;

proptest! {
    /// BRN balance must monotonically increase with time (no burns/stakes).
    #[test]
    fn brn_accrual_monotonic(
        rate in 1u128..1_000_000,
        t1 in 1000u64..1_000_000,
        t2_offset in 1u64..100_000,
    ) {
        let engine = BrnEngine::with_rate(rate, Timestamp::new(0));
        let state = BrnWalletState::new(Timestamp::new(0));
        let b1 = engine.compute_balance(&state, Timestamp::new(t1));
        let b2 = engine.compute_balance(&state, Timestamp::new(t1 + t2_offset));
        prop_assert!(b2 >= b1, "balance must not decrease: b1={}, b2={}", b1, b2);
    }

    /// After a valid burn, balance must remain non-negative.
    #[test]
    fn brn_burn_never_exceeds_balance(
        rate in 1u128..1_000_000,
        time in 100u64..100_000,
        burn_frac_pct in 0u64..100,
    ) {
        let engine = BrnEngine::with_rate(rate, Timestamp::new(0));
        let mut state = BrnWalletState::new(Timestamp::new(0));
        let now = Timestamp::new(time);
        let balance = engine.compute_balance(&state, now);
        let burn = balance * burn_frac_pct as u128 / 100;
        if burn > 0 && burn <= balance {
            engine.record_burn(&mut state, burn, now).unwrap();
            let after = engine.compute_balance(&state, now);
            prop_assert!(after <= balance, "post-burn balance {} > pre-burn {}", after, balance);
        }
    }

    /// Accrued BRN is always >= burned + staked (no negative balance).
    #[test]
    fn accrued_gte_burned_plus_staked(
        rate in 1u128..10_000,
        time in 1u64..100_000,
        burned in 0u128..1_000_000,
        staked in 0u128..1_000_000,
    ) {
        let engine = BrnEngine::with_rate(rate, Timestamp::new(0));
        let mut state = BrnWalletState::new(Timestamp::new(0));
        let now = Timestamp::new(time);
        let accrued = engine.rate_history.total_accrued(Timestamp::new(0), now);
        if burned + staked <= accrued {
            state.total_burned = burned;
            state.total_staked = staked;
            let balance = engine.compute_balance(&state, now);
            prop_assert_eq!(balance, accrued - burned - staked);
        }
    }

    /// Rate change preserves total accrual at the exact change point.
    #[test]
    fn rate_change_preserves_accrual_at_boundary(
        rate1 in 1u128..10_000,
        rate2 in 1u128..10_000,
        change_time in 100u64..50_000,
    ) {
        let mut engine = BrnEngine::with_rate(rate1, Timestamp::new(0));
        let state = BrnWalletState::new(Timestamp::new(0));
        let change_at = Timestamp::new(change_time);

        let balance_before = engine.compute_balance(&state, change_at);
        engine.apply_rate_change(rate2, change_at).unwrap();
        let balance_after = engine.compute_balance(&state, change_at);

        prop_assert_eq!(
            balance_before, balance_after,
            "rate change must not alter balance at the boundary"
        );
    }

    /// With multiple rate segments, total accrual equals the sum of each segment.
    #[test]
    fn multi_segment_accrual_is_additive(
        rate1 in 1u128..1_000,
        rate2 in 1u128..1_000,
        rate3 in 1u128..1_000,
        dur1 in 100u64..10_000,
        dur2 in 100u64..10_000,
        dur3 in 100u64..10_000,
    ) {
        let start = 0u64;
        let t1 = start + dur1;
        let t2 = t1 + dur2;
        let t3 = t2 + dur3;

        let mut engine = BrnEngine::with_rate(rate1, Timestamp::new(start));
        let state = BrnWalletState::new(Timestamp::new(start));

        engine.apply_rate_change(rate2, Timestamp::new(t1)).unwrap();
        engine.apply_rate_change(rate3, Timestamp::new(t2)).unwrap();

        let total = engine.compute_balance(&state, Timestamp::new(t3));
        let expected = rate1 * dur1 as u128
            + rate2 * dur2 as u128
            + rate3 * dur3 as u128;

        prop_assert_eq!(total, expected, "segment accrual mismatch");
    }

    /// Zero rate produces zero accrual.
    #[test]
    fn zero_rate_zero_accrual(time in 1u64..1_000_000) {
        let engine = BrnEngine::with_rate(0, Timestamp::new(0));
        let state = BrnWalletState::new(Timestamp::new(0));
        let balance = engine.compute_balance(&state, Timestamp::new(time));
        prop_assert_eq!(balance, 0, "zero rate must produce zero balance");
    }

    /// Checked and unchecked balance agree for non-overflow inputs.
    #[test]
    fn checked_agrees_with_unchecked(
        rate in 1u128..1_000,
        time in 1u64..100_000,
    ) {
        let engine = BrnEngine::with_rate(rate, Timestamp::new(0));
        let state = BrnWalletState::new(Timestamp::new(0));
        let now = Timestamp::new(time);
        let checked = engine.compute_balance_checked(&state, now).unwrap();
        let unchecked = engine.compute_balance(&state, now);
        prop_assert_eq!(checked, unchecked);
    }

    /// Staking and returning stake restores original balance.
    #[test]
    fn stake_return_restores_balance(
        rate in 1u128..10_000,
        time in 1000u64..100_000,
        stake_frac_pct in 1u64..100,
    ) {
        let mut engine = BrnEngine::with_rate(rate, Timestamp::new(0));
        let mut state = BrnWalletState::new(Timestamp::new(0));
        let now = Timestamp::new(time);
        let balance_before = engine.compute_balance(&state, now);
        let stake_amount = balance_before * stake_frac_pct as u128 / 100;
        if stake_amount > 0 && stake_amount <= balance_before {
            let staker = burst_types::WalletAddress::new("brst_staker_wallet_00000000000000000000000000000000000000");
            let mut stake = engine.stake(
                &staker,
                &mut state,
                stake_amount,
                burst_brn::StakeKind::Verification { target_wallet: burst_types::WalletAddress::new("brst_test_wallet_000000000000000000000000000000000000000") },
                now,
            ).unwrap();
            let balance_during = engine.compute_balance(&state, now);
            prop_assert_eq!(balance_during, balance_before - stake_amount);

            engine.return_stake(&staker, &mut state, &mut stake).unwrap();
            let balance_after = engine.compute_balance(&state, now);
            prop_assert_eq!(balance_after, balance_before);
        }
    }
}
