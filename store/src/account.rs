//! Account storage trait.

use crate::StoreError;
use burst_types::{BlockHash, Timestamp, WalletAddress, WalletState};
use serde::{Deserialize, Serialize};

/// Per-account information stored in the ledger.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountInfo {
    pub address: WalletAddress,
    pub state: WalletState,
    pub verified_at: Option<Timestamp>,
    /// Hash of the latest block in this account's chain.
    pub head: BlockHash,
    /// Number of blocks in this account's chain.
    pub block_count: u64,
    /// Inclusive height of the cemented prefix; zero means no cemented blocks.
    pub confirmation_height: u64,
    /// Consensus representative for ORV.
    pub representative: WalletAddress,
    /// Total BRN burned by this account.
    pub total_brn_burned: u128,
    /// Total BRN currently locked in active stakes (verification/challenge).
    pub total_brn_staked: u128,
    /// Last signed account-chain TRST balance. Expiry and revocation do not
    /// rewrite it; the provenance portfolio determines what is spendable.
    pub trst_balance: u128,
    /// Recorded expired TRST total, separate from spendable holdings.
    pub expired_trst: u128,
    /// Total TRST revoked due to fraud detection.
    pub revoked_trst: u128,
    /// Account epoch/version, upgraded by epoch blocks.
    #[serde(default)]
    pub epoch: u8,
    /// Recorded verifier opt-in time. None means opted out; Some alone does
    /// not establish selection eligibility.
    /// Set/cleared by on-chain VerifierOptIn / VerifierOptOut blocks. Because it
    /// is updated during block application, it may include provisional effects.
    /// Verifier selection must use a replayed, certified state cut, not this
    /// mutable account view.
    #[serde(default)]
    pub verifier_opted_in_at: Option<Timestamp>,
    /// Recorded representative eviction status. Agreed authority derives the
    /// voting effect from certified state; this mutable flag alone does not
    /// install a new electorate or prove that governance can make progress.
    #[serde(default)]
    pub orv_evicted: bool,
}

impl AccountInfo {
    /// Create an unverified account record with no blocks or balances.
    pub fn unopened(address: WalletAddress) -> Self {
        Self {
            representative: address.clone(),
            address,
            state: WalletState::Unverified,
            verified_at: None,
            head: BlockHash::ZERO,
            block_count: 0,
            confirmation_height: 0,
            total_brn_burned: 0,
            total_brn_staked: 0,
            trst_balance: 0,
            expired_trst: 0,
            revoked_trst: 0,
            epoch: 0,
            verifier_opted_in_at: None,
            orv_evicted: false,
        }
    }
}

/// Trait for account storage operations.
pub trait AccountStore {
    fn get_account(&self, address: &WalletAddress) -> Result<AccountInfo, StoreError>;
    fn put_account(&self, info: &AccountInfo) -> Result<(), StoreError>;
    /// Atomically advance finality without rewriting a stale account snapshot.
    fn advance_confirmation_height(
        &self,
        address: &WalletAddress,
        height: u64,
    ) -> Result<(), StoreError>;
    fn exists(&self, address: &WalletAddress) -> Result<bool, StoreError>;
    fn account_count(&self) -> Result<u64, StoreError>;
    fn all_accounts(&self) -> Result<Vec<AccountInfo>, StoreError>;
    fn all_verified_accounts(&self) -> Result<Vec<AccountInfo>, StoreError>;

    /// Eligible addresses without requiring full account payloads from indexed stores.
    fn all_verified_addresses(&self) -> Result<Vec<WalletAddress>, StoreError> {
        self.all_verified_accounts().map(|rows| rows.into_iter().map(|account| account.address).collect())
    }

    /// Visit accounts in one consistent read view when supported by the backend.
    /// The default preserves compatibility; streaming backends avoid a full Vec.
    fn visit_accounts(
        &self,
        visitor: &mut dyn FnMut(AccountInfo) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        for account in self.all_accounts()? {
            visitor(account)?;
        }
        Ok(())
    }

    /// Count verified accounts. The default materializes all verified accounts;
    /// indexed backends can override it with a count query.
    fn verified_account_count(&self) -> Result<u64, StoreError> {
        self.all_verified_accounts().map(|v| v.len() as u64)
    }

    /// Backend pagination hook. The default ignores cursor and limit and returns
    /// all accounts. Callers needing bounded pages must use a backend override.
    fn accounts_page(
        &self,
        _cursor: Option<&WalletAddress>,
        _limit: usize,
    ) -> Result<Vec<AccountInfo>, StoreError> {
        self.all_accounts()
    }
}
