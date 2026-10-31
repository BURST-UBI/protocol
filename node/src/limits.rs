//! New-wallet per-payment spending admission.
//!
//! Applies to each Send or Burn amount; there is no daily accumulated allowance.

use burst_store::account::AccountInfo;
use burst_types::{ProtocolParams, Timestamp};

/// Enforce a per-operation cap until the verification age reaches
/// `new_wallet_rate_limit_duration_secs`. A missing verification timestamp stays
/// subject to the cap; this helper does not check wallet state. Zero disables it.
pub fn check_wallet_spending_limit(
    account: &AccountInfo,
    amount: u128,
    now: Timestamp,
    params: &ProtocolParams,
) -> Result<(), String> {
    if params.new_wallet_spending_limit == 0 {
        return Ok(());
    }

    let is_new_wallet = match &account.verified_at {
        Some(verified_at) => {
            let age_secs = now.as_secs().saturating_sub(verified_at.as_secs());
            age_secs < params.new_wallet_rate_limit_duration_secs
        }
        None => true, // No timestamp establishes exemption from the age window.
    };

    if !is_new_wallet {
        return Ok(());
    }

    if params.new_wallet_spending_limit > 0 && amount > params.new_wallet_spending_limit {
        return Err(format!(
            "transaction amount {} exceeds new wallet spending limit {}",
            amount, params.new_wallet_spending_limit
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
            verifier_opted_in_at: None,
            orv_evicted: false,
        }
    }

    fn test_params() -> ProtocolParams {
        ProtocolParams {
            new_wallet_spending_limit: 5000,
            new_wallet_rate_limit_duration_secs: 30 * 24 * 3600,
            ..ProtocolParams::default()
        }
    }

    #[test]
    fn new_wallet_within_spending_limit() {
        let account = make_account(Some(1000));
        let params = test_params();
        let now = Timestamp::new(1000 + 86400); // 1 day after verification

        let result = check_wallet_spending_limit(&account, 4000, now, &params);
        assert!(result.is_ok());
    }

    #[test]
    fn new_wallet_exceeds_spending_limit() {
        let account = make_account(Some(1000));
        let params = test_params();
        let now = Timestamp::new(1000 + 86400); // 1 day after verification

        let result = check_wallet_spending_limit(&account, 6000, now, &params);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("exceeds new wallet spending limit"));
    }

    #[test]
    fn established_wallet_exempt_from_limits() {
        let account = make_account(Some(1000));
        let params = test_params();
        let now = Timestamp::new(1000 + params.new_wallet_rate_limit_duration_secs + 1);

        let result = check_wallet_spending_limit(&account, 999999, now, &params);
        assert!(result.is_ok());
    }

    #[test]
    fn unverified_wallet_subject_to_limits() {
        let account = make_account(None);
        let params = test_params();
        let now = Timestamp::new(1000);

        let result = check_wallet_spending_limit(&account, 6000, now, &params);
        assert!(result.is_err());
    }

    #[test]
    fn zero_limits_means_no_enforcement() {
        let account = make_account(Some(1000));
        let mut params = test_params();
        params.new_wallet_spending_limit = 0;
        let now = Timestamp::new(1000 + 86400);

        let result = check_wallet_spending_limit(&account, 999999, now, &params);
        assert!(result.is_ok());
    }

    #[test]
    fn exact_spending_limit_allowed() {
        let account = make_account(Some(1000));
        let params = test_params();
        let now = Timestamp::new(1000 + 86400);

        let result = check_wallet_spending_limit(&account, 5000, now, &params);
        assert!(result.is_ok());
    }
}
