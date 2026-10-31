//! Bounded pre-election storage for already authenticated votes.
//! Candidate validation and frozen-authority weighting happen at replay, not here.

use crate::VoteInfo;
use burst_types::{BlockHash, Timestamp, WalletAddress};
use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

const MAX_VOTES: usize = 65536;
const MAX_VOTERS_PER_HASH: usize = 64;
const VOTE_CACHE_TTL: Duration = Duration::from_secs(900);

#[derive(Clone, Debug)]
struct CacheEntry {
    votes: Vec<VoteInfo>,
    arrived: Instant,
}

/// At most 65536 votes globally and 64 voters per hash. Each bucket has exactly
/// one expiry-index entry; replacement cannot extend its retention indefinitely.
/// At a full bucket, a heavier voter can replace the lightest. At global capacity,
/// insertion evicts the oldest whole bucket. This is a lossy cache, not vote history.
#[derive(Clone, Debug)]
pub struct VoteCache {
    entries: HashMap<BlockHash, CacheEntry>,
    time_index: BTreeSet<(Instant, BlockHash)>,
    vote_count: usize,
}

impl VoteCache {
    pub fn new() -> Self {
        Self { entries: HashMap::new(), time_index: BTreeSet::new(), vote_count: 0 }
    }

    /// Legacy component interface. Wire callers must use insert_signed so the
    /// signed sequence is retained independently of the timestamp.
    pub fn insert(&mut self, hash: BlockHash, voter: WalletAddress, weight: u128,
        timestamp: u64, is_final: bool) {
        self.insert_signed(VoteInfo::new(voter, hash, weight, is_final,
            Timestamp::new(timestamp), timestamp.max(1)));
    }

    /// Signature verification is the caller's responsibility. This cache never
    /// grants authority or confirms a block. Final votes obey Election ordering.
    pub fn insert_signed(&mut self, vote: VoteInfo) {
        self.insert_at(vote, Instant::now());
    }

    fn insert_at(&mut self, vote: VoteInfo, now: Instant) {
        if vote.sequence == 0 || vote.weight == 0 { return; }
        let hash = vote.block_hash;
        if self.entries.get(&hash).is_some_and(|e| now.duration_since(e.arrived) >= VOTE_CACHE_TTL) {
            self.remove(&hash);
        }
        if let Some(entry) = self.entries.get_mut(&hash) {
            if let Some(old) = entry.votes.iter_mut().find(|v| v.voter == vote.voter) {
                if !old.is_final && (vote.is_final || vote.sequence > old.sequence) { *old = vote; }
                return;
            }
            if entry.votes.len() == MAX_VOTERS_PER_HASH {
                let (index, smallest) = entry.votes.iter().enumerate().min_by_key(|(_, v)| v.weight).unwrap();
                if vote.weight > smallest.weight { entry.votes[index] = vote; }
                return;
            }
        }
        // Evict one whole oldest bucket, without scanning the cache or running
        // an unbounded expiry sweep on the ingress path.
        if self.vote_count == MAX_VOTES {
            if let Some((_, oldest)) = self.time_index.first().copied() { self.remove(&oldest); }
        }
        let entry = self.entries.entry(hash).or_insert_with(|| {
            self.time_index.insert((now, hash));
            CacheEntry { votes: Vec::new(), arrived: now }
        });
        entry.votes.push(vote);
        self.vote_count += 1;
    }

    fn remove(&mut self, hash: &BlockHash) -> Option<CacheEntry> {
        let entry = self.entries.remove(hash)?;
        self.time_index.remove(&(entry.arrived, *hash));
        self.vote_count -= entry.votes.len();
        Some(entry)
    }

    pub fn drain_signed(&mut self, hash: &BlockHash) -> Vec<VoteInfo> {
        self.drain_at(hash, Instant::now())
    }

    fn drain_at(&mut self, hash: &BlockHash, now: Instant) -> Vec<VoteInfo> {
        self.remove(hash).filter(|e| now.duration_since(e.arrived) < VOTE_CACHE_TTL)
            .map_or_else(Vec::new, |e| e.votes)
    }

    pub fn drain(&mut self, hash: &BlockHash) -> Vec<(WalletAddress, u128, u64, bool)> {
        self.drain_signed(hash).into_iter()
            .map(|v| (v.voter, v.weight, v.timestamp.as_secs(), v.is_final)).collect()
    }

    /// Cached combined and final weights. Does not filter expired buckets or
    /// refresh weights from authority; callers use these values only as hints.
    pub fn tally(&self, hash: &BlockHash) -> (u128, u128) {
        self.entries.get(hash).map_or((0, 0), |e| e.votes.iter().fold((0u128, 0u128), |(all, final_), v|
            (all.saturating_add(v.weight), final_.saturating_add(if v.is_final { v.weight } else { 0 }))))
    }

    /// Sort all cached hashes by combined weight, then retain at most `n`.
    /// Expired buckets remain visible until maintenance; ties are unspecified.
    pub fn top(&self, n: usize) -> Vec<(BlockHash, u128)> {
        let mut entries: Vec<_> = self.entries.keys().map(|hash| (*hash, self.tally(hash).0)).collect();
        entries.sort_by_key(|b| std::cmp::Reverse(b.1));
        entries.truncate(n);
        entries
    }

    /// Explicit maintenance; ingress and drain do not scan unrelated entries.
    pub fn cleanup(&mut self) {
        let now = Instant::now();
        while let Some(&(arrived, hash)) = self.time_index.first() {
            if now.duration_since(arrived) < VOTE_CACHE_TTL { break; }
            self.remove(&hash);
        }
    }

    pub fn len(&self) -> usize { self.entries.len() }
    pub fn is_empty(&self) -> bool { self.entries.is_empty() }
    pub fn total_votes(&self) -> usize { self.vote_count }
}

impl Default for VoteCache {
    fn default() -> Self { Self::new() }
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
    fn signed_ordering_and_finality_survive_cache_replay() {
        let mut cache = VoteCache::new();
        let hash = make_hash(1);
        let vote = |sequence, timestamp, final_| VoteInfo::new(make_voter("alice"), hash,
            100, final_, Timestamp::new(timestamp), sequence);
        cache.insert_signed(vote(0, 100, true));
        assert!(cache.is_empty());
        cache.insert_signed(vote(10, 100, false));
        cache.insert_signed(vote(9, 200, false));
        cache.insert_signed(vote(11, 50, false));
        let soft = cache.drain_signed(&hash).pop().unwrap();
        assert_eq!((soft.sequence, soft.timestamp.as_secs()), (11, 50));
        cache.insert_signed(soft);
        cache.insert_signed(vote(8, 25, true));
        cache.insert_signed(vote(12, 300, false));
        let final_ = cache.drain_signed(&hash).pop().unwrap();
        assert!(final_.is_final);
        assert_eq!((final_.sequence, final_.timestamp.as_secs()), (8, 25));
        assert!(cache.time_index.is_empty());
    }

    #[test]
    fn expiry_and_drain_remove_index_without_refresh_on_replacement() {
        let mut cache = VoteCache::new();
        let now = Instant::now();
        let hash = make_hash(1);
        let vote = |sequence| VoteInfo::new(make_voter("alice"), hash, 100,
            false, Timestamp::new(100), sequence);
        cache.insert_at(vote(1), now);
        cache.insert_at(vote(2), now + VOTE_CACHE_TTL / 2);
        assert_eq!(cache.time_index.len(), 1);
        assert!(cache.drain_at(&hash, now + VOTE_CACHE_TTL).is_empty());
        assert_eq!(cache.total_votes(), 0);
        assert!(cache.time_index.is_empty());
        for _ in 0..100 {
            cache.insert_at(vote(1), now);
            assert_eq!(cache.drain_at(&hash, now).len(), 1);
        }
        assert!(cache.time_index.is_empty());
    }

    #[test]
    fn global_vote_budget_and_per_hash_limit_are_strict() {
        let mut cache = VoteCache::new();
        let now = Instant::now();
        for n in 0..=MAX_VOTES {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&(n as u64).to_be_bytes());
            cache.insert_at(VoteInfo::new(make_voter("alice"), BlockHash::new(bytes),
                1, false, Timestamp::new(1), 1), now);
        }
        assert_eq!(cache.total_votes(), MAX_VOTES);
        assert_eq!(cache.time_index.len(), cache.len());
        assert!(!cache.entries.contains_key(&BlockHash::ZERO));
        let hash = make_hash(255);
        for n in 0..100 {
            cache.insert_at(VoteInfo::new(make_voter(&n.to_string()), hash,
                2, true, Timestamp::new(1), 1), now);
            assert!(cache.total_votes() <= MAX_VOTES);
        }
        assert_eq!(cache.drain_at(&hash, now).len(), MAX_VOTERS_PER_HASH);
        assert_eq!(cache.time_index.len(), cache.len());
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
    fn final_vote_supersedes_soft_even_with_lower_timestamp() {
        let mut cache = VoteCache::new();
        cache.insert(make_hash(1), make_voter("alice"), 100, 2000, false);
        cache.insert(make_hash(1), make_voter("alice"), 300, 1000, true);

        assert_eq!(cache.total_votes(), 1);
        // Matches Election: a final vote supersedes a soft vote.
        assert_eq!(cache.tally(&make_hash(1)), (300, 300));
    }

    #[test]
    fn soft_duplicate_sequence_is_ignored() {
        let mut cache = VoteCache::new();
        cache.insert(make_hash(1), make_voter("alice"), 100, 1000, false);
        cache.insert(make_hash(1), make_voter("alice"), 999, 1000, false);

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
    fn final_vote_cannot_be_downgraded() {
        let mut cache = VoteCache::new();
        cache.insert(make_hash(1), make_voter("alice"), 100, 1000, true);
        assert_eq!(cache.tally(&make_hash(1)), (100, 100));

        // A later soft vote cannot replace a final vote.
        cache.insert(make_hash(1), make_voter("alice"), 250, 2000, false);
        assert_eq!(cache.tally(&make_hash(1)), (100, 100));
    }

    #[test]
    fn default_impl() {
        let cache = VoteCache::default();
        assert!(cache.is_empty());
    }
}
