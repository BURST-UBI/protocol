//! Governance storage trait.

use crate::StoreError;
use burst_types::{TxHash, WalletAddress};

/// Trait for storing governance state (proposals, votes, delegations, consti).
pub trait GovernanceStore {
    /// Store a proposal.
    fn put_proposal(&self, hash: &TxHash, data: &[u8]) -> Result<(), StoreError>;

    /// Get a proposal by hash.
    fn get_proposal(&self, hash: &TxHash) -> Result<Vec<u8>, StoreError>;

    /// List all active proposals.
    fn list_active_proposals(&self) -> Result<Vec<TxHash>, StoreError>;

    /// Store a vote on a proposal.
    fn put_vote(
        &self,
        proposal: &TxHash,
        voter: &WalletAddress,
        data: &[u8],
    ) -> Result<(), StoreError>;

    /// Get a specific voter's vote on a proposal.
    fn get_vote(&self, proposal: &TxHash, voter: &WalletAddress) -> Result<Vec<u8>, StoreError>;

    /// Get all votes for a proposal.
    fn get_votes(&self, proposal: &TxHash) -> Result<Vec<Vec<u8>>, StoreError>;

    /// Store a delegation record.
    fn put_delegation(&self, delegator: &WalletAddress, data: &[u8]) -> Result<(), StoreError>;

    /// Get the current delegation for a wallet.
    fn get_delegation(&self, delegator: &WalletAddress) -> Result<Option<Vec<u8>>, StoreError>;

    /// Store the current constitution text.
    fn put_constitution(&self, data: &[u8]) -> Result<(), StoreError>;

    /// Get the current constitution text.
    fn get_constitution(&self) -> Result<Vec<u8>, StoreError>;
}
