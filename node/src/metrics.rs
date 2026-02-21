//! Prometheus metrics for the BURST node.
//!
//! Exposes counters, gauges, and histograms covering block processing,
//! consensus, networking, and verification activity.  The [`NodeMetrics`]
//! struct owns a dedicated [`Registry`] that the RPC `/metrics` endpoint
//! can encode into the Prometheus text exposition format.

use prometheus::{
    register_histogram_with_registry, register_int_counter_with_registry,
    register_int_gauge_with_registry, Histogram, HistogramOpts, IntCounter, IntGauge, Opts,
    Registry,
};

/// Central collection of all node-level Prometheus metrics.
pub struct NodeMetrics {
    /// The Prometheus registry that owns every metric below.
    pub registry: Registry,

    // ── Counters ────────────────────────────────────────────────────────
    /// Total number of blocks that entered the processing pipeline.
    pub blocks_processed: IntCounter,
    /// Total number of blocks accepted by the block processor (persisted to ledger).
    pub blocks_accepted: IntCounter,
    /// Total number of blocks that reached confirmed (cemented) status via consensus.
    pub blocks_confirmed: IntCounter,
    /// Total number of transactions received from the network or RPC.
    pub transactions_received: IntCounter,
    /// Total number of consensus votes received from representatives.
    pub votes_received: IntCounter,

    // ── Gauges ──────────────────────────────────────────────────────────
    /// Current number of blocks in the ledger.
    pub block_count: IntGauge,
    /// Current number of accounts with at least one block.
    pub account_count: IntGauge,
    /// Current number of connected peers.
    pub peer_count: IntGauge,
    /// Current number of active elections (conflict resolution rounds).
    pub election_count: IntGauge,
    /// Current number of blocks in the unchecked map (awaiting dependencies).
    pub unchecked_count: IntGauge,

    // ── Histograms ──────────────────────────────────────────────────────
    /// Time from block reception to confirmation, in milliseconds.
    pub confirmation_latency_ms: Histogram,
    /// Time spent in the block-processing pipeline, in milliseconds.
    pub block_process_time_ms: Histogram,
}

impl NodeMetrics {
    /// Create a fresh set of metrics, all registered under a new
    /// [`Registry`].
    pub fn new() -> Self {
        let registry = Registry::new();

        // Counters
        let blocks_processed = register_int_counter_with_registry!(
            Opts::new(
                "burst_blocks_processed_total",
                "Total blocks processed by this node"
            ),
            registry
        )
        .expect("failed to register blocks_processed counter");

        let blocks_accepted = register_int_counter_with_registry!(
            Opts::new(
                "burst_blocks_accepted_total",
                "Total blocks accepted by the block processor"
            ),
            registry
        )
        .expect("failed to register blocks_accepted counter");

        let blocks_confirmed = register_int_counter_with_registry!(
            Opts::new(
                "burst_blocks_confirmed_total",
                "Total blocks confirmed (cemented) via consensus"
            ),
            registry
        )
        .expect("failed to register blocks_confirmed counter");

        let transactions_received = register_int_counter_with_registry!(
            Opts::new(
                "burst_transactions_received_total",
                "Total transactions received"
            ),
            registry
        )
        .expect("failed to register transactions_received counter");

        let votes_received = register_int_counter_with_registry!(
            Opts::new(
                "burst_votes_received_total",
                "Total consensus votes received"
            ),
            registry
        )
        .expect("failed to register votes_received counter");

        // Gauges
        let block_count = register_int_gauge_with_registry!(
            Opts::new(
                "burst_block_count",
                "Current number of blocks in the ledger"
            ),
            registry
        )
        .expect("failed to register block_count gauge");

        let account_count = register_int_gauge_with_registry!(
            Opts::new("burst_account_count", "Current number of accounts"),
            registry
        )
        .expect("failed to register account_count gauge");

        let peer_count = register_int_gauge_with_registry!(
            Opts::new("burst_peer_count", "Current number of connected peers"),
            registry
        )
        .expect("failed to register peer_count gauge");

        let election_count = register_int_gauge_with_registry!(
            Opts::new("burst_election_count", "Current number of active elections"),
            registry
        )
        .expect("failed to register election_count gauge");

        let unchecked_count = register_int_gauge_with_registry!(
            Opts::new(
                "burst_unchecked_count",
                "Current number of unchecked blocks"
            ),
            registry
        )
        .expect("failed to register unchecked_count gauge");

        // Histograms – use exponential buckets covering 1 ms → ~16 s.
        let confirmation_latency_ms = register_histogram_with_registry!(
            HistogramOpts::new(
                "burst_confirmation_latency_ms",
                "Confirmation latency in milliseconds"
            )
            .buckets(prometheus::exponential_buckets(1.0, 2.0, 15).unwrap()),
            registry
        )
        .expect("failed to register confirmation_latency_ms histogram");

        let block_process_time_ms = register_histogram_with_registry!(
            HistogramOpts::new(
                "burst_block_process_time_ms",
                "Block processing time in milliseconds"
            )
            .buckets(prometheus::exponential_buckets(0.1, 2.0, 15).unwrap()),
            registry
        )
        .expect("failed to register block_process_time_ms histogram");

        Self {
            registry,
            blocks_processed,
            blocks_accepted,
            blocks_confirmed,
            transactions_received,
            votes_received,
            block_count,
            account_count,
            peer_count,
            election_count,
            unchecked_count,
            confirmation_latency_ms,
            block_process_time_ms,
        }
    }
}

impl Default for NodeMetrics {
    fn default() -> Self {
        Self::new()
    }
}
