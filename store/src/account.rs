//! Account storage trait.

use crate::StoreError;
use burst_types::{BlockHash, Timestamp, WalletAddress, WalletState};
use serde::{Deserialize, Serialize};

/// Per-account information stored in the ledger.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountInfo {
    pub address: WalletAddress,
    pub state: WalletState,
    pub verified_at: Option<Timestamp>,
    /// Hash of the latest block in this account's chain.
    pub head: BlockHash,
    /// Number of blocks in this account's chain.
    pub block_count: u64,
    /// Height below which all blocks are cemented (confirmed and final).
    pub confirmation_height: u64,
    /// Consensus representative for ORV.
    pub representative: WalletAddress,
    /// Total BRN burned by this account.
    pub total_brn_burned: u128,
    /// Total BRN currently locked in active stakes (verification/challenge).
    pub total_brn_staked: u128,
    /// Total transferable TRST balance.
    pub trst_balance: u128,
    /// Total expired TRST (non-transferable reputation/virtue points).
    pub expired_trst: u128,
    /// Total TRST revoked due to fraud detection.
    pub revoked_trst: u128,
    /// Account epoch/version, upgraded by epoch blocks.
    #[serde(default)]
    pub epoch: u8,
}

/// Trait for account storage operations.
pub trait AccountStore {
    fn get_account(&self, address: &WalletAddress) -> Result<AccountInfo, StoreError>;
    fn put_account(&self, info: &AccountInfo) -> Result<(), StoreError>;
    fn exists(&self, address: &WalletAddress) -> Result<bool, StoreError>;
    fn account_count(&self) -> Result<u64, StoreError>;
    fn iter_accounts(&self) -> Result<Vec<AccountInfo>, StoreError>;
    fn iter_verified_accounts(&self) -> Result<Vec<AccountInfo>, StoreError>;

    /// Count verified accounts without allocating the full result set.
    fn verified_account_count(&self) -> Result<u64, StoreError> {
        self.iter_verified_accounts().map(|v| v.len() as u64)
    }

    /// Iterate accounts with pagination support.
    /// Returns up to `limit` accounts starting after `cursor` (or from the beginning if None).
    fn iter_accounts_paged(
        &self,
        _cursor: Option<&WalletAddress>,
        _limit: usize,
    ) -> Result<Vec<AccountInfo>, StoreError> {
        self.iter_accounts()
    }
}
