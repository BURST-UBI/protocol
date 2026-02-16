//! Statistics collection and reporting utilities.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

/// A thread-safe counter collection for protocol statistics.
pub struct StatsCounter {
    counters: HashMap<&'static str, AtomicU64>,
}

impl StatsCounter {
    pub fn new(names: &[&'static str]) -> Self {
        let mut counters = HashMap::new();
        for &name in names {
            counters.insert(name, AtomicU64::new(0));
        }
        Self { counters }
    }

    pub fn increment(&self, name: &str) {
        if let Some(counter) = self.counters.get(name) {
            counter.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn add(&self, name: &str, value: u64) {
        if let Some(counter) = self.counters.get(name) {
            counter.fetch_add(value, Ordering::Relaxed);
        }
    }

    pub fn get(&self, name: &str) -> u64 {
        self.counters
            .get(name)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    pub fn snapshot(&self) -> HashMap<&'static str, u64> {
        self.counters
            .iter()
            .map(|(&k, v)| (k, v.load(Ordering::Relaxed)))
            .collect()
    }
}
