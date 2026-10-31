//! Governance storage trait.

use crate::StoreError;
use burst_types::{TxHash, WalletAddress};

/// Trait for storing governance state (proposals, votes, delegations, consti).
pub trait GovernanceStore {
    /// Store a proposal.
    fn put_proposal(&self, hash: &TxHash, data: &[u8]) -> Result<(), StoreError>;

    /// Get a proposal by hash.
    fn get_proposal(&self, hash: &TxHash) -> Result<Vec<u8>, StoreError>;

    /// Hashes of all stored proposals, regardless of voting status.
    fn all_proposal_hashes(&self) -> Result<Vec<TxHash>, StoreError>;

    /// Proposal keys in ascending order, strictly after `after` when supplied.
    /// `offset` skips matching keys; keyset callers use zero. The default loads
    /// and sorts every proposal hash before selecting the requested page.
    fn proposal_hash_page(
        &self,
        after: Option<&TxHash>,
        offset: u64,
        limit: usize,
    ) -> Result<Vec<TxHash>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut hashes = self.all_proposal_hashes()?;
        hashes.sort_unstable();
        Ok(hashes
            .into_iter()
            .filter(|hash| after.is_none_or(|key| hash > key))
            .skip(usize::try_from(offset).unwrap_or(usize::MAX))
            .take(limit)
            .collect())
    }

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
