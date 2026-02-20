//! Unchecked block queue — holds blocks whose dependencies are not yet known.
//!
//! When a block arrives referencing a previous block or a source (linked send)
//! block we haven't seen, it is stored here keyed by the missing dependency hash.
//! Once that dependency is confirmed, all waiting blocks are drained and
//! re-submitted to the block processor.

use burst_ledger::StateBlock;
use burst_types::BlockHash;
use std::collections::HashMap;

/// Reason a block is in the unchecked map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GapType {
    /// Block's `previous` hash is unknown.
    Previous,
    /// Block's `link` (source send block) is unknown (for receive blocks).
    Source,
}

/// A block waiting for its dependency to arrive.
#[derive(Clone, Debug)]
pub struct UncheckedEntry {
    /// The block awaiting its dependency.
    pub block: StateBlock,
    /// Unix timestamp (seconds) when this entry was received.
    pub received_at: u64,
}

/// Extended unchecked map that tracks both gap-previous and gap-source.
///
/// Maps `dependency_hash -> Vec<UncheckedEntry>`. When the missing block arrives,
/// dependents can be drained and re-processed.
pub struct UncheckedMap {
    /// Maps previous_hash → blocks waiting for it (gap-previous).
    entries: HashMap<BlockHash, Vec<UncheckedEntry>>,
    /// Maps source_hash → blocks waiting for the linked send block (gap-source).
    source_dependents: HashMap<BlockHash, Vec<UncheckedEntry>>,
    /// Total number of individual entries across all dependency keys.
    count: usize,
    /// Maximum total entries allowed (prevents memory exhaustion from spam).
    max_size: usize,
}

impl UncheckedMap {
    /// Create a new unchecked map with the given capacity limit.
    pub fn new(max_size: usize) -> Self {
        Self {
            entries: HashMap::new(),
            source_dependents: HashMap::new(),
            count: 0,
            max_size,
        }
    }

    /// Insert a block that depends on `dependency` (the hash of its missing previous block).
    ///
    /// Returns `true` if the entry was inserted, `false` if the map is full.
    pub fn insert(&mut self, dependency: BlockHash, block: StateBlock, now: u64) -> bool {
        if self.count >= self.max_size {
            return false;
        }
        let entry = UncheckedEntry {
            block,
            received_at: now,
        };
        self.entries.entry(dependency).or_default().push(entry);
        self.count += 1;
        true
    }

    /// Insert a block waiting for its source (linked send) block to arrive.
    ///
    /// Returns `true` if the entry was inserted, `false` if the map is full.
    pub fn insert_source(&mut self, source_hash: BlockHash, block: StateBlock, now: u64) -> bool {
        if self.count >= self.max_size {
            return false;
        }
        let entry = UncheckedEntry {
            block,
            received_at: now,
        };
        self.source_dependents
            .entry(source_hash)
            .or_default()
            .push(entry);
        self.count += 1;
        true
    }

    /// Drain all entries that were waiting for `hash` to arrive (gap-previous).
    ///
    /// Returns the blocks that can now be re-processed.
    pub fn get_dependents(&mut self, hash: &BlockHash) -> Vec<StateBlock> {
        match self.entries.remove(hash) {
            Some(entries) => {
                self.count -= entries.len();
                entries.into_iter().map(|e| e.block).collect()
            }
            None => Vec::new(),
        }
    }

    /// Drain all entries that were waiting for `source_hash` as their linked send
    /// block (gap-source).
    ///
    /// Returns the blocks that can now be re-processed.
    pub fn get_source_dependents(&mut self, source_hash: &BlockHash) -> Vec<StateBlock> {
        match self.source_dependents.remove(source_hash) {
            Some(entries) => {
                self.count -= entries.len();
                entries.into_iter().map(|e| e.block).collect()
            }
            None => Vec::new(),
        }
    }

    /// Total number of unchecked entries (both gap-previous and gap-source).
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether the unchecked map is empty.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Remove entries older than `max_age_secs` relative to `now`.
    ///
    /// Returns the number of entries removed.
    pub fn clear_expired(&mut self, max_age_secs: u64, now: u64) -> usize {
        let mut removed = 0;

        // Clear expired gap-previous entries
        self.entries.retain(|_dep, entries| {
            let before = entries.len();
            entries.retain(|e| {
                let age = now.saturating_sub(e.received_at);
                age < max_age_secs
            });
            let after = entries.len();
            removed += before - after;
            !entries.is_empty()
        });

        // Clear expired gap-source entries
        self.source_dependents.retain(|_dep, entries| {
            let before = entries.len();
            entries.retain(|e| {
                let age = now.saturating_sub(e.received_at);
                age < max_age_secs
            });
            let after = entries.len();
            removed += before - after;
            !entries.is_empty()
        });

        self.count -= removed;
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_ledger::{BlockType, StateBlock, CURRENT_BLOCK_VERSION};
    use burst_types::{BlockHash, Signature, Timestamp, TxHash, WalletAddress};

    fn test_account() -> WalletAddress {
        WalletAddress::new(
            "brst_1111111111111111111111111111111111111111111111111111111111111111111",
        )
    }

    fn test_representative() -> WalletAddress {
        WalletAddress::new(
            "brst_2222222222222222222222222222222222222222222222222222222222222222222",
        )
    }

    fn make_block(previous: BlockHash) -> StateBlock {
        let mut block = StateBlock {
            version: CURRENT_BLOCK_VERSION,
            block_type: BlockType::Send,
            account: test_account(),
            previous,
            representative: test_representative(),
            brn_balance: 100,
            trst_balance: 50,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1000),
            work: 0,
            signature: Signature([1u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block
    }

    #[test]
    fn insert_and_retrieve_dependents() {
        let mut map = UncheckedMap::new(100);
        let dep = BlockHash::new([0xAA; 32]);
        let block = make_block(dep);

        assert!(map.insert(dep, block.clone(), 1000));
        assert_eq!(map.len(), 1);

        let dependents = map.get_dependents(&dep);
        assert_eq!(dependents.len(), 1);
        assert_eq!(dependents[0].hash, block.hash);
        assert_eq!(map.len(), 0);
    }

    #[test]
    fn multiple_dependents_for_same_hash() {
        let mut map = UncheckedMap::new(100);
        let dep = BlockHash::new([0xBB; 32]);

        let b1 = make_block(dep);
        let b2 = make_block(dep);

        assert!(map.insert(dep, b1, 1000));
        assert!(map.insert(dep, b2, 1001));
        assert_eq!(map.len(), 2);

        let dependents = map.get_dependents(&dep);
        assert_eq!(dependents.len(), 2);
        assert_eq!(map.len(), 0);
    }

    #[test]
    fn get_dependents_for_unknown_hash_returns_empty() {
        let mut map = UncheckedMap::new(100);
        let unknown = BlockHash::new([0xCC; 32]);
        assert!(map.get_dependents(&unknown).is_empty());
    }

    #[test]
    fn max_size_enforced() {
        let mut map = UncheckedMap::new(2);
        let dep = BlockHash::new([0xDD; 32]);

        assert!(map.insert(dep, make_block(dep), 1000));
        assert!(map.insert(dep, make_block(dep), 1001));
        assert!(!map.insert(dep, make_block(dep), 1002));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn clear_expired_removes_old_entries() {
        let mut map = UncheckedMap::new(100);
        let dep1 = BlockHash::new([0x01; 32]);
        let dep2 = BlockHash::new([0x02; 32]);

        map.insert(dep1, make_block(dep1), 100);
        map.insert(dep2, make_block(dep2), 500);
        assert_eq!(map.len(), 2);

        // At time 700, entries older than 300s are expired → dep1 (age=600) removed, dep2 (age=200) stays
        let removed = map.clear_expired(300, 700);
        assert_eq!(removed, 1);
        assert_eq!(map.len(), 1);

        // dep2 should still be retrievable
        let dependents = map.get_dependents(&dep2);
        assert_eq!(dependents.len(), 1);
    }

    #[test]
    fn clear_expired_removes_nothing_when_all_fresh() {
        let mut map = UncheckedMap::new(100);
        let dep = BlockHash::new([0x03; 32]);
        map.insert(dep, make_block(dep), 1000);

        let removed = map.clear_expired(300, 1100);
        assert_eq!(removed, 0);
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn is_empty_works() {
        let mut map = UncheckedMap::new(100);
        assert!(map.is_empty());

        let dep = BlockHash::new([0x04; 32]);
        map.insert(dep, make_block(dep), 1000);
        assert!(!map.is_empty());
    }

    // ── Gap-source tests ──────────────────────────────────────────────────

    fn make_receive_block(previous: BlockHash, source: BlockHash) -> StateBlock {
        let mut block = StateBlock {
            version: CURRENT_BLOCK_VERSION,
            block_type: BlockType::Receive,
            account: test_account(),
            previous,
            representative: test_representative(),
            brn_balance: 100,
            trst_balance: 150,
            link: source,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(2000),
            work: 0,
            signature: Signature([1u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block
    }

    #[test]
    fn insert_and_retrieve_source_dependents() {
        let mut map = UncheckedMap::new(100);
        let source = BlockHash::new([0xFA; 32]);
        let prev = BlockHash::new([0xFB; 32]);
        let block = make_receive_block(prev, source);

        assert!(map.insert_source(source, block.clone(), 1000));
        assert_eq!(map.len(), 1);

        let dependents = map.get_source_dependents(&source);
        assert_eq!(dependents.len(), 1);
        assert_eq!(dependents[0].hash, block.hash);
        assert_eq!(map.len(), 0);
    }

    #[test]
    fn multiple_source_dependents_for_same_hash() {
        let mut map = UncheckedMap::new(100);
        let source = BlockHash::new([0xFC; 32]);
        let prev1 = BlockHash::new([0xFD; 32]);
        let prev2 = BlockHash::new([0xFE; 32]);

        let b1 = make_receive_block(prev1, source);
        let b2 = make_receive_block(prev2, source);

        assert!(map.insert_source(source, b1, 1000));
        assert!(map.insert_source(source, b2, 1001));
        assert_eq!(map.len(), 2);

        let dependents = map.get_source_dependents(&source);
        assert_eq!(dependents.len(), 2);
        assert_eq!(map.len(), 0);
    }

    #[test]
    fn get_source_dependents_for_unknown_hash_returns_empty() {
        let mut map = UncheckedMap::new(100);
        let unknown = BlockHash::new([0xCC; 32]);
        assert!(map.get_source_dependents(&unknown).is_empty());
    }

    #[test]
    fn max_size_shared_between_previous_and_source() {
        let mut map = UncheckedMap::new(3);
        let dep = BlockHash::new([0xDD; 32]);
        let source = BlockHash::new([0xEE; 32]);

        assert!(map.insert(dep, make_block(dep), 1000));
        assert!(map.insert_source(source, make_block(dep), 1001));
        assert!(map.insert(dep, make_block(dep), 1002));
        // Map is now full (3 entries)
        assert!(!map.insert_source(source, make_block(dep), 1003));
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn clear_expired_removes_old_source_entries() {
        let mut map = UncheckedMap::new(100);
        let source1 = BlockHash::new([0x10; 32]);
        let source2 = BlockHash::new([0x20; 32]);
        let prev = BlockHash::new([0x30; 32]);

        map.insert_source(source1, make_receive_block(prev, source1), 100);
        map.insert_source(source2, make_receive_block(prev, source2), 500);
        assert_eq!(map.len(), 2);

        // At time 700, entries older than 300s → source1 (age=600) removed, source2 (age=200) stays
        let removed = map.clear_expired(300, 700);
        assert_eq!(removed, 1);
        assert_eq!(map.len(), 1);

        let dependents = map.get_source_dependents(&source2);
        assert_eq!(dependents.len(), 1);
    }

    #[test]
    fn gap_type_enum_equality() {
        assert_eq!(GapType::Previous, GapType::Previous);
        assert_eq!(GapType::Source, GapType::Source);
        assert_ne!(GapType::Previous, GapType::Source);
    }
}
