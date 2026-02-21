//! New wallet spending and rate limits.
//!
//! Enforces `new_wallet_spending_limit`, `new_wallet_tx_limit_per_day`, and
//! `new_wallet_rate_limit_duration_secs` from `ProtocolParams`. These limits
//! only apply to wallets that have been verified for less than the configured
//! duration; established wallets are exempt.

use burst_store::account::AccountInfo;
use burst_types::{ProtocolParams, Timestamp};

/// Check if a transaction from this account exceeds new-wallet limits.
///
/// Returns `Ok(())` if the transaction is allowed, or `Err(reason)` if it
/// violates a spending or rate limit.
pub fn check_wallet_limits(
    account: &AccountInfo,
    amount: u128,
    now: Timestamp,
    params: &ProtocolParams,
) -> Result<(), String> {
    // If no limits are configured, skip entirely
    if params.new_wallet_spending_limit == 0 && params.new_wallet_tx_limit_per_day == 0 {
        return Ok(());
    }

    // Determine whether this is a "new" wallet based on verification age
    let is_new_wallet = match &account.verified_at {
        Some(verified_at) => {
            let age_secs = now.as_secs().saturating_sub(verified_at.as_secs());
            age_secs < params.new_wallet_rate_limit_duration_secs
        }
        None => true, // Unverified wallets are always subject to limits
    };

    if !is_new_wallet {
        return Ok(()); // Established wallet, no limits
    }

    // Check per-transaction spending limit
    if params.new_wallet_spending_limit > 0 && amount > params.new_wallet_spending_limit {
        return Err(format!(
            "transaction amount {} exceeds new wallet spending limit {}",
            amount, params.new_wallet_spending_limit
        ));
    }

    Ok(())
}

/// Check if a new wallet has exceeded its daily transaction limit.
pub fn check_daily_tx_limit(block_count_today: u32, params: &ProtocolParams) -> Result<(), String> {
    if params.new_wallet_tx_limit_per_day == 0 {
        return Ok(());
    }
    if block_count_today >= params.new_wallet_tx_limit_per_day {
        return Err(format!(
            "new wallet daily transaction limit exceeded: {}/{} per day",
            block_count_today, params.new_wallet_tx_limit_per_day
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::{BlockHash, WalletAddress, WalletState};

    fn make_account(verified_at: Option<u64>) -> AccountInfo {
        AccountInfo {
            address: WalletAddress::new(
                "brst_1111111111111111111111111111111111111111111111111111111111111111111111111111",
            ),
            state: if verified_at.is_some() {
                WalletState::Verified
            } else {
                WalletState::Unverified
            },
            verified_at: verified_at.map(Timestamp::new),
            head: BlockHash::ZERO,
            block_count: 1,
            confirmation_height: 0,
            representative: WalletAddress::new(
                "brst_2222222222222222222222222222222222222222222222222222222222222222222222222222",
            ),
            total_brn_burned: 0,
            trst_balance: 10000,
            total_brn_staked: 0,
            expired_trst: 0,
            revoked_trst: 0,
            epoch: 0,
        }
    }

    fn test_params() -> ProtocolParams {
        let mut params = ProtocolParams::default();
        params.new_wallet_spending_limit = 5000;
        params.new_wallet_tx_limit_per_day = 10;
        params.new_wallet_rate_limit_duration_secs = 30 * 24 * 3600; // 30 days
        params
    }

    #[test]
    fn new_wallet_within_spending_limit() {
        let account = make_account(Some(1000));
        let params = test_params();
        let now = Timestamp::new(1000 + 86400); // 1 day after verification

        let result = check_wallet_limits(&account, 4000, now, &params);
        assert!(result.is_ok());
    }

    #[test]
    fn new_wallet_exceeds_spending_limit() {
        let account = make_account(Some(1000));
        let params = test_params();
        let now = Timestamp::new(1000 + 86400); // 1 day after verification

        let result = check_wallet_limits(&account, 6000, now, &params);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("exceeds new wallet spending limit"));
    }

    #[test]
    fn established_wallet_exempt_from_limits() {
        let account = make_account(Some(1000));
        let params = test_params();
        // Well past the rate limit duration (30 days + extra)
        let now = Timestamp::new(1000 + params.new_wallet_rate_limit_duration_secs + 1);

        let result = check_wallet_limits(&account, 999999, now, &params);
        assert!(result.is_ok());
    }

    #[test]
    fn unverified_wallet_subject_to_limits() {
        let account = make_account(None);
        let params = test_params();
        let now = Timestamp::new(1000);

        let result = check_wallet_limits(&account, 6000, now, &params);
        assert!(result.is_err());
    }

    #[test]
    fn zero_limits_means_no_enforcement() {
        let account = make_account(Some(1000));
        let mut params = test_params();
        params.new_wallet_spending_limit = 0;
        params.new_wallet_tx_limit_per_day = 0;
        let now = Timestamp::new(1000 + 86400);

        let result = check_wallet_limits(&account, 999999, now, &params);
        assert!(result.is_ok());
    }

    #[test]
    fn exact_spending_limit_allowed() {
        let account = make_account(Some(1000));
        let params = test_params();
        let now = Timestamp::new(1000 + 86400);

        let result = check_wallet_limits(&account, 5000, now, &params);
        assert!(result.is_ok());
    }
}
