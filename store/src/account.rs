//! Account storage trait.

use crate::StoreError;
use burst_types::{BlockHash, Timestamp, WalletAddress, WalletState};

/// Per-account information stored in the ledger.
#[derive(Clone, Debug)]
pub struct AccountInfo {
    pub address: WalletAddress,
    pub state: WalletState,
    pub verified_at: Option<Timestamp>,
    /// Hash of the latest block in this account's chain.
    pub head: BlockHash,
    /// Number of blocks in this account's chain.
    pub block_count: u64,
    /// Consensus representative for ORV.
    pub representative: WalletAddress,
    /// Total BRN burned by this account.
    pub total_brn_burned: u128,
    /// Total transferable TRST balance.
    pub trst_balance: u128,
}

/// Trait for account storage operations.
pub trait AccountStore {
    fn get_account(&self, address: &WalletAddress) -> Result<AccountInfo, StoreError>;
    fn put_account(&self, info: &AccountInfo) -> Result<(), StoreError>;
    fn exists(&self, address: &WalletAddress) -> Result<bool, StoreError>;
    fn account_count(&self) -> Result<u64, StoreError>;
    fn iter_accounts(&self) -> Result<Vec<AccountInfo>, StoreError>;
    fn iter_verified_accounts(&self) -> Result<Vec<AccountInfo>, StoreError>;
}
