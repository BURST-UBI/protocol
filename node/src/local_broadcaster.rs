//! Local block re-broadcaster — ensures locally created blocks reach consensus.
//! Re-broadcasts with exponential backoff until confirmed.

use burst_types::BlockHash;
use std::collections::HashMap;

/// Maximum locally tracked blocks.
const MAX_LOCAL_BLOCKS: usize = 1024;
/// Initial re-broadcast interval (ms).
const INITIAL_INTERVAL_MS: u64 = 1_000;
/// Maximum re-broadcast interval (ms).
const MAX_INTERVAL_MS: u64 = 60_000;
/// Maximum re-broadcasts per block.
const MAX_REBROADCASTS: u32 = 15;

pub struct LocalBroadcaster {
    /// Blocks created locally that haven't been confirmed yet.
    blocks: HashMap<BlockHash, LocalBlockEntry>,
    /// Maximum tracked blocks.
    max_entries: usize,
}

struct LocalBlockEntry {
    block_bytes: Vec<u8>,
    created_at_ms: u64,
    last_broadcast_ms: u64,
    broadcast_count: u32,
    interval_ms: u64,
}

impl LocalBroadcaster {
    pub fn new(max_entries: usize) -> Self {
        Self {
            blocks: HashMap::new(),
            max_entries,
        }
    }

    pub fn with_default() -> Self {
        Self::new(MAX_LOCAL_BLOCKS)
    }

    /// Track a locally created block for re-broadcasting.
    pub fn track(&mut self, hash: BlockHash, block_bytes: Vec<u8>, now_ms: u64) {
        if self.blocks.len() >= self.max_entries {
            // Evict oldest
            if let Some(oldest) = self
                .blocks
                .iter()
                .min_by_key(|(_, e)| e.created_at_ms)
                .map(|(h, _)| *h)
            {
                self.blocks.remove(&oldest);
            }
        }
        self.blocks.insert(
            hash,
            LocalBlockEntry {
                block_bytes,
                created_at_ms: now_ms,
                last_broadcast_ms: now_ms,
                broadcast_count: 1,
                interval_ms: INITIAL_INTERVAL_MS,
            },
        );
    }

    /// Get blocks that need re-broadcasting now.
    /// Returns (hash, block_bytes) for each block needing re-broadcast.
    pub fn blocks_needing_rebroadcast(&mut self, now_ms: u64) -> Vec<(BlockHash, Vec<u8>)> {
        let mut result = Vec::new();
        for (hash, entry) in &mut self.blocks {
            if entry.broadcast_count >= MAX_REBROADCASTS {
                continue;
            }
            if now_ms.saturating_sub(entry.last_broadcast_ms) >= entry.interval_ms {
                result.push((*hash, entry.block_bytes.clone()));
                entry.last_broadcast_ms = now_ms;
                entry.broadcast_count += 1;
                // Exponential backoff
                entry.interval_ms = (entry.interval_ms * 2).min(MAX_INTERVAL_MS);
            }
        }
        result
    }

    /// Mark a block as confirmed (stop re-broadcasting).
    pub fn confirmed(&mut self, hash: &BlockHash) {
        self.blocks.remove(hash);
    }

    /// Number of tracked blocks.
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Whether no blocks are being tracked.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Cleanup blocks that exceeded max retries.
    pub fn cleanup_expired(&mut self) {
        self.blocks
            .retain(|_, entry| entry.broadcast_count < MAX_REBROADCASTS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(n: u8) -> BlockHash {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        BlockHash::new(bytes)
    }

    #[test]
    fn track_and_confirm() {
        let mut broadcaster = LocalBroadcaster::with_default();
        let h = hash(1);
        broadcaster.track(h, vec![1, 2, 3], 1000);

        assert_eq!(broadcaster.len(), 1);
        assert!(!broadcaster.is_empty());

        broadcaster.confirmed(&h);
        assert_eq!(broadcaster.len(), 0);
        assert!(broadcaster.is_empty());
    }

    #[test]
    fn no_rebroadcast_before_interval() {
        let mut broadcaster = LocalBroadcaster::with_default();
        let h = hash(1);
        broadcaster.track(h, vec![1, 2, 3], 1000);

        // Only 500ms later — too soon for the 1000ms initial interval
        let result = broadcaster.blocks_needing_rebroadcast(1500);
        assert!(result.is_empty());
    }

    #[test]
    fn rebroadcast_after_interval() {
        let mut broadcaster = LocalBroadcaster::with_default();
        let h = hash(1);
        broadcaster.track(h, vec![1, 2, 3], 1000);

        // 1000ms later — interval met
        let result = broadcaster.blocks_needing_rebroadcast(2000);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, h);
        assert_eq!(result[0].1, vec![1, 2, 3]);
    }

    #[test]
    fn exponential_backoff() {
        let mut broadcaster = LocalBroadcaster::with_default();
        let h = hash(1);
        broadcaster.track(h, vec![42], 0);

        // First rebroadcast at 1000ms (initial interval)
        let result = broadcaster.blocks_needing_rebroadcast(1000);
        assert_eq!(result.len(), 1);

        // Next interval should be 2000ms (doubled)
        let result = broadcaster.blocks_needing_rebroadcast(2000);
        assert!(result.is_empty()); // only 1000ms since last

        let result = broadcaster.blocks_needing_rebroadcast(3000);
        assert_eq!(result.len(), 1); // 2000ms since last
    }

    #[test]
    fn max_rebroadcasts_honored() {
        let mut broadcaster = LocalBroadcaster::with_default();
        let h = hash(1);
        broadcaster.track(h, vec![42], 0);

        // Rapidly exhaust all rebroadcasts
        let mut time = 0u64;
        let mut count = 0;
        for _ in 0..100 {
            time += 100_000; // large jumps to always exceed interval
            let result = broadcaster.blocks_needing_rebroadcast(time);
            count += result.len();
        }

        // track() sets broadcast_count = 1, then MAX_REBROADCASTS - 1 more
        assert_eq!(count, (MAX_REBROADCASTS - 1) as usize);
    }

    #[test]
    fn confirm_unknown_block_is_noop() {
        let mut broadcaster = LocalBroadcaster::with_default();
        broadcaster.confirmed(&hash(99));
        assert!(broadcaster.is_empty());
    }

    #[test]
    fn eviction_when_full() {
        let mut broadcaster = LocalBroadcaster::new(3);

        broadcaster.track(hash(1), vec![1], 100);
        broadcaster.track(hash(2), vec![2], 200);
        broadcaster.track(hash(3), vec![3], 300);
        assert_eq!(broadcaster.len(), 3);

        // Adding a 4th should evict the oldest (hash(1), created at 100)
        broadcaster.track(hash(4), vec![4], 400);
        assert_eq!(broadcaster.len(), 3);
        // hash(1) was evicted
        assert!(broadcaster.blocks.get(&hash(1)).is_none());
        assert!(broadcaster.blocks.get(&hash(4)).is_some());
    }

    #[test]
    fn cleanup_expired_removes_exhausted_blocks() {
        let mut broadcaster = LocalBroadcaster::with_default();
        let h = hash(1);
        broadcaster.track(h, vec![42], 0);

        // Exhaust rebroadcasts
        let mut time = 0u64;
        for _ in 0..100 {
            time += 100_000;
            broadcaster.blocks_needing_rebroadcast(time);
        }

        assert_eq!(broadcaster.len(), 1);
        broadcaster.cleanup_expired();
        assert_eq!(broadcaster.len(), 0);
    }

    #[test]
    fn multiple_blocks_independent() {
        let mut broadcaster = LocalBroadcaster::with_default();
        broadcaster.track(hash(1), vec![1], 1000);
        broadcaster.track(hash(2), vec![2], 2000);

        // At 2000ms: hash(1) interval met, hash(2) just created
        let result = broadcaster.blocks_needing_rebroadcast(2000);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, hash(1));

        // At 3000ms: hash(2) interval met
        let result = broadcaster.blocks_needing_rebroadcast(3000);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, hash(2));
    }
}
