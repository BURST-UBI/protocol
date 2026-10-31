//! Weighted round-robin between origins, priority order within each origin.
use std::collections::{BTreeMap, BinaryHeap, VecDeque};

/// Unbounded queue whose caller defines origins, item priority and admission.
/// Each active origin receives up to its weight in consecutive pops per turn.
/// Within an origin the largest `Ord` item wins; FIFO ties require a tie-breaker
/// in the item type. Weights count items, not bytes or processing time.
pub struct FairQueue<K, T> {
    queues: BTreeMap<K, BinaryHeap<T>>,
    active: VecDeque<(K, usize, usize)>,
    len: usize,
}

impl<K: Ord + Clone, T: Ord> Default for FairQueue<K, T> {
    fn default() -> Self {
        Self {
            queues: BTreeMap::new(),
            active: VecDeque::new(),
            len: 0,
        }
    }
}

impl<K: Ord + Clone, T: Ord> FairQueue<K, T> {
    /// Retain an item. A newly active key gets weight clamped to 1..=16;
    /// pushes to an already active key keep its existing weight until it empties.
    /// The caller enforces capacity before calling.
    pub fn push(&mut self, key: K, item: T, weight: usize) {
        let weight = weight.clamp(1, 16);
        if !self.queues.contains_key(&key) {
            self.active.push_back((key.clone(), weight, weight));
        }
        self.queues.entry(key).or_default().push(item);
        self.len += 1;
    }

    pub fn pop(&mut self) -> Option<(K, T)> {
        let (key, remaining, weight) = self.active.pop_front()?;
        let queue = self.queues.get_mut(&key).expect("active origin exists");
        let item = queue.pop().expect("active origin has an item");
        if queue.is_empty() {
            self.queues.remove(&key);
        } else if remaining > 1 {
            self.active.push_front((key.clone(), remaining - 1, weight));
        } else {
            self.active.push_back((key.clone(), weight, weight));
        }
        self.len -= 1;
        Some((key, item))
    }

    /// Inspect the next scheduled item, not the globally highest priority.
    pub fn peek(&self) -> Option<(&K, &T)> {
        let (key, _, _) = self.active.front()?;
        self.queues.get(key)?.peek().map(|item| (key, item))
    }

    pub fn len_for(&self, key: &K) -> usize {
        self.queues.get(key).map_or(0, BinaryHeap::len)
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn high_priority_flood_cannot_starve_another_origin() {
        let mut q = FairQueue::default();
        for n in 100..110 {
            q.push("flood", n, 1);
        }
        q.push("honest", 1, 1);
        assert_eq!(q.pop(), Some(("flood", 109)));
        q.push("flood", 1000, 1);
        assert_eq!(q.pop(), Some(("honest", 1)));
        assert_eq!(q.pop(), Some(("flood", 1000)));
    }
    #[test]
    fn bounded_weight_preserves_service_for_every_origin_and_reclaims_empty_keys() {
        let mut q = FairQueue::default();
        for n in 0..8 {
            q.push("local", n, 4);
            q.push("network", n, 1);
        }
        let served: Vec<_> = (0..5).map(|_| q.pop().unwrap().0).collect();
        assert_eq!(served, ["local", "local", "local", "local", "network"]);
        while q.pop().is_some() {}
        assert!(q.queues.is_empty());
        assert!(q.active.is_empty());
        assert_eq!(q.len(), 0);
    }
}
