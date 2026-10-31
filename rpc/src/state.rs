//! Node-facing RPC service interfaces; no HTTP listener implementation.
use burst_brn::BrnEngine;
use burst_consensus::RepWeightCache;
use burst_store::account::AccountStore;
use burst_store::block::BlockStore;
use burst_store::governance::GovernanceStore;
use burst_store::verification::VerificationStore;
use burst_store::{FrontierStore, PendingStore};
use burst_types::{ProtocolParams, WalletAddress};

/// Trait for O(1) ledger counter lookups. Implemented by the node's
/// `LedgerCache` and injected into `RpcState` for bounded read-only access.
pub trait LedgerCacheView {
    fn block_count(&self) -> u64;
    fn account_count(&self) -> u64;
    fn pending_count(&self) -> u64;
}
use burst_work::WorkGenerator;
use prometheus::Registry;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Per-client one-second windows, starting with that client's first request.
/// Retains at most 4,096 client keys; new clients are refused until cleanup frees space.
pub struct RateLimiter {
    buckets: Mutex<HashMap<String, (u32, Instant)>>,
    max_requests_per_second: u32,
}

impl RateLimiter {
    pub fn new(max_rps: u32) -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            max_requests_per_second: max_rps,
        }
    }

    /// Returns `true` if the request should be allowed.
    pub fn check(&self, client_ip: &str) -> bool {
        self.check_cost(client_ip, 1)
    }

    pub fn check_cost(&self, client_ip: &str, cost: u32) -> bool {
        let mut buckets = self.buckets.lock().unwrap_or_else(|p| p.into_inner());
        if !buckets.contains_key(client_ip) && buckets.len() >= 4096 {
            return false;
        }
        let now = Instant::now();
        let entry = buckets.entry(client_ip.to_string()).or_insert((0, now));
        if now.duration_since(entry.1).as_secs() >= 1 {
            *entry = (0, now);
        }
        if cost == 0 || cost > self.max_requests_per_second.saturating_sub(entry.0) {
            return false;
        }
        entry.0 += cost;
        true
    }

    /// Remove buckets whose current window started at least 60 seconds ago.
    /// Call periodically; request admission does not evict other clients' buckets.
    pub fn cleanup(&self) {
        let mut buckets = match self.buckets.lock() {
            Ok(b) => b,
            Err(poisoned) => poisoned.into_inner(),
        };
        let now = Instant::now();
        buckets.retain(|_, (_, last)| now.duration_since(*last).as_secs() < 60);
    }
}

/// RPC submission outcome reported by the node adapter.
///
/// Acceptance or queuing does not establish ledger confirmation or finality.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessResult {
    Accepted,
    Gap,
    Fork,
    Rejected(String),
    Duplicate,
    Queued,
}

pub type HistoricalAccountPage = Vec<(u64, burst_ledger::StateBlock)>;
pub type HistoricalAccountPageFuture<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<HistoricalAccountPage, String>> + Send + 'a>,
>;

/// Callback trait for processing a block through the node's pipeline.
///
/// The node provides a concrete implementation that calls its `BlockProcessor`.
/// This indirection breaks the `rpc → node → rpc` circular dependency.
pub trait BlockProcessorCallback: Send + Sync {
    fn bootstrap_creator(&self) -> Result<burst_types::WalletAddress, String> {
        Err("bootstrap identity is unavailable".into())
    }

    /// Advisory synchronization observations, not authority to sign or confirm.
    fn checkpoint_sync_status(&self) -> Result<serde_json::Value, String> {
        Err("checkpoint synchronization status is unavailable on this node".into())
    }
    /// Authenticated account-prefix entries, keyed by their original heights.
    /// Missing entries are unavailable, not proof that the account has no history.
    fn historical_account_page(
        &self,
        _account: burst_types::WalletAddress,
        _first: u64,
        _count: u32,
    ) -> HistoricalAccountPageFuture<'_> {
        Box::pin(async { Ok(Vec::new()) })
    }
    /// Read-only membership proof. This must never admit or execute the block.
    fn historical_block(
        &self,
        _hash: burst_types::BlockHash,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Option<burst_ledger::StateBlock>, String>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async { Ok(None) })
    }
    /// Return authenticated historical members; absent entries remain unavailable.
    /// The default calls `historical_block` sequentially and omits missing results.
    fn historical_blocks(
        &self,
        hashes: Vec<burst_types::BlockHash>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<burst_ledger::StateBlock>, String>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let mut blocks = Vec::new();
            for hash in hashes {
                if let Some(block) = self.historical_block(hash).await? {
                    blocks.push(block);
                }
            }
            Ok(blocks)
        })
    }
    fn confirmed_account_slot(
        &self,
        _query: burst_types::payment_recovery::SlotQuery,
    ) -> Result<burst_types::payment_recovery::SlotReview, String> {
        Err("confirmed account slot recovery is unavailable on this node".into())
    }
    fn confirmed_account_slot_async(
        &self,
        query: burst_types::payment_recovery::SlotQuery,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<burst_types::payment_recovery::SlotReview, String>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move { self.confirmed_account_slot(query) })
    }
    fn payment_exclusion(
        &self,
        _query: burst_types::payment_recovery::ExclusionQuery,
    ) -> Result<burst_types::payment_recovery::ExclusionReview, String> {
        Err("payment exclusion recovery is unavailable on this node".into())
    }
    fn process_block(&self, block_bytes: &[u8]) -> Result<ProcessResult, String>;
    /// Async submission hook. The default calls the synchronous method directly
    /// when polled; implementations must override it to await queue admission.
    fn process_block_async<'a>(
        &'a self,
        block_bytes: &'a [u8],
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ProcessResult, String>> + Send + 'a>,
    > {
        Box::pin(async move { self.process_block(block_bytes) })
    }
    fn supports_full_governance_content(&self) -> bool {
        false
    }
    fn supports_delegation_key_journal(&self) -> bool {
        false
    }
    fn appeal_review(
        &self,
        _account: &WalletAddress,
    ) -> Result<burst_types::appeal::AppealReview, String> {
        Err("appeal review is unavailable on this node".into())
    }
    fn delegation_keys(
        &self,
        _query: burst_governance::signing_delegation::view::Query,
    ) -> Result<burst_governance::signing_delegation::view::Page, String> {
        Err("scoped delegation discovery is unavailable".into())
    }

    fn process_block_with_content(
        &self,
        _block_bytes: &[u8],
        _content: &[u8],
    ) -> Result<ProcessResult, String> {
        Err("full governance content submission is unavailable".into())
    }

    /// Optional wait for admission/persistence, not consensus finality.
    /// The default returns `None` immediately. The caller supplies its timeout and
    /// keeps an unresolved submission queued rather than treating it as rejected.
    fn wait_for_result<'a>(
        &'a self,
        _block_bytes: &'a [u8],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ProcessResult>> + Send + 'a>>
    {
        Box::pin(async { None })
    }
}

/// Shared state accessible by all handlers.
///
/// The node supplies controlled service operations; the application composes
/// the public listener around this state.
pub struct RpcState {
    pub governance_reader: Option<Arc<dyn crate::governance_snapshot::Reader>>,
    pub read_executor: crate::ReadExecutor,
    pub governance_projection_cache: crate::GovernanceProjectionCache,
    pub merger_stats_cache: Arc<tokio::sync::Mutex<crate::MergerStatsCache>>,
    /// Node clock for read-only time projections, not signing authority.
    pub display_now: Arc<dyn Fn() -> burst_types::Timestamp + Send + Sync>,
    pub meta_store: Arc<dyn burst_store::meta::MetaStore + Send + Sync>,
    /// Actual node network, independent of convenience-tool configuration.
    pub network: burst_types::NetworkId,
    /// Serialize genesis-chain transactions authored by these RPC helpers.
    pub genesis_authoring: tokio::sync::Mutex<()>,
    /// Unix timestamp (seconds) when the node started.
    pub started_at: u64,
    /// Prometheus metric registry (optional; when present the `/metrics`
    /// endpoint will serve the text exposition format).
    pub metrics_registry: Option<Registry>,
    /// Account storage backend.
    pub account_store: Arc<dyn AccountStore + Send + Sync>,
    /// Block storage backend (the DAG block-lattice).
    pub block_store: Arc<dyn BlockStore + Send + Sync>,
    /// Pending receive storage.
    pub pending_store: Arc<dyn PendingStore + Send + Sync>,
    /// Frontier storage (account chain heads).
    pub frontier_store: Arc<dyn FrontierStore + Send + Sync>,
    /// Verification data storage.
    pub verification_store: Arc<dyn VerificationStore + Send + Sync>,
    /// Governance data storage.
    pub governance_store: Arc<dyn GovernanceStore + Send + Sync>,
    /// Governance engine (shared with the node) for live proposal state.
    pub governance_engine: Option<Arc<tokio::sync::Mutex<burst_governance::GovernanceEngine>>>,
    /// BRN computation engine (shared with the node).
    pub brn_engine: Arc<tokio::sync::Mutex<BrnEngine>>,
    pub brn_store: Arc<dyn burst_store::brn::BrnStore + Send + Sync>,
    pub clock_sync: Arc<tokio::sync::Mutex<burst_network::ClockSync>>,
    pub signing_clock: Option<Arc<burst_network::trusted_clock::TrustedClock>>,
    pub verification_engine: Arc<tokio::sync::Mutex<burst_verification::VerificationOrchestrator>>,
    pub delegation_engine: Arc<tokio::sync::Mutex<burst_governance::DelegationEngine>>,
    pub trst_engine: Arc<tokio::sync::Mutex<burst_trst::TrstEngine>>,
    pub consti_engine: Arc<tokio::sync::Mutex<burst_consti::ConstiEngine>>,
    /// Cached representative weights (shared with the node).
    pub rep_weight_cache: Arc<tokio::sync::RwLock<RepWeightCache>>,
    /// Proof-of-work generator.
    pub work_generator: Arc<WorkGenerator>,
    /// Startup parameters used when no persisted parameter record exists.
    pub params: Arc<ProtocolParams>,
    /// Block processor callback — the node injects a concrete implementation.
    pub block_processor: Arc<dyn BlockProcessorCallback>,
    /// Entries returned by `representatives_online`, as (address, voting_weight).
    /// The current node adapter initializes this list empty and has no updater.
    pub online_reps: Arc<std::sync::RwLock<Vec<(WalletAddress, u128)>>>,
    /// Peer manager for connected peer count.
    pub peer_manager: Arc<tokio::sync::RwLock<burst_network::PeerManager>>,
    /// Whether the testnet faucet endpoint is enabled. Default: `false`.
    /// Only set to `true` on dev/test nodes.
    pub enable_faucet: bool,
    /// Genesis signing seed — present ONLY on the creator's authority node.
    /// Gates `genesis_endorse` (bootstrap wallet verification). Must never be
    /// exposed: keep this node's RPC bound to localhost / firewalled.
    pub genesis_seed: Option<[u8; 32]>,
    /// Per-IP rate limiter for RPC requests.
    pub rate_limiter: Arc<RateLimiter>,
    /// Optional constant-time ledger counters. Telemetry falls back to the block
    /// store for block count when absent; account totals still use an account scan.
    pub ledger_cache: Option<Arc<dyn LedgerCacheView + Send + Sync>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_cost_is_atomic_and_cannot_bypass_rate_limit() {
        let limiter = RateLimiter::new(5);
        assert!(limiter.check_cost("127.0.0.1", 4));
        assert!(!limiter.check_cost("127.0.0.1", 2));
        assert!(limiter.check("127.0.0.1"));
        assert!(!limiter.check("127.0.0.1"));
    }

    #[test]
    fn rate_limiter_allows_first_request() {
        let limiter = RateLimiter::new(10);
        assert!(limiter.check("127.0.0.1"));
    }

    #[test]
    fn rate_limiter_allows_up_to_max_rps() {
        let limiter = RateLimiter::new(5);
        for _ in 0..5 {
            assert!(limiter.check("127.0.0.1"));
        }
        assert!(
            !limiter.check("127.0.0.1"),
            "6th request should be rejected"
        );
    }

    #[test]
    fn rate_limiter_separate_ips_independent() {
        let limiter = RateLimiter::new(2);
        assert!(limiter.check("10.0.0.1"));
        assert!(limiter.check("10.0.0.1"));
        assert!(!limiter.check("10.0.0.1"));

        assert!(limiter.check("10.0.0.2"), "different IP should be allowed");
        assert!(limiter.check("10.0.0.2"));
        assert!(!limiter.check("10.0.0.2"));
    }

    #[test]
    fn rate_limiter_zero_max_blocks_all() {
        let limiter = RateLimiter::new(0);
        assert!(!limiter.check("127.0.0.1"));
    }

    #[test]
    fn rate_limiter_cleanup_removes_old_entries() {
        let limiter = RateLimiter::new(100);
        limiter.check("10.0.0.1");
        limiter.check("10.0.0.2");
        {
            let buckets = limiter.buckets.lock().unwrap();
            assert_eq!(buckets.len(), 2);
        }
        limiter.cleanup();
        {
            let buckets = limiter.buckets.lock().unwrap();
            assert_eq!(buckets.len(), 2, "recent entries should survive cleanup");
        }
    }

    #[test]
    fn rate_limiter_single_request_exactly_at_limit() {
        let limiter = RateLimiter::new(1);
        assert!(limiter.check("x"), "first request allowed");
        assert!(!limiter.check("x"), "second request blocked");
    }
}
