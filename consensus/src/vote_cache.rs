//! Vote cache — stores votes that arrive before their election is created.
//!
//! In a real network, votes can arrive out of order. A representative might
//! broadcast a vote for a block before the node has even seen the conflicting
//! fork. The vote cache holds these "early" votes so they're available when
//! the election starts.
//!
//! Unlike the simple list-based approach, this cache tracks per-voter
//! deduplication (replacing votes with higher timestamps), maintains running
//! tallies, enforces a per-hash voter limit, and expires stale entries via TTL.

use burst_types::{BlockHash, WalletAddress};
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

const MAX_CACHE_SIZE: usize = 65536;
const MAX_VOTERS_PER_HASH: usize = 64;
const VOTE_CACHE_TTL: Duration = Duration::from_secs(900);

struct CachedVote {
    voter: WalletAddress,
    weight: u128,
    timestamp: u64,
    is_final: bool,
    arrived: Instant,
}

struct CacheEntry {
    votes: Vec<CachedVote>,
    tally: u128,
    final_tally: u128,
}

/// Pre-election vote storage with per-voter deduplication and running tallies.
///
/// Votes are keyed by block hash. When an election starts for that hash,
/// all cached votes are drained and replayed into the new election.
/// If the block already has an active election, votes go directly to
/// the election — they should NOT be inserted here.
pub struct VoteCache {
    entries: HashMap<BlockHash, CacheEntry>,
    /// Time index: maps vote arrival instants to hashes that received
    /// votes at that time. Enables O(log n) eviction by iterating only
    /// entries older than the TTL cutoff.
    time_index: BTreeMap<Instant, Vec<BlockHash>>,
}

impl VoteCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            time_index: BTreeMap::new(),
        }
    }

    /// Cache a vote for a block hash.
    ///
    /// Handles duplicate voters by replacing if the new timestamp is higher.
    /// Enforces a per-hash voter limit by evicting the lowest-weight voter.
    /// Triggers TTL cleanup when the cache exceeds capacity.
    pub fn insert(
        &mut self,
        hash: BlockHash,
        voter: WalletAddress,
        weight: u128,
        timestamp: u64,
        is_final: bool,
    ) {
        if self.entries.len() >= MAX_CACHE_SIZE {
            self.cleanup();
        }

        let entry = self.entries.entry(hash).or_insert_with(|| CacheEntry {
            votes: Vec::new(),
            tally: 0,
            final_tally: 0,
        });

        if let Some(existing) = entry.votes.iter_mut().find(|v| v.voter == voter) {
            if timestamp > existing.timestamp {
                entry.tally -= existing.weight;
                if existing.is_final {
                    entry.final_tally -= existing.weight;
                }
                existing.weight = weight;
                existing.timestamp = timestamp;
                existing.is_final = is_final;
                existing.arrived = Instant::now();
                entry.tally += weight;
                if is_final {
                    entry.final_tally += weight;
                }
            }
            return;
        }

        if entry.votes.len() >= MAX_VOTERS_PER_HASH {
            if let Some(min_idx) = entry
                .votes
                .iter()
                .enumerate()
                .min_by_key(|(_, v)| v.weight)
                .map(|(i, _)| i)
            {
                if weight > entry.votes[min_idx].weight {
                    let removed = entry.votes.remove(min_idx);
                    entry.tally -= removed.weight;
                    if removed.is_final {
                        entry.final_tally -= removed.weight;
                    }
                } else {
                    return;
                }
            }
        }

        let now = Instant::now();
        entry.tally += weight;
        if is_final {
            entry.final_tally += weight;
        }
        entry.votes.push(CachedVote {
            voter,
            weight,
            timestamp,
            is_final,
            arrived: now,
        });
        self.time_index.entry(now).or_default().push(hash);
    }

    /// Get and remove all cached votes for a block hash (called when election starts).
    pub fn drain(&mut self, hash: &BlockHash) -> Vec<(WalletAddress, u128, u64, bool)> {
        if let Some(entry) = self.entries.remove(hash) {
            entry
                .votes
                .into_iter()
                .map(|v| (v.voter, v.weight, v.timestamp, v.is_final))
                .collect()
        } else {
            vec![]
        }
    }

    /// Get the tally for a block hash without removing.
    /// Returns `(total_tally, final_tally)`.
    pub fn tally(&self, hash: &BlockHash) -> (u128, u128) {
        self.entries
            .get(hash)
            .map(|e| (e.tally, e.final_tally))
            .unwrap_or((0, 0))
    }

    /// Get the top N hashes by accumulated vote weight, sorted descending.
    ///
    /// Used by the hinted scheduler to find which blocks have the most
    /// pre-election support.
    pub fn top(&self, n: usize) -> Vec<(BlockHash, u128)> {
        let mut entries: Vec<(BlockHash, u128)> = self
            .entries
            .iter()
            .map(|(hash, entry)| (*hash, entry.tally))
            .collect();
        entries.sort_by_key(|b| std::cmp::Reverse(b.1));
        entries.truncate(n);
        entries
    }

    /// Remove entries whose votes have all expired beyond the TTL.
    ///
    /// Uses the `time_index` BTreeMap to only inspect entries that
    /// actually have votes older than the cutoff — O(k log n) where
    /// k is the number of expired vote batches rather than O(n) over
    /// the entire cache.
    pub fn cleanup(&mut self) {
        let cutoff = Instant::now() - VOTE_CACHE_TTL;

        // Collect hashes that may have expired votes (via the time index).
        let mut candidate_hashes = std::collections::HashSet::new();
        let expired_keys: Vec<Instant> =
            self.time_index.range(..=cutoff).map(|(k, _)| *k).collect();

        for key in &expired_keys {
            if let Some(hashes) = self.time_index.remove(key) {
                for h in hashes {
                    candidate_hashes.insert(h);
                }
            }
        }

        // Prune expired votes only in candidate entries.
        for hash in candidate_hashes {
            let remove_entry = if let Some(entry) = self.entries.get_mut(&hash) {
                entry.votes.retain(|v| v.arrived > cutoff);
                entry.tally = entry.votes.iter().map(|v| v.weight).sum();
                entry.final_tally = entry
                    .votes
                    .iter()
                    .filter(|v| v.is_final)
                    .map(|v| v.weight)
                    .sum();
                entry.votes.is_empty()
            } else {
                false
            };

            if remove_entry {
                self.entries.remove(&hash);
            }
        }
    }

    /// Number of distinct block hashes with cached votes.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Total number of individual cached vote entries across all hashes.
    pub fn total_votes(&self) -> usize {
        self.entries.values().map(|e| e.votes.len()).sum()
    }
}

impl Default for VoteCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_hash(byte: u8) -> BlockHash {
        BlockHash::new([byte; 32])
    }

    fn make_voter(name: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{name}"))
    }

    #[test]
    fn new_cache_is_empty() {
        let cache = VoteCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.total_votes(), 0);
    }

    #[test]
    fn insert_single_vote() {
        let mut cache = VoteCache::new();
        cache.insert(make_hash(1), make_voter("alice"), 100, 1000, false);

        assert_eq!(cache.len(), 1);
        assert_eq!(cache.total_votes(), 1);
        assert_eq!(cache.tally(&make_hash(1)), (100, 0));
    }

    #[test]
    fn insert_final_vote_updates_final_tally() {
        let mut cache = VoteCache::new();
        cache.insert(make_hash(1), make_voter("alice"), 100, 1000, true);

        assert_eq!(cache.tally(&make_hash(1)), (100, 100));
    }

    #[test]
    fn multiple_voters_same_hash() {
        let mut cache = VoteCache::new();
        cache.insert(make_hash(1), make_voter("alice"), 100, 1000, false);
        cache.insert(make_hash(1), make_voter("bob"), 200, 1001, true);

        assert_eq!(cache.len(), 1);
        assert_eq!(cache.total_votes(), 2);
        assert_eq!(cache.tally(&make_hash(1)), (300, 200));
    }

    #[test]
    fn multiple_hashes() {
        let mut cache = VoteCache::new();
        cache.insert(make_hash(1), make_voter("alice"), 100, 1000, false);
        cache.insert(make_hash(2), make_voter("bob"), 200, 1001, false);

        assert_eq!(cache.len(), 2);
        assert_eq!(cache.tally(&make_hash(1)), (100, 0));
        assert_eq!(cache.tally(&make_hash(2)), (200, 0));
    }

    #[test]
    fn duplicate_voter_higher_timestamp_replaces() {
        let mut cache = VoteCache::new();
        cache.insert(make_hash(1), make_voter("alice"), 100, 1000, false);
        cache.insert(make_hash(1), make_voter("alice"), 300, 2000, true);

        assert_eq!(cache.total_votes(), 1);
        assert_eq!(cache.tally(&make_hash(1)), (300, 300));
    }

    #[test]
    fn duplicate_voter_lower_timestamp_ignored() {
        let mut cache = VoteCache::new();
        cache.insert(make_hash(1), make_voter("alice"), 100, 2000, false);
        cache.insert(make_hash(1), make_voter("alice"), 300, 1000, true);

        assert_eq!(cache.total_votes(), 1);
        // Original vote preserved
        assert_eq!(cache.tally(&make_hash(1)), (100, 0));
    }

    #[test]
    fn duplicate_voter_same_timestamp_ignored() {
        let mut cache = VoteCache::new();
        cache.insert(make_hash(1), make_voter("alice"), 100, 1000, false);
        cache.insert(make_hash(1), make_voter("alice"), 999, 1000, true);

        assert_eq!(cache.total_votes(), 1);
        assert_eq!(cache.tally(&make_hash(1)), (100, 0));
    }

    #[test]
    fn drain_returns_all_votes() {
        let mut cache = VoteCache::new();
        cache.insert(make_hash(1), make_voter("alice"), 100, 1000, false);
        cache.insert(make_hash(1), make_voter("bob"), 200, 1001, true);

        let votes = cache.drain(&make_hash(1));
        assert_eq!(votes.len(), 2);
        assert!(cache.is_empty());
        assert_eq!(cache.tally(&make_hash(1)), (0, 0));

        let alice_vote = votes.iter().find(|v| v.0 == make_voter("alice")).unwrap();
        assert_eq!(alice_vote.1, 100);
        assert_eq!(alice_vote.2, 1000);
        assert!(!alice_vote.3);

        let bob_vote = votes.iter().find(|v| v.0 == make_voter("bob")).unwrap();
        assert_eq!(bob_vote.1, 200);
        assert_eq!(bob_vote.2, 1001);
        assert!(bob_vote.3);
    }

    #[test]
    fn drain_nonexistent_returns_empty() {
        let mut cache = VoteCache::new();
        let votes = cache.drain(&make_hash(99));
        assert!(votes.is_empty());
    }

    #[test]
    fn drain_is_idempotent() {
        let mut cache = VoteCache::new();
        cache.insert(make_hash(1), make_voter("alice"), 100, 1000, false);

        let first = cache.drain(&make_hash(1));
        assert_eq!(first.len(), 1);

        let second = cache.drain(&make_hash(1));
        assert!(second.is_empty());
    }

    #[test]
    fn drain_doesnt_affect_other_hashes() {
        let mut cache = VoteCache::new();
        cache.insert(make_hash(1), make_voter("alice"), 100, 1000, false);
        cache.insert(make_hash(2), make_voter("bob"), 200, 1001, false);

        cache.drain(&make_hash(1));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.tally(&make_hash(2)), (200, 0));
    }

    #[test]
    fn tally_nonexistent_returns_zeros() {
        let cache = VoteCache::new();
        assert_eq!(cache.tally(&make_hash(99)), (0, 0));
    }

    #[test]
    fn tally_mixed_final_and_nonfinal() {
        let mut cache = VoteCache::new();
        cache.insert(make_hash(1), make_voter("alice"), 100, 1000, false);
        cache.insert(make_hash(1), make_voter("bob"), 200, 1001, true);
        cache.insert(make_hash(1), make_voter("carol"), 150, 1002, true);

        assert_eq!(cache.tally(&make_hash(1)), (450, 350));
    }

    #[test]
    fn top_returns_sorted_by_tally() {
        let mut cache = VoteCache::new();
        cache.insert(make_hash(1), make_voter("alice"), 100, 1000, false);
        cache.insert(make_hash(1), make_voter("bob"), 200, 1001, false);
        cache.insert(make_hash(2), make_voter("carol"), 500, 1002, false);
        cache.insert(make_hash(3), make_voter("dave"), 50, 1003, false);

        let top = cache.top(10);
        assert_eq!(top.len(), 3);
        assert_eq!(top[0], (make_hash(2), 500));
        assert_eq!(top[1], (make_hash(1), 300));
        assert_eq!(top[2], (make_hash(3), 50));
    }

    #[test]
    fn top_truncates_to_n() {
        let mut cache = VoteCache::new();
        cache.insert(make_hash(1), make_voter("a"), 100, 100, false);
        cache.insert(make_hash(2), make_voter("b"), 200, 101, false);
        cache.insert(make_hash(3), make_voter("c"), 300, 102, false);

        let top = cache.top(2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].0, make_hash(3));
        assert_eq!(top[1].0, make_hash(2));
    }

    #[test]
    fn top_empty_cache() {
        let cache = VoteCache::new();
        assert!(cache.top(10).is_empty());
    }

    #[test]
    fn replace_updates_tallies_correctly() {
        let mut cache = VoteCache::new();
        cache.insert(make_hash(1), make_voter("alice"), 100, 1000, true);
        assert_eq!(cache.tally(&make_hash(1)), (100, 100));

        // Replace with non-final at higher timestamp
        cache.insert(make_hash(1), make_voter("alice"), 250, 2000, false);
        assert_eq!(cache.tally(&make_hash(1)), (250, 0));
    }

    #[test]
    fn default_impl() {
        let cache = VoteCache::default();
        assert!(cache.is_empty());
    }
}
