//! BRN balance computation (client-side).

use burst_brn::state::RateHistory;
use burst_types::Timestamp;
use crate::WalletError;

/// Compute BRN balance using the full rate history (piecewise integration).
///
/// `BRN = Σ(rate_i × duration_i) − total_burned − total_staked`
///
/// This is the correct computation that accounts for governance rate changes.
/// Accrual overflow is an error; deductions beyond accrual display as zero.
pub fn compute_balance_with_history(
    verified_at: Timestamp,
    now: Timestamp,
    rate_history: &RateHistory,
    total_burned: u128,
    total_staked: u128,
) -> Result<u128, WalletError> {
    let accrued = rate_history.total_accrued_checked(verified_at, now)
        .ok_or_else(|| WalletError::Other("BRN accrual overflow".into()))?;
    Ok(accrued
        .saturating_sub(total_burned)
        .saturating_sub(total_staked))
}

/// Compute BRN balance for display (single-rate estimate).
///
/// `BRN = rate × (now − verified_at) − total_burned − total_staked`
///
/// This is a simplified offline estimate that assumes a constant rate.
/// For correct results across governance rate changes, use
/// `compute_balance_with_history` instead.
/// Accrual overflow is an error, not a saturated estimate.
pub fn compute_display_balance(
    verified_at: Timestamp,
    now: Timestamp,
    rate: u128,
    total_burned: u128,
    total_staked: u128,
) -> Result<u128, WalletError> {
    let elapsed = verified_at.elapsed_since(now) as u128;
    let accrued = rate.checked_mul(elapsed)
        .ok_or_else(|| WalletError::Other("BRN accrual overflow".into()))?;
    Ok(accrued
        .saturating_sub(total_burned)
        .saturating_sub(total_staked))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_balance_basic_accrual() {
        let verified = Timestamp::new(1000);
        let now = Timestamp::new(2000);
        let balance = compute_display_balance(verified, now, 10, 0, 0).unwrap();
        assert_eq!(balance, 10_000);
    }

    #[test]
    fn display_balance_subtracts_burned_and_staked() {
        let verified = Timestamp::new(0);
        let now = Timestamp::new(100);
        let balance = compute_display_balance(verified, now, 10, 300, 200).unwrap();
        assert_eq!(balance, 500); // 1000 - 300 - 200
    }

    #[test]
    fn display_balance_saturates_when_deductions_exceed_accrual() {
        let verified = Timestamp::new(0);
        let now = Timestamp::new(10);
        let balance = compute_display_balance(verified, now, 1, 50, 50).unwrap();
        assert_eq!(balance, 0); // 10 - 50 saturates to 0
    }

    #[test]
    fn display_balance_zero_elapsed() {
        let t = Timestamp::new(5000);
        let balance = compute_display_balance(t, t, 100, 0, 0).unwrap();
        assert_eq!(balance, 0);
    }

    #[test]
    fn display_balance_direction_regression() {
        // This is the regression test for the bug we fixed:
        // verified_at should be BEFORE now, and elapsed = now - verified_at.
        let verified = Timestamp::new(1000);
        let now = Timestamp::new(5000);
        let balance = compute_display_balance(verified, now, 1, 0, 0).unwrap();
        assert_eq!(balance, 4000, "elapsed should be now - verified_at = 4000");
    }

    #[test]
    fn history_balance_single_rate() {
        let history = RateHistory::new(100, Timestamp::new(0));
        let balance = compute_balance_with_history(
            Timestamp::new(1000),
            Timestamp::new(2000),
            &history,
            0,
            0,
        );
        assert_eq!(balance.unwrap(), 100_000);
    }

    #[test]
    fn history_balance_two_rates() {
        let mut history = RateHistory::new(100, Timestamp::new(0));
        history
            .apply_rate_change(200, Timestamp::new(5000))
            .unwrap();
        let balance = compute_balance_with_history(
            Timestamp::new(1000),
            Timestamp::new(8000),
            &history,
            0,
            0,
        );
        // 100*(5000-1000) + 200*(8000-5000) = 400_000 + 600_000 = 1_000_000
        assert_eq!(balance.unwrap(), 1_000_000);
    }

    #[test]
    fn history_balance_with_deductions() {
        let history = RateHistory::new(10, Timestamp::new(0));
        let balance = compute_balance_with_history(
            Timestamp::new(0),
            Timestamp::new(100),
            &history,
            500,
            200,
        );
        assert_eq!(balance.unwrap(), 300); // 1000 - 500 - 200
    }

    #[test]
    fn history_balance_saturates_on_large_deductions() {
        let history = RateHistory::new(1, Timestamp::new(0));
        let balance =
            compute_balance_with_history(Timestamp::new(0), Timestamp::new(10), &history, 100, 100);
        assert_eq!(balance.unwrap(), 0);
    }

    #[test]
    fn overflow_is_not_reported_as_zero_or_a_saturated_balance() {
        let start = Timestamp::EPOCH;
        let now = Timestamp::new(2);
        let history = RateHistory::new(u128::MAX, start);
        assert!(compute_balance_with_history(start, now, &history, 0, 0).is_err());
        assert!(compute_display_balance(start, now, u128::MAX, 0, 0).is_err());
        assert_eq!(compute_display_balance(start, Timestamp::new(1), u128::MAX, 0, 0).unwrap(), u128::MAX);
    }
}
