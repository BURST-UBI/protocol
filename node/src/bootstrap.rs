//! Bootstrap protocol — syncs historical blocks from peers.
//!
//! When a new node joins the network, it has no ledger data. The bootstrap
//! protocol enables it to:
//! 1. Request the set of account frontiers from a peer
//! 2. Identify accounts that are missing or behind locally
//! 3. Pull entire account chains (bulk pull) to catch up
//! 4. Request individual blocks by hash
//!
//! The protocol uses a request/response pattern over the existing P2P TCP
//! connections with serialized messages.

use burst_ledger::StateBlock;
use burst_types::{BlockHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// Bootstrap protocol messages.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum BootstrapMessage {
    /// Request: "What are your account frontiers?" (paginated)
    FrontierReq {
        /// Start scanning from this account (inclusive). Use a zero-like
        /// address to start from the beginning.
        start_account: WalletAddress,
        /// Maximum number of frontier entries to return.
        max_count: u32,
    },
    /// Response: list of (account, head_hash) pairs
    FrontierResp {
        /// The frontier entries for the requested page.
        frontiers: Vec<(WalletAddress, BlockHash)>,
        /// Whether there are more entries beyond this page.
        has_more: bool,
    },
    /// Request: "Give me the blocks for this account chain"
    BulkPullReq {
        /// The account whose chain to pull.
        account: WalletAddress,
        /// Pull up to this hash (BlockHash::ZERO = pull everything).
        end: BlockHash,
    },
    /// Response: sequence of blocks for the requested account
    BulkPullResp {
        /// Serialized StateBlocks (bincode-encoded).
        blocks: Vec<Vec<u8>>,
    },
    /// Request: "Give me a specific block by hash"
    BlockReq {
        /// The hash of the block to retrieve.
        hash: BlockHash,
    },
    /// Response: the requested block (None if not found)
    BlockResp {
        /// Serialized StateBlock (bincode-encoded), or None if not found.
        block: Option<Vec<u8>>,
    },
}

/// Bootstrap client — syncs ledger data from peers.
///
/// The client drives the bootstrap process by:
/// 1. Requesting frontiers from a peer
/// 2. Comparing against local frontiers to identify gaps
/// 3. Issuing bulk pull requests for missing/behind accounts
pub struct BootstrapClient {
    /// Maximum blocks to request per bulk_pull
    max_blocks_per_pull: usize,
    /// Accounts that still need syncing (account, remote_head).
    pending_accounts: Vec<(WalletAddress, BlockHash)>,
}

impl BootstrapClient {
    /// Create a new bootstrap client.
    ///
    /// * `max_blocks_per_pull` — cap on how many blocks to request in a single
    ///   bulk pull response (prevents memory exhaustion).
    pub fn new(max_blocks_per_pull: usize) -> Self {
        Self {
            max_blocks_per_pull,
            pending_accounts: Vec::new(),
        }
    }

    /// Start bootstrap process: request frontiers from peer.
    ///
    /// Returns a `FrontierReq` message to send to the peer. The start account
    /// is set to the minimum possible address to scan from the beginning.
    pub fn start_frontier_scan(&mut self) -> BootstrapMessage {
        self.pending_accounts.clear();
        BootstrapMessage::FrontierReq {
            start_account: WalletAddress::new(
                "brst_0000000000000000000000000000000000000000000000000000000000000000000",
            ),
            max_count: 1000,
        }
    }

    /// Process a frontier response — compare against our local frontiers
    /// and queue accounts that need syncing.
    ///
    /// Returns a list of `BulkPullReq` messages for accounts we're missing
    /// or behind on.
    pub fn process_frontier_resp(
        &mut self,
        resp_frontiers: &[(WalletAddress, BlockHash)],
        resp_has_more: bool,
        local_frontiers: &[(WalletAddress, BlockHash)],
    ) -> Vec<BootstrapMessage> {
        let mut requests = Vec::new();

        // Build a lookup table of local frontiers for fast comparison
        let local_map: std::collections::HashMap<&WalletAddress, &BlockHash> =
            local_frontiers.iter().map(|(a, h)| (a, h)).collect();

        for (remote_account, remote_head) in resp_frontiers {
            match local_map.get(remote_account) {
                Some(local_head) if **local_head == *remote_head => {
                    // Account is up-to-date, skip
                }
                Some(_local_head) => {
                    // Account exists locally but is behind — pull from our head
                    // to the remote head
                    self.pending_accounts
                        .push((remote_account.clone(), *remote_head));
                    requests.push(BootstrapMessage::BulkPullReq {
                        account: remote_account.clone(),
                        end: BlockHash::ZERO,
                    });
                }
                None => {
                    // Account doesn't exist locally — pull the entire chain
                    self.pending_accounts
                        .push((remote_account.clone(), *remote_head));
                    requests.push(BootstrapMessage::BulkPullReq {
                        account: remote_account.clone(),
                        end: BlockHash::ZERO,
                    });
                }
            }
        }

        // If there are more frontiers, request the next page
        if resp_has_more {
            if let Some((last_account, _)) = resp_frontiers.last() {
                requests.push(BootstrapMessage::FrontierReq {
                    start_account: last_account.clone(),
                    max_count: 1000,
                });
            }
        }

        requests
    }

    /// Process a bulk pull response — returns deserialized blocks.
    ///
    /// Blocks that fail to deserialize are silently skipped.
    pub fn process_bulk_pull_resp(&self, blocks: &[Vec<u8>]) -> Vec<StateBlock> {
        blocks
            .iter()
            .filter_map(|bytes| bincode::deserialize::<StateBlock>(bytes).ok())
            .collect()
    }

    /// Mark an account as synced (remove from pending).
    pub fn mark_synced(&mut self, account: &WalletAddress) {
        self.pending_accounts.retain(|(a, _)| a != account);
    }

    /// Number of accounts still pending sync.
    pub fn pending_count(&self) -> usize {
        self.pending_accounts.len()
    }

    /// Whether bootstrap is complete (no pending accounts).
    pub fn is_complete(&self) -> bool {
        self.pending_accounts.is_empty()
    }

    /// Get the list of pending accounts and their remote heads.
    pub fn pending_accounts(&self) -> &[(WalletAddress, BlockHash)] {
        &self.pending_accounts
    }

    /// Maximum blocks per pull request.
    pub fn max_blocks_per_pull(&self) -> usize {
        self.max_blocks_per_pull
    }
}

/// Bootstrap server — responds to bootstrap requests from peers.
///
/// Stateless — each method takes the data it needs as parameters.
pub struct BootstrapServer;

impl BootstrapServer {
    /// Handle a frontier request by reading from our frontier data.
    ///
    /// The `frontiers` slice should be sorted by account address for
    /// deterministic pagination. Returns a `FrontierResp` message.
    pub fn handle_frontier_req(
        start_account: &WalletAddress,
        max_count: u32,
        frontiers: &[(WalletAddress, BlockHash)],
    ) -> BootstrapMessage {
        let max = max_count as usize;

        // Find the starting position via binary search (or linear scan).
        // Frontiers are expected to be sorted by account address.
        let start_idx = frontiers
            .iter()
            .position(|(a, _)| a.as_str() >= start_account.as_str())
            .unwrap_or(frontiers.len());

        let end_idx = (start_idx + max).min(frontiers.len());
        let page: Vec<(WalletAddress, BlockHash)> = frontiers[start_idx..end_idx].to_vec();
        let has_more = end_idx < frontiers.len();

        BootstrapMessage::FrontierResp {
            frontiers: page,
            has_more,
        }
    }

    /// Handle a bulk pull request by reading blocks from our block store.
    ///
    /// * `get_block` — closure that looks up a block by hash and returns its
    ///   serialized bytes.
    /// * `get_chain` — closure that returns the ordered list of block hashes
    ///   for an account (from open block to head).
    pub fn handle_bulk_pull_req(
        account: &WalletAddress,
        end: &BlockHash,
        get_block: impl Fn(&BlockHash) -> Option<Vec<u8>>,
        get_chain: impl Fn(&WalletAddress) -> Vec<BlockHash>,
    ) -> BootstrapMessage {
        let chain = get_chain(account);
        let mut blocks = Vec::new();

        for hash in &chain {
            if let Some(block_bytes) = get_block(hash) {
                blocks.push(block_bytes);
            }
            // Stop if we've reached the requested end hash
            if !end.is_zero() && *hash == *end {
                break;
            }
        }

        BootstrapMessage::BulkPullResp { blocks }
    }

    /// Handle a single block request.
    ///
    /// * `get_block` — closure that looks up a block by hash and returns its
    ///   serialized bytes.
    pub fn handle_block_req(
        hash: &BlockHash,
        get_block: impl Fn(&BlockHash) -> Option<Vec<u8>>,
    ) -> BootstrapMessage {
        BootstrapMessage::BlockResp {
            block: get_block(hash),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_ledger::{BlockType, CURRENT_BLOCK_VERSION};
    use burst_types::{Signature, Timestamp, TxHash};

    fn test_account_1() -> WalletAddress {
        WalletAddress::new(
            "brst_1111111111111111111111111111111111111111111111111111111111111111111",
        )
    }

    fn test_account_2() -> WalletAddress {
        WalletAddress::new(
            "brst_2222222222222222222222222222222222222222222222222222222222222222222",
        )
    }

    fn test_account_3() -> WalletAddress {
        WalletAddress::new(
            "brst_3333333333333333333333333333333333333333333333333333333333333333333",
        )
    }

    fn make_test_block(account: &WalletAddress, previous: BlockHash) -> StateBlock {
        let mut block = StateBlock {
            version: CURRENT_BLOCK_VERSION,
            block_type: if previous.is_zero() {
                BlockType::Open
            } else {
                BlockType::Send
            },
            account: account.clone(),
            previous,
            representative: account.clone(),
            brn_balance: 100,
            trst_balance: 50,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1000),
            params_hash: BlockHash::ZERO,
            work: 0,
            signature: Signature([1u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block
    }

    fn serialize_block(block: &StateBlock) -> Vec<u8> {
        bincode::serialize(block).unwrap()
    }

    // ── BootstrapClient tests ──────────────────────────────────────────

    #[test]
    fn client_start_frontier_scan_returns_req() {
        let mut client = BootstrapClient::new(1000);
        let msg = client.start_frontier_scan();

        match msg {
            BootstrapMessage::FrontierReq {
                start_account,
                max_count,
            } => {
                assert_eq!(max_count, 1000);
                assert!(start_account.as_str().starts_with("brst_"));
            }
            _ => panic!("expected FrontierReq"),
        }
    }

    #[test]
    fn client_identifies_missing_accounts() {
        let mut client = BootstrapClient::new(1000);

        let remote_head = BlockHash::new([0xAA; 32]);
        let remote_frontiers = vec![(test_account_1(), remote_head)];
        let local_frontiers: Vec<(WalletAddress, BlockHash)> = vec![];

        let requests = client.process_frontier_resp(&remote_frontiers, false, &local_frontiers);

        // Should request a bulk pull for the missing account
        assert_eq!(requests.len(), 1);
        match &requests[0] {
            BootstrapMessage::BulkPullReq { account, end } => {
                assert_eq!(account, &test_account_1());
                assert!(end.is_zero());
            }
            _ => panic!("expected BulkPullReq"),
        }

        assert_eq!(client.pending_count(), 1);
        assert!(!client.is_complete());
    }

    #[test]
    fn client_identifies_behind_accounts() {
        let mut client = BootstrapClient::new(1000);

        let local_head = BlockHash::new([0x11; 32]);
        let remote_head = BlockHash::new([0x22; 32]);

        let remote_frontiers = vec![(test_account_1(), remote_head)];
        let local_frontiers = vec![(test_account_1(), local_head)];

        let requests = client.process_frontier_resp(&remote_frontiers, false, &local_frontiers);

        assert_eq!(requests.len(), 1);
        assert_eq!(client.pending_count(), 1);
    }

    #[test]
    fn client_skips_up_to_date_accounts() {
        let mut client = BootstrapClient::new(1000);

        let head = BlockHash::new([0xAA; 32]);
        let remote_frontiers = vec![(test_account_1(), head)];
        let local_frontiers = vec![(test_account_1(), head)];

        let requests = client.process_frontier_resp(&remote_frontiers, false, &local_frontiers);

        assert!(requests.is_empty());
        assert!(client.is_complete());
    }

    #[test]
    fn client_requests_next_page_when_has_more() {
        let mut client = BootstrapClient::new(1000);

        let head = BlockHash::new([0xAA; 32]);
        let remote_frontiers = vec![(test_account_1(), head)];
        let local_frontiers: Vec<(WalletAddress, BlockHash)> = vec![];

        let requests = client.process_frontier_resp(&remote_frontiers, true, &local_frontiers);

        // Should have a bulk pull + a frontier req for the next page
        assert_eq!(requests.len(), 2);
        assert!(matches!(&requests[1], BootstrapMessage::FrontierReq { .. }));
    }

    #[test]
    fn client_process_bulk_pull_resp_deserializes_blocks() {
        let client = BootstrapClient::new(1000);

        let block1 = make_test_block(&test_account_1(), BlockHash::ZERO);
        let block2 = make_test_block(&test_account_1(), block1.hash);

        let blocks_bytes = vec![serialize_block(&block1), serialize_block(&block2)];
        let deserialized = client.process_bulk_pull_resp(&blocks_bytes);

        assert_eq!(deserialized.len(), 2);
        assert_eq!(deserialized[0].hash, block1.hash);
        assert_eq!(deserialized[1].hash, block2.hash);
    }

    #[test]
    fn client_process_bulk_pull_resp_skips_invalid() {
        let client = BootstrapClient::new(1000);

        let block = make_test_block(&test_account_1(), BlockHash::ZERO);
        let blocks_bytes = vec![
            serialize_block(&block),
            vec![0xFF, 0xFF, 0xFF], // invalid bytes
        ];

        let deserialized = client.process_bulk_pull_resp(&blocks_bytes);
        assert_eq!(deserialized.len(), 1);
    }

    #[test]
    fn client_mark_synced_removes_from_pending() {
        let mut client = BootstrapClient::new(1000);

        let remote_frontiers = vec![
            (test_account_1(), BlockHash::new([0xAA; 32])),
            (test_account_2(), BlockHash::new([0xBB; 32])),
        ];
        let local_frontiers: Vec<(WalletAddress, BlockHash)> = vec![];

        client.process_frontier_resp(&remote_frontiers, false, &local_frontiers);
        assert_eq!(client.pending_count(), 2);

        client.mark_synced(&test_account_1());
        assert_eq!(client.pending_count(), 1);

        client.mark_synced(&test_account_2());
        assert!(client.is_complete());
    }

    // ── BootstrapServer tests ──────────────────────────────────────────

    #[test]
    fn server_frontier_req_returns_page() {
        let frontiers = vec![
            (test_account_1(), BlockHash::new([0x11; 32])),
            (test_account_2(), BlockHash::new([0x22; 32])),
            (test_account_3(), BlockHash::new([0x33; 32])),
        ];

        let start = WalletAddress::new(
            "brst_0000000000000000000000000000000000000000000000000000000000000000000",
        );
        let resp = BootstrapServer::handle_frontier_req(&start, 2, &frontiers);

        match resp {
            BootstrapMessage::FrontierResp {
                frontiers: page,
                has_more,
            } => {
                assert_eq!(page.len(), 2);
                assert!(has_more);
                assert_eq!(page[0].0, test_account_1());
                assert_eq!(page[1].0, test_account_2());
            }
            _ => panic!("expected FrontierResp"),
        }
    }

    #[test]
    fn server_frontier_req_last_page() {
        let frontiers = vec![
            (test_account_1(), BlockHash::new([0x11; 32])),
            (test_account_2(), BlockHash::new([0x22; 32])),
        ];

        let start = WalletAddress::new(
            "brst_0000000000000000000000000000000000000000000000000000000000000000000",
        );
        let resp = BootstrapServer::handle_frontier_req(&start, 10, &frontiers);

        match resp {
            BootstrapMessage::FrontierResp {
                frontiers: page,
                has_more,
            } => {
                assert_eq!(page.len(), 2);
                assert!(!has_more);
            }
            _ => panic!("expected FrontierResp"),
        }
    }

    #[test]
    fn server_frontier_req_pagination() {
        let frontiers = vec![
            (test_account_1(), BlockHash::new([0x11; 32])),
            (test_account_2(), BlockHash::new([0x22; 32])),
            (test_account_3(), BlockHash::new([0x33; 32])),
        ];

        // Request starting from account_2
        let resp = BootstrapServer::handle_frontier_req(&test_account_2(), 10, &frontiers);

        match resp {
            BootstrapMessage::FrontierResp {
                frontiers: page,
                has_more,
            } => {
                assert_eq!(page.len(), 2);
                assert!(!has_more);
                assert_eq!(page[0].0, test_account_2());
                assert_eq!(page[1].0, test_account_3());
            }
            _ => panic!("expected FrontierResp"),
        }
    }

    #[test]
    fn server_bulk_pull_returns_chain_blocks() {
        let block1 = make_test_block(&test_account_1(), BlockHash::ZERO);
        let block2 = make_test_block(&test_account_1(), block1.hash);

        let blocks: std::collections::HashMap<BlockHash, Vec<u8>> = vec![
            (block1.hash, serialize_block(&block1)),
            (block2.hash, serialize_block(&block2)),
        ]
        .into_iter()
        .collect();

        let chain = vec![block1.hash, block2.hash];

        let resp = BootstrapServer::handle_bulk_pull_req(
            &test_account_1(),
            &BlockHash::ZERO,
            |hash| blocks.get(hash).cloned(),
            |_account| chain.clone(),
        );

        match resp {
            BootstrapMessage::BulkPullResp {
                blocks: resp_blocks,
            } => {
                assert_eq!(resp_blocks.len(), 2);
            }
            _ => panic!("expected BulkPullResp"),
        }
    }

    #[test]
    fn server_bulk_pull_stops_at_end_hash() {
        let block1 = make_test_block(&test_account_1(), BlockHash::ZERO);
        let block2 = make_test_block(&test_account_1(), block1.hash);
        let block3 = make_test_block(&test_account_1(), block2.hash);

        let blocks: std::collections::HashMap<BlockHash, Vec<u8>> = vec![
            (block1.hash, serialize_block(&block1)),
            (block2.hash, serialize_block(&block2)),
            (block3.hash, serialize_block(&block3)),
        ]
        .into_iter()
        .collect();

        let chain = vec![block1.hash, block2.hash, block3.hash];

        // Request blocks up to block2 only
        let resp = BootstrapServer::handle_bulk_pull_req(
            &test_account_1(),
            &block2.hash,
            |hash| blocks.get(hash).cloned(),
            |_account| chain.clone(),
        );

        match resp {
            BootstrapMessage::BulkPullResp {
                blocks: resp_blocks,
            } => {
                assert_eq!(resp_blocks.len(), 2);
            }
            _ => panic!("expected BulkPullResp"),
        }
    }

    #[test]
    fn server_block_req_found() {
        let block = make_test_block(&test_account_1(), BlockHash::ZERO);
        let block_bytes = serialize_block(&block);

        let resp = BootstrapServer::handle_block_req(&block.hash, |hash| {
            if *hash == block.hash {
                Some(block_bytes.clone())
            } else {
                None
            }
        });

        match resp {
            BootstrapMessage::BlockResp { block: Some(bytes) } => {
                let deserialized: StateBlock = bincode::deserialize(&bytes).unwrap();
                assert_eq!(deserialized.hash, block.hash);
            }
            _ => panic!("expected BlockResp with Some"),
        }
    }

    #[test]
    fn server_block_req_not_found() {
        let unknown_hash = BlockHash::new([0xFF; 32]);

        let resp = BootstrapServer::handle_block_req(&unknown_hash, |_| None);

        match resp {
            BootstrapMessage::BlockResp { block: None } => {}
            _ => panic!("expected BlockResp with None"),
        }
    }

    #[test]
    fn server_bulk_pull_empty_chain() {
        let resp = BootstrapServer::handle_bulk_pull_req(
            &test_account_1(),
            &BlockHash::ZERO,
            |_| None,
            |_| Vec::new(),
        );

        match resp {
            BootstrapMessage::BulkPullResp { blocks } => {
                assert!(blocks.is_empty());
            }
            _ => panic!("expected BulkPullResp"),
        }
    }

    #[test]
    fn bootstrap_message_serialization_roundtrip() {
        let msg = BootstrapMessage::FrontierReq {
            start_account: test_account_1(),
            max_count: 500,
        };

        let bytes = bincode::serialize(&msg).unwrap();
        let decoded: BootstrapMessage = bincode::deserialize(&bytes).unwrap();

        match decoded {
            BootstrapMessage::FrontierReq {
                start_account,
                max_count,
            } => {
                assert_eq!(start_account, test_account_1());
                assert_eq!(max_count, 500);
            }
            _ => panic!("roundtrip failed"),
        }
    }

    #[test]
    fn full_bootstrap_flow() {
        // Simulate a complete bootstrap flow between client and server
        let mut client = BootstrapClient::new(1000);

        // Server has two accounts
        let block_a1 = make_test_block(&test_account_1(), BlockHash::ZERO);
        let block_a2 = make_test_block(&test_account_1(), block_a1.hash);
        let block_b1 = make_test_block(&test_account_2(), BlockHash::ZERO);

        let server_frontiers = vec![
            (test_account_1(), block_a2.hash),
            (test_account_2(), block_b1.hash),
        ];

        // Step 1: Client starts frontier scan
        let req = client.start_frontier_scan();
        assert!(matches!(req, BootstrapMessage::FrontierReq { .. }));

        // Step 2: Server responds with frontiers
        let resp = BootstrapServer::handle_frontier_req(
            &WalletAddress::new(
                "brst_0000000000000000000000000000000000000000000000000000000000000000000",
            ),
            1000,
            &server_frontiers,
        );

        // Step 3: Client processes frontier response (client has nothing local)
        let local_frontiers: Vec<(WalletAddress, BlockHash)> = vec![];
        match &resp {
            BootstrapMessage::FrontierResp {
                frontiers,
                has_more,
            } => {
                let pull_requests =
                    client.process_frontier_resp(frontiers, *has_more, &local_frontiers);
                assert_eq!(pull_requests.len(), 2); // two accounts to sync
                assert_eq!(client.pending_count(), 2);
            }
            _ => panic!("expected FrontierResp"),
        }

        // Step 4: Client processes bulk pull responses
        let blocks_a = vec![serialize_block(&block_a1), serialize_block(&block_a2)];
        let deserialized = client.process_bulk_pull_resp(&blocks_a);
        assert_eq!(deserialized.len(), 2);
        client.mark_synced(&test_account_1());

        let blocks_b = vec![serialize_block(&block_b1)];
        let deserialized = client.process_bulk_pull_resp(&blocks_b);
        assert_eq!(deserialized.len(), 1);
        client.mark_synced(&test_account_2());

        // Step 5: Bootstrap is complete
        assert!(client.is_complete());
    }
}
