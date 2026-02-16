//! Sync protocol — bootstrap and catch up with the network.

use crate::NetworkError;
use burst_types::WalletAddress;

/// Sync protocol for bootstrapping and catching up.
pub struct SyncProtocol;

impl SyncProtocol {
    /// Bootstrap from a peer — pull all account frontiers and missing blocks.
    pub async fn bootstrap(&self, _peer: &str) -> Result<(), NetworkError> {
        todo!("request frontiers, compare with local, pull missing blocks")
    }

    /// Sync a specific account's chain from a peer.
    pub async fn sync_account(
        &self,
        _peer: &str,
        _account: &WalletAddress,
    ) -> Result<(), NetworkError> {
        todo!("request blocks for account, validate and append")
    }

    /// Lazy bootstrap — pull blocks on demand as they're referenced.
    pub async fn lazy_pull(&self, _block_hash: &burst_types::BlockHash) -> Result<(), NetworkError> {
        todo!("request specific block from peers")
    }
}
