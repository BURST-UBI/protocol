//! Blocks awaiting a missing predecessor or another ledger dependency.
//!
//! Callers insert and drain dependency buckets; the queue does not establish
//! availability or confirmation. Drained blocks must pass admission again.

use crate::block_retention::{BlockRetention, RetainedBlock, Slot};
use burst_ledger::StateBlock;
use burst_types::BlockHash;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Age cutoff used by timed drains; cleanup accepts its own cutoff.
pub(crate) const MAX_AGE_SECS: u64 = 15 * 60;

/// Reason a block is in the unchecked map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GapType {
    /// Block's `previous` hash is unknown.
    Previous,
    /// A non-previous ledger dependency is unknown, such as a receive source.
    Source,
}

/// A block waiting for its dependency to arrive.
#[derive(Clone, Debug)]
pub struct UncheckedEntry {
    /// The block awaiting its dependency.
    pub block: StateBlock,
    /// Unix timestamp (seconds) when this entry was received.
    pub received_at: u64,
    slot: Option<Slot>,
    _in_flight_payload: Option<Arc<crate::block_validation::InFlightPayload>>,
}

/// Unchecked map that tracks both gap-previous and gap-source.
///
/// Maps `dependency_hash -> Vec<UncheckedEntry>`. When the missing block arrives,
/// dependents can be drained and re-processed.
pub struct UncheckedMap {
    /// Maps previous_hash → blocks waiting for it (gap-previous).
    entries: HashMap<BlockHash, Vec<UncheckedEntry>>,
    /// Maps non-previous dependency hashes to their waiting blocks.
    source_dependents: HashMap<BlockHash, Vec<UncheckedEntry>>,
    queued: HashSet<BlockHash>,
    /// Total number of individual entries across all dependency keys.
    count: usize,
    /// Maximum entries across both maps; this is not a byte budget.
    max_size: usize,
    retention: Option<Arc<BlockRetention>>,
    pub(crate) admission_slot: Option<Slot>,
    pub(crate) admission_payload: Option<Arc<crate::block_validation::InFlightPayload>>,
}

impl UncheckedMap {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.source_dependents.clear();
        self.queued.clear();
        self.count = 0;
    }
    /// Create a new unchecked map with the given capacity limit.
    pub fn new(max_size: usize) -> Self {
        Self {
            entries: HashMap::new(),
            source_dependents: HashMap::new(),
            queued: HashSet::new(),
            count: 0,
            max_size,
            retention: None,
            admission_slot: None,
            admission_payload: None,
        }
    }

    pub(crate) fn with_retention(max_size: usize, retention: Arc<BlockRetention>) -> Self {
        let mut map = Self::new(max_size);
        map.retention = Some(retention);
        map
    }

    fn slot(&self) -> Option<Option<Slot>> {
        if let Some(slot) = &self.admission_slot {
            return Some(Some(slot.clone()));
        }
        match &self.retention {
            Some(budget) => budget.acquire().map(Some),
            None => Some(None),
        }
    }

    /// Insert a block that depends on `dependency` (the hash of its missing previous block).
    ///
    /// Duplicate arrivals are retained once and do not extend the expiry time.
    /// Deduplication uses the declared hash without authentication. Returns false
    /// when entry capacity or the shared retention budget is exhausted.
    pub fn insert(&mut self, dependency: BlockHash, block: StateBlock, now: u64) -> bool {
        if self.queued.contains(&block.hash) {
            return true;
        }
        if self.count >= self.max_size {
            return false;
        }
        let Some(slot) = self.slot() else {
            return false;
        };
        self.queued.insert(block.hash);
        let entry = UncheckedEntry {
            block,
            received_at: now,
            slot,
            _in_flight_payload: self.admission_payload.clone(),
        };
        self.entries.entry(dependency).or_default().push(entry);
        self.count += 1;
        true
    }

    /// Insert a block waiting for a non-previous dependency.
    ///
    /// Duplicate arrivals, including those waiting on a predecessor, do not
    /// allocate another entry or change its bucket/time. Returns false when
    /// entry capacity or the shared retention budget is exhausted.
    pub fn insert_source(&mut self, source_hash: BlockHash, block: StateBlock, now: u64) -> bool {
        if self.queued.contains(&block.hash) {
            return true;
        }
        if self.count >= self.max_size {
            return false;
        }
        let Some(slot) = self.slot() else {
            return false;
        };
        self.queued.insert(block.hash);
        let entry = UncheckedEntry {
            block,
            received_at: now,
            slot,
            _in_flight_payload: self.admission_payload.clone(),
        };
        self.source_dependents
            .entry(source_hash)
            .or_default()
            .push(entry);
        self.count += 1;
        true
    }

    /// Drain a predecessor bucket without filtering age. Returning bare blocks
    /// drops their retention permits; use take_retained to carry permits onward.
    pub fn take_dependents(&mut self, hash: &BlockHash) -> Vec<StateBlock> {
        self.take_entries(hash, GapType::Previous, None)
    }

    /// Drain a non-previous dependency bucket without filtering age or carrying
    /// retention permits into the returned bare blocks.
    pub fn take_source_dependents(&mut self, source_hash: &BlockHash) -> Vec<StateBlock> {
        self.take_entries(source_hash, GapType::Source, None)
    }

    /// Drain one bucket and return blocks younger than MAX_AGE_SECS. Expired
    /// entries are removed too; returned bare blocks do not carry their permits.
    pub fn take_ready(&mut self, hash: &BlockHash, gap: GapType, now: u64) -> Vec<StateBlock> {
        self.take_entries(hash, gap, Some(now))
    }

    fn take_entries(
        &mut self,
        hash: &BlockHash,
        gap: GapType,
        now: Option<u64>,
    ) -> Vec<StateBlock> {
        self.take_retained(hash, gap, now)
            .into_iter()
            .map(|entry| entry.block)
            .collect()
    }

    /// Drain a whole bucket, optionally filtering age, and transfer live permits.
    /// Payload-dedup guards are dropped so retries can authenticate again.
    pub(crate) fn take_retained(
        &mut self,
        hash: &BlockHash,
        gap: GapType,
        now: Option<u64>,
    ) -> Vec<RetainedBlock> {
        let entries = match gap {
            GapType::Previous => self.entries.remove(hash),
            GapType::Source => self.source_dependents.remove(hash),
        }
        .unwrap_or_default();
        self.count -= entries.len();
        entries
            .into_iter()
            .filter_map(|entry| {
                self.queued.remove(&entry.block.hash);
                now.is_none_or(|now| now.saturating_sub(entry.received_at) < MAX_AGE_SECS)
                    .then_some(RetainedBlock {
                        block: entry.block,
                        slot: entry.slot,
                    })
            })
            .collect()
    }

    /// Total number of unchecked entries (both gap-previous and gap-source).
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether the unchecked map is empty.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Scan both maps and remove entries aged at least max_age_secs. Clock
    /// rewind saturates age to zero. Returns the number removed.
    pub fn clear_expired(&mut self, max_age_secs: u64, now: u64) -> usize {
        let mut removed = 0;
        let queued = &mut self.queued;

        self.entries.retain(|_dep, entries| {
            let before = entries.len();
            entries.retain(|e| {
                let age = now.saturating_sub(e.received_at);
                if age >= max_age_secs {
                    queued.remove(&e.block.hash);
                }
                age < max_age_secs
            });
            let after = entries.len();
            removed += before - after;
            !entries.is_empty()
        });

        self.source_dependents.retain(|_dep, entries| {
            let before = entries.len();
            entries.retain(|e| {
                let age = now.saturating_sub(e.received_at);
                if age >= max_age_secs {
                    queued.remove(&e.block.hash);
                }
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
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Send,
            account: test_account(),
            previous,
            representative: test_representative(),
            brn_spent: 100,
            trst_balance: 50,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1000),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
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

        let dependents = map.take_dependents(&dep);
        assert_eq!(dependents.len(), 1);
        assert_eq!(dependents[0].hash, block.hash);
        assert_eq!(map.len(), 0);
    }

    #[test]
    fn multiple_dependents_for_same_hash() {
        let mut map = UncheckedMap::new(100);
        let dep = BlockHash::new([0xBB; 32]);

        let b1 = make_block(dep);
        let mut b2 = make_block(dep);
        b2.timestamp = Timestamp::new(1001);
        b2.hash = b2.compute_hash();

        assert!(map.insert(dep, b1, 1000));
        assert!(map.insert(dep, b2, 1001));
        assert_eq!(map.len(), 2);

        let dependents = map.take_dependents(&dep);
        assert_eq!(dependents.len(), 2);
        assert_eq!(map.len(), 0);
    }

    #[test]
    fn take_dependents_for_unknown_hash_returns_empty() {
        let mut map = UncheckedMap::new(100);
        let unknown = BlockHash::new([0xCC; 32]);
        assert!(map.take_dependents(&unknown).is_empty());
    }

    #[test]
    fn max_size_enforced() {
        let mut map = UncheckedMap::new(2);
        let dep = BlockHash::new([0xDD; 32]);

        for time in 1000..=1002 {
            let mut block = make_block(dep);
            block.timestamp = Timestamp::new(time);
            block.hash = block.compute_hash();
            assert_eq!(map.insert(dep, block, time), time < 1002);
        }
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

        // Ages are 600 and 200 seconds; only the first exceeds the cutoff.
        let removed = map.clear_expired(300, 700);
        assert_eq!(removed, 1);
        assert_eq!(map.len(), 1);

        let dependents = map.take_dependents(&dep2);
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

    #[test]
    fn selected_timeout_preserves_fresh_dependencies_and_expires_duplicates_at_fifteen_minutes() {
        assert_eq!(MAX_AGE_SECS, 900);
        let previous = BlockHash::new([21; 32]);
        let source = BlockHash::new([22; 32]);
        for source_gap in [false, true] {
            let mut map = UncheckedMap::new(2);
            let block = make_receive_block(previous, source);
            let dependency = if source_gap { source } else { previous };
            let insert = |map: &mut UncheckedMap, now| {
                if source_gap {
                    map.insert_source(dependency, block.clone(), now)
                } else {
                    map.insert(dependency, block.clone(), now)
                }
            };
            assert!(insert(&mut map, 1000));
            assert!(insert(&mut map, 1899));
            assert_eq!(map.len(), 1);
            assert_eq!(map.clear_expired(MAX_AGE_SECS, 1899), 0);
            let gap = if source_gap {
                GapType::Source
            } else {
                GapType::Previous
            };
            let ready = map.take_ready(&dependency, gap.clone(), 1899);
            assert_eq!(ready.len(), 1);
            assert_eq!(ready[0].hash, block.hash);
            assert!(insert(&mut map, 2000));
            assert!(insert(&mut map, 2899));
            assert_eq!(map.clear_expired(MAX_AGE_SECS, 2900), 1);
            assert!(map.is_empty());
            assert!(map.take_dependents(&dependency).is_empty());
            assert!(map.take_source_dependents(&dependency).is_empty());
            assert!(insert(&mut map, 2901));
            assert!(map.take_ready(&dependency, gap, 3801).is_empty());
            assert!(map.is_empty());
            assert!(insert(&mut map, 3802));
        }
    }


    fn make_receive_block(previous: BlockHash, source: BlockHash) -> StateBlock {
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Receive,
            account: test_account(),
            previous,
            representative: test_representative(),
            brn_spent: 100,
            trst_balance: 150,
            link: source,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(2000),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
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

        let dependents = map.take_source_dependents(&source);
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

        let dependents = map.take_source_dependents(&source);
        assert_eq!(dependents.len(), 2);
        assert_eq!(map.len(), 0);
    }

    #[test]
    fn take_source_dependents_for_unknown_hash_returns_empty() {
        let mut map = UncheckedMap::new(100);
        let unknown = BlockHash::new([0xCC; 32]);
        assert!(map.take_source_dependents(&unknown).is_empty());
    }

    #[test]
    fn max_size_shared_between_previous_and_source() {
        let mut map = UncheckedMap::new(3);
        let dep = BlockHash::new([0xDD; 32]);
        let source = BlockHash::new([0xEE; 32]);

        for time in 1000..=1003 {
            let mut block = make_block(dep);
            block.timestamp = Timestamp::new(time);
            block.hash = block.compute_hash();
            let retained = if time % 2 == 0 {
                map.insert(dep, block, time)
            } else {
                map.insert_source(source, block, time)
            };
            assert_eq!(retained, time < 1003);
        }
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

        // Ages are 600 and 200 seconds; only the first exceeds the cutoff.
        let removed = map.clear_expired(300, 700);
        assert_eq!(removed, 1);
        assert_eq!(map.len(), 1);

        let dependents = map.take_source_dependents(&source2);
        assert_eq!(dependents.len(), 1);
    }

    #[test]
    fn gap_type_enum_equality() {
        assert_eq!(GapType::Previous, GapType::Previous);
        assert_eq!(GapType::Source, GapType::Source);
        assert_ne!(GapType::Previous, GapType::Source);
    }

    #[test]
    fn duplicate_gap_arrivals_cannot_fill_the_queue_or_refresh_their_expiry() {
        let mut map = UncheckedMap::new(2);
        let dependency = BlockHash::new([1; 32]);
        let source = BlockHash::new([2; 32]);
        let block = make_receive_block(dependency, source);
        assert!(map.insert(dependency, block.clone(), 100));
        for now in 101..200 {
            assert!(map.insert(dependency, block.clone(), now));
            assert!(map.insert_source(source, block.clone(), now));
        }
        assert_eq!(map.len(), 1);
        assert_eq!(map.clear_expired(100, 200), 1);
        assert!(map.is_empty());
        assert!(map.queued.is_empty());
        assert!(map.insert_source(source, block.clone(), 201));
        assert_eq!(map.take_source_dependents(&source).len(), 1);
        assert!(map.queued.is_empty());
        assert!(map.insert(dependency, block, 202));
        assert_eq!(map.take_dependents(&dependency).len(), 1);
        assert!(map.queued.is_empty());
    }
}
