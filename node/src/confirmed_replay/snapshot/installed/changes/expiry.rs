use crate::confirmed_replay::ReplayError;
use burst_store::account::AccountInfo;
use burst_trst::TrstEngine;
use burst_types::Timestamp;

/// A private cache view at one fixed clock. Pending returns and signed balances
/// are not expiry caches and must remain products of normal certified replay.
pub(super) struct ExpiryView {
    pub trst: TrstEngine,
    at: Timestamp,
}

impl ExpiryView {
    pub(super) fn new(trst: &TrstEngine, period: u64, at: Timestamp) -> Self {
        let mut trst = trst.clone();
        trst.set_expiry_period(period, at);
        Self { trst, at }
    }

    pub(super) fn account(&self, source: &AccountInfo) -> Result<AccountInfo, ReplayError> {
        normalized_account(source, &self.trst, self.at)
    }
}

pub(super) fn normalized_account(
    source: &AccountInfo,
    trst: &TrstEngine,
    at: Timestamp,
) -> Result<AccountInfo, ReplayError> {
    let mut account = source.clone();
    if trst.get_portfolio(&account.address).is_some() {
        crate::ledger_updater::update_token_counters(&mut account, trst, at)
            .map_err(crate::confirmed_replay::invalid)?;
    } else {
        account.expired_trst = 0;
        account.revoked_trst = 0;
    }
    Ok(account)
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::{TrstState, TxHash, WalletAddress};

    #[test]
    fn normalized_expiry_preserves_signed_balances_revocation_and_original_state() {
        let holder = WalletAddress::new("brst_expiry_holder");
        let origin = WalletAddress::new("brst_expiry_origin");
        let mut trst = TrstEngine::with_expiry(100);
        let token = trst
            .mint(
                TxHash::new([1; 32]),
                holder.clone(),
                500,
                origin.clone(),
                Timestamp::new(1000),
            )
            .unwrap();
        trst.track_token(token);
        let graph = trst.merger_graph.to_bytes();
        let active = trst.save_wallets();
        trst.flush_all_expired(Timestamp::new(1200));
        let expired = trst.save_wallets();
        let view = ExpiryView::new(&trst, 100, Timestamp::new(1050));
        assert_eq!(view.trst.save_wallets(), active);
        assert_eq!(trst.save_wallets(), expired);
        assert_eq!(view.trst.merger_graph.to_bytes(), graph);
        let mut account = AccountInfo::unopened(holder.clone());
        account.trst_balance = 500;
        account.expired_trst = 500;
        account.confirmation_height = 2;
        account.total_brn_burned = 7;
        let current = view.account(&account).unwrap();
        assert_eq!(current.expired_trst, 0);
        assert_eq!(current.trst_balance, 500);
        assert_eq!(current.confirmation_height, 2);
        assert_eq!(current.total_brn_burned, 7);
        assert_eq!(account.expired_trst, 500);
        let expired_view = ExpiryView::new(&trst, 100, Timestamp::new(1200));
        assert_eq!(expired_view.account(&account).unwrap().expired_trst, 500);
        trst.revoke_by_origin(&origin);
        let revoked = ExpiryView::new(&trst, 10000, Timestamp::new(1050));
        assert_eq!(
            revoked.trst.get_portfolio(&holder).unwrap().tokens[0].state,
            TrstState::Revoked
        );
        let current = revoked.account(&account).unwrap();
        assert_eq!(current.revoked_trst, 500);
        assert_eq!(current.expired_trst, 0);
        assert_eq!(current.trst_balance, 500);
    }
}
