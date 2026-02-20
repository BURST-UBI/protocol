//! Representative crawler — discovers representatives by probing peers.
//!
//! Process:
//! 1. Periodically send `confirm_req` for a random confirmed block to connected peers
//! 2. When a vote response arrives, learn the peer's representative account
//! 3. Track which peers are representatives and their weight
//!
//! This is essential for the consensus layer to know how much voting weight
//! is reachable and to build the quorum picture.

use burst_types::{BlockHash, WalletAddress};
use std::collections::HashMap;

/// Discovers representatives by probing peers with confirm_req messages.
///
/// When multiple peers are connected, we periodically ask them to vote on
/// a known-confirmed block. Their vote response reveals their representative
/// account and weight, allowing the node to build a picture of reachable
/// voting weight.
pub struct RepCrawler {
    /// Peer ID -> discovered representative info
    discovered_reps: HashMap<String, DiscoveredRep>,
    /// Pending queries: query_hash -> (peer_id, sent_at)
    pending_queries: HashMap<BlockHash, Vec<(String, u64)>>,
    /// Query timeout in seconds
    query_timeout_secs: u64,
    /// Interval between crawl rounds in seconds
    crawl_interval_secs: u64,
    /// Last crawl timestamp
    last_crawl: u64,
    /// Whether we've reached sufficient quorum coverage
    sufficient_weight: bool,
}

/// Information about a discovered representative peer.
#[derive(Clone, Debug)]
pub struct DiscoveredRep {
    pub peer_id: String,
    pub representative: WalletAddress,
    pub weight: u64,
    pub last_seen: u64,
}

impl RepCrawler {
    /// Create a new rep crawler.
    ///
    /// # Arguments
    /// - `query_timeout_secs` — how long to wait for a vote response before considering a query stale
    /// - `crawl_interval_secs` — minimum interval between crawl rounds
    pub fn new(query_timeout_secs: u64, crawl_interval_secs: u64) -> Self {
        Self {
            discovered_reps: HashMap::new(),
            pending_queries: HashMap::new(),
            query_timeout_secs,
            crawl_interval_secs,
            last_crawl: 0,
            sufficient_weight: false,
        }
    }

    /// Check if it's time for another crawl round.
    pub fn should_crawl(&self, now: u64) -> bool {
        now - self.last_crawl >= self.crawl_interval_secs
    }

    /// Start a crawl: register a confirmed block hash and the peers to query.
    /// Returns the list of peer IDs that should receive `confirm_req` messages.
    pub fn start_crawl(
        &mut self,
        confirmed_block: BlockHash,
        peer_ids: &[String],
        now: u64,
    ) -> Vec<String> {
        self.last_crawl = now;
        for peer_id in peer_ids {
            self.pending_queries
                .entry(confirmed_block)
                .or_default()
                .push((peer_id.clone(), now));
        }
        peer_ids.to_vec()
    }

    /// Process a vote response from a peer during crawling.
    /// Returns the discovered representative info if successfully recorded.
    pub fn process_response(
        &mut self,
        peer_id: &str,
        voter: &WalletAddress,
        weight: u64,
        now: u64,
    ) -> Option<DiscoveredRep> {
        let rep = DiscoveredRep {
            peer_id: peer_id.to_string(),
            representative: voter.clone(),
            weight,
            last_seen: now,
        };
        self.discovered_reps
            .insert(peer_id.to_string(), rep.clone());
        Some(rep)
    }

    /// Remove expired pending queries (those older than `query_timeout_secs`).
    pub fn cleanup_expired(&mut self, now: u64) {
        self.pending_queries.retain(|_, peers| {
            peers.retain(|(_, sent_at)| now - *sent_at <= self.query_timeout_secs);
            !peers.is_empty()
        });
    }

    /// Get all discovered representatives.
    pub fn discovered_reps(&self) -> &HashMap<String, DiscoveredRep> {
        &self.discovered_reps
    }

    /// Total discovered representative weight.
    pub fn total_discovered_weight(&self) -> u64 {
        self.discovered_reps.values().map(|r| r.weight).sum()
    }

    /// Number of discovered representatives.
    pub fn discovered_count(&self) -> usize {
        self.discovered_reps.len()
    }

    /// Mark whether we've reached sufficient quorum coverage.
    pub fn set_sufficient_weight(&mut self, sufficient: bool) {
        self.sufficient_weight = sufficient;
    }

    /// Whether sufficient quorum coverage has been reached.
    pub fn has_sufficient_weight(&self) -> bool {
        self.sufficient_weight
    }

    /// Number of pending query entries (block hashes being queried).
    pub fn pending_query_count(&self) -> usize {
        self.pending_queries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_address(suffix: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{suffix}"))
    }

    fn test_hash(byte: u8) -> BlockHash {
        BlockHash::new([byte; 32])
    }

    #[test]
    fn new_crawler_is_empty() {
        let crawler = RepCrawler::new(10, 60);
        assert_eq!(crawler.discovered_count(), 0);
        assert_eq!(crawler.total_discovered_weight(), 0);
        assert_eq!(crawler.pending_query_count(), 0);
        assert!(!crawler.has_sufficient_weight());
    }

    #[test]
    fn should_crawl_initially() {
        let crawler = RepCrawler::new(10, 60);
        // last_crawl is 0, so any positive now should trigger
        assert!(crawler.should_crawl(61));
    }

    #[test]
    fn should_not_crawl_too_soon() {
        let mut crawler = RepCrawler::new(10, 60);
        let peers = vec!["peer1".to_string()];
        crawler.start_crawl(test_hash(1), &peers, 100);
        assert!(!crawler.should_crawl(110)); // only 10s later
    }

    #[test]
    fn should_crawl_after_interval() {
        let mut crawler = RepCrawler::new(10, 60);
        let peers = vec!["peer1".to_string()];
        crawler.start_crawl(test_hash(1), &peers, 100);
        assert!(crawler.should_crawl(161)); // 61s later
    }

    #[test]
    fn start_crawl_registers_pending() {
        let mut crawler = RepCrawler::new(10, 60);
        let peers = vec!["peer1".to_string(), "peer2".to_string()];
        let result = crawler.start_crawl(test_hash(1), &peers, 100);
        assert_eq!(result.len(), 2);
        assert_eq!(crawler.pending_query_count(), 1); // 1 block hash
    }

    #[test]
    fn process_response_records_rep() {
        let mut crawler = RepCrawler::new(10, 60);
        let addr = test_address("rep_account_1");
        let rep = crawler.process_response("peer1", &addr, 1000, 100);
        assert!(rep.is_some());
        assert_eq!(crawler.discovered_count(), 1);
        assert_eq!(crawler.total_discovered_weight(), 1000);
    }

    #[test]
    fn process_response_updates_existing() {
        let mut crawler = RepCrawler::new(10, 60);
        let addr = test_address("rep_account_1");
        crawler.process_response("peer1", &addr, 1000, 100);
        crawler.process_response("peer1", &addr, 2000, 200);
        assert_eq!(crawler.discovered_count(), 1);
        assert_eq!(crawler.total_discovered_weight(), 2000);
    }

    #[test]
    fn multiple_reps_accumulate_weight() {
        let mut crawler = RepCrawler::new(10, 60);
        crawler.process_response("peer1", &test_address("rep1"), 1000, 100);
        crawler.process_response("peer2", &test_address("rep2"), 2000, 100);
        crawler.process_response("peer3", &test_address("rep3"), 3000, 100);
        assert_eq!(crawler.discovered_count(), 3);
        assert_eq!(crawler.total_discovered_weight(), 6000);
    }

    #[test]
    fn cleanup_expired_removes_old_queries() {
        let mut crawler = RepCrawler::new(10, 60);
        let peers = vec!["peer1".to_string()];
        crawler.start_crawl(test_hash(1), &peers, 100);
        assert_eq!(crawler.pending_query_count(), 1);

        // Cleanup at now=111 (11s after send, timeout=10s)
        crawler.cleanup_expired(111);
        assert_eq!(crawler.pending_query_count(), 0);
    }

    #[test]
    fn cleanup_keeps_fresh_queries() {
        let mut crawler = RepCrawler::new(10, 60);
        let peers = vec!["peer1".to_string()];
        crawler.start_crawl(test_hash(1), &peers, 100);

        // Cleanup at now=105 (5s after send, timeout=10s)
        crawler.cleanup_expired(105);
        assert_eq!(crawler.pending_query_count(), 1);
    }

    #[test]
    fn sufficient_weight_flag() {
        let mut crawler = RepCrawler::new(10, 60);
        assert!(!crawler.has_sufficient_weight());
        crawler.set_sufficient_weight(true);
        assert!(crawler.has_sufficient_weight());
        crawler.set_sufficient_weight(false);
        assert!(!crawler.has_sufficient_weight());
    }

    #[test]
    fn discovered_reps_accessor() {
        let mut crawler = RepCrawler::new(10, 60);
        crawler.process_response("peer1", &test_address("rep1"), 500, 100);
        let reps = crawler.discovered_reps();
        assert!(reps.contains_key("peer1"));
        let rep = &reps["peer1"];
        assert_eq!(rep.weight, 500);
        assert_eq!(rep.last_seen, 100);
    }

    #[test]
    fn multiple_peers_same_block_query() {
        let mut crawler = RepCrawler::new(10, 60);
        let peers = vec![
            "peer1".to_string(),
            "peer2".to_string(),
            "peer3".to_string(),
        ];
        crawler.start_crawl(test_hash(0xAB), &peers, 100);
        // All 3 peers registered under 1 block hash
        assert_eq!(crawler.pending_query_count(), 1);
    }
}
