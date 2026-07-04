//! Pending receive storage trait.

use crate::StoreError;
use burst_types::{Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// Information about a pending incoming transfer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingInfo {
    pub source: WalletAddress,
    pub amount: u128,
    pub timestamp: Timestamp,
    /// Provenance from the consumed tokens (origin, origin_wallet, timestamps).
    /// Empty if the sender wasn't tracked in the TRST engine.
    #[serde(default)]
    pub provenance: Vec<PendingProvenance>,
}

/// Origin provenance for a consumed token portion, stored in pending entries.
///
/// Carries only the single origin pointer (IMPLEMENTATION_DECISIONS 6.17b) —
/// for merged tokens the origin is the merge tx hash and constituent origins
/// are found by following the merger graph.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingProvenance {
    pub amount: u128,
    pub origin: TxHash,
    pub origin_wallet: WalletAddress,
    pub origin_timestamp: Timestamp,
    pub effective_origin_timestamp: Timestamp,
}

/// Trait for tracking pending receives.
///
/// Keys are `(destination, source_hash)` pairs. Each pending entry represents
/// an incoming transfer that has not yet been pocketed by the destination account.
pub trait PendingStore {
    /// Record a pending receive for the destination account.
    fn put_pending(
        &self,
        destination: &WalletAddress,
        source_hash: &TxHash,
        info: &PendingInfo,
    ) -> Result<(), StoreError>;

    /// Retrieve a specific pending receive.
    fn get_pending(
        &self,
        destination: &WalletAddress,
        source_hash: &TxHash,
    ) -> Result<PendingInfo, StoreError>;

    /// Delete a pending receive (once it has been pocketed).
    fn delete_pending(
        &self,
        destination: &WalletAddress,
        source_hash: &TxHash,
    ) -> Result<(), StoreError>;

    /// Get all pending receives for a given destination account.
    fn get_pending_for_account(
        &self,
        destination: &WalletAddress,
    ) -> Result<Vec<PendingInfo>, StoreError>;

    /// Get all pending receives with their source block hashes (for receive RPC).
    fn get_pending_for_account_with_hashes(
        &self,
        destination: &WalletAddress,
    ) -> Result<Vec<(TxHash, PendingInfo)>, StoreError>;

    /// Total number of pending receives across all accounts.
    fn pending_count(&self) -> Result<u64, StoreError>;

    /// Get every pending receive across all accounts.
    ///
    /// Used by the expiry sweep (6.16a): pending TRST whose token has expired
    /// is returned to the sender. O(total pending) — call from periodic
    /// background tasks only.
    fn get_all_pending(&self) -> Result<Vec<(WalletAddress, TxHash, PendingInfo)>, StoreError>;
}
