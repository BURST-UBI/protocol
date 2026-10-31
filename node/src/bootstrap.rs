//! Bootstrap protocol — ascending sync of historical blocks from peers.
//!
//! [`Bootstrapper`] discovers accounts through frontier pages and requests
//! ascending block batches over existing peer connections. Responses are matched
//! to their requests and checked for chain continuity before admission. Cursors
//! advance from locally persisted state, not merely from received bytes.
//! [`BootstrapServer`] can serve only the history available to its data providers.
//!
//! The protocol uses a request/response pattern over the existing P2P TCP
//! connections with serialized messages.

use std::collections::{HashMap, HashSet, VecDeque};

use burst_types::{BlockHash, WalletAddress};

pub(crate) fn stored_blocks_after(
    store: &burst_store_lmdb::LmdbStore,
    account: &WalletAddress,
    start: &BlockHash,
    count: u16,
) -> Result<Vec<Vec<u8>>, burst_store::StoreError> {
    let txn = store.tx_begin_read()?;
    let blocks = store.block_store();
    let height = if start.is_zero() {
        0
    } else {
        let Some(height) = blocks.height_of_block_txn(&txn, start)? else {
            return Ok(Vec::new());
        };
        if blocks.block_at_height_txn(&txn, account, height)? != Some(*start) {
            return Ok(Vec::new());
        }
        height
    };
    let mut result = Vec::new();
    for offset in 1..=u64::from(count.min(ASC_PULL_MAX_BLOCKS)) {
        let next = height.checked_add(offset).ok_or_else(|| {
            burst_store::StoreError::Corruption("bootstrap height overflow".into())
        })?;
        let Some(hash) = blocks.block_at_height_txn(&txn, account, next)? else {
            break;
        };
        result.push(blocks.get_block_txn(&txn, &hash)?);
    }
    Ok(result)
}

/// Bootstrap protocol messages — the ascending-bootstrap request/response pair.
pub use burst_messages::bootstrap::{AscPullAckPayload, AscPullReqPayload, BootstrapMessage};

/// Bootstrap server — responds to bootstrap requests from peers.
///
/// Stateless — each method takes the data it needs as parameters.
pub struct BootstrapServer;

impl BootstrapServer {
    /// Serve an ascending-bootstrap pull request, producing the matching
    /// [`BootstrapMessage::AscPullAck`] (echoing `id`). Ledger access is
    /// supplied through these callbacks:
    /// - `blocks_after(account, start, count)` → up to `count` serialized
    ///   blocks for `account`, contiguous & ascending, beginning at the
    ///   successor of `start` (or the open block if `start` is ZERO).
    /// - `account_info(account)` → `(head, block_count)` (`ZERO`/`0` if unknown).
    /// - `frontiers_from(start_account, count)` → a page of `(account, head)`.
    pub fn handle_asc_pull_req<FB, FA, FF>(
        id: u64,
        payload: &AscPullReqPayload,
        blocks_after: FB,
        account_info: FA,
        frontiers_from: FF,
    ) -> BootstrapMessage
    where
        FB: FnOnce(&WalletAddress, &BlockHash, u16) -> Vec<Vec<u8>>,
        FA: FnOnce(&WalletAddress) -> (BlockHash, u64),
        FF: FnOnce(&WalletAddress, u16) -> Vec<(WalletAddress, BlockHash)>,
    {
        let payload = match payload {
            AscPullReqPayload::Blocks {
                account,
                start,
                count,
            } => {
                let count = (*count).min(ASC_PULL_MAX_BLOCKS);
                // Reserve the Protobuf envelope, nested message lengths and
                // maximum-width request ID. Each block reserves its tag/length.
                let mut remaining = burst_types::MAX_P2P_MESSAGE_BYTES - 32;
                let blocks = blocks_after(account, start, count)
                    .into_iter()
                    .take(count as usize)
                    .take_while(|bytes| {
                        let Some(size) = bytes.len().checked_add(8) else {
                            return false;
                        };
                        if size > remaining {
                            return false;
                        }
                        remaining -= size;
                        true
                    })
                    .collect();
                AscPullAckPayload::Blocks(blocks)
            }
            AscPullReqPayload::AccountInfo { account } => {
                let (head, block_count) = account_info(account);
                AscPullAckPayload::AccountInfo {
                    account: account.clone(),
                    head,
                    block_count,
                }
            }
            AscPullReqPayload::Frontiers {
                start_account,
                count,
            } => {
                let count = (*count).min(ASC_PULL_MAX_FRONTIERS);
                AscPullAckPayload::Frontiers(frontiers_from(start_account, count))
            }
        };
        BootstrapMessage::AscPullAck { id, payload }
    }
}

/// Max blocks served in one ascending-bootstrap Blocks response.
pub const ASC_PULL_MAX_BLOCKS: u16 = 128;

/// Max frontier entries served in one ascending-bootstrap Frontiers response.
pub const ASC_PULL_MAX_FRONTIERS: u16 = 512;

/// A peer may not rewind a scan, return an unordered page, or exceed its limit.
pub fn valid_frontier_page(start: &WalletAddress, entries: &[(WalletAddress, BlockHash)]) -> bool {
    entries.len() <= ASC_PULL_MAX_FRONTIERS as usize
        && entries
            .first()
            .is_none_or(|(account, _)| account.as_str() >= start.as_str())
        && entries
            .windows(2)
            .all(|pair| pair[0].0.as_str() < pair[1].0.as_str())
}

/// Feedback from the network read loop to the bootstrap task about received
/// ascending-pull acks. Decouples the [`Bootstrapper`] from the read loop: the
/// read loop only holds an `mpsc::Sender<BootstrapFeedback>`, and the bootstrap
/// task owns the `Bootstrapper` and applies these.
#[derive(Clone, Debug)]
pub enum BootstrapFeedback {
    /// Untrusted response; the coordinator must correlate and validate it
    /// before any blocks enter the processing queue.
    Blocks {
        peer_id: String,
        id: u64,
        blocks: Vec<Vec<u8>>,
    },
    /// Frontiers discovered from a peer: `(account, remote_head)` pairs, used to
    /// find accounts we're behind on and enqueue them.
    Frontiers {
        peer_id: String,
        id: u64,
        entries: Vec<(WalletAddress, BlockHash)>,
    },
}

struct RunningQuery {
    account: WalletAddress,
    start: BlockHash,
    sent_at: u64,
    peer: Option<String>,
    /// Receiving data is not acceptance. Observe locally persisted progress.
    received: Option<(BlockHash, bool)>,
}

/// Client-side ascending bootstrapper: drives catch-up by pulling blocks for
/// accounts we're behind on, from peers in parallel, with id-correlated
/// requests and per-query timeouts.
///
/// Keep a round-robin queue of accounts and issue at most `max_in_flight`
/// concurrent block requests. Refresh frontiers from persisted local blocks;
/// requeue unfinished accounts and timed-out requests.
pub struct Bootstrapper {
    /// Accounts waiting to be pulled in round-robin order.
    queue: VecDeque<WalletAddress>,
    /// Membership mirror of `queue` to avoid duplicate enqueues.
    queued: HashSet<WalletAddress>,
    /// Our current frontier per account — where the next pull starts.
    frontier: HashMap<WalletAddress, BlockHash>,
    /// In-flight queries keyed by request ID.
    in_flight: HashMap<u64, RunningQuery>,
    next_id: u64,
    max_in_flight: usize,
    query_timeout_secs: u64,
    blocks_per_pull: u16,
}

impl Bootstrapper {
    pub fn new(max_in_flight: usize, query_timeout_secs: u64, blocks_per_pull: u16) -> Self {
        Self {
            queue: VecDeque::new(),
            queued: HashSet::new(),
            frontier: HashMap::new(),
            in_flight: HashMap::new(),
            next_id: 1,
            max_in_flight,
            query_timeout_secs,
            blocks_per_pull: blocks_per_pull.clamp(1, ASC_PULL_MAX_BLOCKS),
        }
    }

    /// Queue an account for syncing, starting from `our_frontier` (the last
    /// block we have for it; `ZERO` if none). No-op if already queued or
    /// in-flight (but the start frontier is always refreshed).
    pub fn enqueue(&mut self, account: WalletAddress, our_frontier: BlockHash) {
        self.frontier.insert(account.clone(), our_frontier);
        let in_flight = self.in_flight.values().any(|q| q.account == account);
        if !self.queued.contains(&account) && !in_flight {
            self.queued.insert(account.clone());
            self.queue.push_back(account);
        }
    }

    /// Produce the next batch of pull requests, up to the in-flight budget.
    /// Each `(id, payload)` is sent to a peer; the `id` correlates the ack via
    /// [`on_blocks_ack`](Self::on_blocks_ack).
    pub fn next_requests(&mut self, now_secs: u64) -> Vec<(u64, AscPullReqPayload)> {
        let mut out = Vec::new();
        while self.in_flight.len() < self.max_in_flight {
            let Some(account) = self.queue.pop_front() else {
                break;
            };
            self.queued.remove(&account);
            let start = self
                .frontier
                .get(&account)
                .copied()
                .unwrap_or(BlockHash::ZERO);
            let id = self.next_id;
            self.next_id += 1;
            self.in_flight.insert(
                id,
                RunningQuery {
                    account: account.clone(),
                    start,
                    sent_at: now_secs,
                    peer: None,
                    received: None,
                },
            );
            out.push((
                id,
                AscPullReqPayload::Blocks {
                    account,
                    start,
                    count: self.blocks_per_pull,
                },
            ));
        }
        out
    }

    /// Handle a blocks ack for query `id`. `new_frontier` = hash of the last
    /// block we accepted from the run (None if none/invalid). `full_batch` =
    /// the response was a full `blocks_per_pull` (⇒ likely more to pull).
    pub fn on_blocks_ack(&mut self, id: u64, new_frontier: Option<BlockHash>, full_batch: bool) {
        let Some(query) = self.in_flight.remove(&id) else {
            return;
        };
        let account = query.account;
        if let Some(head) = new_frontier {
            self.frontier.insert(account.clone(), head);
        }
        if full_batch {
            let start = self
                .frontier
                .get(&account)
                .copied()
                .unwrap_or(BlockHash::ZERO);
            self.enqueue(account, start);
        }
    }

    /// Requeue expired queries, including replies that made no persisted progress.
    pub fn tick_timeouts(&mut self, now_secs: u64) {
        let timed_out: Vec<u64> = self
            .in_flight
            .iter()
            .filter(|(_, q)| now_secs.saturating_sub(q.sent_at) >= self.query_timeout_secs)
            .map(|(id, _)| *id)
            .collect();
        for id in timed_out {
            if let Some(query) = self.in_flight.remove(&id) {
                let account = query.account;
                let start = self
                    .frontier
                    .get(&account)
                    .copied()
                    .unwrap_or(BlockHash::ZERO);
                self.enqueue(account, start);
            }
        }
    }

    /// Associate the request with its authenticated transport peer.
    pub fn bind_peer(&mut self, id: u64, peer: &str) {
        if let Some(query) = self.in_flight.get_mut(&id) {
            query.peer = Some(peer.to_owned());
        }
    }

    /// Validate a response against the actual request, not merely itself.
    /// Signatures, work, balances and all BURST rules remain checked by the
    /// normal block processor. A bootstrap response confers no trust.
    pub fn receive_blocks(
        &mut self,
        id: u64,
        peer: &str,
        bytes: &[Vec<u8>],
    ) -> Result<Vec<burst_ledger::StateBlock>, &'static str> {
        let query = self
            .in_flight
            .get_mut(&id)
            .ok_or("unsolicited bootstrap response")?;
        if query.peer.as_deref() != Some(peer) {
            return Err("wrong bootstrap peer");
        }
        if query.received.is_some() {
            return Err("duplicate bootstrap response");
        }
        if bytes.len() > self.blocks_per_pull as usize {
            return Err("oversized bootstrap response");
        }
        let mut previous = query.start;
        let mut blocks = Vec::with_capacity(bytes.len());
        for bytes in bytes {
            let block: burst_ledger::StateBlock =
                bincode::deserialize(bytes).map_err(|_| "malformed bootstrap block")?;
            if block.account != query.account
                || block.previous != previous
                || block.hash != block.compute_hash()
            {
                return Err("bootstrap response does not match requested chain");
            }
            previous = block.hash;
            blocks.push(block);
        }
        if let Some(last) = blocks.last() {
            query.received = Some((last.hash, blocks.len() == self.blocks_per_pull as usize));
        } else {
            self.on_blocks_ack(id, None, false);
        }
        Ok(blocks)
    }

    /// Refresh cursors from locally persisted state. Queue admission alone must
    /// never advance them (validation can fail or queue pressure can drop data).
    pub fn observe_persisted(
        &mut self,
        mut head: impl FnMut(&WalletAddress) -> BlockHash,
        mut exists: impl FnMut(&BlockHash) -> bool,
    ) {
        let mut complete = Vec::new();
        for (id, query) in &self.in_flight {
            let current = head(&query.account);
            self.frontier.insert(query.account.clone(), current);
            if let Some((last, full)) = query.received {
                if exists(&last) {
                    complete.push((*id, current, full));
                } else if current != query.start {
                    // A cross-account dependency can reject a later block while
                    // an earlier prefix commits. Retry from that local prefix
                    // without waiting for the peer-response timeout. Never skip
                    // the remainder just because this response was not full.
                    complete.push((*id, current, true));
                }
            }
        }
        for (id, current, full) in complete {
            self.on_blocks_ack(id, Some(current), full);
        }
    }

    /// Number of in-flight queries.
    pub fn in_flight_count(&self) -> usize {
        self.in_flight.len()
    }

    /// Number of accounts waiting in the queue.
    pub fn queued_count(&self) -> usize {
        self.queue.len()
    }

    /// Whether this coordinator has no queued or in-flight requests.
    /// Does not prove that peer discovery found every account or history item.
    pub fn is_idle(&self) -> bool {
        self.queue.is_empty() && self.in_flight.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn encoded_block(
        account: WalletAddress,
        previous: BlockHash,
    ) -> (burst_ledger::StateBlock, Vec<u8>) {
        let mut block = burst_ledger::StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: burst_ledger::CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: burst_ledger::BlockType::Open,
            account: account.clone(),
            previous,
            representative: account,
            brn_spent: 0,
            trst_balance: 0,
            link: BlockHash::ZERO,
            origin: burst_types::TxHash::ZERO,
            transaction: burst_types::TxHash::ZERO,
            timestamp: burst_types::Timestamp::new(1),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: burst_types::Signature([1; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        let bytes = bincode::serialize(&block).unwrap();
        (block, bytes)
    }

    #[test]
    fn stored_pull_never_restarts_at_open_for_an_unknown_or_foreign_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let store = burst_store_lmdb::LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let account = test_account_1();
        let other = test_account_2();
        let (open, open_bytes) = encoded_block(account.clone(), BlockHash::ZERO);
        let (next, next_bytes) = encoded_block(account.clone(), open.hash);
        let (foreign, foreign_bytes) = encoded_block(other.clone(), BlockHash::ZERO);
        let mut batch = store.tx_begin_write().unwrap();
        for (block, bytes, height) in [
            (&open, &open_bytes, 1),
            (&next, &next_bytes, 2),
            (&foreign, &foreign_bytes, 1),
        ] {
            batch
                .put_block_with_account(&block.hash, bytes, &block.account, height)
                .unwrap();
        }
        batch.commit().unwrap();
        assert_eq!(
            stored_blocks_after(&store, &account, &BlockHash::ZERO, 2).unwrap(),
            vec![open_bytes, next_bytes.clone()]
        );
        assert_eq!(
            stored_blocks_after(&store, &account, &open.hash, 2).unwrap(),
            vec![next_bytes]
        );
        assert!(stored_blocks_after(&store, &account, &next.hash, 2)
            .unwrap()
            .is_empty());
        assert!(stored_blocks_after(&store, &account, &foreign.hash, 2)
            .unwrap()
            .is_empty());
        assert!(
            stored_blocks_after(&store, &account, &BlockHash::new([99; 32]), 2)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn large_block_pages_fit_transport_and_resume_without_skipping() {
        use crate::wire_message::WireMessage;
        let account = test_account_1();
        let mut chain = Vec::new();
        let mut previous = BlockHash::ZERO;
        for _ in 0..ASC_PULL_MAX_BLOCKS {
            let (mut block, _) = encoded_block(account.clone(), previous);
            block.block_type = burst_ledger::BlockType::Merge;
            block.merge_sources = vec![burst_types::TxHash::new([1; 32]); 256];
            block.hash = block.compute_hash();
            assert!(block.fits_p2p_message());
            previous = block.hash;
            let bytes = bincode::serialize(&block).unwrap();
            chain.push((block, bytes));
        }
        let mut cursor = BlockHash::ZERO;
        let mut received = Vec::new();
        while received.len() < chain.len() {
            let response = BootstrapServer::handle_asc_pull_req(
                17,
                &AscPullReqPayload::Blocks {
                    account: account.clone(),
                    start: cursor,
                    count: ASC_PULL_MAX_BLOCKS,
                },
                |_, start, count| {
                    let offset = if start.is_zero() {
                        0
                    } else {
                        chain
                            .iter()
                            .position(|(block, _)| block.hash == *start)
                            .unwrap()
                            + 1
                    };
                    chain
                        .iter()
                        .skip(offset)
                        .take(count as usize)
                        .map(|(_, bytes)| bytes.clone())
                        .collect()
                },
                |_| (BlockHash::ZERO, 0),
                |_, _| Vec::new(),
            );
            let wire = WireMessage::Bootstrap(response.clone()).encode().unwrap();
            assert!(wire.len() <= burst_types::MAX_P2P_MESSAGE_BYTES);
            let BootstrapMessage::AscPullAck {
                id,
                payload: AscPullAckPayload::Blocks(blocks),
            } = response
            else {
                panic!("expected blocks");
            };
            assert_eq!(id, 17);
            assert!(!blocks.is_empty());
            assert!(blocks.len() < ASC_PULL_MAX_BLOCKS as usize);
            cursor = bincode::deserialize::<burst_ledger::StateBlock>(blocks.last().unwrap())
                .unwrap()
                .hash;
            received.extend(blocks);
        }
        assert_eq!(
            received,
            chain
                .into_iter()
                .map(|(_, bytes)| bytes)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn largest_transportable_block_fits_a_bootstrap_response() {
        let (mut block, _) = encoded_block(test_account_1(), BlockHash::ZERO);
        while block.fits_p2p_message() {
            block.merge_sources.push(burst_types::TxHash::ZERO);
        }
        block.merge_sources.pop();
        let bytes = bincode::serialize(&block).unwrap();
        let response = BootstrapServer::handle_asc_pull_req(
            5,
            &AscPullReqPayload::Blocks {
                account: block.account,
                start: BlockHash::ZERO,
                count: 1,
            },
            |_, _, _| vec![bytes.clone()],
            |_| (BlockHash::ZERO, 0),
            |_, _| Vec::new(),
        );
        let wire = crate::wire_message::WireMessage::Bootstrap(response.clone())
            .encode()
            .unwrap();
        assert!(wire.len() <= burst_types::MAX_P2P_MESSAGE_BYTES);
        assert!(
            matches!(response, BootstrapMessage::AscPullAck { payload: AscPullAckPayload::Blocks(blocks), .. } if blocks == vec![bytes])
        );
    }

    #[test]
    fn full_frontier_page_fits_transport() {
        let entries = vec![(test_account_1(), BlockHash::ZERO); ASC_PULL_MAX_FRONTIERS as usize];
        let wire = crate::wire_message::WireMessage::Bootstrap(BootstrapMessage::AscPullAck {
            id: 99,
            payload: AscPullAckPayload::Frontiers(entries),
        });
        assert!(wire.encode().unwrap().len() <= burst_types::MAX_P2P_MESSAGE_BYTES);
    }

    #[test]
    fn bootstrap_binds_response_to_peer_query_account_and_start() {
        let mut bootstrapper = Bootstrapper::new(1, 20, 1);
        let account = test_account_1();
        bootstrapper.enqueue(account.clone(), BlockHash::ZERO);
        let id = bootstrapper.next_requests(1)[0].0;
        bootstrapper.bind_peer(id, "peer-a");
        let (_, valid) = encoded_block(account.clone(), BlockHash::ZERO);
        assert!(bootstrapper
            .receive_blocks(id + 1, "peer-a", std::slice::from_ref(&valid))
            .is_err());
        assert!(bootstrapper
            .receive_blocks(id, "peer-b", std::slice::from_ref(&valid))
            .is_err());
        let (_, wrong_account) = encoded_block(test_account_2(), BlockHash::ZERO);
        assert!(bootstrapper
            .receive_blocks(id, "peer-a", &[wrong_account])
            .is_err());
        let (_, wrong_start) = encoded_block(account, BlockHash::new([9; 32]));
        assert!(bootstrapper
            .receive_blocks(id, "peer-a", &[wrong_start])
            .is_err());
        assert!(bootstrapper
            .receive_blocks(id, "peer-a", &[valid.clone(), valid.clone()])
            .is_err());
        assert!(bootstrapper
            .receive_blocks(id, "peer-a", &[vec![0]])
            .is_err());
        assert_eq!(
            bootstrapper
                .receive_blocks(id, "peer-a", std::slice::from_ref(&valid))
                .unwrap()
                .len(),
            1
        );
        assert!(bootstrapper.receive_blocks(id, "peer-a", &[valid]).is_err());
    }

    #[test]
    fn bootstrap_cursor_only_advances_after_persistence() {
        let mut bootstrapper = Bootstrapper::new(1, 20, 1);
        let account = test_account_1();
        let (block, bytes) = encoded_block(account.clone(), BlockHash::ZERO);
        bootstrapper.enqueue(account, BlockHash::ZERO);
        let id = bootstrapper.next_requests(1)[0].0;
        bootstrapper.bind_peer(id, "peer");
        bootstrapper.receive_blocks(id, "peer", &[bytes]).unwrap();
        bootstrapper.observe_persisted(|_| BlockHash::ZERO, |_| false);
        assert!(bootstrapper.next_requests(2).is_empty());
        bootstrapper.observe_persisted(|_| block.hash, |hash| *hash == block.hash);
        let next = bootstrapper.next_requests(3);
        assert!(
            matches!(&next[0].1, AscPullReqPayload::Blocks { start, .. } if *start == block.hash)
        );
    }

    #[test]
    fn partial_persistence_retries_the_remainder_without_waiting_for_timeout() {
        let mut bootstrapper = Bootstrapper::new(1, 20, 128);
        let account = test_account_1();
        let (first, first_bytes) = encoded_block(account.clone(), BlockHash::ZERO);
        let (second, second_bytes) = encoded_block(account.clone(), first.hash);
        bootstrapper.enqueue(account, BlockHash::ZERO);
        let id = bootstrapper.next_requests(1)[0].0;
        bootstrapper.bind_peer(id, "peer");
        bootstrapper
            .receive_blocks(id, "peer", &[first_bytes, second_bytes.clone()])
            .unwrap();
        bootstrapper.observe_persisted(|_| first.hash, |hash| *hash == first.hash);
        let next = bootstrapper.next_requests(2);
        assert_eq!(next.len(), 1);
        assert!(
            matches!(&next[0].1, AscPullReqPayload::Blocks { start, .. } if *start == first.hash)
        );
        let id = next[0].0;
        bootstrapper.bind_peer(id, "peer");
        bootstrapper
            .receive_blocks(id, "peer", &[second_bytes])
            .unwrap();
        // No further persisted progress: retain the bounded timeout, not a spin.
        bootstrapper.observe_persisted(|_| first.hash, |hash| *hash == first.hash);
        assert!(bootstrapper.next_requests(3).is_empty());
        bootstrapper.observe_persisted(|_| second.hash, |_| true);
        assert!(bootstrapper.is_idle());
    }

    #[test]
    fn rejected_or_dropped_bootstrap_data_retries_without_skipping() {
        let mut bootstrapper = Bootstrapper::new(1, 20, 1);
        let account = test_account_1();
        let (_, bytes) = encoded_block(account.clone(), BlockHash::ZERO);
        bootstrapper.enqueue(account, BlockHash::ZERO);
        let id = bootstrapper.next_requests(1)[0].0;
        bootstrapper.bind_peer(id, "peer");
        bootstrapper.receive_blocks(id, "peer", &[bytes]).unwrap();
        bootstrapper.observe_persisted(|_| BlockHash::ZERO, |_| false);
        bootstrapper.tick_timeouts(21);
        let next = bootstrapper.next_requests(22);
        assert!(matches!(&next[0].1, AscPullReqPayload::Blocks { start, .. } if start.is_zero()));
    }

    #[test]
    fn frontier_pages_are_ordered_but_allow_verified_unopened_accounts() {
        let a = test_account_1();
        let b = test_account_2();
        let entries = vec![
            (a.clone(), BlockHash::ZERO),
            (b.clone(), BlockHash::new([2; 32])),
        ];
        assert!(valid_frontier_page(&a, &entries));
        assert!(!valid_frontier_page(&b, &entries));
        assert!(!valid_frontier_page(
            &a,
            &[entries[1].clone(), entries[0].clone()]
        ));
        assert!(!valid_frontier_page(
            &a,
            &[entries[0].clone(), entries[0].clone()]
        ));
    }

    #[test]
    fn asc_pull_blocks_caps_count_and_echoes_id() {
        let acct = test_account_1();
        let payload = AscPullReqPayload::Blocks {
            account: acct.clone(),
            start: BlockHash::ZERO,
            count: 5000, // over the cap
        };
        let resp = BootstrapServer::handle_asc_pull_req(
            7,
            &payload,
            |a, s, c| {
                assert_eq!(a, &acct);
                assert!(s.is_zero());
                assert_eq!(c, ASC_PULL_MAX_BLOCKS, "count must be capped");
                vec![vec![1, 2, 3], vec![4, 5, 6]]
            },
            |_| (BlockHash::ZERO, 0),
            |_, _| Vec::new(),
        );
        match resp {
            BootstrapMessage::AscPullAck {
                id,
                payload: AscPullAckPayload::Blocks(b),
            } => {
                assert_eq!(id, 7);
                assert_eq!(b.len(), 2);
            }
            other => panic!("expected Blocks ack, got {:?}", other),
        }
    }

    #[test]
    fn asc_pull_account_info_response() {
        let acct = test_account_2();
        let payload = AscPullReqPayload::AccountInfo {
            account: acct.clone(),
        };
        let resp = BootstrapServer::handle_asc_pull_req(
            9,
            &payload,
            |_, _, _| Vec::new(),
            |a| {
                assert_eq!(a, &acct);
                (BlockHash::new([7; 32]), 42)
            },
            |_, _| Vec::new(),
        );
        match resp {
            BootstrapMessage::AscPullAck {
                id,
                payload:
                    AscPullAckPayload::AccountInfo {
                        account,
                        head,
                        block_count,
                    },
            } => {
                assert_eq!(id, 9);
                assert_eq!(account, acct);
                assert_eq!(head, BlockHash::new([7; 32]));
                assert_eq!(block_count, 42);
            }
            other => panic!("expected AccountInfo ack, got {:?}", other),
        }
    }

    #[test]
    fn asc_pull_frontiers_caps_and_returns_page() {
        let payload = AscPullReqPayload::Frontiers {
            start_account: test_account_1(),
            count: 60_000, // over the cap (ASC_PULL_MAX_FRONTIERS), within u16
        };
        let resp = BootstrapServer::handle_asc_pull_req(
            3,
            &payload,
            |_, _, _| Vec::new(),
            |_| (BlockHash::ZERO, 0),
            |start, c| {
                assert_eq!(start, &test_account_1());
                assert_eq!(c, ASC_PULL_MAX_FRONTIERS, "count must be capped");
                vec![(test_account_1(), BlockHash::new([1; 32]))]
            },
        );
        match resp {
            BootstrapMessage::AscPullAck {
                id,
                payload: AscPullAckPayload::Frontiers(f),
            } => {
                assert_eq!(id, 3);
                assert_eq!(f.len(), 1);
            }
            other => panic!("expected Frontiers ack, got {:?}", other),
        }
    }

    #[test]
    fn bootstrapper_respects_in_flight_budget_and_correlates() {
        let mut bootstrapper = Bootstrapper::new(2, 30, 128);
        bootstrapper.enqueue(test_account_1(), BlockHash::ZERO);
        bootstrapper.enqueue(test_account_2(), BlockHash::new([9; 32]));
        bootstrapper.enqueue(test_account_3(), BlockHash::ZERO);

        // Only 2 may be in flight at once.
        let reqs = bootstrapper.next_requests(100);
        assert_eq!(reqs.len(), 2);
        assert_eq!(bootstrapper.in_flight_count(), 2);
        assert_eq!(bootstrapper.queued_count(), 1);
        // ids are distinct; the start frontier is threaded through.
        assert_ne!(reqs[0].0, reqs[1].0);
        match &reqs[1].1 {
            AscPullReqPayload::Blocks { account, start, .. } if account == &test_account_2() => {
                assert_eq!(*start, BlockHash::new([9; 32]));
            }
            AscPullReqPayload::Blocks { .. } => {} // order not guaranteed for acct1
            other => panic!("expected Blocks req, got {:?}", other),
        }

        // No more slots until persisted progress or a timeout frees one.
        assert!(bootstrapper.next_requests(100).is_empty());
    }

    #[test]
    fn bootstrapper_advances_frontier_and_requeues_on_full_batch() {
        let mut bootstrapper = Bootstrapper::new(4, 30, 128);
        bootstrapper.enqueue(test_account_1(), BlockHash::ZERO);
        let reqs = bootstrapper.next_requests(100);
        let id = reqs[0].0;

        // Report a persisted full batch: advance the cursor and requeue the account.
        bootstrapper.on_blocks_ack(id, Some(BlockHash::new([5; 32])), true);
        assert_eq!(bootstrapper.in_flight_count(), 0);
        assert_eq!(bootstrapper.queued_count(), 1);
        // Next request for it starts from the advanced frontier.
        let reqs2 = bootstrapper.next_requests(101);
        match &reqs2[0].1 {
            AscPullReqPayload::Blocks { start, .. } => {
                assert_eq!(*start, BlockHash::new([5; 32]))
            }
            other => panic!("expected Blocks req, got {:?}", other),
        }

        // A non-full batch (end of chain) → done, no re-queue.
        let id2 = reqs2[0].0;
        bootstrapper.on_blocks_ack(id2, Some(BlockHash::new([6; 32])), false);
        assert!(bootstrapper.is_idle());
    }

    #[test]
    fn bootstrapper_requeues_timed_out_queries() {
        let mut bootstrapper = Bootstrapper::new(4, 30, 128);
        bootstrapper.enqueue(test_account_1(), BlockHash::ZERO);
        let _ = bootstrapper.next_requests(100);
        assert_eq!(bootstrapper.in_flight_count(), 1);

        // Not yet timed out.
        bootstrapper.tick_timeouts(120);
        assert_eq!(bootstrapper.in_flight_count(), 1);

        // Past the 30s timeout → re-queued.
        bootstrapper.tick_timeouts(131);
        assert_eq!(bootstrapper.in_flight_count(), 0);
        assert_eq!(bootstrapper.queued_count(), 1);
    }
}
