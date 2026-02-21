//! Sync protocol — bootstrap, account sync, and lazy pull.
//!
//! Provides three sync modes for catching up with the network:
//! - **Bootstrap**: full frontier download followed by bulk-pull of missing blocks
//! - **SyncAccount**: sync a single account's chain from a peer
//! - **LazyPull**: request a specific block on demand

use crate::NetworkError;
use burst_types::{BlockHash, WalletAddress};
use tokio::sync::mpsc;

/// Channel buffer size for sync request/response channels.
const CHANNEL_BUFFER: usize = 64;

/// Request types for the sync protocol.
#[derive(Debug)]
pub enum SyncRequest {
    /// Full bootstrap: download all frontiers then pull missing blocks.
    Bootstrap { peer: String },
    /// Sync a specific account chain.
    SyncAccount {
        peer: String,
        account: WalletAddress,
    },
    /// Lazy pull: request a specific block on demand.
    LazyPull { peer: String, block_hash: BlockHash },
}

/// Response from sync operations.
#[derive(Debug)]
pub enum SyncResponse {
    /// List of (account, frontier_hash) pairs from a peer.
    Frontiers(Vec<(WalletAddress, BlockHash)>),
    /// Multiple serialized blocks (for bootstrap / account sync).
    Blocks(Vec<Vec<u8>>),
    /// A single serialized block (for lazy pull).
    Block(Vec<u8>),
    /// An error message from the remote peer or transport layer.
    Error(String),
}

/// Result of a full bootstrap operation.
#[derive(Debug, Clone)]
pub struct BootstrapResult {
    /// Number of frontier entries received from the peer.
    pub frontiers_received: usize,
    /// Number of blocks downloaded during the bulk-pull phase.
    pub blocks_downloaded: usize,
}

/// Result of syncing a single account's chain.
#[derive(Debug, Clone)]
pub struct SyncAccountResult {
    /// Number of blocks received for the account.
    pub blocks_received: usize,
}

/// Sync protocol client — used by application code to initiate sync operations.
///
/// Communicates with the network layer via mpsc channels. The companion
/// [`SyncHandle`] is given to the network layer so it can process incoming
/// sync requests and send back responses.
pub struct SyncProtocol {
    /// Channel to send sync requests to the network layer.
    request_tx: mpsc::Sender<SyncRequest>,
    /// Channel to receive sync responses.
    response_rx: mpsc::Receiver<SyncResponse>,
}

/// Handle given to the network layer to process sync messages.
///
/// The network layer reads requests from `request_rx`, performs the
/// network I/O, and writes responses to `response_tx`.
pub struct SyncHandle {
    /// Incoming sync requests from the protocol client.
    pub request_rx: mpsc::Receiver<SyncRequest>,
    /// Outgoing sync responses back to the protocol client.
    pub response_tx: mpsc::Sender<SyncResponse>,
}

impl SyncProtocol {
    /// Create a new `SyncProtocol` and its companion [`SyncHandle`].
    ///
    /// The protocol sends requests and receives responses; the handle
    /// receives requests and sends responses.
    pub fn new() -> (Self, SyncHandle) {
        let (request_tx, request_rx) = mpsc::channel(CHANNEL_BUFFER);
        let (response_tx, response_rx) = mpsc::channel(CHANNEL_BUFFER);

        let protocol = Self {
            request_tx,
            response_rx,
        };

        let handle = SyncHandle {
            request_rx,
            response_tx,
        };

        (protocol, handle)
    }

    /// Bootstrap from a peer — pull all account frontiers and then bulk-pull missing blocks.
    ///
    /// Steps:
    /// 1. Send `Bootstrap` request to the network layer
    /// 2. Receive `Frontiers` response with all account heads
    /// 3. Receive `Blocks` response with the missing blocks
    pub async fn bootstrap(&mut self, peer: &str) -> Result<BootstrapResult, NetworkError> {
        // 1. Send the bootstrap request
        self.request_tx
            .send(SyncRequest::Bootstrap {
                peer: peer.to_string(),
            })
            .await
            .map_err(|_| NetworkError::ChannelClosed)?;

        // 2. Receive the frontiers response
        let frontiers = match self.response_rx.recv().await {
            Some(SyncResponse::Frontiers(f)) => f,
            Some(SyncResponse::Error(e)) => return Err(NetworkError::SyncFailed(e)),
            Some(_) => {
                return Err(NetworkError::SyncFailed(
                    "unexpected response: expected Frontiers".into(),
                ))
            }
            None => return Err(NetworkError::ChannelClosed),
        };
        let frontiers_received = frontiers.len();

        // 3. Receive the blocks that fill in the gaps
        let blocks = match self.response_rx.recv().await {
            Some(SyncResponse::Blocks(b)) => b,
            Some(SyncResponse::Error(e)) => return Err(NetworkError::SyncFailed(e)),
            Some(_) => {
                return Err(NetworkError::SyncFailed(
                    "unexpected response: expected Blocks".into(),
                ))
            }
            None => return Err(NetworkError::ChannelClosed),
        };

        Ok(BootstrapResult {
            frontiers_received,
            blocks_downloaded: blocks.len(),
        })
    }

    /// Sync a specific account's chain from a peer.
    ///
    /// Sends a `SyncAccount` request and waits for a `Blocks` response
    /// containing the account's blocks in chain order.
    pub async fn sync_account(
        &mut self,
        peer: &str,
        account: &WalletAddress,
    ) -> Result<SyncAccountResult, NetworkError> {
        self.request_tx
            .send(SyncRequest::SyncAccount {
                peer: peer.to_string(),
                account: account.clone(),
            })
            .await
            .map_err(|_| NetworkError::ChannelClosed)?;

        let blocks = match self.response_rx.recv().await {
            Some(SyncResponse::Blocks(b)) => b,
            Some(SyncResponse::Error(e)) => return Err(NetworkError::SyncFailed(e)),
            Some(_) => {
                return Err(NetworkError::SyncFailed(
                    "unexpected response: expected Blocks".into(),
                ))
            }
            None => return Err(NetworkError::ChannelClosed),
        };

        Ok(SyncAccountResult {
            blocks_received: blocks.len(),
        })
    }

    /// Lazy pull — request a specific block that we're missing.
    ///
    /// Sends a `LazyPull` request and waits for a single `Block` response
    /// containing the serialized block bytes.
    pub async fn lazy_pull(
        &mut self,
        peer: &str,
        block_hash: &BlockHash,
    ) -> Result<Vec<u8>, NetworkError> {
        self.request_tx
            .send(SyncRequest::LazyPull {
                peer: peer.to_string(),
                block_hash: *block_hash,
            })
            .await
            .map_err(|_| NetworkError::ChannelClosed)?;

        match self.response_rx.recv().await {
            Some(SyncResponse::Block(data)) => Ok(data),
            Some(SyncResponse::Error(e)) => Err(NetworkError::SyncFailed(e)),
            Some(_) => Err(NetworkError::SyncFailed(
                "unexpected response: expected Block".into(),
            )),
            None => Err(NetworkError::ChannelClosed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_bootstrap_roundtrip() {
        let (mut protocol, mut handle) = SyncProtocol::new();

        let peer = "192.168.1.1:7075";
        let account = WalletAddress::new("brst_test_account_1");
        let frontier = BlockHash::new([1u8; 32]);

        // Spawn a "network layer" that responds to the bootstrap request
        let network = tokio::spawn(async move {
            let req = handle.request_rx.recv().await.unwrap();
            match req {
                SyncRequest::Bootstrap { peer: p } => {
                    assert_eq!(p, "192.168.1.1:7075");
                }
                _ => panic!("expected Bootstrap request"),
            }

            // Send frontier response
            handle
                .response_tx
                .send(SyncResponse::Frontiers(vec![(account, frontier)]))
                .await
                .unwrap();

            // Send blocks response
            handle
                .response_tx
                .send(SyncResponse::Blocks(vec![vec![1, 2, 3], vec![4, 5, 6]]))
                .await
                .unwrap();
        });

        let result = protocol.bootstrap(peer).await.unwrap();
        assert_eq!(result.frontiers_received, 1);
        assert_eq!(result.blocks_downloaded, 2);

        network.await.unwrap();
    }

    #[tokio::test]
    async fn test_sync_account_roundtrip() {
        let (mut protocol, mut handle) = SyncProtocol::new();

        let peer = "192.168.1.1:7075";
        let account = WalletAddress::new("brst_sync_target");

        let network = tokio::spawn(async move {
            let req = handle.request_rx.recv().await.unwrap();
            match req {
                SyncRequest::SyncAccount {
                    peer: p,
                    account: a,
                } => {
                    assert_eq!(p, "192.168.1.1:7075");
                    assert_eq!(a.as_str(), "brst_sync_target");
                }
                _ => panic!("expected SyncAccount request"),
            }

            handle
                .response_tx
                .send(SyncResponse::Blocks(vec![
                    vec![10, 20],
                    vec![30, 40],
                    vec![50, 60],
                ]))
                .await
                .unwrap();
        });

        let result = protocol.sync_account(peer, &account).await.unwrap();
        assert_eq!(result.blocks_received, 3);

        network.await.unwrap();
    }

    #[tokio::test]
    async fn test_lazy_pull_roundtrip() {
        let (mut protocol, mut handle) = SyncProtocol::new();

        let peer = "192.168.1.1:7075";
        let hash = BlockHash::new([42u8; 32]);
        let expected_data = vec![0xDE, 0xAD, 0xBE, 0xEF];

        let network = tokio::spawn({
            let expected_hash = hash;
            let data = expected_data.clone();
            async move {
                let req = handle.request_rx.recv().await.unwrap();
                match req {
                    SyncRequest::LazyPull {
                        peer: p,
                        block_hash: h,
                    } => {
                        assert_eq!(p, "192.168.1.1:7075");
                        assert_eq!(h, expected_hash);
                    }
                    _ => panic!("expected LazyPull request"),
                }

                handle
                    .response_tx
                    .send(SyncResponse::Block(data))
                    .await
                    .unwrap();
            }
        });

        let block = protocol.lazy_pull(peer, &hash).await.unwrap();
        assert_eq!(block, expected_data);

        network.await.unwrap();
    }

    #[tokio::test]
    async fn test_bootstrap_error_response() {
        let (mut protocol, mut handle) = SyncProtocol::new();

        let network = tokio::spawn(async move {
            let _req = handle.request_rx.recv().await.unwrap();
            handle
                .response_tx
                .send(SyncResponse::Error("peer unavailable".into()))
                .await
                .unwrap();
        });

        let result = protocol.bootstrap("bad_peer").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            NetworkError::SyncFailed(msg) => assert_eq!(msg, "peer unavailable"),
            other => panic!("expected SyncFailed, got {:?}", other),
        }

        network.await.unwrap();
    }

    #[tokio::test]
    async fn test_channel_closed_returns_error() {
        let (mut protocol, handle) = SyncProtocol::new();

        // Drop the handle so the channel closes
        drop(handle);

        let result = protocol.bootstrap("any_peer").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            NetworkError::ChannelClosed => {}
            other => panic!("expected ChannelClosed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_sync_account_error_response() {
        let (mut protocol, mut handle) = SyncProtocol::new();

        let account = WalletAddress::new("brst_some_account");

        let network = tokio::spawn(async move {
            let _req = handle.request_rx.recv().await.unwrap();
            handle
                .response_tx
                .send(SyncResponse::Error("account not found".into()))
                .await
                .unwrap();
        });

        let result = protocol.sync_account("peer", &account).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            NetworkError::SyncFailed(msg) => assert_eq!(msg, "account not found"),
            other => panic!("expected SyncFailed, got {:?}", other),
        }

        network.await.unwrap();
    }

    #[tokio::test]
    async fn test_lazy_pull_error_response() {
        let (mut protocol, mut handle) = SyncProtocol::new();

        let hash = BlockHash::new([99u8; 32]);

        let network = tokio::spawn(async move {
            let _req = handle.request_rx.recv().await.unwrap();
            handle
                .response_tx
                .send(SyncResponse::Error("block not found".into()))
                .await
                .unwrap();
        });

        let result = protocol.lazy_pull("peer", &hash).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            NetworkError::SyncFailed(msg) => assert_eq!(msg, "block not found"),
            other => panic!("expected SyncFailed, got {:?}", other),
        }

        network.await.unwrap();
    }
}
