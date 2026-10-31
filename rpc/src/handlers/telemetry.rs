use crate::error::RpcError;
use burst_trst::TrstEngine;
use burst_types::{Timestamp, TrstState};

use burst_store::{account::AccountInfo, StoreError};

pub(super) struct Summary {
    pub accounts: u64,
    pub verified: u64,
    pub unverified: u64,
    pub holdings: [u128; 3],
}

// Capture holdings before scanning accounts and release the engine lock first.
// The caller supplies account membership and counts from its read view; holdings
// and accounts are not captured atomically together. Only visited accounts count.
// Totals are token amounts, with revoked taking precedence over expired. Pending
// transfers are excluded, and unverified counts exclude revoked/deactivated accounts.
pub(super) fn read(
    engine: &tokio::sync::Mutex<TrstEngine>,
    now: Timestamp,
    visit: impl FnOnce(&mut dyn FnMut(AccountInfo) -> Result<(), StoreError>) -> Result<(), StoreError>,
) -> Result<Summary, RpcError> {
    let (wallets, expiry_secs) = {
        let trst = engine.blocking_lock();
        (trst.wallets.clone(), trst.expiry_secs)
    };
    let mut summary = Summary { accounts: 0, verified: 0, unverified: 0, holdings: [0; 3] };
    visit(&mut |account| {
        summary.accounts += 1;
        summary.verified += u64::from(account.state.is_verified());
        summary.unverified += u64::from(account.state == burst_types::WalletState::Unverified);
        if let Some(portfolio) = wallets.get(&account.address) {
            for token in portfolio.iter_tokens() {
                let index = if token.state == TrstState::Revoked {
                    2
                } else if token.is_expired(now, expiry_secs) {
                    1
                } else {
                    0
                };
                summary.holdings[index] = summary.holdings[index].checked_add(token.amount)
                    .ok_or_else(|| StoreError::Corruption("TRST total overflow".into()))?;
            }
        }
        Ok(())
    })?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::{TxHash, WalletAddress};

    fn holdings_totals<'a>(
        engine: &tokio::sync::Mutex<TrstEngine>,
        accounts: impl Iterator<Item = &'a WalletAddress>,
        now: Timestamp,
    ) -> Result<[u128; 3], RpcError> {
        read(engine, now, |visit| {
            for account in accounts { visit(AccountInfo::unopened(account.clone()))?; }
            Ok(())
        }).map(|summary| summary.holdings)
    }

    #[test]
    fn telemetry_streams_counts_and_stops_on_store_failure() {
        let engine = tokio::sync::Mutex::new(TrstEngine::new());
        let mut visited = 0;
        let summary = read(&engine, Timestamp::new(1), |visit| {
            for n in 0..10_000 {
                let mut account = AccountInfo::unopened(WalletAddress::new(format!("brst_{n}")));
                account.state = match n % 4 {
                    0 => burst_types::WalletState::Verified,
                    1 => burst_types::WalletState::Revoked,
                    2 => burst_types::WalletState::Deactivated,
                    _ => burst_types::WalletState::Unverified,
                };
                visit(account)?;
                visited += 1;
            }
            Ok(())
        }).unwrap();
        assert_eq!((summary.accounts, summary.verified, summary.unverified), (10_000, 2500, 2500));
        assert_eq!(visited, 10_000);
        assert_eq!(summary.holdings, [0; 3]);
        let result = read(&engine, Timestamp::new(1), |visit| {
            visit(AccountInfo::unopened(WalletAddress::new("brst_first")))?;
            Err(StoreError::Corruption("broken account row".into()))
        });
        assert!(matches!(result, Err(RpcError::Store(message)) if message.contains("broken account row")));
    }


    fn holding(engine: &mut TrstEngine, id: u8, wallet: &WalletAddress, at: u64, revoked: bool) {
        let mut token = engine.mint(TxHash::new([id; 32]), wallet.clone(),
            id as u128, wallet.clone(), Timestamp::new(at)).unwrap();
        if revoked {
            token.state = TrstState::Revoked;
            token.revoked_origin = Some(token.origin);
        }
        engine.track_token(token);
    }

    #[test]
    fn telemetry_captures_roots_and_releases_lock_before_scanning_accounts() {
        let account = WalletAddress::new("brst_account");
        let orphan = WalletAddress::new("brst_not_in_account_store");
        let mut engine = TrstEngine::new();
        engine.expiry_secs = 100;
        holding(&mut engine, 1, &account, 101, false);
        holding(&mut engine, 2, &account, 100, false);
        holding(&mut engine, 3, &account, 100, true);
        holding(&mut engine, 4, &orphan, 101, false);
        let engine = tokio::sync::Mutex::new(engine);
        let accounts = [&account];
        let iter = accounts.into_iter().inspect(|_| {
            let mut live = engine.try_lock().expect("holdings scan must release live lock");
            live.expiry_secs = 1;
            holding(&mut live, 5, &account, 200, false);
        });
        assert_eq!(holdings_totals(&engine, iter, Timestamp::new(200)).unwrap(), [1, 2, 3]);
        assert_eq!(holdings_totals(&engine, accounts.into_iter(), Timestamp::new(200)).unwrap(), [5, 3, 3]);
        assert_eq!(holdings_totals(&engine, [&WalletAddress::new("brst_missing")].into_iter(),
            Timestamp::new(200)).unwrap(), [0, 0, 0]);
    }

    #[test]
    fn telemetry_rejects_overflow() {
        let account = WalletAddress::new("brst_account");
        let mut engine = TrstEngine::new();
        holding(&mut engine, 1, &account, 100, false);
        holding(&mut engine, 2, &account, 100, false);
        engine.wallets.get_mut(&account).unwrap().tokens[0].amount = u128::MAX;
        let error = holdings_totals(&tokio::sync::Mutex::new(engine), [&account].into_iter(),
            Timestamp::new(100)).unwrap_err();
        assert!(error.to_string().contains("TRST total overflow"));
    }
}

pub(super) fn signing_clock(
    clock: Option<&burst_network::trusted_clock::TrustedClock>,
) -> burst_rpc_messages::node::SigningClockStatus {
    use burst_rpc_messages::node::SigningClockStatus;
    let Some(clock) = clock else { return SigningClockStatus::Disabled; };
    match clock.reading() {
        Ok(time) => match time.now.checked_add(time.error_bound) {
            Some(latest) => SigningClockStatus::Ready { earliest: time.now, latest },
            None => SigningClockStatus::Unavailable { reason: "signing-time bound overflow".into() },
        },
        Err(error) => SigningClockStatus::Unavailable { reason: error.to_string() },
    }
}

#[cfg(test)]
mod clock_tests {
    use super::*;
    use burst_rpc_messages::node::SigningClockStatus;

    #[test]
    fn missing_or_unsampled_clock_never_reports_ready() {
        assert_eq!(signing_clock(None), SigningClockStatus::Disabled);
        let clock = burst_network::trusted_clock::TrustedClock::new(4).unwrap();
        assert!(matches!(signing_clock(Some(&clock)), SigningClockStatus::Unavailable { .. }));
    }
}
