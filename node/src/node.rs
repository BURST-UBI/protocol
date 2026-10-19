//! The main BURST node struct — wires all protocol subsystems together.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;

use burst_brn::BrnEngine;
use burst_consensus::{
    ActiveElections, OnlineWeightSampler, PriorityScheduler, RepWeightCache, VoteCache,
    VoteGenerator,
};
use burst_governance::delegation::DelegationEngine;
use burst_governance::GovernanceEngine;
use burst_ledger::{BlockType, DagFrontier, StateBlock, CURRENT_BLOCK_VERSION};
use burst_messages::PeerAddress;
use burst_network::{Broadcaster, ClockSync, PeerManager, PortMapper, UpnpState};
use burst_rpc::{BlockProcessorCallback, ProcessResult as RpcProcessResult, RpcServer, RpcState};
use burst_store::block::BlockStore;
use burst_store::brn::BrnStore;
use burst_store::frontier::FrontierStore;
use burst_store_lmdb::LmdbStore;
use burst_trst::TrstEngine;
use burst_types::{BlockHash, ProtocolParams, Signature, Timestamp, TxHash, WalletAddress};
use burst_websocket::{WebSocketServer, WsState};
use burst_work::WorkGenerator;

use burst_store::account::AccountStore;
use burst_store::delegation::DelegationStore;
use burst_store::meta::MetaStore;
use burst_store::pending::PendingStore;
use burst_store::rep_weights::RepWeightStore;

use crate::block_processor::{BlockProcessor, ProcessResult};
use crate::bounded_backlog::BoundedBacklog;
use crate::config::NodeConfig;
use crate::confirming_set::ConfirmingSet;
use crate::connection_registry::{spawn_peer_read_loop, ConnectionRegistry};
use crate::error::NodeError;
use crate::ledger_cache::LedgerCache;
use crate::local_broadcaster::LocalBroadcaster;
use crate::metrics::NodeMetrics;
use crate::online_weight::OnlineWeightTracker;
use crate::priority_queue::BlockPriorityQueue;
use crate::recently_confirmed::RecentlyConfirmed;
use crate::shutdown::ShutdownController;
use crate::verification_processor::{VerificationProcessor, VerifierPool};
use crate::wire_message::{WireMessage, WireVote};

/// Default LMDB map size: 1 GiB.
pub(crate) const DEFAULT_MAP_SIZE: usize = 1 << 30;
/// Number of named LMDB databases.
pub(crate) const MAX_DBS: u32 = 29;
/// Channel capacity for the block-processing pipeline.
const BLOCK_CHANNEL_CAPACITY: usize = 4096;
/// Channel capacity for outbound peer messages.
const OUTBOUND_CHANNEL_CAPACITY: usize = 4096;
/// Timeout for waiting on background tasks during shutdown.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// Meta-store key used to persist the serialized MergerGraph.
const MERGER_GRAPH_META_KEY: &str = "merger_graph";

use crate::genesis_key;

// ── BlockProcessorCallback bridge ───────────────────────────────────────

/// Adapts the node's concrete [`BlockProcessor`] into the trait expected by
/// the RPC crate, breaking the circular dependency.
struct NodeBlockProcessor {
    checkpoint_sync_status: Arc<crate::checkpoint_sync::tip::Published>,
    historical_lookup: crate::historical_lookup::Reader,
    recovery_store: Arc<LmdbStore>,
    appeal_view: crate::appeal_view::Reader,
    delegation_view: crate::delegation_view::Reader,
    block_queue: Arc<BlockPriorityQueue>,
    submission_results: Arc<crate::submission_results::SubmissionResults>,
    governance_transfers: Arc<crate::governance_content::transfer::Transfers>,
}

impl BlockProcessorCallback for NodeBlockProcessor {
    fn checkpoint_sync_status(&self) -> Result<serde_json::Value, String> {
        let epoch = self
            .recovery_store
            .meta_store()
            .payment_epoch()
            .map_err(|e| e.to_string())?;
        Ok(self.checkpoint_sync_status.read(epoch))
    }
    fn historical_account_page(
        &self,
        account: burst_types::WalletAddress,
        first: u64,
        count: u32,
    ) -> burst_rpc::server::HistoricalAccountPageFuture<'_> {
        Box::pin(self.historical_lookup.account_page(account, first, count))
    }
    fn historical_block(
        &self,
        hash: burst_types::BlockHash,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Option<burst_ledger::StateBlock>, String>>
                + Send
                + '_,
        >,
    > {
        Box::pin(self.historical_lookup.read(hash))
    }
    fn confirmed_account_slot(
        &self,
        query: burst_types::payment_recovery::SlotQuery,
    ) -> Result<burst_types::payment_recovery::SlotReview, String> {
        crate::payment_exclusion::slot(&self.recovery_store, query)
    }
    fn payment_exclusion(
        &self,
        query: burst_types::payment_recovery::ExclusionQuery,
    ) -> Result<burst_types::payment_recovery::ExclusionReview, String> {
        crate::payment_exclusion::read(&self.recovery_store, query)
    }
    fn appeal_review(
        &self,
        account: &WalletAddress,
    ) -> Result<burst_types::appeal::AppealReview, String> {
        self.appeal_view.read(
            account,
            cfg!(test)
                || (crate::release_policy::appeals(self.delegation_view.network)
                    && burst_verification::checkpoint_selection_enabled(
                        self.delegation_view.network,
                    )),
        )
    }
    fn delegation_keys(
        &self,
        query: burst_governance::key_delegation::view::Query,
    ) -> Result<burst_governance::key_delegation::view::Page, String> {
        self.delegation_view
            .read(query, self.supports_delegation_key_journal())
    }
    fn supports_full_governance_content(&self) -> bool {
        crate::governance_content::enabled(self.delegation_view.network)
    }
    fn supports_delegation_key_journal(&self) -> bool {
        cfg!(test) || crate::release_policy::delegation(self.delegation_view.network)
    }
    fn process_block_with_content(
        &self,
        block_bytes: &[u8],
        content: &[u8],
    ) -> Result<RpcProcessResult, String> {
        if !self.supports_full_governance_content() {
            return Err("full governance content is behind the release gate".into());
        }
        let block: StateBlock =
            bincode::deserialize(block_bytes).map_err(|error| error.to_string())?;
        self.governance_transfers.offer(&block, content)?;
        self.submission_results.track(block_bytes);
        // The content service retries queue admission if this immediate push
        // encounters contention or a full queue. The result remains queued.
        self.block_queue.try_push(block);
        Ok(RpcProcessResult::Queued)
    }

    fn process_block(&self, block_bytes: &[u8]) -> Result<RpcProcessResult, String> {
        let block: StateBlock = bincode::deserialize(block_bytes)
            .map_err(|e| format!("failed to deserialize block: {e}"))?;

        self.submission_results.track(block_bytes);
        if self.block_queue.try_push(block) {
            Ok(RpcProcessResult::Queued)
        } else {
            Err("block queue full — try again later".to_string())
        }
    }

    fn wait_for_result<'a>(
        &'a self,
        block_bytes: &'a [u8],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<RpcProcessResult>> + Send + 'a>>
    {
        Box::pin(self.submission_results.wait(block_bytes))
    }
}

/// Maximum number of recently confirmed hashes to remember.
const RECENTLY_CONFIRMED_CAPACITY: usize = 65_536;
/// Default maximum concurrent elections.
const MAX_ACTIVE_ELECTIONS: usize = 5000;
/// Conservative placeholder; replaced from ledger-derived weight before networking.
const DEFAULT_ONLINE_WEIGHT: u128 = 1_000_000;
/// Default vote cache size.
/// A running BURST node.
pub struct BurstNode {
    pub config: NodeConfig,
    /// Immutable replay inputs, separate from the currently governed rules.
    initial_params: ProtocolParams,
    pub brn_engine: Arc<Mutex<BrnEngine>>,
    pub trst_engine: Arc<Mutex<TrstEngine>>,
    pub governance: Arc<Mutex<GovernanceEngine>>,
    pub block_processor: Arc<Mutex<BlockProcessor>>,
    pub frontier: Arc<RwLock<DagFrontier>>,
    pub peer_manager: Arc<RwLock<PeerManager>>,
    pub store: Arc<LmdbStore>,
    pub metrics: Arc<NodeMetrics>,
    pub shutdown: Arc<ShutdownController>,
    pub ws_state: Arc<WsState>,
    /// Registry mapping peer IDs to their TCP write halves.
    pub connection_registry: Arc<RwLock<ConnectionRegistry>>,
    /// Active consensus elections for double-spend resolution.
    pub active_elections: Arc<RwLock<ActiveElections>>,
    /// Pre-election vote cache for out-of-order vote arrival.
    pub vote_cache: Arc<RwLock<VoteCache>>,
    /// Bounded cache of recently confirmed block hashes (prevents re-elections).
    pub recently_confirmed: Arc<RwLock<RecentlyConfirmed>>,
    /// Vote generator for this node's representative key.
    pub vote_generator: Arc<Mutex<VoteGenerator>>,
    /// Cached representative weights for vote routing.
    pub rep_weights: Arc<RwLock<RepWeightCache>>,
    /// Cementation counters. Pending work is persisted in LMDB metadata.
    pub confirming_set: Arc<Mutex<ConfirmingSet>>,
    /// Bounded backlog of unconfirmed blocks for DoS protection.
    pub backlog: Arc<Mutex<BoundedBacklog>>,
    /// Local block re-broadcaster for locally created blocks.
    pub local_broadcaster: Arc<Mutex<LocalBroadcaster>>,
    /// Verification processor for UHV flow.
    pub verification_processor: Arc<VerificationProcessor>,
    /// Verifier pool — opted-in verifiers.
    pub verifier_pool: Arc<Mutex<VerifierPool>>,
    /// Fork cache — stores fork block candidates for elections.
    pub fork_cache: Arc<Mutex<burst_consensus::ForkCache>>,
    /// Vote spacing — prevents rapid vote flip-flopping.
    pub vote_spacing: Arc<Mutex<burst_consensus::VoteSpacing>>,
    /// Request aggregator — batches inbound vote requests.
    pub request_aggregator: Arc<Mutex<burst_consensus::RequestAggregator>>,
    /// SYN cookies — challenge-response handshake validation for inbound connections.
    pub syn_cookies: Arc<Mutex<burst_network::SynCookies>>,
    /// Online weight sampler for telemetry, never the consensus denominator.
    pub online_weight_sampler: Arc<Mutex<OnlineWeightSampler>>,
    /// Message deduplication filter — prevents processing duplicate P2P messages.
    pub message_dedup: Arc<Mutex<burst_network::MessageDedup>>,
    /// Clock synchronization service for BRN time-dependent computation.
    pub clock_sync: Arc<Mutex<ClockSync>>,
    /// Delegation engine for governance vote delegation.
    pub delegation_engine: Arc<Mutex<DelegationEngine>>,
    /// VRF client for fetching drand randomness (verifier selection).
    pub vrf_client: Arc<burst_vrf::DrandClient>,
    /// Legacy delegation records; not used to authorize secondary signatures.
    pub delegation_store: Arc<dyn DelegationStore + Send + Sync>,
    /// Verification orchestrator — end-to-end UHV workflow engine.
    pub verification_orchestrator: Arc<Mutex<burst_verification::VerificationOrchestrator>>,
    /// Adaptive PoW difficulty adjuster based on recent throughput.
    pub difficulty_adjuster: Arc<Mutex<burst_work::DifficultyAdjuster>>,
    /// Constitutional engine for managing on-chain amendments.
    pub consti_engine: Arc<Mutex<burst_consti::ConstiEngine>>,
    /// Election priority scheduler — higher-balance accounts get elections first.
    pub priority_scheduler: Arc<Mutex<PriorityScheduler>>,
    /// Aggregate online weight tracker with historical sampling for quorum stability.
    pub online_weight_tracker: Arc<Mutex<OnlineWeightTracker>>,

    /// Priority queue for submitting blocks into the processing pipeline.
    /// Blocks are ordered by PoW difficulty (higher = processed first).
    block_queue: Arc<BlockPriorityQueue>,
    submission_results: Arc<crate::submission_results::SubmissionResults>,
    snapshot_installer: crate::snapshot_installation::SnapshotInstaller,
    snapshot_install_rx: Option<mpsc::Receiver<crate::snapshot_installation::Request>>,
    recovered_checkpoint: Option<crate::confirmed_replay::snapshot::installed::ReplayCache>,
    pub(crate) governance_transfers: Arc<crate::governance_content::transfer::Transfers>,
    /// Broadcaster for flooding messages to connected peers.
    broadcaster: Broadcaster,
    /// Node identity private key for P2P handshakes.
    node_private_key: burst_types::PrivateKey,
    /// Node identity address (derived from the keypair).
    node_address: WalletAddress,
    /// UPnP port mapper for NAT traversal (None if disabled or dev network).
    port_mapper: Option<PortMapper>,
    /// Atomic counters for block/account/pending counts (O(1) lookups).
    pub ledger_cache: Arc<LedgerCache>,
    /// Handles for spawned background tasks (joined during shutdown).
    task_handles: Vec<JoinHandle<()>>,
    /// Taken only after persisted engines have been restored during start.
    outbound_rx: Option<mpsc::Receiver<(String, Vec<u8>)>>,
    /// Shared with payment voting so both paths keep the startup identity.
    representative_key: Arc<burst_types::PrivateKey>,
    nts_service: Option<burst_network::trusted_clock::NtsClock>,
    pub signing_clock: Option<Arc<burst_network::trusted_clock::TrustedClock>>,
    #[cfg(test)]
    pub(crate) verification_fixture_clock: Option<crate::checkpoint_service::Clock>,
    #[cfg(test)]
    pub(crate) checkpoint_publication_probe: Option<fn(&str)>,
}

mod recovery_peers;
mod recovery_publication;
mod snapshot_bootstrap;
#[cfg(test)]
mod startup_tests;

impl BurstNode {
    /// Obtain local control before moving this node into its running task.
    pub fn snapshot_installer(&self) -> crate::snapshot_installation::SnapshotInstaller {
        self.snapshot_installer.clone()
    }

    fn certified_selection_enabled(&self) -> bool {
        #[cfg(test)]
        if self.verification_fixture_clock.is_some() {
            assert_eq!(self.config.network, burst_types::NetworkId::Dev);
            return true;
        }
        burst_verification::checkpoint_selection_enabled(self.config.network)
    }

    /// Create and initialize a new BURST node.
    ///
    /// Opens the LMDB environment at `config.data_dir` and prepares all
    /// subsystems. Call [`start`] to begin accepting connections and
    /// processing blocks.
    pub async fn new(mut config: NodeConfig) -> Result<Self, NodeError> {
        genesis_key::validate_identity(config.network)
            .map_err(|error| NodeError::Config(error.into()))?;
        config.validate_drand_relays()?;
        config
            .snapshot_transfer
            .validate()
            .map_err(|error| NodeError::Config(error.to_string()))?;
        let min_work_difficulty = config.params.min_work_difficulty;
        let nts_service = if config.nts_sources.is_empty() || !config.enable_representative {
            None
        } else {
            Some(
                burst_network::trusted_clock::NtsClock::new(config.nts_sources.clone())
                    .map_err(|e| NodeError::Config(format!("invalid signing time sources: {e}")))?,
            )
        };
        let signing_clock = nts_service.as_ref().map(|service| service.clock());

        // Open LMDB storage
        let store = LmdbStore::open(&config.data_dir, MAX_DBS, DEFAULT_MAP_SIZE)
            .map_err(|e| NodeError::Other(format!("failed to open LMDB: {e}")))?;
        let store = Arc::new(store);
        crate::network_identity::verify_network(&store, config.network)?;
        let initial_params =
            crate::network_identity::genesis_params(&store, config.network, &config.params)?;

        // Peer manager
        let peer_manager = PeerManager::with_config(
            config.max_peers,
            config.bootstrap_peers.clone(),
            15, // keepalive interval (must be < READ_TIMEOUT of 30s)
        );
        let peer_manager = Arc::new(RwLock::new(peer_manager));

        // Block priority queue (replaces FIFO channel — higher PoW = higher priority)
        let retention = crate::block_retention::BlockRetention::new(
            crate::block_retention::MAX_RETAINED_BLOCKS,
        );
        let block_queue = Arc::new(BlockPriorityQueue::with_retention(
            BLOCK_CHANNEL_CAPACITY,
            retention.clone(),
        ));

        // Outbound message channel
        let (outbound_tx, outbound_rx) =
            mpsc::channel::<(String, Vec<u8>)>(OUTBOUND_CHANNEL_CAPACITY);
        let broadcaster = Broadcaster::new(outbound_tx);

        // Shutdown controller
        let shutdown = Arc::new(ShutdownController::new());

        // Metrics
        let metrics = Arc::new(NodeMetrics::new());

        // WebSocket shared state (always created; only served if enabled)
        let ws_state = Arc::new(WsState::new(256));

        // Connection registry (maps peer_id -> TCP write half)
        let connection_registry = Arc::new(RwLock::new(ConnectionRegistry::for_recovery()));

        // Block processor + frontier (loaded from store)
        let frontier = Self::load_frontier_from_store(&store)?;
        let frontier = Arc::new(RwLock::new(frontier));
        let mut block_processor = BlockProcessor::with_genesis_account(
            min_work_difficulty,
            genesis_key::genesis_address(config.network),
            crate::genesis_state::block(config.network, &initial_params).hash,
        )
        .with_network(config.network);
        block_processor.set_retention(retention);
        block_processor.block_store = Some(Arc::new(store.block_store()));
        let block_processor = Arc::new(Mutex::new(block_processor));

        // Consensus subsystems
        let active_elections = Arc::new(RwLock::new(ActiveElections::new(
            MAX_ACTIVE_ELECTIONS,
            DEFAULT_ONLINE_WEIGHT,
        )));
        let vote_cache = Arc::new(RwLock::new(VoteCache::new()));
        let recently_confirmed = Arc::new(RwLock::new(RecentlyConfirmed::new(
            RECENTLY_CONFIRMED_CAPACITY,
        )));

        // Vote generator — produce votes when acting as a representative.
        // Generate a transient node key; in production the key would come
        // from persistent configuration.
        //
        // Nano-style: a node votes AS A REAL LEDGER ACCOUNT so its voting weight
        // comes from verified people delegating to that account. The genesis-key
        // node votes as genesis, which has exactly the same unit per verified
        // delegator as everyone else. A fresh node key has no weight until a
        // verified person delegates to it; running extra nodes adds no weight.
        let (vote_generator, representative_key) = {
            let (rep_addr, rep_key) = match config
                .representative_key()?
                .or_else(|| genesis_key::genesis_signing_key(config.network))
            {
                Some(kp) => (burst_crypto::derive_address(&kp.public), kp.private),
                None => {
                    let kp = burst_crypto::generate_keypair();
                    (burst_crypto::derive_address(&kp.public), kp.private)
                }
            };
            let rep_key = Arc::new(rep_key);
            let mut vg = VoteGenerator::with_shared_key(rep_addr.clone(), rep_key.clone());
            if config.enable_representative {
                vg.set_representative(true);
                tracing::info!(representative = %rep_addr, "node voting as representative (enabled)");
            } else {
                tracing::info!(representative = %rep_addr, "node representative voting disabled");
            }
            (Arc::new(Mutex::new(vg)), rep_key)
        };
        let node_kp = burst_crypto::generate_keypair();
        let node_address = burst_crypto::derive_address(&node_kp.public);
        let node_private_key = node_kp.private;

        // Representative weight cache (rebuilt at startup from the account set)
        let rep_weights = Arc::new(RwLock::new(RepWeightCache::new()));

        // Confirming set for batched cementation of confirmed blocks
        let confirming_set = Arc::new(Mutex::new(ConfirmingSet::new(5)));

        // Bounded backlog for DoS-resistant unconfirmed block tracking
        let backlog = Arc::new(Mutex::new(BoundedBacklog::with_default_size()));
        let local_broadcaster = Arc::new(Mutex::new(LocalBroadcaster::with_default()));

        let verification_processor = Arc::new(VerificationProcessor::new(
            config.params.endorsement_threshold,
            config.params.num_verifiers,
            0.67, // vote threshold — 67% of verifiers must participate
        ));
        let verifier_pool = Arc::new(Mutex::new(VerifierPool::new(
            config.params.verifier_stake_amount,
        )));

        // Consensus infrastructure — fork cache, vote spacing, request aggregator
        let fork_cache = Arc::new(Mutex::new(burst_consensus::ForkCache::new()));
        let vote_spacing = Arc::new(Mutex::new(burst_consensus::VoteSpacing::new()));
        let request_aggregator = Arc::new(Mutex::new(burst_consensus::RequestAggregator::new(
            4096, 16,
        )));

        // SYN cookie handshake for inbound connection validation
        let syn_cookies = Arc::new(Mutex::new(burst_network::SynCookies::new(1024, 30, 5)));

        // Online weight sampler — 5-minute window for representative liveness
        let online_weight_sampler = Arc::new(Mutex::new(OnlineWeightSampler::new(300)));

        // Message deduplication — bounded filter to prevent duplicate P2P message processing
        let message_dedup = Arc::new(Mutex::new(burst_network::MessageDedup::new(65_536)));

        // Clock synchronization (5-second max drift tolerance)
        let clock_sync = Arc::new(Mutex::new(ClockSync::new(5_000)));

        let delegation_engine = Arc::new(Mutex::new(DelegationEngine::default()));
        let vrf_client = Arc::new(
            burst_vrf::DrandClient::mainnet()
                .and_then(|client| client.with_relays(&config.drand_relays))
                .map_err(|e| NodeError::Other(format!("invalid drand configuration: {e}")))?,
        );

        let verification_orchestrator = Arc::new(Mutex::new(
            burst_verification::VerificationOrchestrator::new(),
        ));

        // Load persisted BRN engine state from LMDB (fall back to fresh engine)
        // Load self-amended protocol params from LMDB (persisted by governance activation).
        {
            let brn_store = store.brn_store();
            match brn_store.get_meta(b"protocol_params") {
                Ok(Some(ref bytes)) => match bincode::deserialize::<ProtocolParams>(bytes) {
                    Ok(persisted) => {
                        tracing::info!("loaded self-amended protocol params from LMDB");
                        config.params = persisted;
                    }
                    Err(e) => {
                        return Err(NodeError::Other(format!("cannot decode persisted protocol params; refusing to replace governed rules with defaults: {e}")));
                    }
                },
                Ok(None) => {
                    tracing::info!("no persisted protocol params found, using config defaults");
                }
                Err(e) => {
                    return Err(NodeError::Other(format!(
                        "cannot read persisted protocol params: {e}"
                    )))
                }
            }
        }

        let brn_engine = {
            let brn_store = store.brn_store();
            match BrnEngine::load_from_store(&brn_store) {
                Ok(mut loaded) => {
                    let wallet_count = loaded.wallets.len();
                    tracing::info!(wallets = wallet_count, "BRN engine state loaded from LMDB");
                    // On a fresh database the rate_history has rate=0 (default).
                    // Ensure it matches the protocol's configured brn_rate.
                    let has_rate_history = brn_store
                        .get_meta(b"rate_history")
                        .map_err(|e| NodeError::Other(e.to_string()))?
                        .is_some();
                    if !has_rate_history && loaded.wallets.is_empty() {
                        tracing::info!(
                            stored_rate = loaded.current_rate(),
                            protocol_rate = config.params.brn_rate,
                            "BRN rate mismatch — reinitializing rate history"
                        );
                        loaded.rate_history =
                            burst_brn::RateHistory::new(config.params.brn_rate, Timestamp::new(0));
                        let rates = bincode::serialize(&loaded.rate_history)
                            .map_err(|e| NodeError::Other(e.to_string()))?;
                        let mut batch = store
                            .tx_begin_write()
                            .map_err(|e| NodeError::Other(e.to_string()))?;
                        batch
                            .put_brn_meta(b"rate_history", &rates)
                            .map_err(|e| NodeError::Other(e.to_string()))?;
                        batch
                            .put_brn_meta(b"next_stake_id", &loaded.next_stake_id().to_be_bytes())
                            .map_err(|e| NodeError::Other(e.to_string()))?;
                        batch.commit().map_err(|e| {
                            NodeError::Other(format!("cannot persist initial BRN state: {e}"))
                        })?;
                    } else if loaded.current_rate() != config.params.brn_rate {
                        return Err(NodeError::Other("persisted BRN rate differs from governed params; refusing to overwrite historical earnings".into()));
                    }
                    loaded
                }
                Err(e) => {
                    return Err(NodeError::Other(format!(
                        "cannot load persisted BRN state: {e}"
                    )));
                }
            }
        };

        let trst_expiry = config.params.trst_expiry_secs;
        let ledger_cache = {
            let block_store = store.block_store();
            let account_store = store.account_store();
            let pending_store = store.pending_store();
            let bc = block_store.block_count().unwrap_or(0);
            let ac = account_store.account_count().unwrap_or(0);
            let pc = pending_store.pending_count().unwrap_or(0);
            tracing::info!(
                blocks = bc,
                accounts = ac,
                pending = pc,
                "ledger cache initialized"
            );
            Arc::new(LedgerCache::new(bc, ac, pc))
        };

        let (snapshot_installer, snapshot_install_rx) =
            crate::snapshot_installation::SnapshotInstaller::channel(
                config.snapshot_transfer.max_archive_bytes,
                store.clone(),
                config.network,
                initial_params.clone(),
                connection_registry.clone(),
                shutdown.clone(),
            );
        let node = Self {
            config,
            initial_params,
            brn_engine: Arc::new(Mutex::new(brn_engine)),
            trst_engine: Arc::new(Mutex::new(TrstEngine::with_expiry(trst_expiry))),
            governance: {
                let brn_store = store.brn_store();
                match brn_store.get_meta(b"governance_engine") {
                    Ok(Some(ref bytes)) => match bincode::deserialize::<GovernanceEngine>(bytes) {
                        Ok(engine) => {
                            let count = engine.all_proposals().count();
                            tracing::info!(
                                proposals = count,
                                "loaded GovernanceEngine state from LMDB"
                            );
                            Arc::new(Mutex::new(engine))
                        }
                        Err(e) => {
                            return Err(NodeError::Other(format!(
                                "invalid persisted governance engine: {e}"
                            )));
                        }
                    },
                    Ok(None) => {
                        tracing::info!("no persisted GovernanceEngine state, starting fresh");
                        Arc::new(Mutex::new(GovernanceEngine::new()))
                    }
                    Err(e) => {
                        return Err(NodeError::Other(format!(
                            "cannot read persisted governance engine: {e}"
                        )))
                    }
                }
            },
            block_processor,
            frontier,
            peer_manager,
            store,
            metrics,
            shutdown,
            ws_state,
            connection_registry,
            active_elections,
            vote_cache,
            recently_confirmed,
            vote_generator,
            rep_weights,
            confirming_set,
            backlog,
            local_broadcaster,
            verification_processor,
            verifier_pool,
            fork_cache,
            vote_spacing,
            request_aggregator,
            syn_cookies,
            online_weight_sampler,
            message_dedup,
            clock_sync,
            delegation_engine,
            vrf_client,
            delegation_store: Arc::new(burst_nullables::NullDelegationStore::new()),
            verification_orchestrator,
            difficulty_adjuster: Arc::new(Mutex::new(burst_work::DifficultyAdjuster::new(
                min_work_difficulty,
                100,
                10000,
            ))),
            consti_engine: Arc::new(Mutex::new(burst_consti::ConstiEngine::bootstrap())),
            priority_scheduler: Arc::new(Mutex::new(PriorityScheduler::new(MAX_ACTIVE_ELECTIONS))),
            online_weight_tracker: Arc::new(Mutex::new(OnlineWeightTracker::new(
                DEFAULT_ONLINE_WEIGHT,
                60_000_000, // minimum weight floor
            ))),
            block_queue,
            submission_results: Arc::new(crate::submission_results::SubmissionResults::new(4096)),
            snapshot_installer,
            snapshot_install_rx: Some(snapshot_install_rx),
            recovered_checkpoint: None,
            governance_transfers: Arc::new(
                crate::governance_content::transfer::Transfers::default(),
            ),
            broadcaster,
            node_private_key,
            node_address,
            port_mapper: None,
            ledger_cache,
            task_handles: Vec::new(),
            outbound_rx: Some(outbound_rx),
            representative_key,
            nts_service,
            signing_clock,
            #[cfg(test)]
            verification_fixture_clock: None,
            #[cfg(test)]
            checkpoint_publication_probe: None,
        };

        Ok(node)
    }

    /// Load the in-memory frontier from the persistent frontier store.
    fn load_frontier_from_store(store: &LmdbStore) -> Result<DagFrontier, NodeError> {
        let frontier_store = store.frontier_store();
        let mut frontier = DagFrontier::new();

        match frontier_store.iter_frontiers() {
            Ok(entries) => {
                for (account, head) in entries {
                    frontier.update(account, head);
                }
                tracing::info!(
                    accounts = frontier.account_count(),
                    "loaded frontier from LMDB"
                );
            }
            Err(e) => {
                tracing::warn!("failed to load frontiers (new database?): {e}");
            }
        }

        Ok(frontier)
    }

    /// Initialize the genesis block if the database is empty.
    pub fn initialize_genesis(&self) -> Result<(), NodeError> {
        let block_store = self.store.block_store();

        match block_store.block_count() {
            Err(error) => return Err(NodeError::Other(format!("cannot read ledger: {error}"))),
            Ok(0) => {
                let error = || {
                    NodeError::Other(
                    "ledger blocks are missing but economic state remains; explicit ledger replay required, using a known-good backup or a new data directory".into(),
                )
                };
                if self.store.account_store().account_count()? != 0
                    || self.store.pending_store().pending_count()? != 0
                {
                    return Err(error());
                }
                for key in [
                    TrstEngine::meta_key(),
                    crate::trst_storage::FORMAT_KEY,
                    crate::graph_storage::FORMAT_KEY,
                    MERGER_GRAPH_META_KEY,
                    "consti_engine",
                    DelegationEngine::meta_key(),
                    crate::endorsement_commit::SNAPSHOT_KEY,
                ] {
                    match self.store.meta_store().get_meta(key) {
                        Ok(_) => return Err(error()),
                        Err(burst_store::StoreError::NotFound(_)) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                if self
                    .store
                    .brn_store()
                    .get_meta(b"governance_engine")?
                    .is_some()
                {
                    return Err(error());
                }
                tracing::info!("empty database — creating genesis block");
            }
            Ok(count) => {
                crate::bootstrap_phase::require(&self.store)
                    .map_err(|error| NodeError::Other(error.to_string()))?;
                let genesis =
                    crate::genesis_state::block(self.config.network, &self.initial_params);
                let mut batch = self.store.tx_begin_write()?;
                crate::genesis_state::confirm_anchor(&mut batch, &genesis)?;
                let installed = batch.payment_epoch()?;
                if installed != genesis.hash && !self.certified_selection_enabled() {
                    return Err(NodeError::Other(
                        "certified payment context requires the gated runtime".into(),
                    ));
                }
                batch.commit()?;
                tracing::info!(blocks = count, "database already initialized");
                return Ok(());
            }
        }

        let genesis_network = self.config.network;
        let genesis_account = genesis_key::genesis_address(genesis_network);
        // The trust anchor has canonical bytes on every node, independent of
        // whether the creator's signing key is available locally.
        let genesis_block = crate::genesis_state::block(genesis_network, &self.initial_params);

        // Genesis account and block must become visible together.
        let block_bytes =
            bincode::serialize(&genesis_block).map_err(|e| NodeError::Other(e.to_string()))?;
        let mut batch = self
            .store
            .tx_begin_write()
            .map_err(|e| NodeError::Other(format!("failed to start write batch: {e}")))?;
        // Height 1 for the genesis block so the per-account height index has an
        // entry for it — the ascending-bootstrap responder walks this index
        // (block_at_height) to serve an account's chain, and a missing height-1
        // entry would make genesis (and everything after it) unservable.
        batch
            .put_block_with_account(&genesis_block.hash, &block_bytes, &genesis_account, 1)
            .map_err(|e| NodeError::Other(format!("failed to batch genesis block: {e}")))?;
        batch
            .put_frontier(&genesis_account, &genesis_block.hash)
            .map_err(|e| NodeError::Other(format!("failed to batch genesis frontier: {e}")))?;
        let genesis_info =
            crate::genesis_state::account_to_write(None, genesis_account, genesis_block.hash)
                .expect("a new genesis account always needs initialization");
        batch
            .put_account_info(&genesis_info, false)
            .map_err(|e| NodeError::Other(format!("failed to batch genesis account: {e}")))?;
        crate::genesis_state::confirm_anchor(&mut batch, &genesis_block)?;
        crate::payment_epoch::initialize(&mut batch, genesis_block.hash)?;
        crate::bootstrap_phase::initialize(&mut batch, &self.initial_params)
            .map_err(|e| NodeError::Other(format!("failed to initialize bootstrap phase: {e}")))?;
        crate::confirmed_authority::ConfirmedAuthority::initialize(&mut batch, &genesis_info)
            .map_err(|e| {
                NodeError::Other(format!("failed to initialize confirmed authority: {e}"))
            })?;
        let trst = TrstEngine::with_expiry(self.initial_params.trst_expiry_secs);
        let mut brn = BrnEngine::with_rate(self.initial_params.brn_rate, Timestamp::EPOCH);
        brn.track_wallet(
            genesis_info.address.clone(),
            burst_brn::BrnWalletState::new(Timestamp::EPOCH),
        );
        crate::ledger_updater::persist_economics(&mut batch, &brn, &trst)
            .map_err(NodeError::Other)?;
        crate::governance_commit::CandidateGovernance::initial(
            self.initial_params.clone(),
            genesis_network,
            genesis_block.hash,
        )
        .persist(&mut batch)
        .map_err(NodeError::Other)?;
        let mut verification = burst_verification::VerificationOrchestrator::new();
        verification.apply_governed_rules(&self.initial_params);
        crate::genesis_state::initialize_verification(&mut batch, &genesis_block, &verification)?;
        batch
            .commit()
            .map_err(|e| NodeError::Other(format!("failed to commit genesis batch: {e}")))?;

        self.ledger_cache.inc_block_count();
        self.ledger_cache.inc_account_count();
        tracing::info!(hash = %genesis_block.hash, "genesis block created");
        Ok(())
    }

    /// Spawn core tasks after restoring all persisted state in `start()`.
    async fn spawn_initial_tasks(
        &mut self,
        outbound_rx: mpsc::Receiver<(String, Vec<u8>)>,
    ) -> Result<(), NodeError> {
        // ── Block processor task ──────────────────────────────────────────
        self.block_processor.lock().await.historical_finality = self
            .connection_registry
            .read()
            .await
            .historical_finality
            .clone();
        let bp = Arc::clone(&self.block_processor);
        let frontier = Arc::clone(&self.frontier);
        let store = Arc::clone(&self.store);
        let metrics = Arc::clone(&self.metrics);
        let mut shutdown_rx = self.shutdown.subscribe();
        let block_queue = Arc::clone(&self.block_queue);
        let submission_results_bp = Arc::clone(&self.submission_results);
        let governance_transfers_bp = Arc::clone(&self.governance_transfers);
        let active_elections_bp = Arc::clone(&self.active_elections);
        let vote_generator_bp = Arc::clone(&self.vote_generator);
        let online_weight_sampler_bp = Arc::clone(&self.online_weight_sampler);
        let broadcaster_bp = self.broadcaster.clone();
        let peer_manager_bp = Arc::clone(&self.peer_manager);

        let rep_weights_bp = Arc::clone(&self.rep_weights);
        let backlog_bp = Arc::clone(&self.backlog);
        let brn_engine_bp = Arc::clone(&self.brn_engine);
        let trst_engine_bp = Arc::clone(&self.trst_engine);
        let ledger_cache_bp = Arc::clone(&self.ledger_cache);
        let mut config_params_bp = self.config.params.clone();
        let genesis_network_bp = self.config.network;
        let fork_cache_bp = Arc::clone(&self.fork_cache);
        let vote_spacing_bp = Arc::clone(&self.vote_spacing);
        let ws_state_bp = Arc::clone(&self.ws_state);
        let governance_bp = Arc::clone(&self.governance);
        let delegation_bp = Arc::clone(&self.delegation_engine);
        let _verification_processor_bp = Arc::clone(&self.verification_processor);
        let verification_orch_bp = Arc::clone(&self.verification_orchestrator);
        let certified_selection_bp = self.certified_selection_enabled();
        let difficulty_adjuster_bp = Arc::clone(&self.difficulty_adjuster);
        let priority_scheduler_bp = Arc::clone(&self.priority_scheduler);
        let consti_engine_bp = Arc::clone(&self.consti_engine);
        let (fork_install_tx, mut fork_install_rx) = mpsc::channel::<BlockHash>(32);
        let fork_runtime = crate::fork_resolution::Runtime {
            store: self.store.clone(),
            brn: self.brn_engine.clone(),
            trst: self.trst_engine.clone(),
            processor: self.block_processor.clone(),
            frontier: self.frontier.clone(),
            weights: self.rep_weights.clone(),
            scheduler: self.priority_scheduler.clone(),
            elections: self.active_elections.clone(),
            backlog: self.backlog.clone(),
            broadcaster: self.local_broadcaster.clone(),
            counts: self.ledger_cache.clone(),
        };
        let recovered_checkpoint = self.recovered_checkpoint.take();
        let last_complete = recovered_checkpoint.as_ref().map(|cache| {
            let anchor = cache.recovered.continuation.anchor();
            (anchor.height(), anchor.hash())
        });
        let mut checkpoint_runtime = crate::checkpoint_installation::Runtime {
            registry: self.connection_registry.clone(),
            ledger: fork_runtime.clone(),
            verification: self.verification_orchestrator.clone(),
            governance: self.governance.clone(),
            delegation: self.delegation_engine.clone(),
            constitution: self.consti_engine.clone(),
            network: self.config.network,
            initial_params: self.initial_params.clone(),
            shutdown: self.shutdown.clone(),
            last_complete,
            recovery: recovered_checkpoint,
            #[cfg(test)]
            publication_probe: self.checkpoint_publication_probe,
        };

        let key_chain_bp = (
            self.config.network,
            crate::genesis_state::block(self.config.network, &self.initial_params).hash,
        );
        let fork_governance = crate::fork_resolution::GovernanceRuntime {
            genesis: key_chain_bp.1,
            governance: self.governance.clone(),
            delegation: self.delegation_engine.clone(),
            constitution: self.consti_engine.clone(),
        };
        let mut validation_queue = crate::block_validation::BlockValidationQueue::new(
            block_queue.clone(),
            bp.lock().await.authentication_rules(),
            Arc::new(store.block_store()),
        );
        let mut snapshot_install_rx = self
            .snapshot_install_rx
            .take()
            .ok_or_else(|| NodeError::Other("snapshot installer already started".into()))?;
        let (checkpoint_accepted, checkpoint_installed) = {
            let registry = self.connection_registry.read().await;
            (
                registry.checkpoint_accepted.clone(),
                registry.checkpoint_installed.clone(),
            )
        };
        let (historical_dependencies, history_task) = crate::historical_dependencies::start(
            self.store.clone(),
            self.config.network,
            self.initial_params.clone(),
            self.connection_registry.clone(),
            self.block_processor.clone(),
            self.block_queue.clone(),
            self.ledger_cache.clone(),
            self.shutdown.clone(),
        );
        self.task_handles.push(history_task);
        let bp_handle = tokio::spawn(async move {
            let mut checkpoint_tick = tokio::time::interval(Duration::from_secs(1));
            checkpoint_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut last_checkpoint_error: Option<String> = None;
            loop {
                // Pop the highest-priority block (by PoW difficulty).
                // Use select! to remain responsive to shutdown signals.
                let authenticated = tokio::select! {
                    biased;
                    _ = shutdown_rx.recv() => {
                        tracing::info!("block processor task shutting down");
                        break;
                    }
                    Some(request) = snapshot_install_rx.recv(), if certified_selection_bp => {
                        if request.reply.is_closed() {
                            continue;
                        }
                        let epoch = request.candidate.anchor().hash();
                        let result = match checkpoint_runtime.install_snapshot(request.candidate).await {
                            Ok(params) => {
                                config_params_bp = params;
                                validation_queue.set_rules(bp.lock().await.authentication_rules());
                                last_checkpoint_error = None;
                                checkpoint_installed.notify_one();
                                Ok(epoch)
                            }
                            Err(error) => Err(error.to_string()),
                        };
                        let _ = request.reply.send(result);
                        continue;
                    }
                    _ = async {
                        tokio::select! {
                            _ = checkpoint_tick.tick() => (),
                            _ = checkpoint_accepted.notified() => (),
                        }
                    }, if certified_selection_bp => {
                        match checkpoint_runtime.install_next().await {
                            Ok(Some(params)) => {
                                config_params_bp = params;
                                validation_queue.set_rules(bp.lock().await.authentication_rules());
                                last_checkpoint_error = None;
                                checkpoint_installed.notify_one();
                            }
                            Ok(None) => last_checkpoint_error = None,
                            Err(error) => {
                                let error = error.to_string();
                                if last_checkpoint_error.as_deref() != Some(error.as_str()) {
                                    tracing::warn!(%error, "checkpoint state installation deferred");
                                    last_checkpoint_error = Some(error);
                                }
                            }
                        }
                        continue;
                    }
                    Some(winner) = fork_install_rx.recv() => {
                        match fork_runtime.install(winner, &config_params_bp, genesis_network_bp,
                            certified_selection_bp.then_some(&verification_orch_bp),
                            certified_selection_bp.then_some(&fork_governance)).await {
                            Ok(released) => for dependent in released {
                                if !block_queue.try_push_retained(dependent) {
                                    tracing::warn!(%winner, "block queue full after fork replacement");
                                }
                            },
                            Err(error) => tracing::debug!(%winner, %error, "fork replacement deferred"),
                        }
                        continue;
                    }
                    result = validation_queue.next() => match result {
                        Ok(block) => block,
                        Err(error) => {
                            tracing::error!(%error, "block authentication worker failed; stopping node");
                            checkpoint_runtime.shutdown.fail("block authentication", &error);
                            break;
                        }
                    },
                };
                let block = authenticated.block().clone();

                let start = std::time::Instant::now();
                tracing::debug!(hash = %block.hash,
                    authentication_ms = authenticated.authentication_time().as_millis(),
                    "authenticated block reached admission");
                let _loop_now_secs = unix_now_secs();

                // Do not revalidate spent pending entries for blocks already
                // committed. Cross-account catch-up may deliver them again.
                if store.block_store().exists(&block.hash).unwrap_or(false) {
                    continue;
                }

                // Authenticate before retaining a dependency gap. The same
                // pipeline handles live and replay arrival orders; economics
                // starts only after the signed predecessor and sources exist.
                let admission = {
                    let mut processor = bp.lock().await;
                    let f = frontier.read().await;
                    processor.validate_authenticated_candidate(&authenticated, &f)
                };
                tracing::debug!(hash = %block.hash, elapsed_ms = start.elapsed().as_millis(),
                    "block admission validation finished");
                match &admission {
                    ProcessResult::Accepted | ProcessResult::Fork => (),
                    ProcessResult::GapSource if certified_selection_bp => {
                        let _ = historical_dependencies.try_send(block.clone());
                        continue;
                    }
                    ProcessResult::Rejected(reason) => {
                        tracing::debug!(hash = %block.hash, account = %block.account, %reason,
                            "block admission rejected");
                        if certified_selection_bp && reason.contains("backdating rejected") {
                            if let Err(error) = crate::checkpoint_recovery::request(&store, &block)
                            {
                                tracing::error!(%error, "cannot retain checkpoint recovery demand");
                            }
                            if let Ok(Some(authority)) =
                                crate::payment_authority::PaymentAuthority::read(&store)
                            {
                                let registry = checkpoint_runtime.registry.read().await;
                                registry.historical_finality.request(&block, authority);
                            }
                        }
                        governance_transfers_bp.remove(block.hash);
                        if let Ok(payload) = bincode::serialize(&block) {
                            submission_results_bp
                                .complete(&payload, RpcProcessResult::Rejected(reason.clone()));
                        }
                        continue;
                    }
                    _ => continue,
                }

                let proposal_content = if crate::governance_content::is_full(&block) {
                    if !crate::governance_content::enabled(genesis_network_bp) {
                        if let Ok(payload) = bincode::serialize(&block) {
                            submission_results_bp.complete(
                                &payload,
                                RpcProcessResult::Rejected(
                                    "full governance content is behind the release gate".into(),
                                ),
                            );
                        }
                        continue;
                    }
                    if !store
                        .account_store()
                        .get_account(&block.account)
                        .is_ok_and(|account| account.state.is_verified())
                    {
                        governance_transfers_bp.remove(block.hash);
                        if let Ok(payload) = bincode::serialize(&block) {
                            submission_results_bp.complete(
                                &payload,
                                RpcProcessResult::Rejected(
                                    "full proposal requires a verified proposer".into(),
                                ),
                            );
                        }
                        continue;
                    }
                    let ready = governance_transfers_bp.ready(block.hash).and_then(|ready| {
                        if ready.is_some() {
                            return Ok(ready);
                        }
                        store
                            .meta_store()
                            .fork_content(&block.hash)
                            .map(|bytes| bytes.map(Arc::new))
                            .map_err(|e| e.to_string())
                    });
                    match ready {
                        Ok(Some(bytes)) => Some(bytes),
                        _ => {
                            if let Err(error) = governance_transfers_bp.defer(&block) {
                                tracing::debug!(%error, "proposal content admission deferred");
                            }
                            continue;
                        }
                    }
                } else {
                    None
                };

                // Load previous block (if any) for balance validation and
                // ledger updater context.
                let prev_block = if !block.previous.is_zero() {
                    store
                        .block_store()
                        .get_block(&block.previous)
                        .ok()
                        .and_then(|bytes| bincode::deserialize::<StateBlock>(&bytes).ok())
                } else {
                    None
                };
                let prev_brn_balance = prev_block.as_ref().map_or(0, |b| b.brn_balance);

                // Look up previous account info for the ledger updater.
                let prev_account = match store.account_store().get_account(&block.account) {
                    Ok(acct) => Some(acct),
                    Err(burst_store::StoreError::NotFound(_)) => None,
                    Err(e) => {
                        tracing::error!(
                            account = %block.account,
                            error = %e,
                            "account store read failed — rejecting block"
                        );
                        continue;
                    }
                };

                // Fork economics must be checked against the common ancestor,
                // not balances/pending entries already consumed by the incumbent.
                let mut admitted_fork = false;
                let mut new_fork_payload = false;
                if prev_account
                    .as_ref()
                    .is_some_and(|a| a.block_count != 0 && a.head != block.previous)
                {
                    let admission_time = bp.lock().await.admission_time();
                    let _governance = if certified_selection_bp {
                        Some((
                            fork_governance.governance.lock().await,
                            fork_governance.delegation.lock().await,
                            fork_governance.constitution.lock().await,
                        ))
                    } else {
                        None
                    };
                    let _brn = brn_engine_bp.lock().await;
                    let _trst = trst_engine_bp.lock().await;
                    let _verification = if certified_selection_bp {
                        Some(verification_orch_bp.lock().await)
                    } else {
                        None
                    };
                    let evaluated = crate::fork_resolution::evaluate(
                        &store,
                        &block,
                        &config_params_bp,
                        admission_time,
                        genesis_network_bp,
                        certified_selection_bp,
                        certified_selection_bp.then_some(crate::fork_resolution::GovernanceInput {
                            genesis: key_chain_bp.1,
                            content: proposal_content.as_ref().map(|bytes| bytes.as_slice()),
                        }),
                    )
                    .and_then(|target| {
                        if target.is_some() {
                            let bytes = bincode::serialize(&block).map_err(|e| {
                                burst_store::StoreError::Serialization(e.to_string())
                            })?;
                            new_fork_payload =
                                store.meta_store().fork_candidate(&block.hash)?.is_none();
                            store.meta_store().put_fork_candidate_with_content(
                                &block.hash,
                                &bytes,
                                proposal_content.as_ref().map(|bytes| bytes.as_slice()),
                            )?;
                        }
                        Ok(target.is_some())
                    });
                    match evaluated {
                        Ok(fork) => admitted_fork = fork,
                        Err(error) => {
                            tracing::debug!(hash = %block.hash, %error, "fork not admitted");
                            if let Ok(payload) = bincode::serialize(&block) {
                                submission_results_bp.complete(
                                    &payload,
                                    RpcProcessResult::Rejected(error.to_string()),
                                );
                            }
                            continue;
                        }
                    }
                }

                // Pre-validate balance transition when the previous block is
                // available in the store.
                let balance_rejected = prev_block.as_ref().and_then(|prev| {
                    BlockProcessor::validate_balance_transition(
                        &block,
                        prev.brn_balance,
                        prev.trst_balance,
                    )
                    .err()
                });

                // Validate Open blocks: balances must not be self-reported.
                // brn must be 0 (the birthright is computed, never claimed);
                // trst must be 0 unless this is a receive-open matching an
                // existing pending send exactly.
                let open_rejected = if block.block_type == BlockType::Open {
                    let expected_pending = if block.link.is_zero() {
                        None
                    } else {
                        let send_hash = burst_types::TxHash::new(*block.link.as_bytes());
                        store
                            .pending_store()
                            .get_pending(&block.account, &send_hash)
                            .ok()
                            .map(|p| p.amount)
                    };
                    BlockProcessor::validate_open_block(&block, expected_pending).err()
                } else {
                    None
                };

                // Validate Receive blocks against the pending store: the
                // referenced pending send must exist and the claimed balance
                // increase must equal its amount exactly. TRST is conserved —
                // it only ever enters a chain from a real send (which traces
                // to a real burn).
                let receive_rejected = if block.block_type == BlockType::Receive {
                    let send_hash = burst_types::TxHash::new(*block.link.as_bytes());
                    match store
                        .pending_store()
                        .get_pending(&block.account, &send_hash)
                    {
                        Ok(pending) => {
                            let prev_trst = prev_block.as_ref().map_or(0, |b| b.trst_balance);
                            let claimed = block.trst_balance.saturating_sub(prev_trst);
                            if claimed != pending.amount {
                                Some(format!(
                                    "receive claims {} TRST but pending send {} holds {}",
                                    claimed, send_hash, pending.amount
                                ))
                            } else {
                                None
                            }
                        }
                        Err(_) => Some(format!(
                            "receive references pending send {} which does not exist",
                            send_hash
                        )),
                    }
                } else {
                    None
                };

                let verification_vote_rejected =
                    if block.block_type == BlockType::VerificationVote && certified_selection_bp {
                        let orchestrator = verification_orch_bp.lock().await;
                        crate::verification_vote::validate(
                            &block,
                            prev_brn_balance,
                            &orchestrator,
                            &config_params_bp,
                        )
                        .err()
                    } else if block.block_type == BlockType::Challenge && certified_selection_bp {
                        None
                    } else {
                        crate::verification_vote::availability_error(
                            &block.block_type,
                            genesis_network_bp,
                        )
                    };

                let result = if certified_selection_bp
                    && block.block_type == BlockType::GovernanceActivation
                {
                    ProcessResult::Rejected(
                        "governance activation requires its certified boundary".into(),
                    )
                } else if admitted_fork {
                    ProcessResult::Fork
                } else if let Some(reason) = balance_rejected {
                    ProcessResult::Rejected(reason)
                } else if let Some(reason) = open_rejected {
                    ProcessResult::Rejected(reason)
                } else if let Some(reason) = receive_rejected {
                    ProcessResult::Rejected(reason)
                } else if let Some(reason) = verification_vote_rejected {
                    ProcessResult::Rejected(reason)
                } else {
                    admission
                };

                metrics.blocks_processed.inc();

                if let ProcessResult::Rejected(reason) = &result {
                    if let Ok(payload) = bincode::serialize(&block) {
                        submission_results_bp
                            .complete(&payload, RpcProcessResult::Rejected(reason.clone()));
                    }
                }

                match &result {
                    ProcessResult::Accepted => {
                        // ── Acquire all locks needed before the unified write
                        // batch. RwTxn is !Send so no awaits are possible while
                        // the batch exists. (Rep weights are NOT touched here —
                        // they're rebuilt from the ledger by the periodic weight
                        // task, so no rep-weight lock is held during the batch.)
                        // Governance tick takes governance -> delegation. Keep
                        // that order here before acquiring economic state.
                        let mut live_governance =
                            if crate::governance_commit::CandidateGovernance::applies_to(
                                &block.block_type,
                            ) {
                                Some((
                                    governance_bp.lock().await,
                                    delegation_bp.lock().await,
                                    consti_engine_bp.lock().await,
                                ))
                            } else {
                                None
                            };
                        let mut staged_governance = None;
                        let mut live_brn = brn_engine_bp.lock().await;
                        let mut live_trst = trst_engine_bp.lock().await;
                        // Compute on isolated state. Failed economic validation
                        // or persistence must not consume BRN/TRST in memory.
                        let mut engines =
                            crate::economic_candidate::StagedEngines::new(&live_brn, &live_trst);

                        // Abort the entire primary-state transaction on any
                        // failed write; publish isolated engines only on success.
                        let mut live_orchestrator = if matches!(
                            block.block_type,
                            BlockType::Endorse
                                | BlockType::Challenge
                                | BlockType::VerificationVote
                                | BlockType::GovernanceActivation
                        ) {
                            Some(verification_orch_bp.lock().await)
                        } else {
                            None
                        };
                        let mut staged_orchestrator = None;
                        let mut commit_attempted = false;
                        tracing::debug!(hash = %block.hash, elapsed_ms = start.elapsed().as_millis(),
                            "block commit locks acquired");
                        let commit = (|| -> Result<crate::economic_candidate::CandidateEconomics, String> {
                            let mut batch = store.tx_begin_write().map_err(|e|e.to_string())?;
                            if live_orchestrator.is_some() {
                                staged_orchestrator = Some(crate::endorsement_commit::restore(&batch)?);
                            }
                            if live_governance.is_some() {
                                staged_governance = Some(crate::governance_commit::CandidateGovernance::restore(
                                    &batch, config_params_bp.clone(), key_chain_bp.0, key_chain_bp.1,
                                )?);
                            }
                            let economics = crate::economic_candidate::stage(
                                &mut batch, &block, &config_params_bp, &mut engines)?;
                            if let Some(bytes) = &proposal_content {
                                crate::governance_content::stage(&mut batch, &block, bytes)?;
                            }
                            if let Some(orch) = staged_orchestrator.as_mut() {
                                if crate::verification_transition::supports(&block.block_type) {
                                    crate::verification_transition::apply(&mut batch, &block, &economics.effect,
                                        &config_params_bp, genesis_network_bp, orch, engines.mutable())?;
                                }
                            }
                            if let Some(governance) = staged_governance.as_mut() {
                                let (brn, trst) = engines.mutable();
                                governance.apply(&mut batch, &block, &economics.effect, brn, trst)?;
                                if block.block_type == BlockType::GovernanceActivation {
                                    let orch = staged_orchestrator.as_mut().ok_or("missing activation verification state")?;
                                    orch.apply_governed_rules(&governance.params);
                                    batch.put_meta(crate::endorsement_commit::SNAPSHOT_KEY,
                                        &orch.snapshot().encode().map_err(|e| e.to_string())?)
                                        .map_err(|e| e.to_string())?;
                                }
                            }
                            crate::bootstrap_phase::observe(&mut batch,
                                staged_governance.as_ref().map_or(&config_params_bp, |governance| &governance.params))?;
                            crate::ledger_updater::refresh_token_counters(&mut batch, &block.account, crate::economic_candidate::Engines::trst(&engines), block.timestamp)?;
                            engines.persist(&mut batch)?;
                            commit_attempted = true;
                            batch.commit().map_err(|e|e.to_string())?;
                            governance_transfers_bp.remove(block.hash);
                            Ok(economics)
                        })();
                        let economics = match commit {
                            Ok(economics) => economics,
                            Err(error) => {
                                tracing::error!(hash = %block.hash, %error, "block persistence failed; candidate not published");
                                if !commit_attempted {
                                    if let Ok(payload) = bincode::serialize(&block) {
                                        submission_results_bp
                                            .complete(&payload, RpcProcessResult::Rejected(error));
                                    }
                                }
                                continue;
                            }
                        };
                        let deferred_pending = economics.pending;
                        tracing::debug!(hash = %block.hash, elapsed_ms = start.elapsed().as_millis(),
                            "block durable commit finished");
                        if let (Some((gov, del, consti)), Some(staged)) =
                            (live_governance.as_mut(), staged_governance)
                        {
                            **gov = staged.governance;
                            **del = staged.delegation;
                            **consti = staged.constitution;
                            config_params_bp = staged.params;
                        }
                        drop(live_governance);
                        if let (Some(live), Some(staged)) =
                            (live_orchestrator.as_mut(), staged_orchestrator)
                        {
                            **live = staged;
                        }
                        drop(live_orchestrator);
                        let (brn, trst) = engines.into_staged();
                        if let Some(brn) = brn {
                            *live_brn = brn;
                        }
                        if let Some(trst) = trst {
                            *live_trst = trst;
                        }
                        let brn = live_brn;
                        drop(live_trst);
                        {
                            let mut processor = bp.lock().await;
                            let mut f = frontier.write().await;
                            processor.set_params_hash(config_params_bp.params_hash());
                            processor.publish_committed(&block, &mut f);
                        }
                        ledger_cache_bp.inc_block_count();
                        if block.block_type == BlockType::Open {
                            ledger_cache_bp.inc_account_count();
                        }
                        if deferred_pending.is_some() {
                            ledger_cache_bp.inc_pending_count();
                        }
                        if block.block_type == BlockType::Receive
                            || (block.block_type == BlockType::Open && block.trst_balance > 0)
                        {
                            ledger_cache_bp.dec_pending_count();
                        }

                        drop(brn);

                        // Publish scheduler work and release dependencies only
                        // after all durable state for this block has committed.
                        {
                            let mut backlog = backlog_bp.lock().await;
                            // The durable scan rediscovers accepted work even when
                            // this optional in-memory tracking cache is full.
                            if backlog.can_accept(&block.account) {
                                backlog.insert(
                                    block.hash,
                                    block.account.clone(),
                                    block.work,
                                    unix_now_secs(),
                                );
                            }
                        }
                        priority_scheduler_bp.lock().await.push(
                            block.hash,
                            block.account.clone(),
                            block.trst_balance.min(u64::MAX as u128) as u64,
                        );
                        let released = {
                            let mut processor = bp.lock().await;
                            processor.release_retained(&block.hash)
                        };
                        for dep in released {
                            if !block_queue.push_retained(dep).await {
                                tracing::warn!(parent = %block.hash, "block queue full re-enqueuing unchecked dependent");
                            }
                        }

                        // Track acceptance (NOT confirmation — that happens via consensus)
                        if let Err(error) = crate::backlog_scan::register_candidate(
                            &store,
                            &mut *active_elections_bp.write().await,
                            &block,
                            Timestamp::new(unix_now_secs()),
                        ) {
                            tracing::debug!(hash = %block.hash, %error, "election deferred to scheduler");
                        }
                        metrics.blocks_accepted.inc();
                        difficulty_adjuster_bp
                            .lock()
                            .await
                            .record_block(block.timestamp.as_secs());
                        tracing::debug!(hash = %block.hash, elapsed_ms = start.elapsed().as_millis(),
                            "block accepted and persisted");
                        if let Ok(payload) = bincode::serialize(&block) {
                            submission_results_bp.complete(&payload, RpcProcessResult::Accepted);
                        }

                        // Account update is provisional; acceptance is not finality.
                        ws_state_bp.publish_account_update(
                            &block.account.to_string(),
                            &block.trst_balance.to_string(),
                            &format!("{:?}", block.block_type),
                        );

                        // Every node publishes accepted blocks, including nodes
                        // that do not vote or whose vote is currently spaced.
                        if let Ok(bytes) = WireMessage::Block(Box::new(block.clone())).encode() {
                            let peers: Vec<_> = peer_manager_bp
                                .read()
                                .await
                                .iter_connected()
                                .map(|(_, peer)| peer.clone())
                                .collect();
                            let _ = broadcaster_bp
                                .broadcast_with_fanout(&bytes, &peers, 4)
                                .await;
                        }

                        // Persist voting intent before signing or broadcasting.
                        'local_vote: {
                            let mut vg = vote_generator_bp.lock().await;
                            if vg.is_representative {
                                let mut vs = vote_spacing_bp.lock().await;
                                // Space votes by the election ROOT (the frontier
                                // position = `previous`), not the account. Rapid
                                // sequential blocks on one account each extend a
                                // distinct `previous`, so they're all votable;
                                // only genuine forks at the same position share a
                                // root and get rate-limited. Open blocks have no
                                // `previous` (ZERO), so use an account-specific
                                // open root, distinct from its first successor.
                                let spacing_root = crate::ledger_updater::election_root(&block);
                                if vs.votable(&spacing_root, &block.hash) {
                                    vs.record(spacing_root, block.hash);
                                    drop(vs);
                                    let vote = match crate::durable_votes::generate_vote(
                                        &store, &mut vg, block.hash, false,
                                    ) {
                                        Ok(vote) => vote,
                                        Err(error) => {
                                            tracing::error!(hash = %block.hash, %error, "refusing vote without durable reservation");
                                            break 'local_vote;
                                        }
                                    };
                                    drop(vg);

                                    // Count OUR OWN vote in OUR election tally.
                                    // Previously the accept path only broadcast
                                    // the vote to peers and never fed it into the
                                    // local active_elections, so a single node (or
                                    // this node's own view) could never reach soft
                                    // quorum → nothing ever confirmed/cemented.
                                    // All candidates at this account position share
                                    // an election, including later-arriving forks.
                                    {
                                        let now = Timestamp::new(unix_now_secs());
                                        let our_weight =
                                            rep_weights_bp.read().await.weight(&vote.voter);
                                        if our_weight > 0
                                            || active_elections_bp
                                                .read()
                                                .await
                                                .has_authorized_voter(&vote.voter)
                                        {
                                            online_weight_sampler_bp
                                                .lock()
                                                .await
                                                .record_vote(&vote.voter, now.as_secs());
                                            let mut ae = active_elections_bp.write().await;
                                            match crate::backlog_scan::register_candidate(
                                                &store, &mut ae, &block, now,
                                            ) {
                                                Ok(true) => {}
                                                Ok(false) => break 'local_vote,
                                                Err(error) => {
                                                    tracing::warn!(hash = %block.hash, %error, "local vote election deferred");
                                                    break 'local_vote;
                                                }
                                            }
                                            let _ = ae.process_signed_vote(
                                                &spacing_root,
                                                burst_consensus::VoteInfo::new(
                                                    vote.voter.clone(),
                                                    block.hash,
                                                    our_weight,
                                                    false,
                                                    Timestamp::new(vote.timestamp / 1000),
                                                    vote.sequence,
                                                ),
                                                now,
                                            );
                                        }
                                    }

                                    let wire_msg = WireMessage::Vote(WireVote {
                                        voter: vote.voter,
                                        block_hashes: vec![vote.block_hash],
                                        is_final: false,
                                        timestamp: vote.timestamp,
                                        sequence: vote.sequence,
                                        signature: vote.signature,
                                    });
                                    let peers: Vec<burst_network::PeerState> = {
                                        let pm = peer_manager_bp.read().await;
                                        pm.iter_connected().map(|(_, s)| s.clone()).collect()
                                    };
                                    if let Ok(msg_bytes) = wire_msg.encode() {
                                        let _ = broadcaster_bp
                                            .broadcast_with_fanout(&msg_bytes, &peers, 4)
                                            .await;
                                    }
                                } else {
                                    tracing::trace!(
                                        hash = %block.hash,
                                        root = %block.previous,
                                        "vote suppressed by vote spacing"
                                    );
                                }
                            }
                        }
                    }
                    ProcessResult::Fork => {
                        let root = crate::ledger_updater::election_root(&block);
                        if let Ok(payload) = bincode::serialize(&block) {
                            submission_results_bp.complete(&payload, RpcProcessResult::Fork);
                        }
                        if new_fork_payload {
                            let peers: Vec<_> = peer_manager_bp
                                .read()
                                .await
                                .iter_connected()
                                .map(|(_, peer)| peer.clone())
                                .collect();
                            if let Ok(bytes) = WireMessage::Block(Box::new(block.clone())).encode()
                            {
                                let _ = broadcaster_bp
                                    .broadcast_with_fanout(&bytes, &peers, 4)
                                    .await;
                            }
                        }
                        // Cache the fork block for election consideration
                        {
                            let mut fc = fork_cache_bp.lock().await;
                            fc.insert(root, block.hash);
                        }
                        // Join the existing election for this account position.
                        let now = Timestamp::new(unix_now_secs());
                        {
                            let mut ae = active_elections_bp.write().await;
                            if let Err(e) = ae.register_candidate(root, block.hash, now) {
                                tracing::debug!(root = %root, error = %e, "could not start election for fork");
                            } else {
                                tracing::info!(root = %root, fork_hash = %block.hash, "election started for fork");
                            }
                        }

                        // Vote for the block WE accepted at this root (the fork's
                        // competitor already in our ledger), so the election can
                        // resolve. The competitor is the successor of `previous`
                        // in this account's chain: block_at_height(previous+1).
                        // Without seeding our own vote a fork election never gets
                        // any votes (fork blocks aren't voted on via the normal
                        // accept path), so it could never confirm.
                        let competitor = {
                            let bs = store.block_store();
                            match bs.height_of_block(&block.previous) {
                                Ok(Some(h)) => {
                                    bs.block_at_height(&block.account, h + 1).ok().flatten()
                                }
                                // Open-block fork (no `previous`): the competitor
                                // is the account's first block (height 1).
                                _ if block.previous.is_zero() => {
                                    bs.block_at_height(&block.account, 1).ok().flatten()
                                }
                                _ => None,
                            }
                        };
                        if let Some(accepted) = competitor {
                            if accepted != block.hash {
                                let _ = active_elections_bp
                                    .write()
                                    .await
                                    .register_candidate(root, accepted, now);
                                let (our_rep, is_rep) = {
                                    let vg = vote_generator_bp.lock().await;
                                    (vg.representative.clone(), vg.is_representative)
                                };
                                if is_rep {
                                    let our_weight =
                                        { rep_weights_bp.read().await.weight(&our_rep) };
                                    let vote = {
                                        let mut generator = vote_generator_bp.lock().await;
                                        match crate::durable_votes::generate_vote(
                                            &store,
                                            &mut generator,
                                            accepted,
                                            false,
                                        ) {
                                            Ok(vote) => vote,
                                            Err(error) => {
                                                tracing::error!(%accepted, %error, "refusing conflicting or non-durable fork vote");
                                                continue;
                                            }
                                        }
                                    };
                                    {
                                        let mut ae = active_elections_bp.write().await;
                                        let _ = ae.process_signed_vote(
                                            &root,
                                            burst_consensus::VoteInfo::new(
                                                our_rep.clone(),
                                                accepted,
                                                our_weight,
                                                false,
                                                Timestamp::new(vote.timestamp / 1000),
                                                vote.sequence,
                                            ),
                                            now,
                                        );
                                    }
                                    let wire = WireMessage::Vote(WireVote {
                                        voter: vote.voter,
                                        block_hashes: vec![vote.block_hash],
                                        is_final: false,
                                        timestamp: vote.timestamp,
                                        sequence: vote.sequence,
                                        signature: vote.signature,
                                    });
                                    if let Ok(bytes) = wire.encode() {
                                        let peers: Vec<burst_network::PeerState> = {
                                            let pm = peer_manager_bp.read().await;
                                            pm.iter_connected().map(|(_, s)| s.clone()).collect()
                                        };
                                        let _ = broadcaster_bp
                                            .broadcast_with_fanout(&bytes, &peers, 4)
                                            .await;
                                    }
                                    tracing::info!(root = %root, voted_for = %accepted, "cast vote for accepted block in fork election");
                                }
                            }
                        }
                    }
                    _ => {
                        tracing::debug!(hash = %block.hash, ?result, "block not accepted");
                    }
                }

                let elapsed =
                    (start.elapsed() + authenticated.authentication_time()).as_secs_f64() * 1000.0;
                metrics.block_process_time_ms.observe(elapsed);
            }
        });
        self.task_handles.push(bp_handle);

        // ── Confirmation task — processes confirmed elections ─────────────
        let active_elections_ct = Arc::clone(&self.active_elections);
        let recently_confirmed_ct = Arc::clone(&self.recently_confirmed);
        let metrics_ct = Arc::clone(&self.metrics);
        let ws_state_ct = Arc::clone(&self.ws_state);
        let mut shutdown_rx_ct = self.shutdown.subscribe();
        let vote_generator_ct = Arc::clone(&self.vote_generator);
        let broadcaster_ct = self.broadcaster.clone();
        let peer_manager_ct = Arc::clone(&self.peer_manager);
        let rep_weights_ct = Arc::clone(&self.rep_weights);
        let store_ct = Arc::clone(&self.store);
        let backlog_ct = Arc::clone(&self.backlog);
        let local_broadcaster_ct = Arc::clone(&self.local_broadcaster);
        let historical_ct = self
            .connection_registry
            .read()
            .await
            .historical_finality
            .clone();

        let confirmation_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(500));
            let mut request_cursor = 0usize;
            let mut last_request = std::time::Instant::now();
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_ct.recv() => {
                        tracing::info!("confirmation task shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                        if last_request.elapsed() >= Duration::from_secs(1) {
                            last_request = std::time::Instant::now();
                            let mut candidates = active_elections_ct.read().await.unconfirmed_candidates();
                            candidates.extend(historical_ct.awaiting_votes());
                            candidates.sort();
                            candidates.dedup();
                            if !candidates.is_empty() {
                                request_cursor %= candidates.len();
                                let hashes: Vec<_> = candidates[request_cursor..].iter().chain(&candidates[..request_cursor])
                                    .take(8).copied().collect();
                                request_cursor = (request_cursor + hashes.len()) % candidates.len();
                                for hash in &hashes {
                                    let vote = {
                                        let mut generator = vote_generator_ct.lock().await;
                                        let ae = active_elections_ct.read().await;
                                        let already_voted = ae.candidate_root(hash).and_then(|root| ae.get_election(&root))
                                            .is_some_and(|election| election.last_votes.contains_key(&generator.representative));
                                        drop(ae);
                                        if already_voted { continue; }
                                        crate::durable_votes::respond_vote(&store_ct, &mut generator, *hash)
                                    };
                                    match vote {
                                        Ok(Some(vote)) => {
                                            let weight = rep_weights_ct.read().await.weight(&vote.voter);
                                            let mut ae = active_elections_ct.write().await;
                                            if let Some(root) = ae.candidate_root(hash) {
                                                let _ = ae.process_signed_vote(&root, burst_consensus::VoteInfo::new(
                                                    vote.voter, *hash, weight, vote.is_final,
                                                    Timestamp::new(vote.timestamp / 1000), vote.sequence,
                                                ), Timestamp::new(unix_now_secs()));
                                            }
                                        }
                                        Ok(None) => {}
                                        Err(error) => tracing::warn!(%hash, %error, "cannot seed recovered election vote"),
                                    }
                                }
                                let request = WireMessage::ConfirmReq(crate::wire_message::ConfirmReqMsg { block_hashes: hashes, remaining_hops: 3 });
                                if let Ok(bytes) = request.encode() {
                                    let peers: Vec<_> = peer_manager_ct.read().await.iter_connected().map(|(_, peer)| peer.clone()).collect();
                                    let result = broadcaster_ct.broadcast_with_fanout(&bytes, &peers, 4).await;
                                    tracing::debug!(candidates = candidates.len(), sent = result.sent, failed = result.failed, "requested missing consensus votes");
                                }
                            }
                        }
                        // Phase 1: emit FINAL votes for elections that reached
                        // SOFT quorum (rsnano two-phase: soft quorum → final
                        // votes → confirmation). Confirmation is gated on final
                        // votes, so without this an election that reached soft
                        // quorum would never cement.
                        {
                            let (our_rep, is_rep) = {
                                let vg = vote_generator_ct.lock().await;
                                (vg.representative.clone(), vg.is_representative)
                            };
                            if is_rep {
                                let ready = {
                                    let ae = active_elections_ct.read().await;
                                    ae.soft_quorum_needing_final(&our_rep)
                                };
                                if !ready.is_empty() {
                                    let our_weight =
                                        { rep_weights_ct.read().await.weight(&our_rep) };
                                    let now = Timestamp::new(unix_now_secs());
                                    for (root, winner) in ready {
                                        let fv = {
                                            let mut generator = vote_generator_ct.lock().await;
                                            match crate::durable_votes::generate_vote(&store_ct, &mut generator, winner, true) {
                                                Ok(vote) => vote,
                                                Err(crate::durable_votes::VoteGenerationError::DependenciesPending) => continue,
                                                Err(error) => {
                                                    tracing::error!(%winner, %error, "refusing conflicting or non-durable final vote");
                                                    continue;
                                                }
                                            }
                                        };
                                        // Count our own final weight locally.
                                        {
                                            let mut ae = active_elections_ct.write().await;
                                            let _ = ae.process_signed_vote(
                                                &root, burst_consensus::VoteInfo::new(our_rep.clone(), winner,
                                                    our_weight, true, Timestamp::new(fv.timestamp / 1000), fv.sequence), now,
                                            );
                                        }
                                        let wire = WireMessage::Vote(WireVote {
                                            voter: fv.voter,
                                            block_hashes: vec![fv.block_hash],
                                            is_final: true,
                                            timestamp: fv.timestamp,
                                            sequence: fv.sequence,
                                            signature: fv.signature,
                                        });
                                        if let Ok(bytes) = wire.encode() {
                                            let peers: Vec<burst_network::PeerState> = {
                                                let pm = peer_manager_ct.read().await;
                                                pm.iter_connected().map(|(_, s)| s.clone()).collect()
                                            };
                                            let _ = broadcaster_ct
                                                .broadcast_with_fanout(&bytes, &peers, 4)
                                                .await;
                                        }
                                    }
                                }
                            }
                        }

                        // Phase 2: collect confirmed elections
                        let confirmed = {
                            let ae = active_elections_ct.read().await;
                            ae.confirmed_elections()
                        };

                        let mut processed_winners = std::collections::HashSet::new();
                        for status in &confirmed {
                            let winner = status.winner;
                            // A fork hash may have votes before its full state can
                            // be installed. Do not advertise or cement it until
                            // validation and persistence have actually succeeded.
                            if !store_ct.block_store().get_block(&winner).ok()
                                .and_then(|bytes| bincode::deserialize::<StateBlock>(&bytes).ok())
                                .is_some_and(|block| block.hash == winner && block.compute_hash() == winner)
                            {
                                if store_ct.meta_store().fork_candidate(&winner).ok().flatten().is_some() {
                                    let _ = fork_install_tx.try_send(winner);
                                }
                                tracing::debug!(%winner, "election winner unavailable in ledger; confirmation deferred");
                                continue;
                            }
                            // Persist the handoff before retiring the election. A
                            // restart must resume cementation without new votes.
                            let handoff: Result<bool, burst_store::StoreError> = (|| {
                                let mut batch = store_ct.tx_begin_write()?;
                                if !crate::causal_cementation::handoff(&mut batch, winner)? {
                                    return Ok(false);
                                }
                                batch.commit()?;
                                Ok(true)
                            })();
                            match handoff {
                                Ok(true) => {}
                                Ok(false) => continue,
                                Err(error) => {
                                    tracing::error!(%winner, %error, "confirmation handoff failed; election retained");
                                    continue;
                                }
                            }
                            processed_winners.insert(winner);
                            metrics_ct.confirmation_latency_ms.observe(status.election_duration_ms as f64);
                        }

                        // Cleanup confirmed elections from active set to free capacity.
                        if !confirmed.is_empty() {
                            let mut ae = active_elections_ct.write().await;
                            let cleaned = ae.cleanup_confirmed_where(|winner| processed_winners.contains(winner));
                            if !cleaned.is_empty() {
                                tracing::debug!(
                                    count = cleaned.len(),
                                    "cleaned up confirmed elections"
                                );
                            }
                        }


                        // Cleanup expired elections
                        let now = Timestamp::new(unix_now_secs());
                        {
                            let mut ae = active_elections_ct.write().await;
                            let expired = ae.cleanup_expired(300_000, now);
                            if !expired.is_empty() {
                                tracing::debug!(
                                    count = expired.len(),
                                    "cleaned up expired elections"
                                );
                            }
                            // Update election count gauge
                            metrics_ct.election_count.set(ae.election_count() as i64);
                        }
                    }
                }
            }
        });
        self.task_handles.push(confirmation_handle);

        // ── Cementation task — durably cements confirmed blocks in batches ─
        let confirming_set_cement = Arc::clone(&self.confirming_set);
        let store_cement = Arc::clone(&self.store);
        let metrics_cement = Arc::clone(&self.metrics);
        let rep_weights_cement = Arc::clone(&self.rep_weights);
        let elections_cement = Arc::clone(&self.active_elections);
        let mut shutdown_rx_cement = self.shutdown.subscribe();

        let cementation_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(100));
            let mut cursor: Option<BlockHash> = None;
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_cement.recv() => {
                        tracing::info!("cementation task shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                        let pending = match store_cement.meta_store().pending_confirmations() {
                            Ok(pending) => pending,
                            Err(error) => {
                                tracing::error!(%error, "cannot read durable confirmation queue");
                                continue;
                            }
                        };
                        let start = cursor.map_or(0, |after| pending.partition_point(|hash| hash.as_bytes() <= after.as_bytes()));
                        let batch: Vec<_> = pending[start..].iter().chain(&pending[..start]).take(256).copied().collect();
                        if !batch.is_empty() {
                            let mut total_cemented: u64 = 0;
                            for block_hash in &batch {
                                cursor = Some(*block_hash);
                                match crate::causal_cementation::cement(&store_cement, *block_hash) {
                                    Ok(result) => {
                                        if let Err(error) = rebuild_rep_weights(&store_cement, &rep_weights_cement, &elections_cement).await {
                                            tracing::error!(%error, "cannot publish confirmed authority");
                                        }
                                        total_cemented += result.hashes.len() as u64;
                                        // Publish only after the heights and queue acknowledgment
                                        // have committed together. Causal ancestors are final too.
                                        for hash in &result.hashes {
                                            recently_confirmed_ct.write().await.insert(*hash);
                                            backlog_ct.lock().await.remove(hash);
                                            local_broadcaster_ct.lock().await.confirmed(hash);
                                            metrics_cement.blocks_confirmed.inc();
                                            if let Ok(bytes) = store_cement.block_store().get_block(hash) {
                                                if let Ok(block) = bincode::deserialize::<StateBlock>(&bytes) {
                                                    elections_cement.write().await.remove_root(&crate::ledger_updater::election_root(&block));
                                                    ws_state_ct.publish_confirmation(block.account.as_str(), &hash.to_string(), &block.trst_balance.to_string());
                                                }
                                            }
                                        }
                                        tracing::debug!(hash = %block_hash, complete = result.complete, "committed causal cementation");
                                    }
                                    Err(error) => {
                                        tracing::error!(hash = %block_hash, %error, "causal cementation failed; retrying");
                                    }
                                }
                            }

                            if total_cemented > 0 {
                                if let Err(error) = crate::fork_resolution::prune_finalized(&store_cement) {
                                    tracing::error!(%error, "finalized fork payload cleanup failed; cache retained");
                                }
                                let cs = confirming_set_cement.lock().await;
                                cs.record_cemented(total_cemented);
                                tracing::debug!(count = total_cemented, "cemented block batch");
                            }
                        }

                    }
                }
            }
        });
        self.task_handles.push(cementation_handle);

        // ── Governance tick task — periodically advances proposals through phases.
        //    When a proposal reaches activation, creates a GovernanceActivation
        //    block on the genesis chain (Tezos-style on-chain self-amendment).
        //    The actual param change is applied when that block is processed. ──
        let governance_tick = Arc::clone(&self.governance);
        let store_gov = Arc::clone(&self.store);
        let delegation_gov = Arc::clone(&self.delegation_engine);
        let block_queue_gov = Arc::clone(&self.block_queue);
        let frontier_gov = Arc::clone(&self.frontier);
        let mut shutdown_rx_gov = self.shutdown.subscribe();
        let mut gov_params = self.config.params.clone();
        let genesis_network_gov = self.config.network;
        let certified_governance = self.certified_selection_enabled();

        let gov_tick_handle = tokio::spawn(async move {
            if certified_governance {
                return;
            }
            let mut interval = tokio::time::interval(Duration::from_secs(10));
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_gov.recv() => {
                        tracing::info!("governance tick task shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                        let now = Timestamp::new(unix_now_secs());
                        // Lock order gov → del matches the governance-vote path
                        // (no deadlock). The tally resolves delegated votes and
                        // uses the live verified-wallet count as the denominator.
                        let live_governance = governance_tick.lock().await;
                        let verified_wallets: Vec<_> = match store_gov.account_store().iter_verified_accounts() {
                            Ok(accounts) if accounts.len() <= u32::MAX as usize => accounts.into_iter().map(|a| a.address).collect(),
                            Ok(_) => {
                                tracing::error!("governance electorate exceeds supported size");
                                continue;
                            }
                            Err(error) => {
                                tracing::error!(%error, "cannot tally governance without the electorate");
                                continue;
                            }
                        };
                        let del = delegation_gov.lock().await;
                        match store_gov.brn_store().get_meta(b"protocol_params") {
                            Ok(Some(bytes)) => match bincode::deserialize(&bytes) {
                                Ok(params) => gov_params = params,
                                Err(error) => {
                                    tracing::error!(%error, "cannot schedule governance with corrupt parameters");
                                    continue;
                                }
                            },
                            Ok(None) => {},
                            Err(error) => {
                                tracing::error!(%error, "cannot read governance parameters");
                                continue;
                            }
                        }
                        let gov = live_governance.project_at(now, &gov_params, &del, &verified_wallets);
                        let activated = gov.due_activation_hashes(now);
                        drop(del);
                        drop(live_governance);
                        if !activated.is_empty() {
                            // For each activated proposal, create a GovernanceActivation
                            // block on the genesis chain. The block processing loop
                            // applies the param change when it processes the block.
                            // One candidate per account head. Remaining proposals
                            // are retried after this activation commits.
                            for proposal_hash in activated.iter().take(1) {
                                let proposal_snapshot = gov.get_proposal(proposal_hash).cloned();
                                if let Some(proposal) = proposal_snapshot {
                                    if let Ok(tentative_params) = gov.preview_activation(&proposal, &gov_params) {
                                        let new_params_hash = tentative_params.params_hash();

                                        let kp = match genesis_key::genesis_signing_key(genesis_network_gov) {
                                            Some(kp) => kp,
                                            None => {
                                                tracing::debug!(%proposal_hash, "node lacks genesis authority — skipping activation block authoring (will sync from creator)");
                                                continue;
                                            }
                                        };
                                        let genesis_addr = genesis_key::genesis_address(genesis_network_gov);
                                        let genesis_head = {
                                            let f = frontier_gov.read().await;
                                            f.get_head(&genesis_addr).copied()
                                        };
                                        let previous = match genesis_head {
                                            Some(h) => h,
                                            None => {
                                                tracing::warn!(%proposal_hash, "genesis account not in frontier, cannot create activation block");
                                                continue;
                                            }
                                        };

                                        let genesis_acct = match store_gov.account_store().get_account(&genesis_addr) {
                                            Ok(acct) if acct.head == previous => acct,
                                            _ => continue,
                                        };
                                        let predecessor: StateBlock = match store_gov.block_store().get_block(&previous)
                                            .ok().and_then(|bytes| bincode::deserialize(&bytes).ok()) {
                                                Some(block) => block,
                                                None => continue,
                                            };
                                        let Some(activation_at) = proposal.activation_at else { continue; };
                                        let payment_epoch = match store_gov.meta_store().payment_epoch() {
                                            Ok(epoch) => epoch,
                                            Err(error) => {
                                                tracing::error!(%error, "cannot author activation without payment epoch");
                                                continue;
                                            }
                                        };

                                        let mut block = StateBlock {
                                            signing_grant: predecessor.active_signing_grant(),
                                            delegation_payload: Vec::new(),
                                            version: CURRENT_BLOCK_VERSION, payment_epoch,
                                            block_type: BlockType::GovernanceActivation,
                                            account: genesis_addr,
                                            previous,
                                            representative: genesis_acct.representative,
                                            brn_balance: predecessor.brn_balance,
                                            trst_balance: genesis_acct.trst_balance,
                                            link: BlockHash::new(*proposal_hash.as_bytes()),
                                            origin: TxHash::ZERO,
                                            transaction: TxHash::new(*new_params_hash.as_bytes()),
                                            timestamp: activation_at,
                                            params_hash: gov_params.params_hash(),
                                            target_head: burst_types::BlockHash::ZERO,
                                            merge_sources: Vec::new(), release_sources: Vec::new(),
                                            work: 0,
                                            signature: Signature([0u8; 64]),
                                            hash: BlockHash::ZERO,
                                        };
                                        block.hash = block.compute_hash();
                                        block.signature = burst_crypto::sign_message(
                                            block.hash.as_bytes(),
                                            &kp.private,
                                        );

                                        let work_thresholds = burst_work::WorkThresholds::with_base(
                                            gov_params.min_work_difficulty,
                                        );
                                        let threshold = work_thresholds.threshold_for(
                                            burst_work::WorkBlockKind::Epoch,
                                        );
                                        let generator = WorkGenerator;
                                        match generator.generate(&block.work_root(), threshold) {
                                            Ok(nonce) => block.work = nonce.0,
                                            Err(e) => {
                                                tracing::warn!(error = %e, "failed to generate PoW for activation block");
                                                continue;
                                            }
                                        }

                                        tracing::info!(
                                            %proposal_hash,
                                            params_hash = %new_params_hash,
                                            block_hash = %block.hash,
                                            "created GovernanceActivation block on genesis chain"
                                        );

                                        // Submit through the block queue for normal processing
                                        if !block_queue_gov.try_push(block) {
                                            tracing::warn!("block queue full, activation block not submitted");
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
        self.task_handles.push(gov_tick_handle);

        // ── Local re-broadcaster — retransmits locally created blocks ────
        let local_broadcaster_rb = Arc::clone(&self.local_broadcaster);
        let broadcaster_rb = self.broadcaster.clone();
        let peer_manager_rb = Arc::clone(&self.peer_manager);
        let mut shutdown_rx_rb = self.shutdown.subscribe();

        let rebroadcast_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(500));
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_rb.recv() => {
                        tracing::info!("local re-broadcaster shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                        let now_ms = unix_now_ms();
                        let blocks = {
                            let mut lb = local_broadcaster_rb.lock().await;
                            lb.blocks_needing_rebroadcast(now_ms)
                        };
                        if !blocks.is_empty() {
                            let peers: Vec<burst_network::PeerState> = {
                                let pm = peer_manager_rb.read().await;
                                pm.iter_connected().map(|(_, s)| s.clone()).collect()
                            };
                            for (hash, block_bytes) in &blocks {
                                let _ = broadcaster_rb
                                    .broadcast_with_fanout(block_bytes, &peers, 4)
                                    .await;
                                tracing::trace!(%hash, "re-broadcast local block");
                            }
                            tracing::debug!(count = blocks.len(), "re-broadcast local blocks");
                        }
                        // Cleanup blocks that exhausted retries
                        {
                            let mut lb = local_broadcaster_rb.lock().await;
                            lb.cleanup_expired();
                        }
                    }
                }
            }
        });
        self.task_handles.push(rebroadcast_handle);

        // ── Outbound message drain (sends queued messages to peers) ───────
        let mut outbound_rx = outbound_rx;
        let mut shutdown_rx2 = self.shutdown.subscribe();
        let conn_registry_drain = Arc::clone(&self.connection_registry);

        let drain_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx2.recv() => {
                        tracing::info!("outbound message task shutting down");
                        break;
                    }
                    Some((peer_id, msg_bytes)) = outbound_rx.recv() => {
                        // Enqueue to the peer's channel (non-blocking). Bandwidth
                        // throttling and the socket write happen in the channel's
                        // own writer task, so a slow peer can't stall this drain
                        // (no head-of-line blocking). A write error there closes
                        // the socket and the read loop performs cleanup.
                        let sent = conn_registry_drain.read().await.send(&peer_id, msg_bytes);
                        if !sent {
                            tracing::trace!(
                                peer = %peer_id,
                                "outbound message dropped: no connection or queue full"
                            );
                        }
                    }
                }
            }
        });
        self.task_handles.push(drain_handle);

        // ── Expired TRST cleanup task ──────────────────────────────────────
        // Every 60s:
        //  1. flush expired tokens across tracked portfolios (amortized —
        //     per-wallet earliest-expiry makes untouched wallets O(1))
        //  2. return expired pending sends to their senders (6.16a)
        //  3. clean up stale expiry index entries
        let store_expiry = Arc::clone(&self.store);
        let trst_engine_expiry = Arc::clone(&self.trst_engine);
        let certified_expiry = self.certified_selection_enabled();
        let mut shutdown_rx_expiry = self.shutdown.subscribe();

        let expiry_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_expiry.recv() => {
                        tracing::info!("expired TRST cleanup task shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                        let now_secs = unix_now_secs();
                        let now = Timestamp::new(now_secs);
                        let cutoff = Timestamp::new(now_secs);

                        {
                            let mut trst = trst_engine_expiry.lock().await;
                            let result = if certified_expiry {
                                crate::expiry_commit::apply_held(&store_expiry, &mut trst, now)
                            } else {
                                crate::expiry_commit::apply(&store_expiry, &mut trst, now)
                            };
                            if let Err(error) = result {
                                tracing::error!(%error, "expiry transaction failed; live portfolios unchanged");
                            }
                        }

                        // Verification deadlines are installed by certified checkpoints.

                        // 4. Clean up expiry index entries (prevents index bloat).
                        let trst_idx = store_expiry.trst_index_store();
                        match trst_idx.clean_expiry_before(cutoff, 1024) {
                            Ok(count) if count > 0 => {
                                tracing::debug!(count, cutoff = now_secs, "cleaned expired TRST lookup rows");
                            }
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!("failed to query expired TRST indices: {e}");
                            }
                        }
                    }
                }
            }
        });
        self.task_handles.push(expiry_handle);

        // Expired unchecked blocks are removed on the next one-minute sweep.
        let block_processor_uc = Arc::clone(&self.block_processor);
        let mut shutdown_rx_uc = self.shutdown.subscribe();

        let unchecked_cleanup_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_uc.recv() => {
                        tracing::info!("unchecked cleanup task shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                        let mut bp = block_processor_uc.lock().await;
                        let now = unix_now_secs();
                        let removed = bp.cleanup_unchecked(crate::unchecked::MAX_AGE_SECS, now);
                        if removed > 0 {
                            tracing::debug!(
                                removed,
                                remaining = bp.unchecked_count(),
                                "cleaned expired unchecked entries"
                            );
                        }
                    }
                }
            }
        });
        self.task_handles.push(unchecked_cleanup_handle);

        // ── Priority scheduler drain — creates elections for highest-priority blocks ──
        let priority_scheduler_drain = Arc::clone(&self.priority_scheduler);
        let active_elections_sched = Arc::clone(&self.active_elections);
        let store_sched = Arc::clone(&self.store);
        let rebroadcast_sched = Arc::clone(&self.local_broadcaster);
        let mut shutdown_rx_sched = self.shutdown.subscribe();

        let scheduler_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(100));
            let mut scan = crate::backlog_scan::BacklogScan::default();
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_sched.recv() => {
                        tracing::debug!("priority scheduler drain task shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                        // One next-unconfirmed block per account, from a bounded
                        // snapshot page. Elections must acquire fresh votes.
                        match scan.next(&store_sched, 256) {
                            Ok(pending) => {
                                let mut scheduler = priority_scheduler_drain.lock().await;
                                for block in pending {
                                    match WireMessage::Block(Box::new(block.clone())).encode() {
                                        Ok(bytes) => rebroadcast_sched.lock().await.track_pending(block.hash, bytes, unix_now_ms()),
                                        Err(error) => tracing::error!(hash = %block.hash, %error, "cannot encode pending block for recovery"),
                                    }
                                    if active_elections_sched.read().await.candidate_root(&block.hash).is_none() {
                                        scheduler.push(block.hash, block.account, block.trst_balance.min(u64::MAX as u128) as u64);
                                    }
                                }
                            }
                            Err(error) => tracing::error!(%error, "cannot scan unconfirmed account backlog"),
                        }
                        let mut sched = priority_scheduler_drain.lock().await;
                        let ae = active_elections_sched.read().await;
                        let capacity = MAX_ACTIVE_ELECTIONS.saturating_sub(ae.election_count());
                        drop(ae);

                        let mut started = 0usize;
                        while started < capacity {
                            match sched.pop() {
                                Some((hash, _account)) => {
                                    let block = store_sched.block_store().get_block(&hash).ok()
                                        .and_then(|bytes| bincode::deserialize::<StateBlock>(&bytes).ok());
                                    let Some(block) = block.filter(|block| block.hash == hash && block.compute_hash() == hash) else {
                                        tracing::warn!(%hash, "scheduler candidate missing or corrupt");
                                        continue;
                                    };
                                    let now = Timestamp::new(unix_now_secs());
                                    let mut ae = active_elections_sched.write().await;
                                    match crate::backlog_scan::register_candidate(&store_sched, &mut ae, &block, now) {
                                        Ok(true) => started += 1,
                                        Ok(false) => {},
                                        Err(error) => tracing::warn!(%hash, %error, "scheduler candidate deferred"),
                                    }
                                }
                                None => break,
                            }
                        }
                        if started > 0 {
                            tracing::debug!(count = started, "priority scheduler started elections");
                        }
                    }
                }
            }
        });
        self.task_handles.push(scheduler_handle);

        // ── Online weight periodic sampling ──────────────────────────────────
        let online_weight_tracker_bg = Arc::clone(&self.online_weight_tracker);
        let online_weight_sampler_bg = Arc::clone(&self.online_weight_sampler);
        let rep_weights_bg = Arc::clone(&self.rep_weights);
        let active_elections_ow = Arc::clone(&self.active_elections);
        let store_ow = Arc::clone(&self.store);
        let mut shutdown_rx_ow = self.shutdown.subscribe();

        let online_weight_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(20));
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_ow.recv() => {
                        tracing::debug!("online weight sampling task shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                        let now_secs = unix_now_secs();
                        // Refresh equal-person weights for verification and
                        // delegation changes. TRST expiry cannot change weight.
                        if let Err(error) = rebuild_rep_weights(&store_ow, &rep_weights_bg, &active_elections_ow).await {
                            tracing::error!(%error, "cannot refresh confirmed voting authority");
                            continue;
                        }
                        let rw = rep_weights_bg.read().await;
                        let weight_map = rw.all_weights();
                        drop(rw);
                        let sampler = online_weight_sampler_bg.lock().await;
                        let total_online = sampler.online_weight(now_secs, &weight_map);
                        drop(sampler);

                        let mut tracker = online_weight_tracker_bg.lock().await;
                        tracker.record_sample(total_online, Timestamp::new(now_secs));

                        // Also update the EMA trend in the per-rep sampler
                        let mut sampler = online_weight_sampler_bg.lock().await;
                        sampler.update_trend(total_online);
                        let effective = sampler.effective_weight(now_secs, &weight_map);
                        drop(sampler);

                        // Attendance is telemetry, never authority to lower quorum.

                        tracing::trace!(
                            total_online,
                            effective,
                            trended = tracker.trended_weight(),
                            quorum_delta = tracker.quorum_delta(),
                            "online weight sample recorded"
                        );

                        // Persist the online weight sample to LMDB
                        let rw_store = store_ow.rep_weight_store();
                        if let Err(e) = rw_store.put_online_weight_sample(now_secs, total_online) {
                            tracing::warn!(error = %e, "failed to persist online weight sample");
                        }
                    }
                }
            }
        });
        self.task_handles.push(online_weight_handle);

        Ok(())
    }

    async fn start_recovery_listener(
        &mut self,
        bootstrap_feedback_tx: &mpsc::Sender<crate::bootstrap::BootstrapFeedback>,
    ) -> Result<(), NodeError> {
        let p2p_port = self.config.port;
        let peer_manager = Arc::clone(&self.peer_manager);
        let mut shutdown_rx_p2p = self.shutdown.subscribe();
        let metrics_p2p = Arc::clone(&self.metrics);
        let conn_registry_p2p = Arc::clone(&self.connection_registry);
        let block_queue_p2p = Arc::clone(&self.block_queue);
        let active_elections_p2p = Arc::clone(&self.active_elections);
        let rep_weights_p2p = Arc::clone(&self.rep_weights);
        let vote_generator_p2p = Arc::clone(&self.vote_generator);
        let syn_cookies_p2p = Arc::clone(&self.syn_cookies);
        let message_dedup_p2p = Arc::clone(&self.message_dedup);
        let online_weight_sampler_p2p = Arc::clone(&self.online_weight_sampler);
        let store_p2p = Arc::clone(&self.store);
        let node_address_p2p = self.node_address.clone();
        let node_private_key_p2p = Arc::new(burst_types::PrivateKey(self.node_private_key.0));
        let config_params_p2p = self.config.params.clone();
        let network_p2p = self.config.network;
        let peering_port_p2p = self.config.port;
        let bootstrap_feedback_p2p = bootstrap_feedback_tx.clone();

        let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{p2p_port}"))
            .await
            .map_err(|error| NodeError::Other(format!("cannot bind P2P listener: {error}")))?;
        tracing::info!(port = p2p_port, "P2P recovery listener started");
        let p2p_handle = tokio::spawn(async move {
            let params_hash_p2p = config_params_p2p.params_hash();
            let mut handshakes = tokio::task::JoinSet::new();

            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_p2p.recv() => {
                        tracing::info!("P2P listener shutting down");
                        break;
                    }
                    Some(result) = handshakes.join_next(), if !handshakes.is_empty() => {
                        if let Err(error) = result { tracing::debug!(%error, "inbound handshake task ended"); }
                    }
                    result = listener.accept() => {
                        match result {
                            Ok((stream, addr)) => {
                                if let Err(error) = burst_network::connection::configure_peer_stream(&stream) {
                                    tracing::debug!(%addr, %error, "inbound TCP setup failed");
                                    continue;
                                }
                                // Handle each inbound connection in its own task so a
                                // slow or malicious handshake never head-of-line-blocks
                                // the accept loop (matches rsnano's per-connection model).
                                let syn_cookies_c = Arc::clone(&syn_cookies_p2p);
                                let conn_registry_c = Arc::clone(&conn_registry_p2p);
                                let peer_manager_c = Arc::clone(&peer_manager);
                                let metrics_c = Arc::clone(&metrics_p2p);
                                let block_queue_c = Arc::clone(&block_queue_p2p);
                                let active_elections_c = Arc::clone(&active_elections_p2p);
                                let rep_weights_c = Arc::clone(&rep_weights_p2p);
                                let vote_generator_c = Arc::clone(&vote_generator_p2p);
                                let message_dedup_c = Arc::clone(&message_dedup_p2p);
                                let online_weight_sampler_c = Arc::clone(&online_weight_sampler_p2p);
                                let store_c = Arc::clone(&store_p2p);
                                let our_node_id = node_address_p2p.clone();
                                let node_private_key_c = Arc::clone(&node_private_key_p2p);
                                let params_hash_c = params_hash_p2p;
                                let network_c = network_p2p;
                                let peering_port_c = peering_port_p2p;
                                let bootstrap_feedback_c = bootstrap_feedback_p2p.clone();

                                handshakes.spawn(async move {
                                    let now_secs = unix_now_secs();
                                    let peer_ip = addr.ip().to_string();
                                    let peer_addr = PeerAddress { ip: peer_ip.clone(), port: addr.port() };
                                    let peer_id = format!("{}:{}", addr.ip(), addr.port());

                                    let cookie_id = addr.to_string();
                                    let Some(cookie) = syn_cookies_c.lock().await.generate_for_connection(&peer_ip, &cookie_id) else {
                                        tracing::debug!(peer = %peer_id, "handshake challenge unavailable");
                                        return;
                                    };
                                    let (mut read_half, mut write_half) = stream.into_split();
                                    let challenge = crate::wire_message::HandshakeMsg {
                                        node_id: our_node_id.clone(),
                                        cookie: Some(cookie),
                                        cookie_signature: None,
                                        params_hash: params_hash_c,
                                        network_id: network_c,
                                        peering_port: peering_port_c,
                                        block_version: CURRENT_BLOCK_VERSION,
                                        wire_version: burst_types::PROTOCOL_VERSION,
                                    };
                                    let exchange = async {
                                        use crate::peer_handshake::{self, Role};
                                        peer_handshake::write(&mut write_half, &challenge).await?;
                                        let remote = peer_handshake::read(&mut read_half).await?;
                                        let expected = syn_cookies_c.lock().await.take_cookie(&cookie_id)
                                            .ok_or("expired listener challenge")?;
                                        if challenge.cookie != Some(expected) { return Err("listener challenge changed".to_string()); }
                                        peer_handshake::validate(&remote, &challenge)?;
                                        peer_handshake::verify(Role::Dialer, &challenge, &remote,
                                            remote.cookie_signature.as_ref().ok_or("missing dialer proof")?)?;
                                        let mut proof = challenge.clone();
                                        proof.cookie = None;
                                        proof.cookie_signature = Some(peer_handshake::sign(Role::Listener, &challenge, &remote, &node_private_key_c)?);
                                        peer_handshake::write(&mut write_half, &proof).await?;
                                        Ok::<_, String>(remote)
                                    };
                                    let remote = match tokio::time::timeout(Duration::from_secs(15), exchange).await {
                                        Ok(Ok(remote)) => remote,
                                        result => {
                                            tracing::debug!(peer = %peer_id, error = ?result, "mutual handshake failed");
                                            return;
                                        }
                                    };
                                    let remote_node_id = remote.node_id;
                                    let remote_peering_port = remote.peering_port;

                                    // The listening port is not a NAT mapping. Route by the
                                    // authenticated identity, never public-IP:local-port.
                                    // Retain the observed socket only as transport metadata.
                                    let _ = remote_peering_port;
                                    let peer_id = remote_node_id.as_str().to_string();

                                    // Never accept a connection claiming our own node_id.
                                    if remote_node_id == our_node_id {
                                        tracing::debug!(peer = %peer_id, "dropping inbound self-connection");
                                        return;
                                    }

                                    let our_nid = our_node_id.as_str().to_string();
                                    let peer_nid = remote_node_id.as_str().to_string();

                                    // Deterministic node_id dedup: if we already dialed this
                                    // peer, keep exactly one (the connection from the lower
                                    // node_id). `None` → this inbound lost; drop it.
                                    let conn_id = {
                                        let mut pm = peer_manager_c.write().await;
                                        let mut registry = conn_registry_c.write().await;
                                        match registry.register_dedup(
                                            peer_nid,
                                            crate::connection_registry::Direction::Inbound,
                                            write_half,
                                            &our_nid,
                                        ) {
                                            Some(id) => {
                                                if !pm.register_connection(peer_id.clone(), peer_addr, false, now_secs) {
                                                    registry.remove_if(&peer_id, id);
                                                    metrics_c.peer_count.set(registry.len() as i64);
                                                    return;
                                                }
                                                metrics_c.peer_count.set(registry.len() as i64);
                                                id
                                            }
                                            None => {
                                                tracing::debug!(peer = %peer_id, "inbound superseded by our outbound (tie-break)");
                                                return;
                                            }
                                        }
                                    };

                                    // Only authenticated peers reach message dispatch.
                                    let reader_registry = conn_registry_c.clone();
                                    let mut registry = reader_registry.write().await;
                                    let reader_task = spawn_peer_read_loop(
                                        peer_id.clone(),
                                        conn_id,
                                        read_half,
                                        block_queue_c,
                                        conn_registry_c,
                                        peer_manager_c,
                                        metrics_c,
                                        active_elections_c,
                                        rep_weights_c,
                                        message_dedup_c,
                                        online_weight_sampler_c,
                                        store_c,
                                        params_hash_c,
                                        bootstrap_feedback_c,
                                        vote_generator_c,
                                    );
                                    registry.track_task(reader_task);

                                    tracing::info!(peer = %peer_id, "inbound peer connected");
                                });
                            }
                            Err(e) => {
                                tracing::warn!("P2P accept error: {e}");
                            }
                        }
                    }
                }
            }
            handshakes.shutdown().await;
        });
        self.task_handles.push(p2p_handle);

        Ok(())
    }

    /// Start the node — begin listening for connections and processing blocks.
    ///
    /// This method:
    /// 1. Initializes the genesis block if the database is empty
    /// 2. Starts the P2P TCP listener
    /// 3. Connects to bootstrap peers
    /// 4. Optionally starts the RPC server
    /// 5. Optionally starts the WebSocket server
    /// 6. Waits for the shutdown signal
    pub async fn start(&mut self) -> Result<(), NodeError> {
        if self.outbound_rx.is_none() {
            return Err(NodeError::Other("node has already been started".into()));
        }
        let result = self.start_inner().await;
        if result.is_err() {
            self.outbound_rx.take();
            if let Err(error) = self.stop().await {
                tracing::error!(%error, "cleanup after failed startup");
            }
        }
        result
    }

    async fn start_inner(&mut self) -> Result<(), NodeError> {
        if self.config.snapshot_bootstrap.is_some() && !self.certified_selection_enabled() {
            return Err(NodeError::Config(
                "snapshot bootstrap is behind the release gate".into(),
            ));
        }
        tracing::info!(
            network = ?self.config.network,
            port = self.config.port,
            data_dir = %self.config.data_dir.display(),
            "BURST node starting"
        );

        // Ascending-bootstrap feedback channel: peer read loops report AscPullAck
        // results here; the bootstrap task owns the receiver and drives the
        // Bootstrapper from them.
        let (bootstrap_feedback_tx, bootstrap_feedback_rx) =
            mpsc::channel::<crate::bootstrap::BootstrapFeedback>(1024);

        // Initialize genesis if needed
        self.initialize_genesis()?;
        #[cfg(test)]
        {
            self.connection_registry.write().await.recovery_probe =
                self.checkpoint_publication_probe;
        }
        self.start_recovery_listener(&bootstrap_feedback_tx).await?;
        if crate::governance_content::enabled(self.config.network) {
            let (content_tx, content_rx) =
                mpsc::channel(crate::governance_content::transfer::INBOX_CAPACITY);
            let recovery = Arc::new(crate::governance_content::transfer::Transfers::default());
            {
                let mut registry = self.connection_registry.write().await;
                registry.set_governance_inbox(content_tx);
                registry.set_recovery_content(recovery.clone());
            }
            self.task_handles
                .push(tokio::spawn(crate::governance_content::transfer::run(
                    self.governance_transfers.clone(),
                    recovery,
                    self.store.clone(),
                    self.block_queue.clone(),
                    self.connection_registry.clone(),
                    content_rx,
                    self.shutdown.clone(),
                )));
        }
        crate::checkpoint_sync::witness::start(
            self.store.clone(),
            self.connection_registry.clone(),
            self.shutdown.clone(),
        )
        .await;
        self.start_recovery_peers(&bootstrap_feedback_tx).await;
        self.install_bootstrap_snapshot().await?;
        let compact = if self.certified_selection_enabled() {
            crate::confirmed_replay::snapshot::installed::recover_metadata_from_peers(
                self.store.clone(),
                self.config.network,
                self.initial_params.clone(),
                self.connection_registry.clone(),
            )
            .await?
        } else {
            None
        };
        if let Some(recovered) = compact {
            let clock = self.clock_sync.lock().await.now();
            #[cfg(test)]
            let clock = self
                .verification_fixture_clock
                .as_ref()
                .and_then(|clock| clock())
                .map(|reading| Timestamp::new(reading.now))
                .unwrap_or(clock);
            self.publish_recovered(recovered, clock).await?;
        } else {
            if self.certified_selection_enabled() {
                let store = self.store.clone();
                let network = self.config.network;
                let params = self.initial_params.clone();
                let authority = tokio::task::spawn_blocking(move || {
                    crate::payment_authority::initialize(&store, network, &params)
                })
                .await
                .map_err(|error| {
                    NodeError::Other(format!("authority recovery worker failed: {error}"))
                })??;
                let mut processor = self.block_processor.lock().await;
                processor.install_payment_epoch(authority.epoch, authority.cutoff);
                processor.install_params(&self.config.params);
                drop(processor);
            }

            // Initialize the creator once; restart must preserve later adjudications.
            {
                let genesis_addr = genesis_key::genesis_address(self.config.network);
                let acct_store = self.store.account_store();
                let existing = match acct_store.get_account(&genesis_addr) {
                    Ok(account) => Some(account),
                    Err(burst_store::StoreError::NotFound(_)) => None,
                    Err(error) => {
                        return Err(NodeError::Other(format!(
                            "cannot read genesis account: {error}"
                        )))
                    }
                };

                // The genesis block IS the account's first block, so the account
                // record's head must point to it (block_count=1). Reading it back
                // from the frontier `initialize_genesis` just wrote keeps head and
                // block_count consistent — otherwise head==ZERO makes the account
                // look empty, and the first genesis transaction (endorse/burn) gets
                // built as an Open block that spends BRN, which every other node
                // correctly rejects (breaking genesis-chain bootstrap).
                let genesis_head = self
                    .store
                    .frontier_store()
                    .get_frontier(&genesis_addr)
                    .map_err(|error| {
                        NodeError::Other(format!("cannot read genesis frontier: {error}"))
                    })?;

                let is_new = existing.is_none();
                if let Some(info) = crate::genesis_state::account_to_write(
                    existing,
                    genesis_addr.clone(),
                    genesis_head,
                ) {
                    acct_store.put_account(&info).map_err(|error| {
                        NodeError::Other(format!("cannot initialize genesis account: {error}"))
                    })?;
                    if is_new {
                        self.ledger_cache.inc_account_count();
                    }
                    tracing::info!(%genesis_addr, head = %info.head, "genesis account initialized or frontier repaired");
                }

                // Track the genesis account in the BRN engine (accruing from epoch
                // 0, matching its verified_at) so it has BRN to burn when authoring
                // bootstrap endorsements — otherwise the endorse burn and the
                // computed-BRN spend check would reject it.
                {
                    let mut brn = self.brn_engine.lock().await;
                    if brn.get_wallet(&genesis_addr).is_none() {
                        let account = acct_store.get_account(&genesis_addr).map_err(|error| {
                            NodeError::Other(format!("cannot read genesis account: {error}"))
                        })?;
                        if account.state.accrues_brn() {
                            if account.block_count != 1
                                || account.total_brn_burned != 0
                                || account.total_brn_staked != 0
                            {
                                return Err(NodeError::Other("missing BRN history for existing genesis account; refusing to reset earned/spent state".into()));
                            }
                            let verified_at = account.verified_at.ok_or_else(|| {
                                NodeError::Other(
                                    "verified genesis has no verification timestamp".into(),
                                )
                            })?;
                            let wallet = burst_brn::BrnWalletState::new(verified_at);
                            let bytes = bincode::serialize(&wallet)
                                .map_err(|error| NodeError::Other(error.to_string()))?;
                            let mut batch = self.store.tx_begin_write()?;
                            batch.put_brn_wallet(&genesis_addr, &bytes)?;
                            batch.commit()?;
                            brn.track_wallet(genesis_addr.clone(), wallet);
                        }
                    }
                }
            }

            // Re-load frontier after genesis init (in case we just created it)
            {
                let new_frontier = Self::load_frontier_from_store(&self.store)?;
                let mut f = self.frontier.write().await;
                *f = new_frontier;
            }

            // Use the same complete persisted state as ordinary admission.
            let restored = {
                let batch = self.store.tx_begin_write()?;
                crate::governance_commit::CandidateGovernance::restore(
                    &batch,
                    self.config.params.clone(),
                    self.config.network,
                    crate::genesis_state::block(self.config.network, &self.initial_params).hash,
                )
                .map_err(|error| {
                    NodeError::Other(format!(
                        "{error}; restore a known-good backup or re-sync into a new data directory"
                    ))
                })?
            };
            *self.governance.lock().await = restored.governance;
            *self.delegation_engine.lock().await = restored.delegation;
            *self.consti_engine.lock().await = restored.constitution;

            // Genesis persists both TRST snapshots atomically. A missing snapshot
            // on an initialized ledger is not authority to reset economic history.
            {
                let graph = {
                    let batch = self.store.tx_begin_write()?;
                    crate::graph_storage::load(&batch).map_err(|error| {
                        NodeError::Other(format!("cannot restore persisted merger graph: {error}; restore a known-good backup or re-sync into a new data directory"))
                    })?
                };
                self.trst_engine.lock().await.merger_graph = graph;
                tracing::info!("restored merger graph from LMDB");
            }

            // Restore the complete snapshot, including private outcome causes.
            // Copying only portfolios and ownership loses independent revocations.
            {
                let mut trst = self.trst_engine.lock().await;
                let batch = self.store.tx_begin_write()?;
                let mut restored = crate::trst_storage::load(&batch, self.config.params.trst_expiry_secs)
                    .map_err(|e| NodeError::Other(format!(
                        "cannot restore persisted TRST holdings: {e}; restore a known-good backup or re-sync into a new data directory"
                    )))?;
                restored.merger_graph = trst.merger_graph.clone();
                restored.validate_bases().map_err(|e| NodeError::Other(format!(
                        "inconsistent persisted TRST snapshots: {e}; restore a known-good backup or re-sync into a new data directory"
                    )))?;
                *trst = restored;
                tracing::info!("TRST engine wallet portfolios restored from LMDB");
            }

            {
                let mut live = self.verification_orchestrator.lock().await;
                let batch = self.store.tx_begin_write()?;
                *live = crate::endorsement_commit::restore(&batch).map_err(NodeError::Other)?;
            }

            // Derive equal-person weights from ledger accounts, never a historical
            // hybrid/bonus snapshot. No special genesis voting weight is injected.
            rebuild_rep_weights(&self.store, &self.rep_weights, &self.active_elections)
                .await
                .map_err(|e| {
                    NodeError::Other(format!("cannot restore confirmed voting authority: {e}"))
                })?;
        }

        // Perform initial NTP clock synchronization
        {
            let mut cs = self.clock_sync.lock().await;
            if let Err(e) = cs.sync_ntp().await {
                tracing::warn!("initial NTP sync failed (will retry): {e}");
            } else {
                tracing::info!(offset_ms = cs.offset_ms, "initial NTP clock sync complete");
            }
        }

        // No expiry, governance, block-processing or cementation worker may
        // observe the empty engines constructed by new() before this point.
        // Record a fresh chain's economic baseline before its first reversible
        // block. Never reconstruct missing history on an existing chain.
        {
            let brn = self.brn_engine.lock().await;
            let trst = self.trst_engine.lock().await;
            let mut batch = self
                .store
                .tx_begin_write()
                .map_err(|e| NodeError::Other(e.to_string()))?;
            let missing = batch
                .undo_value(
                    burst_store_lmdb::block_undo::UndoTable::BrnMeta,
                    b"rate_history",
                )
                .map_err(|e| NodeError::Other(e.to_string()))?
                .is_none()
                || !crate::trst_storage::present(&batch)
                    .map_err(|e| NodeError::Other(e.to_string()))?
                || !crate::graph_storage::present(&batch)
                    .map_err(|e| NodeError::Other(e.to_string()))?;
            if missing {
                if batch
                    .ledger_counts()
                    .map_err(|e| NodeError::Other(e.to_string()))?
                    .0
                    != 1
                {
                    return Err(NodeError::Other(
                        "missing economic snapshot for existing history".into(),
                    ));
                }
                crate::ledger_updater::persist_economics(&mut batch, &brn, &trst)
                    .map_err(NodeError::Other)?;
                batch
                    .commit()
                    .map_err(|e| NodeError::Other(e.to_string()))?;
            }
        }
        // Recovery above has rebuilt missing provisional journals. Change only
        // their storage representation before workers can admit new blocks.
        let migration_store = self.store.clone();
        let migration_expiry = self.config.params.trst_expiry_secs;
        tokio::task::spawn_blocking(move || {
            crate::trst_storage::migrate(&migration_store, migration_expiry)?;
            crate::graph_storage::migrate(&migration_store)
        })
        .await
        .map_err(|error| NodeError::Other(format!("TRST migration worker failed: {error}")))??;

        // Cached candidates must pass normal fork admission again. Restoring
        // their bytes does not restore a quorum or authorize installation.
        crate::fork_resolution::prune_finalized(&self.store)
            .map_err(|e| NodeError::Other(format!("cannot prune finalized fork payloads: {e}")))?;
        for (hash, bytes) in self
            .store
            .meta_store()
            .fork_candidates()
            .map_err(|e| NodeError::Other(e.to_string()))?
        {
            if self
                .store
                .block_store()
                .exists(&hash)
                .map_err(|e| NodeError::Other(e.to_string()))?
            {
                continue;
            }
            let block: StateBlock =
                bincode::deserialize(&bytes).map_err(|e| NodeError::Other(e.to_string()))?;
            if block.hash != hash || block.compute_hash() != hash {
                return Err(NodeError::Other("cached fork identity mismatch".into()));
            }
            if !self.block_queue.push(block).await {
                return Err(NodeError::Other(
                    "block queue full during fork recovery".into(),
                ));
            }
        }
        if let Some(error) = self.shutdown.failure() {
            return Err(NodeError::Other(error));
        }
        let outbound_rx = self
            .outbound_rx
            .take()
            .ok_or_else(|| NodeError::Other("node has already been started".into()))?;
        let (sync_recovery, beacon_recovery, agreement_recovery) =
            if let Some(recovery) = self.recovered_checkpoint.take() {
                let directory = self.store.env().path().to_path_buf();
                let (recovery, sync, beacon, agreement) = tokio::task::spawn_blocking(move || {
                    let sync = recovery.fork(&directory)?;
                    let beacon = recovery.fork(&directory)?;
                    let agreement = recovery.fork(&directory)?;
                    Ok::<_, burst_store::StoreError>((recovery, sync, beacon, agreement))
                })
                .await
                .map_err(|error| NodeError::Other(error.to_string()))??;
                self.recovered_checkpoint = Some(recovery);
                (Some(sync), Some(beacon), Some(agreement))
            } else {
                (None, None, None)
            };
        self.connection_registry.write().await.finish_recovery();
        self.spawn_initial_tasks(outbound_rx).await?;

        if let Some(service) = self.nts_service.take() {
            let mut stop = self.shutdown.subscribe();
            self.task_handles.push(tokio::spawn(async move {
                let (cancel, receiver) = tokio::sync::watch::channel(false);
                let worker = service.run(receiver);
                tokio::pin!(worker);
                tokio::select! {
                    _ = &mut worker => {}
                    _ = stop.recv() => {
                        let _ = cancel.send(true);
                        worker.await;
                    }
                }
            }));
        }

        // Periodic NTP re-sync (every 5 minutes)
        {
            let clock_sync_periodic = Arc::clone(&self.clock_sync);
            let mut shutdown_rx_ntp = self.shutdown.subscribe();
            let ntp_handle = tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(300));
                interval.tick().await; // skip the immediate first tick
                loop {
                    tokio::select! {
                        biased;
                        _ = shutdown_rx_ntp.recv() => {
                            tracing::debug!("periodic NTP sync task shutting down");
                            break;
                        }
                        _ = interval.tick() => {
                            let mut cs = clock_sync_periodic.lock().await;
                            match cs.sync_ntp().await {
                                Ok(()) => tracing::debug!(offset_ms = cs.offset_ms, "periodic NTP sync complete"),
                                Err(e) => tracing::warn!(error = %e, "periodic NTP sync failed"),
                            }
                        }
                    }
                }
            });
            self.task_handles.push(ntp_handle);
        }

        // Update metrics with initial counts
        self.refresh_metrics().await;

        // ── Advertise address (for cloud nodes without UPnP) ───────────────
        // Priority: config > auto-detect > (UPnP will set later if it succeeds)
        let external_addr = if let Some(ref addr_str) = self.config.advertise_address {
            parse_advertise_address(addr_str, self.config.port)
        } else if !matches!(self.config.network, burst_types::NetworkId::Dev) {
            // Auto-detect: on cloud VPSes, outbound IP is typically the public IP.
            detect_outbound_ip(self.config.port)
        } else {
            None
        };
        if let Some(addr) = external_addr {
            let mut pm = self.peer_manager.write().await;
            pm.set_external_address(addr);
            tracing::info!(
                advertise = %addr,
                "advertise address set (cloud VPS / no UPnP)"
            );
        }

        // ── UPnP port mapping (NAT traversal) ────────────────────────────
        let is_dev_network = matches!(self.config.network, burst_types::NetworkId::Dev);
        if self.config.enable_upnp && !is_dev_network && self.config.advertise_address.is_none() {
            let description = format!("BURST Node ({})", self.config.network.as_str());
            let mapper = PortMapper::start(self.config.port, description);
            tracing::info!(port = self.config.port, "UPnP: port mapping initiated");

            let mut state_rx = mapper.subscribe();
            let pm_upnp = Arc::clone(&self.peer_manager);
            let mut shutdown_rx_upnp = self.shutdown.subscribe();

            let upnp_sync_handle = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        biased;
                        _ = shutdown_rx_upnp.recv() => break,
                        result = state_rx.changed() => {
                            if result.is_err() {
                                break;
                            }
                            let new_state = state_rx.borrow().clone();
                            match new_state {
                                UpnpState::Active { external_ip, external_port } => {
                                    let addr = std::net::SocketAddrV4::new(external_ip, external_port);
                                    let mut pm = pm_upnp.write().await;
                                    pm.set_external_address(addr);
                                    tracing::info!(
                                        external = %addr,
                                        "UPnP: external address set on PeerManager"
                                    );
                                }
                                UpnpState::NotFound | UpnpState::NonRoutable | UpnpState::Failed(_) => {
                                    let mut pm = pm_upnp.write().await;
                                    pm.clear_external_address();
                                    if let Some(addr) = external_addr { pm.set_external_address(addr); }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            });
            self.task_handles.push(upnp_sync_handle);
            self.port_mapper = Some(mapper);
        } else if is_dev_network {
            tracing::debug!("UPnP: disabled on dev network");
        } else {
            tracing::debug!("UPnP: disabled by configuration");
        }

        // ── Peer cache loader — reconnect to previously known peers ───────
        {
            use burst_store::peer::PeerStore;

            let peer_store = self.store.peer_store();
            match peer_store.iter_peers() {
                Ok(cached) if !cached.is_empty() => {
                    tracing::info!(
                        count = cached.len(),
                        "peer cache: loaded cached peers from LMDB"
                    );

                    let cache_ctx = crate::peer_connector::PeerConnectorContext {
                        peer_manager: Arc::clone(&self.peer_manager),
                        connection_registry: Arc::clone(&self.connection_registry),
                        block_queue: Arc::clone(&self.block_queue),
                        metrics: Arc::clone(&self.metrics),
                        active_elections: Arc::clone(&self.active_elections),
                        rep_weights: Arc::clone(&self.rep_weights),
                        vote_generator: Arc::clone(&self.vote_generator),
                        message_dedup: Arc::clone(&self.message_dedup),
                        online_weight_sampler: Arc::clone(&self.online_weight_sampler),
                        store: Arc::clone(&self.store),
                        node_private_key: burst_types::PrivateKey(self.node_private_key.0),
                        node_address: self.node_address.clone(),
                        params_hash: self.config.params.params_hash(),
                        network: self.config.network,
                        peering_port: self.config.port,
                        bootstrap_feedback: bootstrap_feedback_tx.clone(),
                    };
                    let mut shutdown_rx_cache = self.shutdown.subscribe();

                    let cache_handle = tokio::spawn(async move {
                        for (addr, _ts) in cached {
                            if crate::peer_connector::is_peer_connected(
                                &addr,
                                &cache_ctx.peer_manager,
                            )
                            .await
                            {
                                continue;
                            }

                            // Check for shutdown between attempts
                            if shutdown_rx_cache.try_recv().is_ok() {
                                break;
                            }

                            tracing::debug!(peer = %addr, "peer cache: connecting to cached peer");
                            match crate::peer_connector::connect_to_peer(&addr, &cache_ctx).await {
                                Ok(connected) => {
                                    tracing::info!(
                                        peer = %connected.peer_id,
                                        "peer cache: reconnected to cached peer"
                                    );
                                }
                                Err(e) => {
                                    tracing::debug!(
                                        peer = %addr,
                                        error = %e,
                                        "peer cache: failed to reconnect"
                                    );
                                }
                            }

                            tokio::time::sleep(Duration::from_millis(500)).await;
                        }
                    });
                    self.task_handles.push(cache_handle);
                }
                Ok(_) => {
                    tracing::debug!("peer cache: no cached peers found");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "peer cache: failed to load cached peers");
                }
            }
        }

        // ── Bootstrap peer discovery ──────────────────────────────────────
        let bootstrap_peers: Vec<String> = {
            let pm = self.peer_manager.read().await;
            pm.bootstrap_peers().to_vec()
        };

        // Configured seeds are optional; inbound peers can have newer history.
        {
            let bs_ctx = crate::peer_connector::PeerConnectorContext {
                peer_manager: Arc::clone(&self.peer_manager),
                connection_registry: Arc::clone(&self.connection_registry),
                block_queue: Arc::clone(&self.block_queue),
                metrics: Arc::clone(&self.metrics),
                active_elections: Arc::clone(&self.active_elections),
                rep_weights: Arc::clone(&self.rep_weights),
                vote_generator: Arc::clone(&self.vote_generator),
                message_dedup: Arc::clone(&self.message_dedup),
                online_weight_sampler: Arc::clone(&self.online_weight_sampler),
                store: Arc::clone(&self.store),
                node_private_key: burst_types::PrivateKey(self.node_private_key.0),
                node_address: self.node_address.clone(),
                params_hash: self.config.params.params_hash(),
                network: self.config.network,
                peering_port: self.config.port,
                bootstrap_feedback: bootstrap_feedback_tx.clone(),
            };
            let frontier_bs = Arc::clone(&self.frontier);
            let store_bs = Arc::clone(&self.store);
            let block_queue_bs = Arc::clone(&self.block_queue);
            let conn_registry_bs = Arc::clone(&self.connection_registry);
            let peer_manager_bs = Arc::clone(&self.peer_manager);
            let mut shutdown_rx_bs = self.shutdown.subscribe();
            let mut bs_feedback_rx = bootstrap_feedback_rx;
            let genesis_addr_bs = genesis_key::genesis_address(self.config.network);

            let bs_handle = tokio::spawn(async move {
                use crate::bootstrap::{
                    AscPullReqPayload, BootstrapFeedback, BootstrapMessage, Bootstrapper,
                    ASC_PULL_MAX_BLOCKS, ASC_PULL_MAX_FRONTIERS,
                };
                // Ascending bootstrap: keep bootstrap peers connected, discover
                // accounts via a periodic frontier scan, and pull each behind
                // account's chain in parallel with id-correlated requests.
                let mut bootstrapper = Bootstrapper::new(16, 20, ASC_PULL_MAX_BLOCKS);
                let mut seeded = false;
                let mut ticks: u64 = 0;
                // Prefix sorts before every actual account (unlike genesis).
                let mut frontier_cursor = WalletAddress::new(WalletAddress::PREFIX);
                // Keep initial discovery due until a connected peer accepts it.
                let mut frontier_page_pending = true;
                let mut frontier_request: Option<(u64, String, WalletAddress, u64)> = None;
                let mut interval = tokio::time::interval(Duration::from_secs(2));

                loop {
                    tokio::select! {
                        biased;
                        _ = shutdown_rx_bs.recv() => {
                            tracing::debug!("bootstrap task shutting down");
                            break;
                        }
                        Some(fb) = bs_feedback_rx.recv() => match fb {
                            BootstrapFeedback::Blocks { peer_id, id, blocks } => {
                                tracing::debug!(peer = %peer_id, id, count = blocks.len(),
                                    "received bootstrap block response");
                                match bootstrapper.receive_blocks(id, &peer_id, &blocks) {
                                    Ok(blocks) => for block in blocks {
                                        if !block_queue_bs.push(block).await {
                                            tracing::warn!(peer = %peer_id, "block queue full during bootstrap; retry from persisted frontier");
                                            break;
                                        }
                                    },
                                    Err(reason) => tracing::warn!(peer = %peer_id, %reason, "rejected bootstrap response"),
                                }
                            }
                            BootstrapFeedback::Frontiers { peer_id, id, entries } => {
                                let Some((expected_id, expected_peer, start, _)) = &frontier_request else { continue; };
                                if id != *expected_id || peer_id != *expected_peer
                                    || !crate::bootstrap::valid_frontier_page(start, &entries)
                                {
                                    tracing::warn!(peer = %peer_id, "rejected unsolicited or malformed frontier page");
                                    continue;
                                }
                                frontier_request = None;
                                frontier_page_pending = entries.len() == ASC_PULL_MAX_FRONTIERS as usize;
                                if frontier_page_pending {
                                    if let Some((last, _)) = entries.last() {
                                        frontier_page_pending = last.as_str() > frontier_cursor.as_str();
                                        frontier_cursor = last.clone();
                                    }
                                } else { frontier_cursor = WalletAddress::new(WalletAddress::PREFIX); }
                                let f = frontier_bs.read().await;
                                for (account, _remote) in entries {
                                    let our = f
                                        .get_head(&account)
                                        .copied()
                                        .unwrap_or(BlockHash::ZERO);
                                    bootstrapper.enqueue(account, our);
                                }
                            }
                        },
                        _ = interval.tick() => {
                            ticks += 1;
                            // Keep bootstrap peers connected.
                            for addr_str in &bootstrap_peers {
                                if !crate::peer_connector::is_peer_connected(
                                    addr_str,
                                    &bs_ctx.peer_manager,
                                )
                                .await
                                {
                                    let _ =
                                        crate::peer_connector::connect_to_peer(addr_str, &bs_ctx)
                                            .await;
                                }
                            }
                            // Seed the genesis account once (always known/baked).
                            if !seeded {
                                let our = frontier_bs
                                    .read()
                                    .await
                                    .get_head(&genesis_addr_bs)
                                    .copied()
                                    .unwrap_or(BlockHash::ZERO);
                                bootstrapper.enqueue(genesis_addr_bs.clone(), our);
                                seeded = true;
                            }
                            let now = unix_now_secs();
                            let blocks = store_bs.block_store();
                            let fronts = store_bs.frontier_store();
                            bootstrapper.observe_persisted(
                                |account| fronts.get_frontier(account).unwrap_or(BlockHash::ZERO),
                                |hash| blocks.exists(hash).unwrap_or(false),
                            );
                            bootstrapper.tick_timeouts(now);
                            if frontier_request.as_ref().is_some_and(|(_, _, _, sent)| now.saturating_sub(*sent) >= 20) {
                                frontier_request = None;
                                frontier_page_pending = true;
                            }

                            // Pull from any connected peer.
                            let peer_id = {
                                let pm = peer_manager_bs.read().await;
                                let mut peers: Vec<_> = pm.iter_connected().map(|(id, _)| id.clone()).collect();
                                peers.sort();
                                if peers.is_empty() { None } else { Some(peers[ticks as usize % peers.len()].clone()) }
                            };
                            if let Some(peer_id) = peer_id {
                                // Periodic frontier-scan discovery (~every 30s).
                                if frontier_request.is_none() && (ticks % 15 == 1 || frontier_page_pending) {
                                    frontier_page_pending = false;
                                    // Separate scan IDs from low, monotonically
                                    // increasing block-query IDs.
                                    let scan_id = u64::MAX - ticks;
                                    let msg = WireMessage::Bootstrap(
                                        BootstrapMessage::AscPullReq {
                                            id: scan_id,
                                            payload: AscPullReqPayload::Frontiers {
                                                start_account: frontier_cursor.clone(),
                                                count: ASC_PULL_MAX_FRONTIERS,
                                            },
                                        },
                                    );
                                    if let Ok(bytes) = msg.encode() {
                                        if conn_registry_bs.read().await.send(&peer_id, bytes) {
                                            frontier_request = Some((scan_id, peer_id.clone(), frontier_cursor.clone(), now));
                                        } else { frontier_page_pending = true; }
                                    }
                                }
                                // Send queued block pulls.
                                for (id, payload) in bootstrapper.next_requests(now) {
                                    bootstrapper.bind_peer(id, &peer_id);
                                    let msg = WireMessage::Bootstrap(
                                        BootstrapMessage::AscPullReq { id, payload },
                                    );
                                    if let Ok(bytes) = msg.encode() {
                                        conn_registry_bs.read().await.send(&peer_id, bytes);
                                    }
                                }
                            }
                        }
                    }
                }
            });
            self.task_handles.push(bs_handle);
        }

        // ── Reachout task (mesh self-formation) ───────────────────────────
        // Actively dials peers learned via keepalive gossip but not yet
        // connected, using their advertised listening address, up to a target
        // out-degree — the piece that lets a mesh self-form from one seed.
        //
        // Enabled by default. Identity-based dedup resolves simultaneous dials;
        // inbound ephemeral ports are excluded from the discovery address book.
        if self.config.enable_peer_reachout {
            let target_out = self.config.max_peers.clamp(3, 8);
            let reach_ctx = crate::peer_connector::PeerConnectorContext {
                peer_manager: Arc::clone(&self.peer_manager),
                connection_registry: Arc::clone(&self.connection_registry),
                block_queue: Arc::clone(&self.block_queue),
                metrics: Arc::clone(&self.metrics),
                active_elections: Arc::clone(&self.active_elections),
                rep_weights: Arc::clone(&self.rep_weights),
                vote_generator: Arc::clone(&self.vote_generator),
                message_dedup: Arc::clone(&self.message_dedup),
                online_weight_sampler: Arc::clone(&self.online_weight_sampler),
                store: Arc::clone(&self.store),
                node_private_key: burst_types::PrivateKey(self.node_private_key.0),
                node_address: self.node_address.clone(),
                params_hash: self.config.params.params_hash(),
                network: self.config.network,
                peering_port: self.config.port,
                bootstrap_feedback: bootstrap_feedback_tx.clone(),
            };
            let mut shutdown_rx_reach = self.shutdown.subscribe();

            let reach_handle = tokio::spawn(async move {
                // Small initial delay so bootstrap connects first.
                let mut interval = tokio::time::interval(Duration::from_secs(20));
                interval.tick().await; // fires immediately; skip the first
                loop {
                    tokio::select! {
                        biased;
                        _ = shutdown_rx_reach.recv() => {
                            tracing::debug!("reachout task shutting down");
                            break;
                        }
                        _ = interval.tick() => {
                            let connected = reach_ctx.peer_manager.read().await.connected_count();
                            if connected >= target_out {
                                continue;
                            }
                            let candidates = {
                                let pm = reach_ctx.peer_manager.read().await;
                                pm.reachout_candidates()
                            };
                            if candidates.is_empty() {
                                continue;
                            }
                            let mut have = connected;
                            for addr in candidates {
                                if have >= target_out {
                                    break;
                                }
                                if crate::peer_connector::is_peer_connected(
                                    &addr,
                                    &reach_ctx.peer_manager,
                                )
                                .await
                                {
                                    continue;
                                }
                                match crate::peer_connector::connect_to_peer(&addr, &reach_ctx).await
                                {
                                    Ok(connected_peer) => {
                                        have += 1;
                                        tracing::info!(
                                            peer = %connected_peer.peer_id,
                                            %addr,
                                            "reachout: connected to discovered peer"
                                        );
                                    }
                                    Err(e) => {
                                        tracing::debug!(%addr, error = %e, "reachout: dial failed");
                                    }
                                }
                            }
                        }
                    }
                }
            });
            self.task_handles.push(reach_handle);
        }

        // ── Keepalive task ────────────────────────────────────────────────
        // Rotate two randomized peer samples. Both reserve slot zero for self
        // (0.0.0.0:0 when unavailable), so receivers never mistake a random peer
        // for the sender's externally mapped listening address.
        let peer_manager_ka = Arc::clone(&self.peer_manager);
        let conn_registry_ka = Arc::clone(&self.connection_registry);
        let mut shutdown_rx_ka = self.shutdown.subscribe();

        let ka_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(15));
            let mut round: u64 = 0;
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_ka.recv() => {
                        tracing::debug!("keepalive task shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                        let now = unix_now_secs();

                        // Disconnect peers idle for >5 minutes and unban expired bans
                        let idle_peers = {
                            let mut pm = peer_manager_ka.write().await;
                            pm.check_bans(now);
                            pm.cleanup_idle(now, 300)
                        };
                        if !idle_peers.is_empty() {
                            tracing::info!(
                                count = idle_peers.len(),
                                "disconnecting idle peers"
                            );
                            let mut registry = conn_registry_ka.write().await;
                            for pid in &idle_peers {
                                registry.remove(pid);
                            }
                        }

                        let mut pm = peer_manager_ka.write().await;
                        if pm.should_keepalive(now) {
                            pm.record_keepalive(now);
                            round = round.wrapping_add(1);

                            let peer_ids: Vec<String> = {
                                let registry = conn_registry_ka.read().await;
                                registry.peer_ids().into_iter().cloned().collect()
                            };

                            if peer_ids.is_empty() {
                                tracing::trace!("keepalive: no connected peers, skipping send");
                            }

                            // Build the two keepalive variants.
                            // Use random_known_peers (not just connected) so that
                            // discovered-but-not-yet-connected peers are propagated,
                            // breaking the chicken-and-egg peer discovery problem.
                            let self_peers: Vec<String> = pm
                                .random_peers_with_self(8)
                                .iter()
                                .map(|a| format!("{}:{}", a.ip, a.port))
                                .collect();
                            let random_peers: Vec<String> = pm
                                .random_peers_with_self(8)
                                .iter()
                                .map(|a| format!("{}:{}", a.ip, a.port))
                                .collect();

                            let self_msg = WireMessage::Keepalive(
                                crate::wire_message::KeepaliveMsg {
                                    peers: self_peers,
                                },
                            );
                            let random_msg = WireMessage::Keepalive(
                                crate::wire_message::KeepaliveMsg {
                                    peers: random_peers,
                                },
                            );

                            let self_bytes = self_msg.encode().ok();
                            let random_bytes = random_msg.encode().ok();

                            // Send self-keepalive to ~25% of peers,
                            // random-keepalive to the remaining ~75%.
                            for (i, pid) in peer_ids.iter().enumerate() {
                                let bytes = if i % 4 == (round as usize % 4) {
                                    &self_bytes
                                } else {
                                    &random_bytes
                                };
                                if let Some(ref payload) = bytes {
                                    conn_registry_ka.read().await.send(pid, payload.clone());
                                }
                            }

                            tracing::trace!(
                                connected = pm.connected_count(),
                                round = round,
                                "keepalive round"
                            );
                        }
                    }
                }
            }
        });
        self.task_handles.push(ka_handle);

        // ── Reachout loop — connect to peers discovered via keepalive ─────
        {
            let reachout_ctx = crate::peer_connector::PeerConnectorContext {
                peer_manager: Arc::clone(&self.peer_manager),
                connection_registry: Arc::clone(&self.connection_registry),
                block_queue: Arc::clone(&self.block_queue),
                metrics: Arc::clone(&self.metrics),
                active_elections: Arc::clone(&self.active_elections),
                rep_weights: Arc::clone(&self.rep_weights),
                vote_generator: Arc::clone(&self.vote_generator),
                message_dedup: Arc::clone(&self.message_dedup),
                online_weight_sampler: Arc::clone(&self.online_weight_sampler),
                store: Arc::clone(&self.store),
                node_private_key: burst_types::PrivateKey(self.node_private_key.0),
                node_address: self.node_address.clone(),
                params_hash: self.config.params.params_hash(),
                network: self.config.network,
                peering_port: self.config.port,
                bootstrap_feedback: bootstrap_feedback_tx.clone(),
            };
            let mut shutdown_rx_ro = self.shutdown.subscribe();

            let ro_handle = tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(1));
                loop {
                    tokio::select! {
                        biased;
                        _ = shutdown_rx_ro.recv() => {
                            tracing::debug!("reachout loop shutting down");
                            break;
                        }
                        _ = interval.tick() => {
                            let keepalive_peers = {
                                let mut pm = reachout_ctx.peer_manager.write().await;
                                pm.pop_random_keepalive()
                            };
                            let Some(peers) = keepalive_peers else {
                                continue;
                            };

                            for peer_addr in &peers {
                                if peer_addr.ip.is_empty()
                                    || peer_addr.ip == "0.0.0.0"
                                    || peer_addr.ip == "::"
                                {
                                    continue;
                                }

                                let addr_str = format!("{}:{}", peer_addr.ip, peer_addr.port);

                                {
                                    let pm = reachout_ctx.peer_manager.read().await;
                                    if pm.is_connected(&addr_str) || pm.is_banned(&addr_str) {
                                        continue;
                                    }
                                    // Skip our own advertised address — peers gossip it
                                    // back to us, and dialing it just churns a
                                    // self-connection that the node_id check rejects.
                                    if pm
                                        .external_address()
                                        .is_some_and(|ext| ext.to_string() == addr_str)
                                    {
                                        continue;
                                    }
                                }

                                tracing::debug!(peer = %addr_str, "reachout: attempting connection");
                                match crate::peer_connector::connect_to_peer(
                                    &addr_str,
                                    &reachout_ctx,
                                )
                                .await
                                {
                                    Ok(connected) => {
                                        tracing::info!(
                                            peer = %connected.peer_id,
                                            "reachout: peer connected"
                                        );
                                    }
                                    Err(e) => {
                                        tracing::debug!(
                                            peer = %addr_str,
                                            error = %e,
                                            "reachout: connection failed"
                                        );
                                    }
                                }

                                // Throttle between connection attempts
                                tokio::time::sleep(Duration::from_millis(250)).await;
                            }
                        }
                    }
                }
            });
            self.task_handles.push(ro_handle);
        }

        let (checkpoint_tx, checkpoint_rx) = mpsc::channel(crate::checkpoint_sync::INBOX_CAPACITY);
        let (agreement_tx, agreement_rx) = mpsc::channel(crate::checkpoint_service::INBOX_CAPACITY);
        self.connection_registry
            .write()
            .await
            .set_agreement_inbox(agreement_tx);
        let signing_clock = self.signing_clock.clone();
        let agreement_clock: crate::checkpoint_service::Clock = Arc::new(move || {
            let time = signing_clock.as_ref()?.reading().ok()?;
            Some(crate::checkpoint_replay::SigningClock {
                now: time.now,
                error_bound: time.error_bound,
            })
        });
        #[cfg(test)]
        let agreement_clock = self
            .verification_fixture_clock
            .clone()
            .unwrap_or(agreement_clock);
        let display_clock = agreement_clock.clone();
        let agreement_settings = crate::checkpoint_service::Settings {
            store: self.store.clone(),
            network: self.config.network,
            initial_params: self.initial_params.clone(),
            // Deadline recovery and full lifecycle replay must pass before
            // ordinary nodes issue new checkpoint commitments automatically.
            key: (self.config.enable_representative && self.certified_selection_enabled())
                .then(|| self.representative_key.clone()),
        };
        let agreement_registry = self.connection_registry.clone();
        let agreement_shutdown = self.shutdown.clone();
        self.task_handles.push(tokio::spawn(async move {
            if let Err(error) = crate::checkpoint_service::run_node_with_recovery(
                agreement_settings,
                agreement_registry,
                agreement_rx,
                agreement_clock,
                agreement_shutdown.clone(),
                agreement_recovery,
            )
            .await
            {
                tracing::error!(%error, "checkpoint agreement worker stopped");
                agreement_shutdown.fail("checkpoint agreement worker", &error);
            }
        }));
        self.connection_registry
            .write()
            .await
            .set_checkpoint_inbox(checkpoint_tx);
        let historical_admissions = self
            .block_processor
            .lock()
            .await
            .historical_admissions
            .clone();
        self.task_handles
            .push(tokio::spawn(crate::checkpoint_sync::run(
                Arc::clone(&self.store),
                crate::checkpoint_sync::Settings {
                    network: self.config.network,
                    params: self.initial_params.clone(),
                    snapshots: self.config.snapshot_transfer.clone(),
                    recovery: sync_recovery,
                },
                Arc::clone(&self.connection_registry),
                checkpoint_rx,
                Arc::clone(&self.shutdown),
                historical_admissions,
            )));
        let beacon_interval = Duration::from_secs(30);
        #[cfg(test)]
        let beacon_interval = if self.verification_fixture_clock.is_some() {
            Duration::from_millis(200)
        } else {
            beacon_interval
        };
        self.task_handles
            .push(tokio::spawn(crate::checkpoint_beacon::run(
                Arc::clone(&self.store),
                self.config.network,
                self.initial_params.clone(),
                Arc::clone(&self.vrf_client),
                Arc::clone(&self.shutdown),
                beacon_interval,
                self.certified_selection_enabled()
                    .then(|| crate::checkpoint_beacon::Live {
                        recovery: beacon_recovery,
                        registry: Arc::clone(&self.connection_registry),
                        orchestrator: Arc::clone(&self.verification_orchestrator),
                    }),
            )));

        // ── Periodic telemetry request task ────────────────────────────────
        let conn_registry_telem = Arc::clone(&self.connection_registry);
        let peer_manager_telem = Arc::clone(&self.peer_manager);
        let mut shutdown_rx_telem = self.shutdown.subscribe();

        let telem_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_telem.recv() => {
                        tracing::debug!("telemetry request task shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                        let peer_ids: Vec<String> = {
                            let pm = peer_manager_telem.read().await;
                            pm.iter_connected().map(|(id, _)| id.clone()).collect()
                        };
                        if peer_ids.is_empty() {
                            continue;
                        }
                        let req = WireMessage::TelemetryReq;
                        let bytes = match req.encode() {
                            Ok(b) => b,
                            Err(_) => continue,
                        };
                        let registry = conn_registry_telem.read().await;
                        for pid in &peer_ids {
                            registry.send(pid, bytes.clone());
                        }
                        tracing::trace!(peers = peer_ids.len(), "sent telemetry requests");
                    }
                }
            }
        });
        self.task_handles.push(telem_handle);

        // ── Peer cache writer — persist connected peers to LMDB ──────────
        {
            let store_pc = Arc::clone(&self.store);
            let peer_manager_pc = Arc::clone(&self.peer_manager);
            let mut shutdown_rx_pc = self.shutdown.subscribe();

            let pc_handle = tokio::spawn(async move {
                use burst_store::peer::PeerStore;

                let mut interval = tokio::time::interval(Duration::from_secs(300));
                loop {
                    tokio::select! {
                        biased;
                        _ = shutdown_rx_pc.recv() => {
                            tracing::debug!("peer cache writer shutting down");
                            break;
                        }
                        _ = interval.tick() => {
                            let connected = {
                                let pm = peer_manager_pc.read().await;
                                pm.connected_peer_addresses()
                            };

                            let peer_store = store_pc.peer_store();
                            let mut saved = 0usize;
                            for (addr, ts) in &connected {
                                if let Err(e) = peer_store.put_peer(addr, *ts) {
                                    tracing::warn!(
                                        peer = %addr,
                                        error = %e,
                                        "failed to cache peer"
                                    );
                                } else {
                                    saved += 1;
                                }
                            }

                            let now = unix_now_secs();
                            let cutoff = now.saturating_sub(3600);
                            match peer_store.purge_older_than(cutoff) {
                                Ok(purged) if purged > 0 => {
                                    tracing::debug!(
                                        purged = purged,
                                        "peer cache: purged stale entries"
                                    );
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "peer cache purge failed");
                                }
                                _ => {}
                            }

                            if saved > 0 {
                                tracing::trace!(
                                    saved = saved,
                                    "peer cache: persisted connected peers"
                                );
                            }
                        }
                    }
                }
            });
            self.task_handles.push(pc_handle);
        }

        // ── RPC server (optional) ─────────────────────────────────────────
        if self.config.enable_rpc {
            let rpc_port = self.config.rpc_port;
            let rpc_bind = self.config.rpc_bind.clone();
            let metrics_registry = if self.config.enable_metrics {
                Some(self.metrics.registry.clone())
            } else {
                None
            };

            let rpc_state = Arc::new(RpcState {
                display_now: Arc::new(move || {
                    display_clock()
                        .map(|reading| burst_types::Timestamp::new(reading.now))
                        .unwrap_or_else(burst_types::Timestamp::now)
                }),
                meta_store: Arc::new(self.store.meta_store()),
                network: self.config.network,
                genesis_authoring: tokio::sync::Mutex::new(()),
                started_at: unix_now_secs(),
                metrics_registry,
                account_store: Arc::new(self.store.account_store()),
                block_store: Arc::new(self.store.block_store()),
                pending_store: Arc::new(self.store.pending_store()),
                frontier_store: Arc::new(self.store.frontier_store()),
                verification_store: Arc::new(self.store.verification_store()),
                governance_store: Arc::new(self.store.governance_store()),
                governance_engine: Some(Arc::clone(&self.governance)),
                brn_engine: self.brn_engine.clone(),
                brn_store: Arc::new(self.store.brn_store()),
                clock_sync: self.clock_sync.clone(),
                verification_engine: self.verification_orchestrator.clone(),
                delegation_engine: self.delegation_engine.clone(),
                trst_engine: self.trst_engine.clone(),
                consti_engine: self.consti_engine.clone(),
                rep_weight_cache: self.rep_weights.clone(),
                work_generator: Arc::new(WorkGenerator),
                params: Arc::new(self.config.params.clone()),
                block_processor: Arc::new(NodeBlockProcessor {
                    checkpoint_sync_status: self
                        .connection_registry
                        .read()
                        .await
                        .checkpoint_sync_status
                        .clone(),
                    historical_lookup: crate::historical_lookup::Reader {
                        store: self.store.clone(),
                        registry: self.connection_registry.clone(),
                        network: self.config.network,
                        initial_params: self.initial_params.clone(),
                        active: Arc::new(tokio::sync::Mutex::new(())),
                        admitted: Arc::new(tokio::sync::Semaphore::new(3)),
                    },
                    recovery_store: self.store.clone(),
                    appeal_view: crate::appeal_view::Reader {
                        store: self.store.clone(),
                        initial_params: self.initial_params.clone(),
                    },
                    delegation_view: crate::delegation_view::Reader {
                        store: self.store.clone(),
                        network: self.config.network,
                        genesis: crate::genesis_state::block(
                            self.config.network,
                            &self.initial_params,
                        )
                        .hash,
                    },
                    block_queue: Arc::clone(&self.block_queue),
                    submission_results: Arc::clone(&self.submission_results),
                    governance_transfers: Arc::clone(&self.governance_transfers),
                }),
                online_reps: Arc::new(std::sync::RwLock::new(Vec::new())),
                peer_manager: Arc::clone(&self.peer_manager),
                enable_faucet: self.config.enable_faucet,
                rate_limiter: Arc::new(burst_rpc::RateLimiter::new(100)),
                ledger_cache: Some(
                    self.ledger_cache.clone() as Arc<dyn burst_rpc::LedgerCacheView + Send + Sync>
                ),
                // Only the creator's node holds this; it gates the
                // genesis_endorse RPC used to bootstrap the first verified
                // wallets. `None` on every other node.
                genesis_seed: genesis_key::genesis_seed(self.config.network),
            });

            let rpc_server = RpcServer::with_bind(rpc_port, rpc_bind, rpc_state);
            let mut shutdown_rx_rpc = self.shutdown.subscribe();
            let rpc_failure = self.shutdown.clone();

            let rpc_handle = tokio::spawn(async move {
                let result = rpc_server
                    .start(async move {
                        let _ = shutdown_rx_rpc.recv().await;
                    })
                    .await;
                match result {
                    Ok(()) => tracing::info!("RPC server exited"),
                    Err(e) => {
                        tracing::error!("RPC server error: {e}");
                        rpc_failure.fail("RPC server", &e);
                    }
                }
            });
            self.task_handles.push(rpc_handle);
        }

        // ── WebSocket server (optional) ───────────────────────────────────
        if self.config.enable_websocket {
            let ws_port = self.config.websocket_port;
            let ws_state_clone = Arc::clone(&self.ws_state);
            let ws_server = WebSocketServer::with_bind(
                ws_port,
                self.config.websocket_bind.clone(),
                ws_state_clone,
            );
            let mut shutdown_rx_ws = self.shutdown.subscribe();
            let ws_failure = self.shutdown.clone();

            let ws_handle = tokio::spawn(async move {
                let result = ws_server
                    .start(async move {
                        let _ = shutdown_rx_ws.recv().await;
                    })
                    .await;
                match result {
                    Ok(()) => tracing::info!("WebSocket server exited"),
                    Err(e) => {
                        tracing::error!("WebSocket server error: {e}");
                        ws_failure.fail("WebSocket server", &e);
                    }
                }
            });
            self.task_handles.push(ws_handle);
        }

        tracing::info!("BURST node started — all subsystems running");

        // Wait for the shutdown signal
        self.shutdown.wait_for_signal().await;

        Ok(())
    }

    /// Stop the node gracefully.
    ///
    /// 1. Sends the shutdown signal to all background tasks.
    /// 2. Disconnects all peers.
    /// 3. Stops and joins background tasks and connections.
    /// 4. Flushes committed LMDB state without overwriting it from caches.
    pub async fn stop(&mut self) -> Result<(), NodeError> {
        tracing::info!("BURST node stopping");

        // Signal all tasks
        self.shutdown.shutdown();

        // Remove UPnP port mapping (be a good citizen for the router)
        if let Some(ref mut mapper) = self.port_mapper {
            tracing::info!("UPnP: removing port mapping");
            mapper.stop().await;
        }

        // Close admission and cancel both halves of every tracked connection.
        {
            let mut registry = self.connection_registry.write().await;
            registry.close();
            tracing::info!("connection registry cleared");
        }

        // Disconnect peers
        {
            let mut pm = self.peer_manager.write().await;
            let connected_ids: Vec<String> =
                pm.iter_connected().map(|(id, _)| id.clone()).collect();
            for id in connected_ids {
                pm.mark_disconnected(&id);
            }
            tracing::info!("all peers disconnected");
        }

        // Stop producers before flushing. Accepted transitions already persist
        // their engine state atomically; caches are never a recovery authority.
        let handles: Vec<JoinHandle<()>> = std::mem::take(&mut self.task_handles);
        let workers = crate::shutdown::drain_tasks(handles, SHUTDOWN_TIMEOUT).await;
        let connections = self.connection_registry.write().await.take_tasks();
        for task in connections {
            let _ = task.await;
        }
        self.store
            .force_sync()
            .map_err(burst_store::StoreError::from)?;

        // Final metrics snapshot
        self.refresh_metrics().await;

        if let Some(error) = self.shutdown.failure() {
            return Err(NodeError::Other(error));
        }
        workers?;
        tracing::info!("BURST node stopped");
        Ok(())
    }

    /// Submit a signed block without waiting for admission or confirmation.
    /// The running worker owns authentication and the durable ledger transaction.
    pub fn process_block(&self, block: &StateBlock) -> Result<ProcessResult, NodeError> {
        if self.block_queue.try_push(block.clone()) {
            Ok(ProcessResult::Queued)
        } else {
            Err(NodeError::Other("block priority queue full or busy".into()))
        }
    }

    /// Legacy entry point. Removing a frontier does not undo a ledger operation.
    /// Keep this non-mutating until transactional branch installation is wired.
    pub fn rollback_block(
        &self,
        _block: &StateBlock,
    ) -> Result<crate::block_processor::RollbackResult, NodeError> {
        Ok(crate::block_processor::RollbackResult::RequiresLedgerRollback)
    }

    /// Process an incoming transaction through the full pipeline (async path).
    ///
    /// 1. Validates the transaction
    /// 2. Converts it to a StateBlock
    /// 3. Submits it to the block processor pipeline
    /// 4. Returns the block hash on acceptance
    pub async fn process_transaction(
        &self,
        tx: burst_transactions::Transaction,
    ) -> Result<BlockHash, NodeError> {
        use burst_transactions::validation::validate_transaction;

        let now_secs = unix_now_secs();
        let now = Timestamp::new(now_secs);

        // Step 1: Validate the transaction
        validate_transaction(&tx, now, 300)
            .map_err(|e| NodeError::Other(format!("transaction validation failed: {e}")))?;

        self.metrics.transactions_received.inc();

        // Step 2: Convert to StateBlock
        let block = self.transaction_to_state_block(&tx, now).await?;
        let block_hash = block.hash;

        // Step 3: Submit to the block priority queue (ordered by PoW difficulty)
        if !self.block_queue.push(block.clone()).await {
            return Err(NodeError::Other("block priority queue full".into()));
        }

        // Step 4: Broadcast to peers
        if let Ok(msg_bytes) = WireMessage::Block(Box::new(block)).encode() {
            let peers: Vec<burst_network::PeerState> = {
                let pm = self.peer_manager.read().await;
                pm.iter_connected().map(|(_, s)| s.clone()).collect()
            };
            let result = self
                .broadcaster
                .broadcast_with_fanout(&msg_bytes, &peers, 4)
                .await;
            tracing::debug!(
                sent = result.sent,
                failed = result.failed,
                "block broadcast"
            );
        }

        Ok(block_hash)
    }

    /// Convert a transaction into a state block by looking up the sender's
    /// current frontier and building the appropriate block fields.
    async fn transaction_to_state_block(
        &self,
        tx: &burst_transactions::Transaction,
        now: Timestamp,
    ) -> Result<StateBlock, NodeError> {
        let sender = tx.sender().clone();

        // Look up sender's current head in the frontier
        let (previous, brn_balance, trst_balance, representative, previous_origin) = {
            let frontier = self.frontier.read().await;
            match frontier.get_head(&sender) {
                Some(head) => {
                    // Account exists — load current balances from the block store.
                    // For simplicity, we read the head block and extract balances.
                    let block_store = self.store.block_store();
                    match block_store.get_block(head) {
                        Ok(bytes) => {
                            if let Ok(prev_block) = bincode::deserialize::<StateBlock>(&bytes) {
                                (
                                    *head,
                                    prev_block.brn_balance,
                                    prev_block.trst_balance,
                                    prev_block.representative,
                                    prev_block.origin,
                                )
                            } else {
                                (*head, 0u128, 0u128, sender.clone(), TxHash::ZERO)
                            }
                        }
                        Err(_) => (*head, 0u128, 0u128, sender.clone(), TxHash::ZERO),
                    }
                }
                None => {
                    // New account — this will be an open block
                    (BlockHash::ZERO, 0u128, 0u128, sender.clone(), TxHash::ZERO)
                }
            }
        };

        let is_open = previous == BlockHash::ZERO;

        // Encode a wallet address into a link field as its public key —
        // the inverse of `extract_receiver_from_link` (which derives the
        // address from the pubkey bytes).
        fn address_link(addr: &burst_types::WalletAddress) -> Result<BlockHash, NodeError> {
            burst_crypto::decode_address(addr.as_str())
                .map(BlockHash::new)
                .ok_or_else(|| {
                    NodeError::Other(format!("invalid receiver address: {}", addr.as_str()))
                })
        }

        // Check a BRN spend against the computed counter BRN(w) — the
        // odometer field on blocks records cumulative spending only.
        let check_brn_spend = |brn: &burst_brn::BrnEngine,
                               account: &burst_types::WalletAddress,
                               amount: u128|
         -> Result<(), NodeError> {
            let state = brn.wallets.get(account).ok_or_else(|| {
                NodeError::Other("account has no BRN accrual state (not verified)".into())
            })?;
            let available = brn
                .compute_balance_checked(state, now)
                .map_err(|error| NodeError::Other(error.to_string()))?;
            if amount > available {
                return Err(NodeError::Other(format!(
                    "insufficient BRN: need {}, computed available {}",
                    amount, available
                )));
            }
            Ok(())
        };

        let mut merge_sources: Vec<TxHash> = Vec::new();
        let (block_type, new_brn, new_trst, link) = match tx {
            burst_transactions::Transaction::Burn(burn) => {
                {
                    let brn = self.brn_engine.lock().await;
                    check_brn_spend(&brn, &sender, burn.amount)?;
                }
                // Ascending odometer: spending BRN increases the field.
                let new_brn = brn_balance
                    .checked_add(burn.amount)
                    .ok_or_else(|| NodeError::Other("BRN spend overflow".into()))?;
                (
                    if is_open {
                        BlockType::Open
                    } else {
                        BlockType::Burn
                    },
                    new_brn,
                    trst_balance,
                    address_link(&burn.receiver)?,
                )
            }
            burst_transactions::Transaction::Send(send) => {
                if send.amount > trst_balance {
                    return Err(NodeError::Other(format!(
                        "insufficient TRST: need {}, have {}",
                        send.amount, trst_balance
                    )));
                }
                // Verify sender has enough transferable (non-expired, non-revoked) TRST
                {
                    let trst = self.trst_engine.lock().await;
                    if let Some((transferable, _)) =
                        trst.candidate_balances(&sender, &send.origin, now)
                    {
                        if send.amount > transferable {
                            return Err(NodeError::Other(format!(
                                "insufficient transferable TRST: need {} but only {} is transferable",
                                send.amount, transferable
                            )));
                        }
                    }
                }
                let new_trst = trst_balance - send.amount;
                (
                    if is_open {
                        BlockType::Open
                    } else {
                        BlockType::Send
                    },
                    brn_balance,
                    new_trst,
                    address_link(&send.receiver)?,
                )
            }
            burst_transactions::Transaction::Endorse(endorse) => {
                {
                    let brn = self.brn_engine.lock().await;
                    check_brn_spend(&brn, &sender, endorse.burn_amount)?;
                }
                (
                    BlockType::Endorse,
                    brn_balance
                        .checked_add(endorse.burn_amount)
                        .ok_or_else(|| NodeError::Other("BRN spend overflow".into()))?,
                    trst_balance,
                    address_link(&endorse.target)?,
                )
            }
            burst_transactions::Transaction::Challenge(challenge) => {
                {
                    let brn = self.brn_engine.lock().await;
                    check_brn_spend(&brn, &sender, challenge.stake_amount)?;
                }
                (
                    BlockType::Challenge,
                    brn_balance
                        .checked_add(challenge.stake_amount)
                        .ok_or_else(|| NodeError::Other("BRN spend overflow".into()))?,
                    trst_balance,
                    address_link(&challenge.target)?,
                )
            }
            burst_transactions::Transaction::VerificationVote(vote) => {
                if vote.stake_amount > 0 {
                    let brn = self.brn_engine.lock().await;
                    check_brn_spend(&brn, &sender, vote.stake_amount)?;
                }
                (
                    BlockType::VerificationVote,
                    brn_balance
                        .checked_add(vote.stake_amount)
                        .ok_or_else(|| NodeError::Other("BRN spend overflow".into()))?,
                    trst_balance,
                    address_link(&vote.target_wallet)?,
                )
            }
            burst_transactions::Transaction::Merge(merge) => {
                // The wallet's chosen token set goes on-chain (6.17b) so the
                // merge inputs are signed, validated, and reconstructible.
                merge_sources = merge.source_hashes.clone();
                (BlockType::Merge, brn_balance, trst_balance, BlockHash::ZERO)
            }
            _ => {
                // For other transaction types, create a generic block
                let block_type = if is_open {
                    BlockType::Open
                } else {
                    match tx {
                        burst_transactions::Transaction::GovernanceProposal(_) => {
                            BlockType::GovernanceProposal
                        }
                        burst_transactions::Transaction::GovernanceVote(_) => {
                            BlockType::GovernanceVote
                        }
                        burst_transactions::Transaction::Delegate(_) => BlockType::Delegate,
                        burst_transactions::Transaction::RevokeDelegation(_) => {
                            BlockType::RevokeDelegation
                        }
                        burst_transactions::Transaction::ChangeRepresentative(_) => {
                            BlockType::ChangeRepresentative
                        }
                        _ => BlockType::Send, // fallback; unreachable
                    }
                };
                (block_type, brn_balance, trst_balance, BlockHash::ZERO)
            }
        };

        let tx_hash = *tx.hash();

        let origin = if block_type == BlockType::Burn {
            tx_hash
        } else {
            previous_origin
        };

        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: self.store.meta_store().payment_epoch()?,
            block_type,
            account: sender,
            previous,
            representative,
            brn_balance: new_brn,
            trst_balance: new_trst,
            link,
            origin,
            transaction: tx_hash,
            timestamp: now,
            params_hash: BlockHash::ZERO,
            merge_sources,
            release_sources: Vec::new(),
            target_head: BlockHash::ZERO,
            work: 0,
            signature: tx.signature().clone(),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();

        Ok(block)
    }

    /// Refresh gauge-style metrics from current state.
    async fn refresh_metrics(&self) {
        let frontier = self.frontier.read().await;
        self.metrics
            .account_count
            .set(frontier.account_count() as i64);
        drop(frontier);

        let bp = self.block_processor.lock().await;
        self.metrics
            .unchecked_count
            .set(bp.unchecked_count() as i64);
        drop(bp);

        let pm = self.peer_manager.read().await;
        self.metrics.peer_count.set(pm.connected_count() as i64);
        drop(pm);

        if let Ok(count) = self.store.block_store().block_count() {
            self.metrics.block_count.set(count as i64);
        }
    }

    /// Get the current protocol parameters.
    pub fn params(&self) -> &burst_types::ProtocolParams {
        &self.config.params
    }

    /// Get a handle to the block priority queue for submitting blocks.
    pub fn block_queue(&self) -> Arc<BlockPriorityQueue> {
        Arc::clone(&self.block_queue)
    }
}

/// Refresh the installed agreed electorate without advancing its epoch here.
/// Legacy mode uses the local confirmed projection until certified mode opens.
async fn rebuild_rep_weights(
    store: &LmdbStore,
    rep_weights: &RwLock<RepWeightCache>,
    elections: &RwLock<ActiveElections>,
) -> Result<(), burst_store::StoreError> {
    let mut rw = rep_weights.write().await;
    if let Some(authority) = crate::payment_authority::PaymentAuthority::read(store)? {
        let weights = authority.weights();
        let mut active = elections.write().await;
        if active
            .authority_epoch()
            .is_some_and(|epoch| epoch != authority.epoch)
        {
            return Err(burst_store::StoreError::Corruption(
                "payment authority awaits coordinated publication".into(),
            ));
        }
        active
            .install_authority(authority.epoch, weights.all_weights(), authority.quorum_bps)
            .map_err(|error| burst_store::StoreError::Corruption(error.to_string()))?;
        *rw = weights;
        return Ok(());
    }
    let mut active = elections.write().await;
    if active.authority_epoch().is_some() {
        return Err(burst_store::StoreError::Corruption(
            "installed payment authority is missing".into(),
        ));
    }
    crate::confirmed_authority::ConfirmedAuthority::read(store)?.rebuild_weights(&mut rw);
    active
        .set_authority(rw.all_weights())
        .map_err(|e| burst_store::StoreError::Corruption(e.to_string()))?;
    Ok(())
}

/// Detect outbound (public) IP by binding a UDP socket to an external address.
/// On cloud VPSes with a direct public IP, local_addr() returns that IP.
fn detect_outbound_ip(port: u16) -> Option<std::net::SocketAddrV4> {
    // TCP connect uses the route; local_addr() returns our source IP.
    let addr: std::net::SocketAddr = "8.8.8.8:80".parse().ok()?;
    let stream =
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(3)).ok()?;
    let local = stream.local_addr().ok()?;
    if let std::net::SocketAddr::V4(v4) = local {
        let ip = *v4.ip();
        if burst_network::upnp::is_public_ipv4(ip) {
            return Some(std::net::SocketAddrV4::new(ip, port));
        }
    }
    None
}

/// Parse advertise_address config: "IP" or "IP:port".
fn parse_advertise_address(s: &str, default_port: u16) -> Option<std::net::SocketAddrV4> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let parts: Vec<&str> = s.rsplitn(2, ':').collect();
    let (ip_str, port) = if parts.len() == 2 {
        let port = parts[0].parse::<u16>().ok()?;
        (parts[1], port)
    } else {
        (s, default_port)
    };
    let ip: std::net::Ipv4Addr = ip_str.parse().ok()?;
    Some(std::net::SocketAddrV4::new(ip, port))
}

/// Helper: current UNIX timestamp in seconds.
fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Helper: current UNIX timestamp in milliseconds.
fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
