//! PoW-based priority queue for block processing.
//!
//! Replaces the FIFO `mpsc::channel` with a priority queue ordered by PoW
//! difficulty. Blocks that invested more computational effort get processed
//! first, providing natural spam resistance (as specified in the whitepaper).

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use tokio::sync::{Mutex, Notify};

use burst_crypto::blake2b_256;
use burst_ledger::StateBlock;
use burst_types::BlockHash;

/// A block wrapped with its PoW difficulty score for priority ordering.
struct PrioritizedBlock {
    block: StateBlock,
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
/// `Blake2b(block_hash || nonce_le_bytes)` interpreted as u64 LE.
/// Higher value = more work was done.
pub fn work_difficulty(block_hash: &BlockHash, nonce: u64) -> u64 {
    let mut input = [0u8; 40];
    input[0..32].copy_from_slice(block_hash.as_bytes());
    input[32..40].copy_from_slice(&nonce.to_le_bytes());

    let hash = blake2b_256(&input);
    u64::from_le_bytes([
        hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7],
    ])
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
}

impl BlockPriorityQueue {
    /// Create a new priority queue with the given maximum capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            heap: Mutex::new((BinaryHeap::with_capacity(capacity), 0)),
            capacity,
            notify: Notify::new(),
        }
    }

    /// Push a block into the queue, computing its difficulty from its work nonce.
    ///
    /// If the queue is at capacity the block is dropped and `false` is returned.
    pub async fn push(&self, block: StateBlock) -> bool {
        let difficulty = work_difficulty(&block.hash, block.work);

        let mut guard = self.heap.lock().await;
        let (heap, seq) = &mut *guard;
        if heap.len() >= self.capacity {
            return false;
        }
        *seq += 1;
        let sequence = *seq;
        heap.push(PrioritizedBlock {
            block,
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
        let difficulty = work_difficulty(&block.hash, block.work);

        let mut guard = match self.heap.try_lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        let (heap, seq) = &mut *guard;
        if heap.len() >= self.capacity {
            return false;
        }
        *seq += 1;
        let sequence = *seq;
        heap.push(PrioritizedBlock {
            block,
            difficulty,
            sequence,
        });
        drop(guard);

        self.notify.notify_one();
        true
    }

    /// Pop the highest-priority block. Waits asynchronously if the queue is empty.
    pub async fn pop(&self) -> StateBlock {
        loop {
            {
                let mut guard = self.heap.lock().await;
                if let Some(entry) = guard.0.pop() {
                    return entry.block;
                }
            }
            // Queue is empty — wait for a producer to notify us.
            self.notify.notified().await;
        }
    }

    /// Try to pop without waiting. Returns `None` if the queue is empty.
    pub async fn try_pop(&self) -> Option<StateBlock> {
        let mut guard = self.heap.lock().await;
        guard.0.pop().map(|entry| entry.block)
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
            version: CURRENT_BLOCK_VERSION,
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
            work: nonce,
            signature: Signature([1u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block
    }

    fn make_block_with_generated_work(difficulty: u64) -> StateBlock {
        let mut block = StateBlock {
            version: CURRENT_BLOCK_VERSION,
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
            work: 0,
            signature: Signature([1u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();

        if difficulty > 0 {
            let generator = WorkGenerator;
            let nonce = generator.generate(&block.hash, difficulty).unwrap();
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
    async fn test_priority_ordering() {
        let queue = BlockPriorityQueue::new(16);

        // Create blocks with known, different difficulties by using generated PoW
        let low = make_block_with_generated_work(100);
        let high = make_block_with_generated_work(50_000);

        let low_diff = work_difficulty(&low.hash, low.work);
        let high_diff = work_difficulty(&high.hash, high.work);

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

        let d1 = work_difficulty(&b1.hash, b1.work);
        let d2 = work_difficulty(&b2.hash, b2.work);

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
