//! Verification data storage trait.

use crate::StoreError;
use burst_types::WalletAddress;

/// Trait for storing verification state (endorsements, votes, challenges).
///
/// Endorsements and votes use `(target, actor)` keys. LMDB reads each target
/// with a prefix scan; collecting the result costs at least one step per row.
pub trait VerificationStore {
    /// Store endorsement data for a target wallet from a specific endorser.
    fn put_endorsement(
        &self,
        target: &WalletAddress,
        endorser: &WalletAddress,
        data: &[u8],
    ) -> Result<(), StoreError>;

    /// Get all endorsements for a target wallet.
    fn get_endorsements(&self, target: &WalletAddress) -> Result<Vec<Vec<u8>>, StoreError>;

    /// Store verification vote data from a specific voter.
    fn put_verification_vote(
        &self,
        target: &WalletAddress,
        voter: &WalletAddress,
        data: &[u8],
    ) -> Result<(), StoreError>;

    /// Get all verification votes for a target wallet.
    fn get_verification_votes(&self, target: &WalletAddress) -> Result<Vec<Vec<u8>>, StoreError>;

    /// Store active challenge data.
    fn put_challenge(&self, target: &WalletAddress, data: &[u8]) -> Result<(), StoreError>;

    /// Get active challenge for a target wallet.
    fn get_challenge(&self, target: &WalletAddress) -> Result<Option<Vec<u8>>, StoreError>;
}
