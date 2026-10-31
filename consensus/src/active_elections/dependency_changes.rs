use burst_types::BlockHash;
use std::collections::{HashMap, VecDeque};

/// Bounded evidence for cementation racing a dependency snapshot. Once evidence
/// is evicted, old snapshots conservatively retry; this never authorizes a vote.
#[derive(Clone, Debug)]
pub(super) struct DependencyChanges {
    generation: u64,
    floor: u64,
    latest: HashMap<BlockHash, u64>,
    order: VecDeque<(BlockHash, u64)>,
}

const CAPACITY: usize = 4096;

impl DependencyChanges {
    pub(super) fn new() -> Self {
        Self {
            generation: 0,
            floor: 0,
            latest: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    pub(super) fn reset(&mut self) {
        self.generation = self.generation.saturating_add(1);
        self.floor = self.generation;
        self.latest.clear();
        self.order.clear();
    }

    pub(super) fn record(&mut self, hashes: &[BlockHash]) {
        if hashes.is_empty() {
            return;
        }
        self.generation = self.generation.saturating_add(1);
        for &hash in hashes {
            if self.order.len() == CAPACITY {
                let (old_hash, old_generation) = self.order.pop_front().unwrap();
                self.floor = self.floor.max(old_generation);
                if self.latest.get(&old_hash) == Some(&old_generation) {
                    self.latest.remove(&old_hash);
                }
            }
            self.latest.insert(hash, self.generation);
            self.order.push_back((hash, self.generation));
        }
    }

    pub(super) fn changed_since<'a>(
        &self,
        generation: u64,
        dependencies: impl IntoIterator<Item = &'a BlockHash>,
    ) -> bool {
        if self.generation == u64::MAX || generation > self.generation || generation < self.floor {
            return true;
        }
        dependencies.into_iter().any(|hash| {
            self.latest
                .get(hash)
                .is_some_and(|&changed| changed > generation)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hash(n: usize) -> BlockHash {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&(n as u64).to_le_bytes());
        BlockHash::new(bytes)
    }

    #[test]
    fn bounded_history_retains_relevant_races_and_falls_back_after_eviction() {
        let mut changes = DependencyChanges::new();
        let before = changes.generation();
        changes.record(&[hash(1)]);
        assert!(changes.changed_since(before, [&hash(1)]));
        assert!(!changes.changed_since(before, [&hash(2)]));
        let after = changes.generation();
        for n in 2..=CAPACITY + 1 {
            changes.record(&[hash(n)]);
        }
        assert_eq!(changes.order.len(), CAPACITY);
        assert_eq!(changes.latest.len(), CAPACITY);
        assert!(changes.changed_since(before, [&hash(0)]));
        assert!(!changes.changed_since(after, [&hash(0)]));
        changes.record(&[hash(1)]);
        assert!(changes.changed_since(after, [&hash(1)]));
    }

    #[test]
    fn repeated_hashes_large_batches_reset_and_generation_exhaustion_are_conservative() {
        let mut changes = DependencyChanges::new();
        changes.record(&[hash(1)]);
        let before = changes.generation();
        changes.record(&[hash(1)]);
        for n in 2..=CAPACITY {
            changes.record(&[hash(n)]);
        }
        assert!(changes.changed_since(before, [&hash(1)]));
        assert_eq!(changes.latest.len(), CAPACITY);
        let before = changes.generation();
        changes.record(&vec![hash(9); CAPACITY + 1]);
        assert_eq!(changes.order.len(), CAPACITY);
        assert_eq!(changes.latest.len(), 1);
        assert!(changes.changed_since(before, [&hash(0)]));
        let before = changes.generation();
        changes.reset();
        assert!(changes.latest.is_empty() && changes.order.is_empty());
        assert!(changes.changed_since(before, [&hash(0)]));
        assert!(!changes.changed_since(changes.generation(), [&hash(0)]));
        changes.generation = u64::MAX - 1;
        changes.record(&[hash(9)]);
        assert!(changes.changed_since(u64::MAX, [&hash(0)]));
        changes.reset();
        assert!(changes.changed_since(u64::MAX, [&hash(0)]));
    }
}
