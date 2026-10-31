//! Weighted round-robin by transport origin, with PoW priority within each origin.
//! Scheduling weights count blocks, not bytes or processing time. This queue
//! computes work scores but leaves authentication and ledger admission to workers.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::net::IpAddr;
use burst_network::{admission::peer_bucket, fair_queue::FairQueue};

use tokio::sync::{Mutex, Notify};

use crate::block_retention::{BlockRetention, RetainedBlock, Slot};
use burst_ledger::StateBlock;
use burst_types::BlockHash;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueAdmissionError {
    Contended,
    Capacity,
    Retention,
    Encoding,
}

impl std::fmt::Display for QueueAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Contended => "queue lock busy",
            Self::Capacity => "ingress capacity exhausted",
            Self::Retention => "shared retention budget exhausted",
            Self::Encoding => "block payload encoding failed",
        })
    }
}

/// A block wrapped with its PoW difficulty score for priority ordering.
struct PrioritizedBlock {
    block: StateBlock,
    slot: Option<Slot>,
    difficulty: u64,
    /// Insertion order counter for FIFO tiebreaking among equal difficulties.
    sequence: u64,
    enqueued_at: Option<std::time::Instant>,
    payload_hash: [u8; 32],
}

impl Eq for PrioritizedBlock {}

impl PartialEq for PrioritizedBlock {
    fn eq(&self, other: &Self) -> bool {
        self.difficulty == other.difficulty && self.sequence == other.sequence
    }
}

impl Ord for PrioritizedBlock {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher difficulty = higher priority.
        // On tie, lower sequence (earlier arrival) = higher priority.
        self.difficulty
            .cmp(&other.difficulty)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

impl PartialOrd for PrioritizedBlock {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Compute the PoW difficulty score for a block.
///
/// `Blake2b-64(nonce_le_bytes || work_root)` interpreted as u64 LE.
/// Higher values satisfy higher difficulty thresholds.
pub fn work_difficulty(work_root: &BlockHash, nonce: u64) -> u64 {
    burst_work::work_value(work_root, nonce)
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Origin { Local, Recovery, Peer(IpAddr) }

#[derive(Default)]
struct ReadyQueue {
    scheduler: FairQueue<Origin, PrioritizedBlock>,
    network_len: usize,
}
impl ReadyQueue {
    fn push(&mut self, origin: Origin, item: PrioritizedBlock) {
        let network = matches!(origin, Origin::Peer(_));
        self.network_len += usize::from(network);
        self.scheduler.push(origin, item, if network { 1 } else { 4 });
    }
    fn pop(&mut self) -> Option<PrioritizedBlock> {
        let (origin, item) = self.scheduler.pop()?;
        self.network_len -= usize::from(matches!(origin, Origin::Peer(_)));
        Some(item)
    }
    fn len(&self) -> usize { self.scheduler.len() }
    #[cfg(test)]
    fn peek(&self) -> Option<&PrioritizedBlock> { self.scheduler.peek().map(|(_, item)| item) }
    fn is_empty(&self) -> bool { self.scheduler.is_empty() }
}

/// Local submissions and retained recovery have separate scheduling origins.
/// Network origins group observed IPv4 addresses or IPv6 /64s; mapped IPv4
/// shares its IPv4 bucket. Local/recovery origins get four pops per turn and
/// each peer bucket gets one. Retained recovery bypasses this queue's capacity
/// check because it already holds a shared permit; len can exceed capacity.
pub struct BlockPriorityQueue {
    state: Mutex<QueueState>,
    capacity: usize,
    stage_log: bool,
    notify: Notify,
    retention: Option<Arc<BlockRetention>>,
}

#[derive(Default)]
struct QueueState {
    ready: ReadyQueue,
    sequence: u64,
    pending_payloads: BTreeSet<[u8; 32]>,
}

impl BlockPriorityQueue {
    pub fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(QueueState::default()),
            capacity,
            stage_log: std::env::var_os("BURST_FINALITY_STAGE_LOG").is_some(),
            notify: Notify::new(),
            retention: None,
        }
    }

    pub fn with_retention(capacity: usize, retention: Arc<BlockRetention>) -> Self {
        let mut queue = Self::new(capacity);
        queue.retention = Some(retention);
        queue
    }

    /// Admit a local payload, or succeed without insertion if already queued.
    /// Waits for the mutex, not for free capacity. Failure drops the input.
    pub async fn push(&self, block: StateBlock) -> bool {
        self.push_with_reason(block).await.is_ok()
    }

    pub async fn push_from_peer(&self, block: StateBlock, address: IpAddr) -> bool {
        self.admit(RetainedBlock { block, slot: None }, Origin::Peer(peer_bucket(address))).await.is_ok()
    }

    pub async fn push_retained(&self, retained: RetainedBlock) -> bool {
        let origin = if retained.slot.is_some() { Origin::Recovery } else { Origin::Local };
        self.admit(retained, origin).await.is_ok()
    }

    pub async fn push_with_reason(&self, block: StateBlock) -> Result<(), QueueAdmissionError> {
        self.admit(RetainedBlock { block, slot: None }, Origin::Local).await
    }

    fn entry(&self, retained: RetainedBlock) -> Result<PrioritizedBlock, QueueAdmissionError> {
        let block = retained.block;
        let difficulty = work_difficulty(&block.work_root(), block.work);
        // Deduplicate the complete payload, so an invalid same-hash variant
        // cannot hide a later valid signature or work value.
        let payload_hash = burst_crypto::blake2b_256(
            &bincode::serialize(&block).map_err(|_| QueueAdmissionError::Encoding)?);
        Ok(PrioritizedBlock { block, slot: retained.slot, difficulty, sequence: 0,
            enqueued_at: self.stage_log.then(std::time::Instant::now), payload_hash })
    }

    fn insert(
        &self, state: &mut QueueState,
        mut entry: PrioritizedBlock, origin: Origin,
    ) -> Result<(), QueueAdmissionError> {
        let QueueState { ready, sequence, pending_payloads } = state;
        if pending_payloads.contains(&entry.payload_hash) { return Ok(()); }
        if entry.slot.is_none() {
            if ready.len() >= self.capacity { return Err(QueueAdmissionError::Capacity); }
            if matches!(origin, Origin::Peer(_)) {
                // Leave a quarter of ingress available to local submissions;
                // already-retained recovery carries its own global capacity.
                let reserve = (self.capacity / 4).max(1).min(self.capacity.saturating_sub(1));
                let per_origin = (self.capacity / 4).clamp(1, 256);
                if ready.network_len >= self.capacity - reserve
                    || ready.scheduler.len_for(&origin) >= per_origin
                { return Err(QueueAdmissionError::Capacity); }
            }
            if let Some(budget) = &self.retention {
                entry.slot = Some(budget.acquire().ok_or(QueueAdmissionError::Retention)?);
            }
        }
        *sequence = sequence.wrapping_add(1);
        entry.sequence = *sequence;
        pending_payloads.insert(entry.payload_hash);
        ready.push(origin, entry);
        Ok(())
    }

    async fn admit(&self, retained: RetainedBlock, origin: Origin) -> Result<(), QueueAdmissionError> {
        let entry = self.entry(retained)?;
        let mut guard = self.state.lock().await;
        self.insert(&mut guard, entry, origin)?;
        drop(guard);
        self.notify.notify_one();
        Ok(())
    }

    pub fn try_push(&self, block: StateBlock) -> bool { self.try_push_with_reason(block).is_ok() }

    /// Compute priority/payload digest before trying the mutex; never wait for it.
    /// A contention or capacity error consumes and drops the supplied block.
    pub fn try_push_with_reason(&self, block: StateBlock) -> Result<(), QueueAdmissionError> {
        self.try_admit(RetainedBlock { block, slot: None }, Origin::Local)
    }

    pub fn try_push_retained(&self, retained: RetainedBlock) -> bool {
        let origin = if retained.slot.is_some() { Origin::Recovery } else { Origin::Local };
        self.try_admit(retained, origin).is_ok()
    }

    fn try_admit(&self, retained: RetainedBlock, origin: Origin) -> Result<(), QueueAdmissionError> {
        let entry = self.entry(retained)?;
        let mut guard = self.state.try_lock().map_err(|_| QueueAdmissionError::Contended)?;
        self.insert(&mut guard, entry, origin)?;
        drop(guard);
        self.notify.notify_one();
        Ok(())
    }

    /// Wait for a block and drop its retention permit when returning the bare block.
    pub async fn pop(&self) -> StateBlock { self.pop_retained().await.block }

    /// Wait until an entry is available and transfer its retention permit.
    /// There is no close signal or built-in timeout.
    pub async fn pop_retained(&self) -> RetainedBlock {
        loop {
            // Register before checking so a producer cannot race an empty check.
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(entry) = self.try_pop_retained().await { return entry; }
            notified.await;
        }
    }

    pub async fn try_pop(&self) -> Option<StateBlock> {
        self.try_pop_retained().await.map(|entry| entry.block)
    }

    /// Wait for the mutex, then return None if empty; never wait for new entries.
    pub async fn try_pop_retained(&self) -> Option<RetainedBlock> {
        let mut guard = self.state.lock().await;
        let entry = guard.ready.pop()?;
        guard.pending_payloads.remove(&entry.payload_hash);
        Some(self.deliver(entry, guard.ready.len()))
    }

    /// Return immediately; None means either mutex contention or an empty queue.
    pub fn try_pop_retained_now(&self) -> Option<RetainedBlock> {
        let mut guard = self.state.try_lock().ok()?;
        let entry = guard.ready.pop()?;
        guard.pending_payloads.remove(&entry.payload_hash);
        Some(self.deliver(entry, guard.ready.len()))
    }

    fn deliver(&self, entry: PrioritizedBlock, remaining: usize) -> RetainedBlock {
        if let Some(enqueued) = entry.enqueued_at {
            tracing::debug!(queue = self as *const Self as usize, hash = %entry.block.hash,
                priority = entry.difficulty, sequence = entry.sequence, remaining,
                elapsed_us = enqueued.elapsed().as_micros(), "ingress queue residence");
        }
        RetainedBlock { block: entry.block, slot: entry.slot }
    }

    /// Current number of blocks in the queue.
    pub async fn len(&self) -> usize {
        self.state.lock().await.ready.len()
    }

    /// Whether the queue is empty.
    pub async fn is_empty(&self) -> bool {
        self.state.lock().await.ready.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_ledger::{BlockType, CURRENT_BLOCK_VERSION};
    use burst_types::{BlockHash, Signature, Timestamp, TxHash, WalletAddress};
    use burst_work::WorkGenerator;

    fn test_account() -> WalletAddress {
        WalletAddress::new(
            "brst_1111111111111111111111111111111111111111111111111111111111111111111",
        )
    }

    fn test_rep() -> WalletAddress {
        WalletAddress::new(
            "brst_2222222222222222222222222222222222222222222222222222222222222222222",
        )
    }

    fn make_block_with_work(nonce: u64) -> StateBlock {
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Open,
            account: test_account(),
            previous: BlockHash::ZERO,
            representative: test_rep(),
            brn_spent: 1000,
            trst_balance: 0,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_000),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: nonce,
            signature: Signature([1u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block
    }

    fn make_block_with_generated_work(difficulty: u64) -> StateBlock {
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Open,
            account: test_account(),
            previous: BlockHash::ZERO,
            representative: test_rep(),
            brn_spent: 1000,
            trst_balance: 0,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::new([difficulty as u8; 32]),
            timestamp: Timestamp::new(1_000_000),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([1u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();

        if difficulty > 0 {
            let generator = WorkGenerator;
            let nonce = generator.generate(&block.work_root(), difficulty).unwrap();
            block.work = nonce.0;
        }

        block
    }

    #[tokio::test]
    async fn peer_flood_preserves_other_origins_local_capacity_and_retained_recovery() {
        let budget = BlockRetention::new(32);
        let queue = BlockPriorityQueue::with_retention(8, budget.clone());
        for (ip, first) in [("192.0.2.1", 10), ("192.0.2.2", 20), ("192.0.2.3", 30)] {
            for nonce in first..first+2 { assert!(queue.push_from_peer(make_block_with_work(nonce), ip.parse().unwrap()).await); }
        }
        assert!(!queue.push_from_peer(make_block_with_work(99), "192.0.2.4".parse().unwrap()).await);
        assert!(queue.push(make_block_with_work(40)).await);
        assert!(queue.push(make_block_with_work(41)).await);
        assert!(queue.push_retained(RetainedBlock { block: make_block_with_work(50), slot: budget.acquire() }).await);
        let mut served = Vec::new();
        for _ in 0..6 { served.push(queue.pop().await.work); }
        assert!(served.contains(&40) && served.contains(&41) && served.contains(&50));
        for first in [10, 20, 30] { assert!(served.iter().any(|work| *work == first || *work == first + 1)); }
    }

    #[tokio::test]
    async fn address_aliases_share_queue_quota_and_retries_do_not_duplicate() {
        let queue = BlockPriorityQueue::new(8);
        let first = make_block_with_work(1);
        assert!(queue.push_from_peer(first.clone(), "192.0.2.1".parse().unwrap()).await);
        assert!(queue.push_from_peer(first, "::ffff:192.0.2.1".parse().unwrap()).await);
        assert!(queue.push_from_peer(make_block_with_work(2), "::ffff:192.0.2.1".parse().unwrap()).await);
        assert!(!queue.push_from_peer(make_block_with_work(3), "192.0.2.1".parse().unwrap()).await);
        assert!(queue.push_from_peer(make_block_with_work(4), "198.51.100.1".parse().unwrap()).await);
        queue.pop().await;
        assert_eq!(queue.pop().await.work, 4);
        assert!(queue.push_from_peer(make_block_with_work(5), "192.0.2.1".parse().unwrap()).await);
    }

    #[test]
    fn test_work_difficulty_computation() {
        let hash = BlockHash::new([0x42; 32]);
        let d0 = work_difficulty(&hash, 0);
        let d1 = work_difficulty(&hash, 1);
        // These two fixed nonce/root pairs have different scores.
        assert_ne!(d0, d1);
    }

    #[test]
    fn test_work_difficulty_matches_validator() {
        let hash = BlockHash::new([0xDE; 32]);
        let generator = WorkGenerator;
        let min_difficulty = 5000;
        let nonce = generator.generate(&hash, min_difficulty).unwrap();

        let computed = work_difficulty(&hash, nonce.0);
        assert!(computed >= min_difficulty);
    }

    #[tokio::test]
    async fn test_push_and_pop() {
        let queue = BlockPriorityQueue::new(16);
        let block = make_block_with_work(42);
        let hash = block.hash;

        assert!(queue.push(block).await);
        let popped = queue.pop().await;
        assert_eq!(popped.hash, hash);
    }

    #[tokio::test]
    async fn admission_errors_distinguish_contention_capacity_and_retention() {
        let budget = BlockRetention::new(2);
        let queue = BlockPriorityQueue::with_retention(1, budget.clone());
        let block = make_block_with_work(1);
        let guard = queue.state.lock().await;
        assert_eq!(queue.try_push_with_reason(block.clone()), Err(QueueAdmissionError::Contended));
        assert!(guard.ready.is_empty());
        let first = budget.acquire().unwrap();
        let second = budget.acquire().unwrap();
        drop(guard);
        assert_eq!(queue.try_push_with_reason(block.clone()), Err(QueueAdmissionError::Retention));
        assert!(queue.is_empty().await);
        drop(first);
        assert_eq!(queue.try_push_with_reason(block.clone()), Ok(()));
        assert_eq!(queue.try_push_with_reason(make_block_with_work(2)), Err(QueueAdmissionError::Capacity));
        assert_eq!(queue.len().await, 1);
        assert!(budget.acquire().is_none());
        let popped = queue.try_pop_retained().await.unwrap();
        assert_eq!(popped.block.hash, block.hash);
        assert!(budget.acquire().is_none());
        drop(popped);
        assert!(budget.acquire().is_some());
        drop(second);
        assert_eq!(queue.try_push_with_reason(block), Ok(()));
    }

    #[tokio::test]
    async fn async_admission_waits_for_lock_and_preserves_cancellation_and_budgets() {
        use std::future::{poll_fn, Future};
        use std::task::Poll;

        let budget = BlockRetention::new(1);
        let queue = BlockPriorityQueue::with_retention(1, budget.clone());
        let block = make_block_with_work(42);
        let guard = queue.state.lock().await;
        let mut cancelled = Box::pin(queue.push_with_reason(block.clone()));
        poll_fn(|cx| {
            assert!(cancelled.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert!(guard.ready.is_empty());
        assert!(budget.acquire().is_some());
        drop(cancelled);
        let mut pending = Box::pin(queue.push_with_reason(block.clone()));
        poll_fn(|cx| {
            assert!(pending.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(guard);
        assert_eq!(pending.await, Ok(()));
        assert_eq!(queue.len().await, 1);
        assert_eq!(
            queue.push_with_reason(make_block_with_work(43)).await,
            Err(QueueAdmissionError::Capacity)
        );
        let retained = queue.pop_retained().await;
        assert_eq!(retained.block.hash, block.hash);
        assert!(queue.is_empty().await);
        assert_eq!(
            queue.push_with_reason(block.clone()).await,
            Err(QueueAdmissionError::Retention)
        );
        drop(retained);
        assert_eq!(queue.push_with_reason(block.clone()).await, Ok(()));
        assert_eq!(queue.pop_retained().await.block.hash, block.hash);
        assert!(queue.is_empty().await);
        assert!(budget.acquire().is_some());
    }

    #[tokio::test]
    async fn recovery_transfer_keeps_reserved_blocks_when_ingress_is_full() {
        let budget = BlockRetention::new(3);
        let queue = BlockPriorityQueue::with_retention(1, budget.clone());
        let recovered = RetainedBlock {
            block: make_block_with_work(1),
            slot: Some(budget.acquire().unwrap()),
        };
        assert!(queue.push(make_block_with_work(2)).await);
        assert!(!queue.push(make_block_with_work(3)).await);
        assert!(queue.push_retained(recovered).await);
        let recovered = RetainedBlock {
            block: make_block_with_work(4),
            slot: Some(budget.acquire().unwrap()),
        };
        assert!(queue.try_push_retained(recovered));
        assert!(budget.acquire().is_none());
        assert_eq!(queue.len().await, 3);
        for _ in 0..3 {
            drop(queue.try_pop_retained().await.unwrap());
        }
        assert!(queue.push(make_block_with_work(5)).await);
        assert_eq!(queue.len().await, 1);
    }

    #[tokio::test]
    async fn test_priority_ordering() {
        let queue = BlockPriorityQueue::new(16);

        // Requested thresholds are lower bounds; actual scores may be reversed.
        let low = make_block_with_generated_work(100);
        let high = make_block_with_generated_work(50_000);

        let low_diff = work_difficulty(&low.work_root(), low.work);
        let high_diff = work_difficulty(&high.work_root(), high.work);

        let low_hash = low.hash;
        let high_hash = high.hash;

        queue.push(low).await;
        queue.push(high).await;

        // Pop should return the higher difficulty block first
        let first = queue.pop().await;
        let second = queue.pop().await;

        if high_diff > low_diff {
            assert_eq!(first.hash, high_hash);
            assert_eq!(second.hash, low_hash);
        } else {
            // This branch checks delivery only, not priority ordering.
            assert!(first.hash == low_hash || first.hash == high_hash);
            assert!(second.hash == low_hash || second.hash == high_hash);
            assert_ne!(first.hash, second.hash);
        }
    }

    #[tokio::test]
    async fn test_capacity_enforcement() {
        let queue = BlockPriorityQueue::new(2);

        let b1 = make_block_with_work(1);
        let b2 = make_block_with_work(2);
        let b3 = make_block_with_work(3);

        assert!(queue.push(b1).await);
        assert!(queue.push(b2).await);
        assert!(!queue.push(b3).await); // should be rejected (at capacity)
        assert_eq!(queue.len().await, 2);
    }

    #[tokio::test]
    async fn test_try_pop_empty() {
        let queue = BlockPriorityQueue::new(8);
        assert!(queue.try_pop().await.is_none());
    }

    #[tokio::test]
    async fn test_len_and_is_empty() {
        let queue = BlockPriorityQueue::new(8);
        assert!(queue.is_empty().await);
        assert_eq!(queue.len().await, 0);

        let block = make_block_with_work(0);
        queue.push(block).await;

        assert!(!queue.is_empty().await);
        assert_eq!(queue.len().await, 1);
    }

    #[tokio::test]
    async fn test_pop_waits_for_push() {
        use std::sync::Arc;
        use tokio::time::{timeout, Duration};

        let queue = Arc::new(BlockPriorityQueue::new(8));
        let queue_clone = Arc::clone(&queue);

        // Spawn a task that pushes after a small delay
        let block = make_block_with_work(99);
        let expected_hash = block.hash;

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            queue_clone.push(block).await;
        });

        // Pop should wait and then return the block
        let result = timeout(Duration::from_secs(2), queue.pop()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().hash, expected_hash);
    }

    #[tokio::test]
    async fn test_fifo_tiebreaking() {
        // When difficulty is identical, earlier arrivals should come first
        let queue = BlockPriorityQueue::new(16);

        // Keep nonce and work root unchanged while changing the block hash.
        let b1 = make_block_with_work(0);
        let b2 = {
            let mut b = make_block_with_work(0);
            b.trst_balance = 999;
            b.hash = b.compute_hash();
            b
        };

        let d1 = work_difficulty(&b1.work_root(), b1.work);
        let d2 = work_difficulty(&b2.work_root(), b2.work);

        let h1 = b1.hash;
        let h2 = b2.hash;

        queue.push(b1).await;
        queue.push(b2).await;

        let first = queue.pop().await;
        let second = queue.pop().await;

        if d1 == d2 {
            // Same difficulty: first pushed should come first (FIFO tiebreak)
            assert_eq!(first.hash, h1);
            assert_eq!(second.hash, h2);
        } else if d1 > d2 {
            assert_eq!(first.hash, h1);
        } else {
            assert_eq!(first.hash, h2);
        }
    }

    #[tokio::test]
    async fn opportunistic_pop_preserves_entries_when_lock_is_contended() {
        let queue = BlockPriorityQueue::new(2);
        let first = make_block_with_work(1);
        let second = make_block_with_work(2);
        queue.push(first).await;
        queue.push(second).await;
        let guard = queue.state.lock().await;
        let expected = guard.ready.peek().unwrap().block.hash;
        assert!(queue.try_pop_retained_now().is_none());
        assert_eq!(guard.ready.len(), 2);
        drop(guard);
        assert_eq!(queue.try_pop_retained_now().unwrap().block.hash, expected);
        assert_eq!(queue.len().await, 1);
        assert!(queue.try_pop_retained_now().is_some());
        assert!(queue.try_pop_retained_now().is_none());
    }

    #[tokio::test]
    async fn queued_payload_coalescing_preserves_variants_retries_and_retention() {
        let budget = BlockRetention::new(4);
        let queue = BlockPriorityQueue::with_retention(4, budget.clone());
        let original = make_block_with_work(11);
        assert!(queue.push(original.clone()).await);
        for _ in 0..10 { assert!(queue.try_push(original.clone())); }
        assert_eq!(queue.len().await, 1);
        let spare = budget.acquire().unwrap();
        assert!(queue.push_retained(RetainedBlock { block: original.clone(), slot: Some(spare) }).await);
        let mut signature = original.clone();
        signature.signature.0[0] ^= 1;
        let mut work = original.clone();
        work.work += 1;
        let mut content = original.clone();
        content.brn_spent += 1;
        for candidate in [&signature, &work, &content] {
            assert_eq!(candidate.hash, original.hash);
            assert!(queue.push(candidate.clone()).await);
        }
        assert!(budget.acquire().is_none());
        assert!(queue.push(original.clone()).await, "identical queued payload needs no new slot");
        assert_eq!(queue.len().await, 4);
        let mut delivered = Vec::new();
        delivered.push(queue.pop().await);
        delivered.push(queue.try_pop().await.unwrap());
        while let Some(candidate) = queue.try_pop_retained_now() { delivered.push(candidate.block); }
        assert_eq!(delivered.len(), 4);
        for candidate in [&original, &signature, &work, &content] {
            assert!(delivered.iter().any(|block| bincode::serialize(block).unwrap() == bincode::serialize(candidate).unwrap()));
        }
        assert!(queue.state.lock().await.pending_payloads.is_empty());
        assert!(queue.push(signature).await, "bad signature can arrive first");
        assert!(queue.push(original.clone()).await, "dequeue permits identical dependency retry");
        assert_eq!(queue.len().await, 2, "bad same-hash signature cannot hide the original");
        drop(queue.pop().await);
        drop(queue.pop().await);
        assert!(budget.acquire().is_some());
    }

}
