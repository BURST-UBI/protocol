//! PoW-based priority queue for block processing.
//!
//! Replaces the FIFO `mpsc::channel` with a priority queue ordered by PoW
//! difficulty. Blocks that invested more computational effort get processed
//! first, providing natural spam resistance (as specified in the whitepaper).

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use tokio::sync::{Mutex, Notify};

use crate::block_retention::{BlockRetention, RetainedBlock, Slot};
use burst_ledger::StateBlock;
use burst_types::BlockHash;
use std::sync::Arc;

/// A block wrapped with its PoW difficulty score for priority ordering.
struct PrioritizedBlock {
    block: StateBlock,
    slot: Option<Slot>,
    difficulty: u64,
    /// Insertion order counter for FIFO tiebreaking among equal difficulties.
    sequence: u64,
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
/// Higher value = more work was done.
pub fn work_difficulty(block_hash: &BlockHash, nonce: u64) -> u64 {
    burst_work::work_value(block_hash, nonce)
}

/// Thread-safe priority queue for blocks, ordered by PoW difficulty.
///
/// Producers call [`push`] to submit blocks; the consumer calls [`pop`] which
/// will `await` until a block is available (like an async channel, but ordered
/// by priority instead of FIFO).
pub struct BlockPriorityQueue {
    heap: Mutex<(BinaryHeap<PrioritizedBlock>, u64)>,
    capacity: usize,
    notify: Notify,
    retention: Option<Arc<BlockRetention>>,
}

impl BlockPriorityQueue {
    /// Create a new priority queue with the given maximum capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            heap: Mutex::new((BinaryHeap::with_capacity(capacity), 0)),
            capacity,
            notify: Notify::new(),
            retention: None,
        }
    }

    pub(crate) fn with_retention(capacity: usize, retention: Arc<BlockRetention>) -> Self {
        let mut queue = Self::new(capacity);
        queue.retention = Some(retention);
        queue
    }

    /// Push a block into the queue, computing its difficulty from its work nonce.
    ///
    /// If the queue is at capacity the block is dropped and `false` is returned.
    pub async fn push(&self, block: StateBlock) -> bool {
        self.push_retained(RetainedBlock { block, slot: None })
            .await
    }

    pub(crate) async fn push_retained(&self, mut retained: RetainedBlock) -> bool {
        let block = retained.block;
        let difficulty = work_difficulty(&block.work_root(), block.work);

        let mut guard = self.heap.lock().await;
        let (heap, seq) = &mut *guard;
        // A recovery transfer already owns a global slot. The smaller ingress
        // cap must not discard dependencies merely because they became ready.
        if heap.len() >= self.capacity && retained.slot.is_none() {
            return false;
        }
        if retained.slot.is_none() {
            if let Some(budget) = &self.retention {
                let Some(slot) = budget.acquire() else {
                    return false;
                };
                retained.slot = Some(slot);
            }
        }
        *seq += 1;
        let sequence = *seq;
        heap.push(PrioritizedBlock {
            block,
            slot: retained.slot,
            difficulty,
            sequence,
        });
        drop(guard);

        self.notify.notify_one();
        true
    }

    /// Non-async push using `try_lock`. Returns `false` if the lock is
    /// contended or the queue is at capacity.
    pub fn try_push(&self, block: StateBlock) -> bool {
        self.try_push_retained(RetainedBlock { block, slot: None })
    }

    pub(crate) fn try_push_retained(&self, mut retained: RetainedBlock) -> bool {
        let block = retained.block;
        let difficulty = work_difficulty(&block.work_root(), block.work);

        let mut guard = match self.heap.try_lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        let (heap, seq) = &mut *guard;
        if heap.len() >= self.capacity && retained.slot.is_none() {
            return false;
        }
        if retained.slot.is_none() {
            if let Some(budget) = &self.retention {
                let Some(slot) = budget.acquire() else {
                    return false;
                };
                retained.slot = Some(slot);
            }
        }
        *seq += 1;
        let sequence = *seq;
        heap.push(PrioritizedBlock {
            block,
            slot: retained.slot,
            difficulty,
            sequence,
        });
        drop(guard);

        self.notify.notify_one();
        true
    }

    /// Pop the highest-priority block. Waits asynchronously if the queue is empty.
    pub async fn pop(&self) -> StateBlock {
        self.pop_retained().await.block
    }

    pub(crate) async fn pop_retained(&self) -> RetainedBlock {
        loop {
            {
                let mut guard = self.heap.lock().await;
                if let Some(entry) = guard.0.pop() {
                    return RetainedBlock {
                        block: entry.block,
                        slot: entry.slot,
                    };
                }
            }
            // Queue is empty — wait for a producer to notify us.
            self.notify.notified().await;
        }
    }

    /// Try to pop without waiting. Returns `None` if the queue is empty.
    pub async fn try_pop(&self) -> Option<StateBlock> {
        self.try_pop_retained().await.map(|entry| entry.block)
    }

    pub(crate) async fn try_pop_retained(&self) -> Option<RetainedBlock> {
        let mut guard = self.heap.lock().await;
        guard.0.pop().map(|entry| RetainedBlock {
            block: entry.block,
            slot: entry.slot,
        })
    }

    /// Current number of blocks in the queue.
    pub async fn len(&self) -> usize {
        self.heap.lock().await.0.len()
    }

    /// Whether the queue is empty.
    pub async fn is_empty(&self) -> bool {
        self.heap.lock().await.0.is_empty()
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
            brn_balance: 1000,
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
            brn_balance: 1000,
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

    #[test]
    fn test_work_difficulty_computation() {
        let hash = BlockHash::new([0x42; 32]);
        let d0 = work_difficulty(&hash, 0);
        let d1 = work_difficulty(&hash, 1);
        // Different nonces produce different difficulty values
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

        // Create blocks with known, different difficulties by using generated PoW
        let low = make_block_with_generated_work(100);
        let high = make_block_with_generated_work(50_000);

        let low_diff = work_difficulty(&low.work_root(), low.work);
        let high_diff = work_difficulty(&high.work_root(), high.work);

        let low_hash = low.hash;
        let high_hash = high.hash;

        // Push low-priority first, then high-priority
        queue.push(low).await;
        queue.push(high).await;

        // Pop should return the higher difficulty block first
        let first = queue.pop().await;
        let second = queue.pop().await;

        if high_diff > low_diff {
            assert_eq!(first.hash, high_hash);
            assert_eq!(second.hash, low_hash);
        } else {
            // In the unlikely case difficulties happen to be equal or reversed,
            // just verify both were dequeued
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

        // Two blocks with work=0 (same difficulty for same hash, but different hashes)
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
}
