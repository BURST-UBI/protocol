//! Bootstrap protocol — ascending sync of historical blocks from peers.
//!
//! When a new node joins the network it has no ledger data. It catches up with
//! a single ascending-pull protocol ([`BootstrapMessage`]): the [`Bootstrapper`]
//! client keeps a priority queue of accounts to sync, discovers them via
//! `Frontiers` pulls, issues parallel id-correlated `Blocks` pulls, advances
//! each account's frontier as verified blocks arrive, and requeues on timeout.
//! [`BootstrapServer`] serves each request from the ledger via injected closures.
//!
//! The protocol uses a request/response pattern over the existing P2P TCP
//! connections with serialized messages.

use std::collections::{HashMap, HashSet, VecDeque};

use burst_types::{BlockHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// Bootstrap protocol messages — the ascending-bootstrap request/response pair.
///
/// A single typed, id-correlated pull protocol (blocks / account-info /
/// frontiers) replaces the old serial Frontier→BulkPull→Block scheme entirely.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum BootstrapMessage {
    /// Ascending-bootstrap pull request: parallel, id-correlated, typed
    /// (blocks / account-info / frontiers).
    AscPullReq {
        /// Correlates the [`BootstrapMessage::AscPullAck`] to this request.
        id: u64,
        /// What to pull.
        payload: AscPullReqPayload,
    },
    /// Ascending-bootstrap pull response, echoing the request `id`.
    AscPullAck {
        /// Echoes the request id.
        id: u64,
        /// The pulled data.
        payload: AscPullAckPayload,
    },
}

/// What an [`BootstrapMessage::AscPullReq`] asks for.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AscPullReqPayload {
    /// Up to `count` blocks for `account`, starting at the block AFTER `start`
    /// (`start` = the requester's current frontier for the account, or
    /// `BlockHash::ZERO` to start from the open block). Returns a contiguous
    /// ascending run.
    Blocks {
        /// Account whose chain to pull.
        account: WalletAddress,
        /// Requester's current frontier (pull begins at its successor).
        start: BlockHash,
        /// Max blocks to return (server also caps at `ASC_PULL_MAX_BLOCKS`).
        count: u16,
    },
    /// The account's current frontier head and block count.
    AccountInfo {
        /// Account to describe.
        account: WalletAddress,
    },
    /// A page of up to `count` account frontiers from `start_account`.
    Frontiers {
        /// First account to return (inclusive).
        start_account: WalletAddress,
        /// Max frontier entries to return.
        count: u16,
    },
}

/// The data returned in an [`BootstrapMessage::AscPullAck`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AscPullAckPayload {
    /// A contiguous, ascending run of serialized blocks (bincode-encoded).
    Blocks(Vec<Vec<u8>>),
    /// Account frontier head + height (`ZERO`/`0` if the account is unknown).
    AccountInfo {
        /// The account described.
        account: WalletAddress,
        /// Its frontier head (`BlockHash::ZERO` if unknown).
        head: BlockHash,
        /// Its block count (`0` if unknown).
        block_count: u64,
    },
    /// A page of `(account, head)` frontiers.
    Frontiers(Vec<(WalletAddress, BlockHash)>),
}

/// Bootstrap server — responds to bootstrap requests from peers.
///
/// Stateless — each method takes the data it needs as parameters.
pub struct BootstrapServer;

impl BootstrapServer {
    /// Serve an ascending-bootstrap pull request, producing the matching
    /// [`BootstrapMessage::AscPullAck`] (echoing `id`). Ledger access is
    /// injected via closures so this stays pure/testable:
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
                let c = (*count).min(ASC_PULL_MAX_BLOCKS);
                AscPullAckPayload::Blocks(blocks_after(account, start, c))
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
                let c = (*count).min(ASC_PULL_MAX_FRONTIERS);
                AscPullAckPayload::Frontiers(frontiers_from(start_account, c))
            }
        };
        BootstrapMessage::AscPullAck { id, payload }
    }
}

/// Max blocks served in one ascending-bootstrap Blocks response.
pub const ASC_PULL_MAX_BLOCKS: u16 = 128;

/// Max frontier entries served in one ascending-bootstrap Frontiers response.
pub const ASC_PULL_MAX_FRONTIERS: u16 = 1000;

/// Feedback from the network read loop to the bootstrap task about received
/// ascending-pull acks. Decouples the [`Bootstrapper`] from the read loop: the
/// read loop only holds an `mpsc::Sender<BootstrapFeedback>`, and the bootstrap
/// task owns the `Bootstrapper` and applies these.
#[derive(Clone, Debug)]
pub enum BootstrapFeedback {
    /// Result of a Blocks pull: query `id`, the last accepted block hash
    /// (None if the run was empty/invalid), and whether it was a full batch
    /// (⇒ the account probably has more to pull).
    Blocks {
        /// The query id being answered.
        id: u64,
        /// Hash of the last accepted block (advances the account's frontier).
        last: Option<BlockHash>,
        /// Whether the run filled the batch (more likely to follow).
        full_batch: bool,
    },
    /// Frontiers discovered from a peer: `(account, remote_head)` pairs, used to
    /// find accounts we're behind on and enqueue them.
    Frontiers(Vec<(WalletAddress, BlockHash)>),
}

/// Client-side ascending bootstrapper: drives catch-up by pulling blocks for
/// accounts we're behind on, from peers in parallel, with id-correlated
/// requests and per-query timeouts.
///
/// The ascending model (vs one serial frontier+bulk-pull): keep a priority
/// queue of accounts to sync, issue up to `max_in_flight` concurrent
/// `AscPullReq::Blocks`, advance each account's frontier as verified blocks
/// arrive, and re-queue accounts that still have more to pull. Timed-out
/// queries are re-queued so a slow/dead peer can't stall progress.
pub struct Bootstrapper {
    /// Accounts waiting to be pulled (round-robin priority).
    queue: VecDeque<WalletAddress>,
    /// Membership mirror of `queue` to avoid duplicate enqueues.
    queued: HashSet<WalletAddress>,
    /// Our current frontier per account — where the next pull starts.
    frontier: HashMap<WalletAddress, BlockHash>,
    /// In-flight queries: `id -> (account, sent_at_secs)`.
    in_flight: HashMap<u64, (WalletAddress, u64)>,
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
            blocks_per_pull,
        }
    }

    /// Queue an account for syncing, starting from `our_frontier` (the last
    /// block we have for it; `ZERO` if none). No-op if already queued or
    /// in-flight (but the start frontier is always refreshed).
    pub fn enqueue(&mut self, account: WalletAddress, our_frontier: BlockHash) {
        self.frontier.insert(account.clone(), our_frontier);
        let in_flight = self.in_flight.values().any(|(a, _)| a == &account);
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
            self.in_flight.insert(id, (account.clone(), now_secs));
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
        let Some((account, _)) = self.in_flight.remove(&id) else {
            return;
        };
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

    /// Re-queue queries that exceeded the timeout (peer didn't answer).
    pub fn tick_timeouts(&mut self, now_secs: u64) {
        let timed_out: Vec<u64> = self
            .in_flight
            .iter()
            .filter(|(_, (_, sent))| now_secs.saturating_sub(*sent) >= self.query_timeout_secs)
            .map(|(id, _)| *id)
            .collect();
        for id in timed_out {
            if let Some((account, _)) = self.in_flight.remove(&id) {
                let start = self
                    .frontier
                    .get(&account)
                    .copied()
                    .unwrap_or(BlockHash::ZERO);
                self.enqueue(account, start);
            }
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

    /// Whether there is no work left (nothing queued or in-flight).
    pub fn is_idle(&self) -> bool {
        self.queue.is_empty() && self.in_flight.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_ledger::{BlockType, StateBlock, CURRENT_BLOCK_VERSION};
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
            merge_sources: Vec::new(),
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

    // ── Ascending-bootstrap responder tests ────────────────────────────

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

    // ── Bootstrapper (client) tests ─────────────────────────────────────

    #[test]
    fn bootstrapper_respects_in_flight_budget_and_correlates() {
        let mut bs = Bootstrapper::new(2, 30, 128);
        bs.enqueue(test_account_1(), BlockHash::ZERO);
        bs.enqueue(test_account_2(), BlockHash::new([9; 32]));
        bs.enqueue(test_account_3(), BlockHash::ZERO);

        // Only 2 may be in flight at once.
        let reqs = bs.next_requests(100);
        assert_eq!(reqs.len(), 2);
        assert_eq!(bs.in_flight_count(), 2);
        assert_eq!(bs.queued_count(), 1);
        // ids are distinct; the start frontier is threaded through.
        assert_ne!(reqs[0].0, reqs[1].0);
        match &reqs[1].1 {
            AscPullReqPayload::Blocks { account, start, .. } if account == &test_account_2() => {
                assert_eq!(*start, BlockHash::new([9; 32]));
            }
            AscPullReqPayload::Blocks { .. } => {} // order not guaranteed for acct1
            other => panic!("expected Blocks req, got {:?}", other),
        }

        // No more slots until an ack frees one.
        assert!(bs.next_requests(100).is_empty());
    }

    #[test]
    fn bootstrapper_advances_frontier_and_requeues_on_full_batch() {
        let mut bs = Bootstrapper::new(4, 30, 128);
        bs.enqueue(test_account_1(), BlockHash::ZERO);
        let reqs = bs.next_requests(100);
        let id = reqs[0].0;

        // Full batch → frontier advances AND the account is re-queued for more.
        bs.on_blocks_ack(id, Some(BlockHash::new([5; 32])), true);
        assert_eq!(bs.in_flight_count(), 0);
        assert_eq!(bs.queued_count(), 1);
        // Next request for it starts from the advanced frontier.
        let reqs2 = bs.next_requests(101);
        match &reqs2[0].1 {
            AscPullReqPayload::Blocks { start, .. } => {
                assert_eq!(*start, BlockHash::new([5; 32]))
            }
            other => panic!("expected Blocks req, got {:?}", other),
        }

        // A non-full batch (end of chain) → done, no re-queue.
        let id2 = reqs2[0].0;
        bs.on_blocks_ack(id2, Some(BlockHash::new([6; 32])), false);
        assert!(bs.is_idle());
    }

    #[test]
    fn bootstrapper_requeues_timed_out_queries() {
        let mut bs = Bootstrapper::new(4, 30, 128);
        bs.enqueue(test_account_1(), BlockHash::ZERO);
        let _ = bs.next_requests(100);
        assert_eq!(bs.in_flight_count(), 1);

        // Not yet timed out.
        bs.tick_timeouts(120);
        assert_eq!(bs.in_flight_count(), 1);

        // Past the 30s timeout → re-queued.
        bs.tick_timeouts(131);
        assert_eq!(bs.in_flight_count(), 0);
        assert_eq!(bs.queued_count(), 1);
    }
}
