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
use burst_ledger::{
    BlockType, DagFrontier, LedgerPruner, PruningConfig, StateBlock, CURRENT_BLOCK_VERSION,
};
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
use burst_store::delegation::{DelegationRecord, DelegationStore};
use burst_store::meta::MetaStore;
use burst_store::pending::PendingStore;
use burst_store::rep_weights::RepWeightStore;
use burst_store::trst_index::TrstIndexStore;

use crate::block_processor::{BlockProcessor, ProcessResult};
use crate::bounded_backlog::BoundedBacklog;
use crate::config::NodeConfig;
use crate::confirmation_processor::{CementResult, ConfirmationProcessor, LmdbChainWalker};
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
const DEFAULT_MAP_SIZE: usize = 1 << 30;
/// Number of named LMDB databases.
const MAX_DBS: u32 = 29;
/// Channel capacity for the block-processing pipeline.
const BLOCK_CHANNEL_CAPACITY: usize = 4096;
/// Channel capacity for outbound peer messages.
const OUTBOUND_CHANNEL_CAPACITY: usize = 4096;
/// Timeout for waiting on background tasks during shutdown.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// Meta-store key used to persist the serialized MergerGraph.
const MERGER_GRAPH_META_KEY: &str = "merger_graph";
/// Meta-store key used to persist the verification orchestrator snapshot.
const VERIFICATION_ORCHESTRATOR_META_KEY: &str = "verification_orchestrator";

use crate::genesis_key;

// ── BlockProcessorCallback bridge ───────────────────────────────────────

/// Adapts the node's concrete [`BlockProcessor`] into the trait expected by
/// the RPC crate, breaking the circular dependency.
struct NodeBlockProcessor {
    block_queue: Arc<BlockPriorityQueue>,
}

impl BlockProcessorCallback for NodeBlockProcessor {
    fn process_block(&self, block_bytes: &[u8]) -> Result<RpcProcessResult, String> {
        let block: StateBlock = bincode::deserialize(block_bytes)
            .map_err(|e| format!("failed to deserialize block: {e}"))?;

        if self.block_queue.try_push(block) {
            Ok(RpcProcessResult::Queued)
        } else {
            Err("block queue full — try again later".to_string())
        }
    }
}

/// Maximum number of recently confirmed hashes to remember.
const RECENTLY_CONFIRMED_CAPACITY: usize = 65_536;
/// Default maximum concurrent elections.
const MAX_ACTIVE_ELECTIONS: usize = 5000;
/// Default initial online weight estimate.
const DEFAULT_ONLINE_WEIGHT: u128 = 1_000_000;
/// Bootstrap voting weight granted to the genesis representative on every node
/// (the analog of Nano's genesis premine, for VOTING WEIGHT only — not spendable
/// TRST). Consistent across nodes, so genesis's votes aggregate network-wide and
/// consensus can confirm at launch before many humans run voting nodes.
///
/// Denominated in the hybrid weight's units, where one verified human ≈
/// `WEIGHT_PER_HUMAN` (10_000). This is ≈ 10,000 verified-human-equivalents:
/// dominant at genuine launch (a handful of verified wallets), and a shrinking
/// fraction as the verified, participating population grows past ~10k, so
/// consensus decentralises organically.
const GENESIS_BOOTSTRAP_WEIGHT: u128 = 10_000 * burst_consensus::rep_weights::WEIGHT_PER_HUMAN;
/// Default vote cache size.
/// A running BURST node.
pub struct BurstNode {
    pub config: NodeConfig,
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
    /// Confirming set — blocks waiting to be cemented.
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
    /// Online weight sampler — tracks recently-active reps for quorum calculation.
    pub online_weight_sampler: Arc<Mutex<OnlineWeightSampler>>,
    /// Message deduplication filter — prevents processing duplicate P2P messages.
    pub message_dedup: Arc<Mutex<burst_network::MessageDedup>>,
    /// Clock synchronization service for BRN time-dependent computation.
    pub clock_sync: Arc<Mutex<ClockSync>>,
    /// Delegation engine for governance vote delegation.
    pub delegation_engine: Arc<Mutex<DelegationEngine>>,
    /// VRF client for fetching drand randomness (verifier selection).
    pub vrf_client: Arc<Mutex<burst_vrf::DrandClient>>,
    /// Delegation store for scope-enforced delegation key verification.
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
}

impl BurstNode {
    /// Create and initialize a new BURST node.
    ///
    /// Opens the LMDB environment at `config.data_dir` and prepares all
    /// subsystems. Call [`start`] to begin accepting connections and
    /// processing blocks.
    pub async fn new(mut config: NodeConfig) -> Result<Self, NodeError> {
        let min_work_difficulty = config.params.min_work_difficulty;

        // Open LMDB storage
        let store = LmdbStore::open(&config.data_dir, MAX_DBS, DEFAULT_MAP_SIZE)
            .map_err(|e| NodeError::Other(format!("failed to open LMDB: {e}")))?;
        let store = Arc::new(store);

        // Peer manager
        let peer_manager = PeerManager::with_config(
            config.max_peers,
            config.bootstrap_peers.clone(),
            15, // keepalive interval (must be < READ_TIMEOUT of 30s)
        );
        let peer_manager = Arc::new(RwLock::new(peer_manager));

        // Block priority queue (replaces FIFO channel — higher PoW = higher priority)
        let block_queue = Arc::new(BlockPriorityQueue::new(BLOCK_CHANNEL_CAPACITY));

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
        let connection_registry = Arc::new(RwLock::new(ConnectionRegistry::new()));

        // Block processor + frontier (loaded from store)
        let frontier = Self::load_frontier_from_store(&store)?;
        let frontier = Arc::new(RwLock::new(frontier));
        let block_processor = Arc::new(Mutex::new(BlockProcessor::with_genesis_account(
            min_work_difficulty,
            genesis_key::genesis_address(config.network),
        )));

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
        // is consistent across the whole network (weight = confirmed balance of
        // that account's delegators). The genesis-authority node (the one holding
        // the genesis seed) votes as the GENESIS account, which carries the
        // bootstrap voting weight every node agrees on (seeded below). Any other
        // node votes as its own key and only carries weight once real TRST is
        // delegated to it — so a weightless node's votes are correctly ignored.
        let vote_generator = {
            let (rep_addr, rep_key) = match genesis_key::genesis_signing_key(config.network) {
                Some(kp) => (genesis_key::genesis_address(config.network), kp.private.0),
                None => {
                    let kp = burst_crypto::generate_keypair();
                    (burst_crypto::derive_address(&kp.public), kp.private.0)
                }
            };
            let mut vg = VoteGenerator::new(rep_addr.clone(), rep_key);
            if config.enable_representative {
                vg.set_representative(true);
                tracing::info!(representative = %rep_addr, "node voting as representative (enabled)");
            } else {
                tracing::info!(representative = %rep_addr, "node representative voting disabled");
            }
            Arc::new(Mutex::new(vg))
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
        let vrf_client = Arc::new(Mutex::new(burst_vrf::DrandClient::new()));

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
                        tracing::warn!(error = %e, "failed to deserialize persisted params, using config defaults");
                    }
                },
                _ => {
                    tracing::info!("no persisted protocol params found, using config defaults");
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
                    if loaded.current_rate() != config.params.brn_rate {
                        tracing::info!(
                            stored_rate = loaded.current_rate(),
                            protocol_rate = config.params.brn_rate,
                            "BRN rate mismatch — reinitializing rate history"
                        );
                        loaded.rate_history =
                            burst_brn::RateHistory::new(config.params.brn_rate, Timestamp::new(0));
                        if let Err(e) = loaded.save_to_store(&brn_store) {
                            tracing::warn!(error = %e, "failed to persist corrected BRN rate");
                        }
                    }
                    loaded
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load BRN engine state, starting fresh");
                    BrnEngine::with_rate(config.params.brn_rate, Timestamp::new(0))
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

        let mut node = Self {
            config,
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
                            tracing::warn!(error = %e, "failed to deserialize GovernanceEngine, starting fresh");
                            Arc::new(Mutex::new(GovernanceEngine::new()))
                        }
                    },
                    _ => {
                        tracing::info!("no persisted GovernanceEngine state, starting fresh");
                        Arc::new(Mutex::new(GovernanceEngine::new()))
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
            broadcaster,
            node_private_key,
            node_address,
            port_mapper: None,
            ledger_cache,
            task_handles: Vec::new(),
        };

        // Stash the receivers on the node so start() can consume them.
        // We use a trick: store them in Options that start() takes.
        // Since Rust doesn't allow partial moves from &mut self, we'll
        // pass them through start() via a separate helper.
        node.spawn_initial_tasks(outbound_rx).await?;

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
    fn initialize_genesis(&self) -> Result<(), NodeError> {
        let block_store = self.store.block_store();

        match block_store.block_count() {
            Ok(0) | Err(_) => {
                tracing::info!("empty database — creating genesis block");
            }
            Ok(count) => {
                tracing::info!(blocks = count, "database already initialized");
                return Ok(());
            }
        }

        let genesis_network = self.config.network;
        let genesis_account = genesis_key::genesis_address(genesis_network);
        let representative = genesis_account.clone();

        let mut genesis_block = StateBlock {
            version: CURRENT_BLOCK_VERSION,
            block_type: BlockType::Open,
            account: genesis_account.clone(),
            previous: BlockHash::ZERO,
            representative,
            brn_balance: 0,
            trst_balance: 0,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(0),
            params_hash: self.config.params.params_hash(),
            merge_sources: Vec::new(),
            work: 0,
            signature: Signature([0u8; 64]),
            hash: BlockHash::ZERO,
        };
        genesis_block.hash = genesis_block.compute_hash();
        // Signed by the creator's node if it holds the seed; a zero signature
        // on other nodes is safe — genesis is trusted by its (signature-
        // independent) hash, identical on every node.
        genesis_block.signature =
            genesis_key::sign_genesis(genesis_network, genesis_block.hash.as_bytes());

        // Persist genesis block, frontier, and schema version in a single write batch
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
        batch
            .put_meta("schema_version", b"1")
            .map_err(|e| NodeError::Other(format!("failed to batch schema version: {e}")))?;
        batch
            .commit()
            .map_err(|e| NodeError::Other(format!("failed to commit genesis batch: {e}")))?;

        self.ledger_cache.inc_block_count();
        tracing::info!(hash = %genesis_block.hash, "genesis block created");
        Ok(())
    }

    /// Spawn the core background tasks. Called once from `new()`.
    async fn spawn_initial_tasks(
        &mut self,
        outbound_rx: mpsc::Receiver<(String, Vec<u8>)>,
    ) -> Result<(), NodeError> {
        // ── Block processor task ──────────────────────────────────────────
        let bp = Arc::clone(&self.block_processor);
        let frontier = Arc::clone(&self.frontier);
        let store = Arc::clone(&self.store);
        let metrics = Arc::clone(&self.metrics);
        let mut shutdown_rx = self.shutdown.subscribe();
        let block_queue = Arc::clone(&self.block_queue);
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
        let trst_expiry_secs = self.config.params.trst_expiry_secs;
        let mut config_params_bp = self.config.params.clone();
        let genesis_network_bp = self.config.network;
        let fork_cache_bp = Arc::clone(&self.fork_cache);
        let vote_spacing_bp = Arc::clone(&self.vote_spacing);
        let ws_state_bp = Arc::clone(&self.ws_state);
        let governance_bp = Arc::clone(&self.governance);
        let delegation_bp = Arc::clone(&self.delegation_engine);
        let delegation_store_bp = Arc::clone(&self.delegation_store);
        let vrf_client_bp = Arc::clone(&self.vrf_client);
        let _verification_processor_bp = Arc::clone(&self.verification_processor);
        let verification_orch_bp = Arc::clone(&self.verification_orchestrator);
        let difficulty_adjuster_bp = Arc::clone(&self.difficulty_adjuster);
        let priority_scheduler_bp = Arc::clone(&self.priority_scheduler);
        let consti_engine_bp = Arc::clone(&self.consti_engine);

        let bp_handle = tokio::spawn(async move {
            loop {
                // Pop the highest-priority block (by PoW difficulty).
                // Use select! to remain responsive to shutdown signals.
                let block = tokio::select! {
                    biased;
                    _ = shutdown_rx.recv() => {
                        tracing::info!("block processor task shutting down");
                        break;
                    }
                    block = block_queue.pop() => block,
                };

                let start = std::time::Instant::now();
                let _loop_now_secs = unix_now_secs();

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

                // Validate BRN-spending blocks against the computed counter:
                // the odometer delta (burn or stake amount) must be covered by
                // BRN(w) = rate-accrual(verified_at → block.timestamp) − burned
                // − staked, computed independently by this node. This is the
                // whitepaper's core property — BRN exists only as computation,
                // and the ledger verifies the computation at spend time.
                let brn_spend_rejected = if matches!(
                    block.block_type,
                    BlockType::Burn
                        | BlockType::Endorse
                        | BlockType::Challenge
                        | BlockType::VerificationVote
                ) {
                    let spend = block.brn_balance.saturating_sub(prev_brn_balance);
                    if spend > 0 {
                        let brn = brn_engine_bp.lock().await;
                        match brn.wallets.get(&block.account) {
                            Some(state) => {
                                let available = brn.compute_balance(state, block.timestamp);
                                if spend > available {
                                    Some(format!(
                                        "BRN spend of {} exceeds computed available balance {}",
                                        spend, available
                                    ))
                                } else {
                                    None
                                }
                            }
                            None => Some(
                                "account has no BRN accrual state (not verified) — cannot spend BRN"
                                    .to_string(),
                            ),
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                // Enforce verification status: these actions require a Verified
                // account (incl. opting in as a verifier — only verified humans
                // may verify others).
                let verification_rejected = if matches!(
                    block.block_type,
                    BlockType::Send
                        | BlockType::Burn
                        | BlockType::Merge
                        | BlockType::Endorse
                        | BlockType::Challenge
                        | BlockType::VerifierOptIn
                ) {
                    match prev_account.as_ref() {
                        // Account exists but isn't verified — cannot spend.
                        Some(acct) if acct.state != burst_types::WalletState::Verified => {
                            Some(format!(
                                "account must be verified to perform {:?} (current state: {:?})",
                                block.block_type, acct.state
                            ))
                        }
                        // Account exists and is verified — allowed.
                        Some(_) => None,
                        // No account record: only an opening block (zero previous)
                        // may establish a chain; a non-open spend from an unknown
                        // account is invalid.
                        None if block.previous.is_zero() => None,
                        None => Some("account not found for verification check".to_string()),
                    }
                } else {
                    None
                };

                // Enforce new wallet spending limits
                let spending_limit_rejected =
                    if matches!(block.block_type, BlockType::Send | BlockType::Burn) {
                        prev_account.as_ref().and_then(|acct| {
                            let amount = if block.block_type == BlockType::Send {
                                acct.trst_balance.saturating_sub(block.trst_balance)
                            } else {
                                block.brn_balance.saturating_sub(prev_brn_balance)
                            };
                            let now = Timestamp::new(unix_now_secs());
                            crate::limits::check_wallet_limits(acct, amount, now, &config_params_bp)
                                .err()
                        })
                    } else {
                        None
                    };

                // Reject sends of expired or revoked TRST.
                // The TrstEngine tracks per-wallet token portfolios in memory;
                // if the sender is tracked, verify the send amount doesn't
                // exceed the non-expired, non-revoked (transferable) balance.
                let trst_transferable_rejected = if matches!(block.block_type, BlockType::Send) {
                    let send_amount = prev_account
                        .as_ref()
                        .map(|acct| acct.trst_balance.saturating_sub(block.trst_balance))
                        .unwrap_or(0);
                    if send_amount > 0 {
                        let mut trst = trst_engine_bp.lock().await;
                        // Bounded block timestamp: deterministic across nodes
                        // (a local clock would let honest nodes disagree at
                        // the expiry margin).
                        let now = block.timestamp;
                        match trst.transferable_balance(&block.account, now) {
                            Some(transferable) if send_amount > transferable => {
                                tracing::warn!(
                                    account = %block.account,
                                    send_amount,
                                    transferable,
                                    "rejected send: insufficient transferable TRST (expired/revoked tokens excluded)"
                                );
                                Some(format!(
                                    "insufficient transferable TRST: need {} but only {} is transferable",
                                    send_amount, transferable
                                ))
                            }
                            Some(_) => {
                                // Send must never cross origin boundaries — the
                                // wallet must merge first (whitepaper §Merging).
                                // Check the referenced origin actually covers
                                // the amount.
                                let origin_avail =
                                    trst.origin_transferable(&block.account, &block.origin, now);
                                if send_amount > origin_avail {
                                    tracing::warn!(
                                        account = %block.account,
                                        origin = %block.origin,
                                        send_amount,
                                        origin_avail,
                                        "rejected send: amount exceeds the referenced origin's tokens (merge first)"
                                    );
                                    Some(format!(
                                        "send of {} exceeds the {} transferable TRST of origin {} — merge tokens first",
                                        send_amount, origin_avail, block.origin
                                    ))
                                } else {
                                    None
                                }
                            }
                            None => None,
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                let result = if let Some(reason) = balance_rejected {
                    ProcessResult::Rejected(reason)
                } else if let Some(reason) = open_rejected {
                    ProcessResult::Rejected(reason)
                } else if let Some(reason) = receive_rejected {
                    ProcessResult::Rejected(reason)
                } else if let Some(reason) = brn_spend_rejected {
                    ProcessResult::Rejected(reason)
                } else if let Some(reason) = verification_rejected {
                    ProcessResult::Rejected(reason)
                } else if let Some(reason) = spending_limit_rejected {
                    ProcessResult::Rejected(reason)
                } else if let Some(reason) = trst_transferable_rejected {
                    ProcessResult::Rejected(reason)
                } else {
                    let mut processor = bp.lock().await;
                    let mut f = frontier.write().await;
                    processor.process(&block, &mut f)
                };

                metrics.blocks_processed.inc();

                match &result {
                    ProcessResult::Accepted => {
                        let bytes = match bincode::serialize(&block) {
                            Ok(b) => b,
                            Err(e) => {
                                tracing::error!(hash = %block.hash, error = %e, "block serialization failed");
                                continue;
                            }
                        };

                        // ── In-memory bookkeeping (no LMDB) ──────────────────
                        {
                            let mut bl = backlog_bp.lock().await;
                            bl.insert(
                                block.hash,
                                block.account.clone(),
                                block.work,
                                unix_now_secs(),
                            );
                        }
                        {
                            let balance = block.trst_balance.min(u64::MAX as u128) as u64;
                            let mut sched = priority_scheduler_bp.lock().await;
                            sched.push(block.hash, block.account.clone(), balance);
                        }

                        // ── Acquire all locks needed before the unified write
                        // batch. RwTxn is !Send so no awaits are possible while
                        // the batch exists. (Rep weights are NOT touched here —
                        // they're rebuilt from the ledger by the periodic weight
                        // task, so no rep-weight lock is held during the batch.)
                        let mut brn = brn_engine_bp.lock().await;
                        let mut trst = trst_engine_bp.lock().await;

                        // ── In-memory economics ──────────────────────────────
                        // The bounded, signed block timestamp — NOT the local
                        // clock — drives all economic computation so every
                        // node stamps identical token timestamps and computes
                        // identical expiry/accrual results.
                        let econ_now = block.timestamp;
                        let econ_result = crate::ledger_bridge::process_block_economics(
                            &block,
                            &mut brn,
                            &mut trst,
                            econ_now,
                            trst_expiry_secs,
                            prev_brn_balance,
                        );
                        tracing::trace!(hash = %block.hash, ?econ_result, "block economics processed");

                        if let crate::ledger_bridge::EconomicResult::Rejected { ref reason } =
                            econ_result
                        {
                            tracing::error!(hash = %block.hash, %reason, "block rejected due to economic invariant violation");
                            drop(trst);
                            drop(brn);
                            continue;
                        }

                        // Token tracking and deferred LMDB write collection
                        // (in-memory — collects data for the unified batch).
                        let mut deferred_pending: Option<(
                            u128,
                            burst_types::WalletAddress,
                            Vec<burst_trst::ConsumedProvenance>,
                        )> = None;
                        let mut deferred_trst_indices: Option<(
                            burst_types::TxHash,
                            burst_types::TxHash,
                            Timestamp,
                        )> = None;

                        match &econ_result {
                            crate::ledger_bridge::EconomicResult::BurnAndMint {
                                mint_token: Some(token),
                                ..
                            } => {
                                // Two-phase burn (8.4a): the freshly minted TRST
                                // goes to PENDING — the provider must publish a
                                // Receive block to claim it, exactly like a send.
                                // This keeps the receiver's on-chain balance in
                                // sync with the engine and lets them reject it.
                                let provenance = vec![burst_trst::ConsumedProvenance {
                                    amount: token.amount,
                                    origin: token.origin,
                                    origin_wallet: token.origin_wallet.clone(),
                                    origin_timestamp: token.origin_timestamp,
                                    effective_origin_timestamp: token.effective_origin_timestamp,
                                }];
                                deferred_pending =
                                    Some((token.amount, token.holder.clone(), provenance));
                                let expiry_ts = Timestamp::new(
                                    token
                                        .effective_origin_timestamp
                                        .as_secs()
                                        .saturating_add(trst_expiry_secs),
                                );
                                deferred_trst_indices = Some((token.origin, token.id, expiry_ts));
                            }
                            crate::ledger_bridge::EconomicResult::Send {
                                ref sender,
                                trst_balance_after,
                                ..
                            } => {
                                if let Some(acct) = prev_account.as_ref() {
                                    let send_amount =
                                        acct.trst_balance.saturating_sub(*trst_balance_after);
                                    let token_origin = &block.origin;
                                    let provenance = trst.debit_wallet_with_provenance(
                                        sender,
                                        token_origin,
                                        send_amount,
                                    );
                                    if let Some(destination) =
                                        crate::ledger_bridge::extract_receiver_from_link(
                                            &block.link,
                                        )
                                    {
                                        deferred_pending =
                                            Some((send_amount, destination, provenance));
                                    }
                                }
                            }
                            crate::ledger_bridge::EconomicResult::Receive {
                                ref receiver,
                                send_block_hash,
                                ..
                            } => {
                                let send_hash =
                                    burst_types::TxHash::new(*send_block_hash.as_bytes());
                                if let Ok(pend) =
                                    store.pending_store().get_pending(receiver, &send_hash)
                                {
                                    let received_token =
                                        crate::ledger_bridge::create_received_token(
                                            &block,
                                            &pend,
                                            trst_expiry_secs,
                                        );
                                    // Applies any revocation that landed while the
                                    // send was in flight (O(1) graph lookup), then
                                    // tracks the token.
                                    let revocations = trst.receive_token(received_token, econ_now);
                                    for ev in &revocations {
                                        tracing::warn!(
                                            %receiver,
                                            %send_block_hash,
                                            revoked_origin = %ev.revoked_origin,
                                            revoked_amount = ev.revoked_amount,
                                            "TRST receive: revocation applied to in-flight token"
                                        );
                                    }
                                    tracing::debug!(
                                        %receiver,
                                        %send_block_hash,
                                        amount = pend.amount,
                                        "TRST receive: token tracked in receiver portfolio"
                                    );
                                } else {
                                    tracing::trace!(
                                        %receiver,
                                        %send_block_hash,
                                        "no pending entry found for receive — receiver portfolio not updated"
                                    );
                                }
                            }
                            crate::ledger_bridge::EconomicResult::Merge { ref account } => {
                                if let Some(portfolio) = trst.get_portfolio(account) {
                                    // Input selection: the block's signed source
                                    // list when the wallet chose one (6.17b),
                                    // otherwise a deterministic expiry-grouped
                                    // auto-selection — merging tokens with similar
                                    // expiries maximizes retained value under the
                                    // earliest-expiry floor rule (whitepaper
                                    // §Merging).
                                    let selected: Vec<burst_trst::TrstToken> = if !block
                                        .merge_sources
                                        .is_empty()
                                    {
                                        let mut chosen =
                                            Vec::with_capacity(block.merge_sources.len());
                                        let mut all_found = true;
                                        for id in &block.merge_sources {
                                            match portfolio.tokens.iter().find(|t| t.id == *id) {
                                                Some(t) => chosen.push(t.clone()),
                                                None => {
                                                    tracing::warn!(
                                                        %account,
                                                        token = %id,
                                                        "merge block references a token not in the portfolio — merge skipped"
                                                    );
                                                    all_found = false;
                                                    break;
                                                }
                                            }
                                        }
                                        if all_found {
                                            chosen
                                        } else {
                                            Vec::new()
                                        }
                                    } else {
                                        select_expiry_merge_group(
                                            portfolio,
                                            econ_now,
                                            trst.expiry_secs,
                                        )
                                    };
                                    if selected.len() >= 2 {
                                        let merge_tx =
                                            burst_types::TxHash::new(*block.hash.as_bytes());
                                        match trst.merge(
                                            &selected,
                                            account.clone(),
                                            merge_tx,
                                            econ_now,
                                        ) {
                                            Ok(merged) => {
                                                let ids_to_remove: std::collections::HashSet<_> =
                                                    selected.iter().map(|t| t.id).collect();
                                                trst.bulk_untrack(account, &ids_to_remove);
                                                trst.track_token(merged);
                                                tracing::info!(%account, count = selected.len(), "TRST merge: tokens merged in portfolio");
                                            }
                                            Err(e) => {
                                                tracing::warn!(%account, error = %e, "TRST merge failed");
                                            }
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }

                        // Drop TRST engine lock before verification orchestrator
                        drop(trst);

                        // ── Unified write batch — single fsync ───────────────
                        // All block, frontier, account, pending, and TRST index
                        // writes are batched into one LMDB transaction.
                        let persisted = 'persist: {
                            let mut batch = match store.tx_begin_write() {
                                Ok(b) => b,
                                Err(e) => {
                                    tracing::error!(hash = %block.hash, "failed to start write batch: {e}");
                                    break 'persist false;
                                }
                            };
                            let height = prev_account.as_ref().map_or(1, |a| a.block_count + 1);
                            if let Err(e) = batch.put_block_with_account(
                                &block.hash,
                                &bytes,
                                &block.account,
                                height,
                            ) {
                                tracing::error!(hash = %block.hash, "failed to batch block: {e}");
                                break 'persist false;
                            }
                            if let Err(e) = batch.put_frontier(&block.account, &block.hash) {
                                tracing::error!(hash = %block.hash, "failed to batch frontier: {e}");
                                break 'persist false;
                            }
                            if let Err(e) = crate::ledger_updater::update_account_on_block(
                                &mut batch,
                                &block,
                                prev_account.as_ref(),
                                prev_brn_balance,
                            ) {
                                tracing::error!(hash = %block.hash, "failed to update account: {e}");
                            }
                            if let Err(e) =
                                crate::ledger_updater::delete_pending_entry(&mut batch, &block)
                            {
                                tracing::warn!(hash = %block.hash, "failed to delete pending: {e}");
                            }
                            if let Some((amount, ref dest, ref provenance)) = deferred_pending {
                                if let Err(e) = crate::ledger_updater::create_pending_entry(
                                    &mut batch,
                                    &block,
                                    amount,
                                    dest,
                                    provenance.clone(),
                                ) {
                                    tracing::warn!(hash = %block.hash, "failed to create pending in unified batch: {e}");
                                }
                            }
                            if let Some((origin, token_id, expiry_ts)) = deferred_trst_indices {
                                if let Err(e) = batch.put_origin_index(&origin, &token_id) {
                                    tracing::warn!(origin = %origin, token_id = %token_id, "failed to batch TRST origin index: {e}");
                                }
                                if let Err(e) = batch.put_expiry_index(expiry_ts, &token_id) {
                                    tracing::warn!(token_id = %token_id, "failed to batch TRST expiry index: {e}");
                                }
                            }

                            if let Err(e) = batch.commit() {
                                tracing::error!(hash = %block.hash, "failed to commit unified batch: {e}");
                                break 'persist false;
                            }

                            // Update atomic ledger cache counters
                            ledger_cache_bp.inc_block_count();
                            if block.block_type == BlockType::Open {
                                ledger_cache_bp.inc_account_count();
                            }
                            if block.block_type == BlockType::Send {
                                ledger_cache_bp.inc_pending_count();
                            }
                            if block.block_type == BlockType::Receive {
                                ledger_cache_bp.dec_pending_count();
                            }

                            true
                        };

                        if !persisted {
                            let mut f = frontier.write().await;
                            if block.previous.is_zero() {
                                f.remove(&block.account);
                            } else {
                                f.update(block.account.clone(), block.previous);
                            }
                            tracing::warn!(
                                hash = %block.hash,
                                "frontier rolled back due to persistence failure"
                            );
                        }

                        if let Some((ref origin, ref token_id, expiry_ts)) = deferred_trst_indices {
                            tracing::debug!(
                                token_id = %token_id,
                                origin = %origin,
                                expiry = expiry_ts.as_secs(),
                                "TRST token indices persisted to LMDB"
                            );
                        }

                        // Release unchecked dependents: any blocks that arrived
                        // before this one (gap-previous) or before its linked send
                        // (gap-source) were parked in the unchecked map. Now that
                        // this block is in the ledger, drain them and re-enqueue for
                        // full processing. This is what lets a bootstrap batch that
                        // the PoW priority queue reordered converge — each accepted
                        // block releases the next, cascading down the chain.
                        if persisted {
                            let released = {
                                let mut processor = bp.lock().await;
                                let mut deps = processor.process_unchecked(&block.hash);
                                deps.extend(processor.process_unchecked_source(&block.hash));
                                deps
                            };
                            for dep in released {
                                if !block_queue.push(dep).await {
                                    tracing::warn!(
                                        parent = %block.hash,
                                        "block queue full re-enqueuing unchecked dependent"
                                    );
                                }
                            }
                        }

                        // Post-commit: verification, governance, etc. (can await)

                        if let crate::ledger_bridge::EconomicResult::Endorse {
                            target: Some(ref target_addr),
                            burn_amount,
                            ..
                        } = econ_result
                        {
                            tracing::info!(
                                endorser = %block.account,
                                target = %target_addr,
                                burn_amount,
                                "endorsement recorded"
                            );

                            let genesis_addr = genesis_key::genesis_address(genesis_network_bp);
                            let verified_count =
                                store.account_store().verified_account_count().unwrap_or(0);
                            let bootstrap_threshold =
                                config_params_bp.bootstrap_exit_threshold as u64;
                            let in_bootstrap = verified_count < bootstrap_threshold;

                            if in_bootstrap && block.account == genesis_addr {
                                let mut orch = verification_orch_bp.lock().await;
                                match orch.genesis_verify(
                                    target_addr,
                                    &genesis_addr,
                                    verified_count,
                                    bootstrap_threshold,
                                ) {
                                    Ok(()) => {
                                        tracing::info!(
                                            target = %target_addr,
                                            verified_count,
                                            "genesis bootstrap: wallet directly verified"
                                        );
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            error = %e,
                                            target = %target_addr,
                                            "genesis bootstrap verification failed"
                                        );
                                    }
                                }
                            } else {
                                {
                                    let mut orch = verification_orch_bp.lock().await;
                                    if let Err(e) = orch.process_endorsement(
                                        target_addr,
                                        &block.account,
                                        burn_amount,
                                        &config_params_bp,
                                    ) {
                                        tracing::warn!(error = %e, "endorsement processing failed in orchestrator");
                                    }
                                }

                                // Fetch VRF randomness and feed selected verifiers to the orchestrator
                                let vrf = Arc::clone(&vrf_client_bp);
                                let orch_vrf = Arc::clone(&verification_orch_bp);
                                let target_for_vrf = target_addr.clone();
                                let params_vrf = config_params_bp.clone();
                                let store_vrf = Arc::clone(&store);
                                tokio::spawn(async move {
                                    let client = vrf.lock().await;
                                    match client.fetch_latest().await {
                                        Ok(beacon) => {
                                            let randomness = hex::decode(&beacon.randomness)
                                                .unwrap_or_else(|_| vec![0u8; 32]);
                                            let mut rand_bytes = [0u8; 32];
                                            let copy_len = randomness.len().min(32);
                                            rand_bytes[..copy_len]
                                                .copy_from_slice(&randomness[..copy_len]);

                                            // Eligible verifier pool derived from
                                            // CONFIRMED account state (opted in +
                                            // verified + old enough) so every node
                                            // computes the same set → deterministic
                                            // VRF selection.
                                            let verifier_addrs = eligible_verifiers(
                                                &store_vrf,
                                                &params_vrf,
                                                unix_now_secs(),
                                            );

                                            let mut orch = orch_vrf.lock().await;
                                            match orch.select_verifiers(
                                                &target_for_vrf,
                                                &verifier_addrs,
                                                &rand_bytes,
                                                &params_vrf,
                                            ) {
                                                Ok(selected) => {
                                                    tracing::info!(
                                                        target = %target_for_vrf,
                                                        selected_count = selected.len(),
                                                        drand_round = beacon.round,
                                                        "verifiers selected via VRF for endorsement"
                                                    );
                                                }
                                                Err(e) => {
                                                    tracing::error!(
                                                        error = %e,
                                                        target = %target_for_vrf,
                                                        "failed to assign verifiers via orchestrator"
                                                    );
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            tracing::error!(
                                                error = %e,
                                                "failed to fetch VRF randomness for verification"
                                            );
                                        }
                                    }
                                });
                            }
                        }

                        // Process challenge through verification/revocation system.
                        // When a challenge is accepted, the target wallet is queued
                        // for re-verification. Pending the re-verification outcome,
                        // if the target is found to be fraudulent (i.e. not a unique
                        // human), all TRST originating from that wallet is revoked
                        // through the merger graph, and the target's wallet state is
                        // set to Unverified.
                        if let crate::ledger_bridge::EconomicResult::Challenge {
                            target: Some(ref target_addr),
                            stake_amount,
                            ..
                        } = econ_result
                        {
                            tracing::info!(
                                challenger = %block.account,
                                target = %target_addr,
                                stake_amount = stake_amount,
                                "challenge recorded — initiating re-verification"
                            );

                            // Register the challenge with the orchestrator for
                            // re-verification. Do NOT revoke TRST or change
                            // account state here — that only happens if the
                            // orchestrator confirms fraud via WalletUnverified.
                            let challenger_verified = prev_account
                                .as_ref()
                                .is_some_and(|a| a.state == burst_types::WalletState::Verified);
                            // A Challenge block's `origin` field marks whether this
                            // is a Fraud challenge (revoke on upheld) or a benign
                            // Inactivity challenge (deactivate, no revoke).
                            let reason = burst_verification::ChallengeReason::from_origin(
                                block.origin.as_bytes(),
                            );
                            let mut orch = verification_orch_bp.lock().await;
                            if let Err(e) = orch.initiate_challenge(
                                target_addr,
                                &block.account,
                                challenger_verified,
                                stake_amount,
                                reason,
                                &config_params_bp,
                            ) {
                                tracing::warn!(
                                    target = %target_addr,
                                    challenger = %block.account,
                                    error = %e,
                                    "challenge initiation failed in orchestrator"
                                );
                            }
                        }

                        // TRST token indices are now persisted in the
                        // unified write batch above — no separate fsync.

                        // BurnOnly: BRN was burned but no valid receiver was found,
                        // so no TRST was minted. The burn was already recorded by
                        // process_block_economics; log for visibility.
                        if let crate::ledger_bridge::EconomicResult::BurnOnly { burn_amount } =
                            econ_result
                        {
                            tracing::info!(
                                account = %block.account,
                                burn_amount,
                                "BRN burned without TRST mint (no valid receiver)"
                            );
                        }

                        // Process governance blocks through the GovernanceEngine
                        if let crate::ledger_bridge::EconomicResult::GovernanceProposal {
                            ref proposer,
                            proposal_hash,
                            ref content,
                        } = econ_result
                        {
                            let mut gov = governance_bp.lock().await;

                            let proposal_content = content.clone().unwrap_or_else(|| {
                                    tracing::warn!(proposer = %proposer, "governance proposal content not decoded from block, using default");
                                    burst_governance::proposal::ProposalContent::ParameterChange {
                                        param: burst_governance::GovernableParam::BrnRate,
                                        new_value: 0,
                                    }
                                });

                            let total_eligible =
                                store.account_store().verified_account_count().unwrap_or(0) as u32;

                            let proposer_verified = store
                                .account_store()
                                .get_account(proposer)
                                .map(|a| a.state == burst_types::WalletState::Verified)
                                .unwrap_or(false);

                            let proposal = burst_governance::proposal::Proposal {
                                hash: proposal_hash,
                                proposer: proposer.clone(),
                                content: proposal_content,
                                phase: burst_governance::proposal::GovernancePhase::Proposal,
                                created_at: Timestamp::new(unix_now_secs()),
                                endorsement_count: 0,
                                exploration_votes_yea: 0,
                                exploration_votes_nay: 0,
                                exploration_votes_abstain: 0,
                                promotion_votes_yea: 0,
                                promotion_votes_nay: 0,
                                promotion_votes_abstain: 0,
                                exploration_started_at: None,
                                cooldown_started_at: None,
                                promotion_started_at: None,
                                activation_at: None,
                                total_eligible_voters: total_eligible,
                                round: 0,
                            };
                            let brn_balance = brn
                                .wallets
                                .get(&block.account)
                                .map(|ws| {
                                    ws.available_balance(
                                        &brn.rate_history,
                                        Timestamp::new(unix_now_secs()),
                                    )
                                })
                                .unwrap_or(0);
                            match gov.submit_proposal(
                                proposal,
                                brn_balance,
                                proposer_verified,
                                &config_params_bp,
                            ) {
                                Ok(hash) => {
                                    tracing::info!(%hash, proposer = %proposer, "governance proposal registered in engine")
                                }
                                Err(e) => {
                                    tracing::warn!(proposer = %proposer, "governance proposal rejected by engine: {e}")
                                }
                            }
                        }

                        // Drop BRN engine lock — no longer needed after governance balance check.
                        // CRITICAL: must drop before verification events which re-acquire.
                        drop(brn);

                        if let crate::ledger_bridge::EconomicResult::GovernanceVote {
                            ref voter,
                            proposal_hash,
                            vote,
                        } = econ_result
                        {
                            let mut gov = governance_bp.lock().await;
                            let now = Timestamp::new(unix_now_secs());

                            let voting_power = {
                                let del = delegation_bp.lock().await;
                                del.voting_power(voter)
                            };
                            tracing::debug!(
                                %proposal_hash,
                                voter = %voter,
                                voting_power,
                                ?vote,
                                "governance vote with delegated voting power"
                            );

                            match gov.cast_exploration_vote(
                                &proposal_hash,
                                voter,
                                vote,
                                now,
                                &config_params_bp,
                            ) {
                                Ok(()) => {
                                    tracing::info!(%proposal_hash, voter = %voter, ?vote, "governance exploration vote recorded")
                                }
                                Err(burst_governance::GovernanceError::WrongPhase) => {
                                    match gov.cast_promotion_vote(
                                        &proposal_hash,
                                        voter,
                                        vote,
                                        now,
                                        &config_params_bp,
                                    ) {
                                        Ok(()) => {
                                            tracing::info!(%proposal_hash, voter = %voter, ?vote, "governance promotion vote recorded")
                                        }
                                        Err(e) => {
                                            tracing::warn!(%proposal_hash, voter = %voter, "governance vote rejected: {e}")
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(%proposal_hash, voter = %voter, "governance vote rejected: {e}")
                                }
                            }
                        }

                        // Process delegation blocks through the DelegationEngine
                        if block.block_type == BlockType::Delegate {
                            let target =
                                crate::ledger_bridge::extract_receiver_from_link(&block.link);
                            if let Some(ref target_addr) = target {
                                let mut del = delegation_bp.lock().await;
                                match del.delegate(&block.account, target_addr) {
                                    Ok(()) => tracing::info!(
                                        delegator = %block.account,
                                        delegate = %target_addr,
                                        "governance delegation registered"
                                    ),
                                    Err(e) => tracing::warn!(
                                        delegator = %block.account,
                                        delegate = %target_addr,
                                        "governance delegation rejected: {e}"
                                    ),
                                }

                                // Store delegation record for scope-enforced signature verification.
                                // The delegation public key is derived from the transaction hash field.
                                let delegation_public_key: [u8; 32] = *block.transaction.as_bytes();
                                let record = DelegationRecord {
                                    delegator: block.account.clone(),
                                    delegate: target_addr.clone(),
                                    delegation_public_key,
                                    created_at: block.timestamp,
                                    revoked: false,
                                };
                                if let Err(e) = delegation_store_bp.put_delegation(&record) {
                                    tracing::warn!(
                                        delegator = %block.account,
                                        "failed to store delegation record: {e}"
                                    );
                                }
                            } else {
                                tracing::warn!(
                                    delegator = %block.account,
                                    "delegate block has no valid target in link field"
                                );
                            }
                        }

                        if block.block_type == BlockType::RevokeDelegation {
                            let mut del = delegation_bp.lock().await;
                            del.undelegate(&block.account);
                            tracing::info!(
                                delegator = %block.account,
                                "governance delegation revoked"
                            );

                            // Revoke delegation in the scope-enforcement store
                            if let Err(e) = delegation_store_bp.revoke_delegation(&block.account) {
                                tracing::warn!(
                                    delegator = %block.account,
                                    "failed to revoke delegation record: {e}"
                                );
                            }
                        }

                        // GovernanceActivation: apply the on-chain parameter change
                        // (Tezos-style self-amendment recorded on the genesis chain).
                        if let crate::ledger_bridge::EconomicResult::GovernanceActivation {
                            proposal_hash,
                            new_params_hash,
                        } = &econ_result
                        {
                            let mut gov = governance_bp.lock().await;
                            if let Some(proposal) = gov.get_proposal(proposal_hash) {
                                let p = proposal.clone();
                                let mut params = config_params_bp.clone();
                                if gov.activate(&p, &mut params).is_ok() {
                                    let computed = params.params_hash();
                                    if computed == *new_params_hash {
                                        config_params_bp = params.clone();
                                        let changes = gov.drain_pending_changes();
                                        if !changes.is_empty() {
                                            for (param, value) in &changes {
                                                match param {
                                                    burst_governance::GovernableParam::BrnRate => {
                                                        let mut brn_lock =
                                                            brn_engine_bp.lock().await;
                                                        if let Err(e) = brn_lock.apply_rate_change(
                                                            *value,
                                                            Timestamp::new(unix_now_secs()),
                                                        ) {
                                                            tracing::warn!(error = %e, "failed to propagate BRN rate change from activation block");
                                                        }
                                                    }
                                                    burst_governance::GovernableParam::TrstExpirySecs => {
                                                        // 6.9: expiry is computed from inception +
                                                        // CURRENT period — previously expired TRST
                                                        // can become transferable again.
                                                        let new_expiry = *value as u64;
                                                        let mut trst_lock =
                                                            trst_engine_bp.lock().await;
                                                        trst_lock.set_expiry_period(
                                                            new_expiry,
                                                            Timestamp::new(unix_now_secs()),
                                                        );
                                                        tracing::info!(
                                                            expiry_secs = new_expiry,
                                                            "TRST expiry period changed via governance — portfolios re-evaluated"
                                                        );
                                                    }
                                                    other => {
                                                        tracing::info!(param = ?other, value = value, "governance parameter activated via on-chain block");
                                                    }
                                                }
                                            }
                                        }
                                        let amendments = gov.drain_activated_amendments();
                                        if !amendments.is_empty() {
                                            let mut consti = consti_engine_bp.lock().await;
                                            for amendment_content in &amendments {
                                                if let burst_governance::ProposalContent::ConstitutionalAmendment { ref title, ref text } = amendment_content {
                                                    let amendment = burst_consti::Amendment {
                                                        hash: TxHash::ZERO,
                                                        proposer: WalletAddress::new("governance"),
                                                        title: title.clone(),
                                                        text: text.clone(),
                                                        phase: burst_governance::GovernancePhase::Activated,
                                                        votes_yea: 0,
                                                        votes_nay: 0,
                                                        votes_abstain: 0,
                                                        created_at: Timestamp::new(unix_now_secs()),
                                                        operations: Vec::new(),
                                                    };
                                                    match consti.activate_amendment_internal(&amendment) {
                                                        Ok(()) => tracing::info!(title = %title, "constitutional amendment applied via activation block"),
                                                        Err(e) => tracing::warn!(title = %title, "failed to apply constitutional amendment from activation block: {e}"),
                                                    }
                                                }
                                            }
                                        }
                                        // ORV evictions/reinstatements: set or
                                        // clear the target's `orv_evicted` flag in
                                        // confirmed account state. Applied on EVERY
                                        // node as it processes this activation block
                                        // (so all nodes agree), and picked up by the
                                        // periodic rep-weight rebuild. Idempotent.
                                        let evictions = gov.drain_pending_evictions();
                                        for (target, evict) in &evictions {
                                            match store.account_store().get_account(target) {
                                                Ok(mut acct) => {
                                                    acct.orv_evicted = *evict;
                                                    if let Err(e) =
                                                        store.account_store().put_account(&acct)
                                                    {
                                                        tracing::warn!(%target, "failed to persist ORV eviction: {e}");
                                                    } else {
                                                        tracing::warn!(%target, evict = *evict, "representative ORV eviction/reinstatement applied via activation block");
                                                    }
                                                }
                                                Err(_) => {
                                                    tracing::warn!(%target, "ORV eviction target account not found — skipping");
                                                }
                                            }
                                        }
                                        // Persist to LMDB
                                        if let Ok(bytes) = bincode::serialize(&config_params_bp) {
                                            let brn_store_meta = store.brn_store();
                                            if let Err(e) =
                                                brn_store_meta.put_meta(b"protocol_params", &bytes)
                                            {
                                                tracing::warn!(error = %e, "failed to persist params from activation block");
                                            } else {
                                                tracing::info!(%proposal_hash, "self-amended protocol params persisted via on-chain activation block");
                                            }
                                        }
                                    } else {
                                        tracing::warn!(
                                            %proposal_hash,
                                            expected = %new_params_hash,
                                            computed = %computed,
                                            "params hash mismatch in governance activation block — skipping"
                                        );
                                    }
                                } else {
                                    tracing::warn!(%proposal_hash, "governance activation failed");
                                }
                            } else {
                                tracing::warn!(%proposal_hash, "governance activation block references unknown proposal");
                            }
                        }

                        // Merge: balance is handled at the ledger level by
                        // update_account_on_block (trst_balance comes from the block).
                        // Individual token provenance tracking (TrstEngine merge)
                        // is deferred until per-token persistence via TrstIndexStore.
                        if let crate::ledger_bridge::EconomicResult::Merge { ref account } =
                            econ_result
                        {
                            tracing::info!(%account, "TRST merge processed at ledger level");
                        }

                        // Send: pending entry already created in the write batch above
                        // via ledger_updater::create_pending_entry.
                        if let crate::ledger_bridge::EconomicResult::Send {
                            ref sender,
                            ref receiver,
                            trst_balance_after,
                        } = econ_result
                        {
                            tracing::debug!(
                                %sender,
                                receiver = receiver.as_ref().map(|r| r.as_str()).unwrap_or("unknown"),
                                trst_balance_after,
                                "TRST send processed, pending entry created in write batch"
                            );
                        }

                        // Receive: pending entry already deleted in the write batch above
                        // via ledger_updater::delete_pending_entry.
                        if let crate::ledger_bridge::EconomicResult::Receive {
                            ref receiver,
                            send_block_hash,
                            trst_balance_after,
                        } = econ_result
                        {
                            tracing::debug!(
                                %receiver,
                                %send_block_hash,
                                trst_balance_after,
                                "TRST receive processed, pending entry deleted in write batch"
                            );
                        }

                        // RejectReceive: pending entry deleted in the write batch above
                        // (delete_pending_entry handles both Receive and RejectReceive).
                        if let crate::ledger_bridge::EconomicResult::RejectReceive {
                            ref rejecter,
                            send_block_hash,
                        } = econ_result
                        {
                            tracing::info!(
                                %rejecter,
                                %send_block_hash,
                                "TRST receive rejected, pending entry deleted in write batch"
                            );
                        }

                        // RepChange: rep weight cache is already updated atomically
                        // in the write batch via update_account_on_block, which calls
                        // RepWeightCache::remove_weight/add_weight. No duplicate update
                        // needed here.
                        if let crate::ledger_bridge::EconomicResult::RepChange {
                            ref account,
                            ref old_rep,
                            ref new_rep,
                            balance,
                        } = econ_result
                        {
                            tracing::debug!(
                                %account,
                                old_rep = old_rep.as_ref().map(|r| r.as_str()).unwrap_or("none"),
                                new_rep = %new_rep,
                                balance,
                                "representative changed, rep weight cache updated in write batch"
                            );
                        }

                        if let crate::ledger_bridge::EconomicResult::VerificationVoteResult {
                            ref voter,
                            target: Some(ref target_addr),
                            vote,
                            stake: _,
                        } = econ_result
                        {
                            let vote_enum = match vote {
                                1 => burst_verification::Vote::Legitimate,
                                2 => burst_verification::Vote::Illegitimate,
                                _ => burst_verification::Vote::Neither,
                            };
                            let mut orch = verification_orch_bp.lock().await;
                            match orch.process_vote(
                                target_addr,
                                voter,
                                vote_enum,
                                &config_params_bp,
                            ) {
                                Ok(maybe_event) => {
                                    tracing::info!(
                                        voter = %voter,
                                        target = %target_addr,
                                        vote,
                                        completed = maybe_event.is_some(),
                                        "verification vote processed by orchestrator"
                                    );
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        voter = %voter,
                                        target = %target_addr,
                                        error = %e,
                                        "verification vote processing failed"
                                    );
                                }
                            }

                            // Challenge re-votes don't auto-tally: finalize as
                            // soon as the last selected verifier has voted.
                            // The resulting WalletUnverified/ChallengeResolved
                            // events queue into pending_events and are drained
                            // just below.
                            match orch.try_resolve_challenge(target_addr, &config_params_bp) {
                                Ok(Some(_)) => {
                                    tracing::info!(target = %target_addr, "challenge vote complete — resolving");
                                }
                                Ok(None) => {}
                                Err(e) => {
                                    tracing::warn!(target = %target_addr, error = %e, "challenge resolution failed");
                                }
                            }

                            // Drain orchestrator events and act on them
                            let events = orch.drain_events();
                            for event in events {
                                match event {
                                        burst_verification::VerificationEvent::EndorsementComplete { ref wallet } => {
                                            tracing::info!(%wallet, "endorsement threshold reached");
                                        }
                                        burst_verification::VerificationEvent::VerifiersSelected { ref wallet, ref verifiers } => {
                                            tracing::info!(%wallet, count = verifiers.len(), "verifiers assigned by orchestrator");
                                        }
                                        burst_verification::VerificationEvent::VerificationComplete { ref wallet, ref result, ref outcomes } => {
                                            tracing::info!(%wallet, ?result, "verification complete");
                                            if *result == burst_verification::VerificationResult::Verified {
                                                // Create-or-update: a freshly endorsed wallet
                                                // (e.g. genesis bootstrap) has no account yet —
                                                // verification is what brings it on-chain.
                                                let mut acct = store
                                                    .account_store()
                                                    .get_account(wallet)
                                                    .unwrap_or_else(|_| {
                                                        burst_store::account::AccountInfo {
                                                            address: wallet.clone(),
                                                            state: burst_types::WalletState::Unverified,
                                                            verified_at: None,
                                                            head: BlockHash::ZERO,
                                                            block_count: 0,
                                                            confirmation_height: 0,
                                                            representative: wallet.clone(),
                                                            total_brn_burned: 0,
                                                            total_brn_staked: 0,
                                                            trst_balance: 0,
                                                            expired_trst: 0,
                                                            revoked_trst: 0,
                                                            epoch: 0,
                                                            verifier_opted_in_at: None,
                                                            orv_evicted: false,
                                                        }
                                                    });
                                                let was_revoked = acct.state == burst_types::WalletState::Revoked;
                                                let was_deactivated = acct.state == burst_types::WalletState::Deactivated;
                                                acct.state = burst_types::WalletState::Verified;
                                                acct.verified_at = Some(Timestamp::now());
                                                if let Err(e) = store.account_store().put_account(&acct) {
                                                    tracing::error!(%wallet, "failed to persist Verified account: {e}");
                                                }

                                                // 6.15(b): re-verification un-revokes the TRST
                                                // this wallet originated, restoring it to all
                                                // current holders.
                                                if was_revoked {
                                                    let mut trst_inner = trst_engine_bp.lock().await;
                                                    let restored = trst_inner.un_revoke_by_origin(wallet, Timestamp::now());
                                                    drop(trst_inner);
                                                    if !restored.is_empty() {
                                                        // Restore to each CURRENT HOLDER (mirror of
                                                        // revocation), not just the originator.
                                                        let total_restored =
                                                            apply_unrevocations_to_holders(&store, &restored);
                                                        tracing::info!(
                                                            %wallet,
                                                            restored_count = restored.len(),
                                                            total_restored,
                                                            "TRST un-revoked after re-verification"
                                                        );
                                                    }
                                                }
                                                // Activate (or resume) BRN accrual. Endorsers
                                                // receive NO protocol reward (decision 33.8a) —
                                                // endorsement is a social obligation; their BRN
                                                // was permanently burned by the Endorse block.
                                                let mut brn_inner = brn_engine_bp.lock().await;
                                                if was_deactivated {
                                                    // Benign deactivation → re-verification
                                                    // resumes accrual, keeping the balance the
                                                    // wallet had earned (gap-shifted).
                                                    if let Some(ws) = brn_inner.get_wallet_mut(wallet) {
                                                        ws.resume_accrual(Timestamp::now());
                                                        tracing::info!(%wallet, "BRN accrual resumed after re-verification of deactivated wallet");
                                                    } else {
                                                        brn_inner.track_wallet(wallet.clone(), burst_brn::BrnWalletState::new(Timestamp::now()));
                                                    }
                                                } else {
                                                    // Fresh verification (or fraud re-verification
                                                    // — "new BRN starts fresh", 6.15).
                                                    let ws = burst_brn::BrnWalletState::new(Timestamp::now());
                                                    brn_inner.track_wallet(wallet.clone(), ws);
                                                    tracing::info!(%wallet, "BRN accrual activated after verification");
                                                }
                                                drop(brn_inner);
                                            }

                                            // Resolve verifier stakes and burn-backed TRST
                                            // rewards (decision 33.7d): dissenter stakes are
                                            // forfeited (burned) and the majority receives
                                            // that value as TRST via pending entries claimed
                                            // with normal Receive blocks.
                                            {
                                                let mut brn_inner = brn_engine_bp.lock().await;
                                                crate::ledger_bridge::resolve_verifier_outcomes(
                                                    &mut brn_inner,
                                                    &store.pending_store(),
                                                    &outcomes.verifiers,
                                                    wallet,
                                                    block.timestamp,
                                                );
                                            }
                                        }
                                        burst_verification::VerificationEvent::WalletUnverified { ref wallet } => {
                                            tracing::warn!(%wallet, "wallet unverified (fraud confirmed)");
                                            let mut trst_inner = trst_engine_bp.lock().await;
                                            let revocations = trst_inner.revoke_by_origin(wallet);
                                            drop(trst_inner);
                                            // Charge each AFFECTED HOLDER (not just the
                                            // fraudulent originator): via the merger graph the
                                            // revoked TRST may now be held by any downstream
                                            // account, so debit every holder's AccountInfo
                                            // (transferable balance). Returns the true total
                                            // revoked. (Consensus weight is expired-TRST based
                                            // and untouched here — see the helper's docs.)
                                            let total_revoked =
                                                apply_revocations_to_holders(&store, &revocations);
                                            if !revocations.is_empty() {
                                                tracing::warn!(
                                                    %wallet,
                                                    revoked_count = revocations.len(),
                                                    total_revoked,
                                                    "TRST revoked via orchestrator fraud confirmation"
                                                );
                                            }
                                            // Fraud stops BRN accrual — the whitepaper's
                                            // "loses all BRN accrual and transaction rights".
                                            {
                                                let mut brn_inner = brn_engine_bp.lock().await;
                                                if let Some(ws) = brn_inner.get_wallet_mut(wallet) {
                                                    ws.stop_accrual(Timestamp::now());
                                                }
                                            }
                                            // Mark the originator Revoked (its own holdings, if
                                            // any, were already debited above as a holder).
                                            if let Ok(mut acct) = store.account_store().get_account(wallet) {
                                                acct.state = burst_types::WalletState::Revoked;
                                                if let Err(e) = store.account_store().put_account(&acct) {
                                                    tracing::error!(%wallet, "failed to persist account Revoked state: {e}");
                                                }
                                            }
                                        }
                                        burst_verification::VerificationEvent::WalletDeactivated { ref wallet } => {
                                            // Benign unverification (whitepaper §Unverification
                                            // Without Revocation): death, prolonged inactivity,
                                            // or other Consti-defined grounds. BRN accrual stops
                                            // and transaction rights are lost, but originated
                                            // TRST is NOT revoked — it was legitimately earned.
                                            tracing::info!(%wallet, "wallet deactivated (benign unverification — TRST NOT revoked)");
                                            {
                                                let mut brn_inner = brn_engine_bp.lock().await;
                                                if let Some(ws) = brn_inner.get_wallet_mut(wallet) {
                                                    ws.stop_accrual(Timestamp::now());
                                                }
                                            }
                                            if let Ok(mut acct) = store.account_store().get_account(wallet) {
                                                acct.state = burst_types::WalletState::Deactivated;
                                                if let Err(e) = store.account_store().put_account(&acct) {
                                                    tracing::error!(%wallet, "failed to persist account Deactivated state: {e}");
                                                }
                                            }
                                        }
                                        burst_verification::VerificationEvent::ChallengeResolved { ref wallet, ref outcome } => {
                                            tracing::info!(%wallet, ?outcome.outcome, "challenge resolved via orchestrator");

                                            // Resolve the challenger's BRN stake.
                                            {
                                                let mut brn_inner = brn_engine_bp.lock().await;
                                                if let Some(ws) = brn_inner.get_wallet_mut(&outcome.challenger) {
                                                    match outcome.outcome {
                                                        burst_verification::ChallengeResult::FraudConfirmed => {
                                                            // Stake returned in full.
                                                            ws.total_staked = ws.total_staked.saturating_sub(outcome.challenger_stake);
                                                            tracing::info!(challenger = %outcome.challenger, stake = outcome.challenger_stake, "challenger stake returned (fraud confirmed)");
                                                        }
                                                        burst_verification::ChallengeResult::ChallengeRejected => {
                                                            // Stake forfeited (burned).
                                                            ws.total_staked = ws.total_staked.saturating_sub(outcome.challenger_stake);
                                                            ws.total_burned = ws.total_burned.saturating_add(outcome.challenger_stake);
                                                            tracing::info!(challenger = %outcome.challenger, stake = outcome.challenger_stake, "challenger stake forfeited (challenge rejected)");
                                                        }
                                                        burst_verification::ChallengeResult::Expired => {
                                                            // Half returned, half burned (time-wasting penalty).
                                                            let penalty = outcome.challenger_stake / 2;
                                                            ws.total_staked = ws.total_staked.saturating_sub(outcome.challenger_stake);
                                                            ws.total_burned = ws.total_burned.saturating_add(penalty);
                                                            tracing::info!(challenger = %outcome.challenger, penalty, "challenge expired — half the stake forfeited");
                                                        }
                                                    }
                                                } else {
                                                    tracing::warn!(challenger = %outcome.challenger, "challenger wallet not tracked in BRN engine, cannot resolve stake");
                                                }

                                                // Resolve challenge-vote verifier stakes and
                                                // burn-backed TRST rewards (33.7d).
                                                crate::ledger_bridge::resolve_verifier_outcomes(
                                                    &mut brn_inner,
                                                    &store.pending_store(),
                                                    &outcome.verifier_outcomes,
                                                    wallet,
                                                    block.timestamp,
                                                );
                                            }

                                            // Challenger TRST reward on confirmed fraud:
                                            // min(revoked × bps, cap) per the parameter table.
                                            // The WalletUnverified event is emitted before
                                            // ChallengeResolved, so the revocation total is
                                            // already recorded on the account. Backed by the
                                            // destroyed revoked TRST (≥ 100x the reward), so
                                            // never net-inflationary.
                                            if outcome.outcome == burst_verification::ChallengeResult::FraudConfirmed {
                                                let revoked_total = store
                                                    .account_store()
                                                    .get_account(wallet)
                                                    .map(|a| a.revoked_trst)
                                                    .unwrap_or(0);
                                                let reward = std::cmp::min(
                                                    revoked_total.saturating_mul(config_params_bp.challenge_reward_bps as u128) / 10_000,
                                                    config_params_bp.challenge_reward_cap,
                                                );
                                                if reward > 0 {
                                                    match crate::ledger_bridge::create_reward_pending(
                                                        &store.pending_store(),
                                                        &outcome.challenger,
                                                        wallet,
                                                        b"challenger-reward",
                                                        reward,
                                                        block.timestamp,
                                                    ) {
                                                        Ok(reward_hash) => tracing::info!(
                                                            challenger = %outcome.challenger,
                                                            reward,
                                                            revoked_total,
                                                            %reward_hash,
                                                            "challenger TRST reward granted as pending"
                                                        ),
                                                        Err(e) => tracing::error!(
                                                            challenger = %outcome.challenger,
                                                            error = %e,
                                                            "failed to create challenger reward pending entry"
                                                        ),
                                                    }
                                                }
                                            }
                                        }
                                        burst_verification::VerificationEvent::VerifierPenalized { ref verifier, ref reason, cooldown_until } => {
                                            tracing::warn!(
                                                %verifier,
                                                %reason,
                                                cooldown_until,
                                                "verifier penalized — excluded from future selection"
                                            );
                                        }
                                    }
                            }
                        }

                        // Track acceptance (NOT confirmation — that happens via consensus)
                        metrics.blocks_accepted.inc();
                        difficulty_adjuster_bp
                            .lock()
                            .await
                            .record_block(block.timestamp.as_secs());
                        tracing::debug!(hash = %block.hash, "block accepted and persisted");

                        // Publish block acceptance event to WebSocket subscribers
                        ws_state_bp.publish_confirmation(
                            &block.account.to_string(),
                            &format!("{}", block.hash),
                            &block.trst_balance.to_string(),
                        );
                        ws_state_bp.publish_account_update(
                            &block.account.to_string(),
                            &block.trst_balance.to_string(),
                            &format!("{:?}", block.block_type),
                        );

                        // TASK 2: Generate and broadcast a vote for the accepted block
                        {
                            let mut vg = vote_generator_bp.lock().await;
                            if vg.is_representative {
                                let mut vs = vote_spacing_bp.lock().await;
                                // Space votes by the election ROOT (the frontier
                                // position = `previous`), not the account. Rapid
                                // sequential blocks on one account each extend a
                                // distinct `previous`, so they're all votable;
                                // only genuine forks at the same position share a
                                // root and get rate-limited. Open blocks have no
                                // `previous` (ZERO), so key them by their own hash
                                // to avoid cross-account collisions on ZERO.
                                let spacing_root = if block.previous.is_zero() {
                                    block.hash
                                } else {
                                    block.previous
                                };
                                if vs.votable(&spacing_root, &block.hash) {
                                    vs.record(spacing_root, block.hash);
                                    drop(vs);
                                    let vote = vg.generate_vote(block.hash);
                                    drop(vg);

                                    // Count OUR OWN vote in OUR election tally.
                                    // Previously the accept path only broadcast
                                    // the vote to peers and never fed it into the
                                    // local active_elections, so a single node (or
                                    // this node's own view) could never reach soft
                                    // quorum → nothing ever confirmed/cemented.
                                    // Start the election for this block (root = its
                                    // own hash) and record our vote + online weight.
                                    {
                                        let now = Timestamp::new(unix_now_secs());
                                        let our_weight =
                                            rep_weights_bp.read().await.weight(&vote.voter);
                                        if our_weight > 0 {
                                            online_weight_sampler_bp
                                                .lock()
                                                .await
                                                .record_vote(&vote.voter, now.as_secs());
                                            let mut ae = active_elections_bp.write().await;
                                            let _ = ae.start_election(block.hash, now);
                                            let _ = ae.process_vote(
                                                &block.hash,
                                                &vote.voter,
                                                block.hash,
                                                our_weight,
                                                false,
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
                                    // Flood the BLOCK itself (Nano-style block
                                    // publishing) so peers have it and can vote —
                                    // without this, peers only got blocks via the
                                    // slow ~30s bootstrap pull. Receive-side
                                    // message dedup breaks re-flood loops.
                                    if let Ok(block_bytes) = bincode::serialize(
                                        &WireMessage::Block(Box::new(block.clone())),
                                    ) {
                                        let _ = broadcaster_bp
                                            .broadcast_with_fanout(&block_bytes, &peers, 4)
                                            .await;
                                    }
                                    if let Ok(msg_bytes) = bincode::serialize(&wire_msg) {
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
                        // Cache the fork block for election consideration
                        {
                            let mut fc = fork_cache_bp.lock().await;
                            fc.insert(block.previous, block.hash);
                        }
                        // Fork detected — start an election on the root (previous block)
                        let now = Timestamp::new(unix_now_secs());
                        let root = block.previous;
                        {
                            let mut ae = active_elections_bp.write().await;
                            if let Err(e) = ae.start_election(root, now) {
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
                            match bs.height_of_block(&root) {
                                Ok(Some(h)) => {
                                    bs.block_at_height(&block.account, h + 1).ok().flatten()
                                }
                                // Open-block fork (no `previous`): the competitor
                                // is the account's first block (height 1).
                                _ if root.is_zero() => {
                                    bs.block_at_height(&block.account, 1).ok().flatten()
                                }
                                _ => None,
                            }
                        };
                        if let Some(accepted) = competitor {
                            if accepted != block.hash {
                                let (our_rep, is_rep) = {
                                    let vg = vote_generator_bp.lock().await;
                                    (vg.representative.clone(), vg.is_representative)
                                };
                                if is_rep {
                                    let our_weight =
                                        { rep_weights_bp.read().await.weight(&our_rep) };
                                    let vote =
                                        { vote_generator_bp.lock().await.generate_vote(accepted) };
                                    {
                                        let mut ae = active_elections_bp.write().await;
                                        let _ = ae.process_vote(
                                            &root, &our_rep, accepted, our_weight, false, now,
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
                                    if let Ok(bytes) = bincode::serialize(&wire) {
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

                let elapsed = start.elapsed().as_secs_f64() * 1000.0;
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
        let block_processor_ct = Arc::clone(&self.block_processor);
        let frontier_ct = Arc::clone(&self.frontier);
        let store_ct = Arc::clone(&self.store);
        let confirming_set_ct = Arc::clone(&self.confirming_set);
        let backlog_ct = Arc::clone(&self.backlog);
        let governance_ct = Arc::clone(&self.governance);
        let brn_engine_ct = Arc::clone(&self.brn_engine);
        let local_broadcaster_ct = Arc::clone(&self.local_broadcaster);

        let confirmation_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(500));
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_ct.recv() => {
                        tracing::info!("confirmation task shutting down");
                        break;
                    }
                    _ = interval.tick() => {
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
                                            vote_generator_ct.lock().await.generate_final_vote(winner)
                                        };
                                        // Count our own final weight locally.
                                        {
                                            let mut ae = active_elections_ct.write().await;
                                            let _ = ae.process_vote(
                                                &root, &our_rep, winner, our_weight, true, now,
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
                                        if let Ok(bytes) = bincode::serialize(&wire) {
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

                        for status in &confirmed {
                            let winner = status.winner;

                            // Mark as recently confirmed
                            {
                                let mut rc = recently_confirmed_ct.write().await;
                                rc.insert(winner);
                            }

                            // Add to confirming set for batched cementation
                            {
                                let mut cs = confirming_set_ct.lock().await;
                                if !cs.add(winner) {
                                    tracing::warn!(%winner, "confirming set full — backpressure");
                                }
                            }

                            // Remove from bounded backlog
                            {
                                let mut bl = backlog_ct.lock().await;
                                bl.remove(&winner);
                            }

                            // Remove from local broadcaster (stop re-broadcasting)
                            {
                                let mut lb = local_broadcaster_ct.lock().await;
                                lb.confirmed(&winner);
                            }

                            // Increment confirmed metric (only here, via consensus)
                            metrics_ct.blocks_confirmed.inc();

                            // Record confirmation latency
                            metrics_ct
                                .confirmation_latency_ms
                                .observe(status.election_duration_ms as f64);

                            // Publish WebSocket notification
                            ws_state_ct.publish_confirmation(
                                "",
                                &format!("{}", winner),
                                "0",
                            );

                            tracing::info!(
                                winner = %winner,
                                tally = status.tally,
                                duration_ms = status.election_duration_ms,
                                "block confirmed by consensus"
                            );

                            // TASK 3: Generate and broadcast a FINAL vote for the winner.
                            //
                            // TODO(consensus-port step 9): this emits the final
                            // vote AFTER confirmation, which is backwards. rsnano
                            // emits a final vote when an election reaches soft
                            // quorum (`Election::has_quorum`), and confirmation
                            // then requires a supermajority of those FINAL votes
                            // (now enforced in `Election::try_confirm`). Until the
                            // node vote-lifecycle is reworked to emit on
                            // has_quorum, fork elections won't self-resolve — but
                            // the non-fork hot path never creates an election, so
                            // live operation is unaffected. This post-confirm
                            // broadcast is now a harmless finalization echo.
                            {
                                let mut vg = vote_generator_ct.lock().await;
                                if vg.is_representative {
                                    let final_vote = vg.generate_final_vote(winner);
                                    let wire_msg = WireMessage::Vote(WireVote {
                                        voter: final_vote.voter,
                                        block_hashes: vec![final_vote.block_hash],
                                        is_final: true,
                                        timestamp: final_vote.timestamp,
                                        sequence: final_vote.sequence,
                                        signature: final_vote.signature,
                                    });
                                    if let Ok(bytes) = bincode::serialize(&wire_msg) {
                                        let peers: Vec<burst_network::PeerState> = {
                                            let pm = peer_manager_ct.read().await;
                                            pm.iter_connected()
                                                .map(|(_, s)| s.clone())
                                                .collect()
                                        };
                                        let _ = broadcaster_ct
                                            .broadcast_with_fanout(&bytes, &peers, 4)
                                            .await;
                                    }
                                }
                            }

                            // Process unchecked dependents that were waiting for this block
                            {
                                let mut bp = block_processor_ct.lock().await;
                                let deps = bp.process_unchecked(&winner);
                                if !deps.is_empty() {
                                    tracing::debug!(
                                        count = deps.len(),
                                        winner = %winner,
                                        "replayed unchecked dependents after confirmation"
                                    );
                                }
                            }

                            // Roll back losing fork blocks: look up the winner's
                            // account, check if the frontier disagrees, and if so
                            // roll back the loser so the winner can be cemented.
                            {
                                let block_store = store_ct.block_store();
                                if let Ok(winner_bytes) = block_store.get_block(&winner) {
                                    if let Ok(winner_block) = bincode::deserialize::<StateBlock>(&winner_bytes) {
                                        let frontier_read = frontier_ct.read().await;
                                        if let Some(&frontier_hash) = frontier_read.get_head(&winner_block.account) {
                                            if frontier_hash != winner {
                                                drop(frontier_read);
                                                if let Ok(loser_bytes) = block_store.get_block(&frontier_hash) {
                                                    if let Ok(loser_block) = bincode::deserialize::<StateBlock>(&loser_bytes) {
                                                        let mut bp = block_processor_ct.lock().await;
                                                        let mut frontier_write = frontier_ct.write().await;
                                                        let result = bp.rollback(&loser_block, &mut frontier_write);
                                                        if result == crate::block_processor::RollbackResult::Success {
                                                            if let Err(e) = block_store.delete_block(&frontier_hash) {
                                                                tracing::warn!(hash = %frontier_hash, "failed to delete rolled-back block: {e}");
                                                            }
                                                            tracing::info!(
                                                                account = %winner_block.account,
                                                                rolled_back = %frontier_hash,
                                                                winner = %winner,
                                                                "rolled back fork loser after confirmation"
                                                            );
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        // Cleanup confirmed elections from active set to free capacity.
                        if !confirmed.is_empty() {
                            let mut ae = active_elections_ct.write().await;
                            let cleaned = ae.cleanup_confirmed();
                            if !cleaned.is_empty() {
                                tracing::debug!(
                                    count = cleaned.len(),
                                    "cleaned up confirmed elections"
                                );
                            }
                        }

                        // Drain pending governance parameter changes and propagate
                        {
                            let mut gov = governance_ct.lock().await;
                            let changes = gov.drain_pending_changes();
                            if !changes.is_empty() {
                                let prop_now = Timestamp::new(unix_now_secs());
                                let mut brn = brn_engine_ct.lock().await;
                                for (param, value) in changes {
                                    match param {
                                        burst_governance::GovernableParam::BrnRate => {
                                            if let Err(e) = brn.apply_rate_change(value, prop_now) {
                                                tracing::warn!(error = %e, "failed to apply BRN rate change");
                                            } else {
                                                tracing::info!(
                                                    new_rate = value,
                                                    "propagated BRN rate change to all tracked wallets"
                                                );
                                            }
                                        }
                                        burst_governance::GovernableParam::TrstExpirySecs => {
                                            tracing::info!(
                                                new_expiry = value,
                                                "governance updated TRST expiry (applied at block processing)"
                                            );
                                            // TRST expiry is read from params at block processing time,
                                            // so no engine update needed — the governance store is the
                                            // source of truth.
                                        }
                                        other => {
                                            tracing::info!(
                                                param = ?other,
                                                value = value,
                                                "governance parameter changed"
                                            );
                                        }
                                    }
                                }
                            }
                        }

                        // Cleanup expired elections
                        let now = Timestamp::new(unix_now_secs());
                        {
                            let mut ae = active_elections_ct.write().await;
                            let expired = ae.cleanup_expired(30_000, now);
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
        let mut shutdown_rx_cement = self.shutdown.subscribe();

        let cementation_handle = tokio::spawn(async move {
            let processor = ConfirmationProcessor;
            let mut interval = tokio::time::interval(Duration::from_millis(100));
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_cement.recv() => {
                        tracing::info!("cementation task shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                        let batch = {
                            let mut cs = confirming_set_cement.lock().await;
                            cs.next_batch()
                        };
                        if !batch.is_empty() {
                            let mut total_cemented: u64 = 0;
                            let account_store = Arc::new(store_cement.account_store());
                            let block_store = Arc::new(store_cement.block_store());

                            for block_hash in &batch {
                                let mut walker = LmdbChainWalker::new(
                                    account_store.clone(),
                                    block_store.clone(),
                                );
                                match processor.process(block_hash, &mut walker) {
                                    (CementResult::Cemented { blocks_cemented, new_height }, _cemented_hashes) => {
                                        tracing::debug!(
                                            blocks = blocks_cemented,
                                            height = new_height,
                                            hash = %block_hash,
                                            "cemented blocks"
                                        );
                                        total_cemented += blocks_cemented;
                                    }
                                    (CementResult::AlreadyCemented, _) => {}
                                    (CementResult::BlockNotFound, _) => {
                                        tracing::warn!(hash = %block_hash, "block not found for cementation");
                                    }
                                    (CementResult::AccountNotFound, _) => {
                                        tracing::warn!(hash = %block_hash, "account not found for cementation");
                                    }
                                }
                            }

                            if total_cemented > 0 {
                                let cs = confirming_set_cement.lock().await;
                                cs.record_cemented(total_cemented);
                                tracing::debug!(count = total_cemented, "cemented block batch");
                            }
                        }

                        // Retry any deferred blocks
                        {
                            let mut cs = confirming_set_cement.lock().await;
                            cs.retry_deferred();
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

        let gov_tick_handle = tokio::spawn(async move {
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
                        let mut gov = governance_tick.lock().await;
                        let verified_count = store_gov
                            .account_store()
                            .verified_account_count()
                            .unwrap_or(0) as u32;
                        let del = delegation_gov.lock().await;
                        let activated = gov.tick(now, &mut gov_params, &del, verified_count);
                        drop(del);
                        if !activated.is_empty() {
                            // For each activated proposal, create a GovernanceActivation
                            // block on the genesis chain. The block processing loop
                            // applies the param change when it processes the block.
                            for proposal_hash in &activated {
                                let proposal_snapshot = gov.get_proposal(proposal_hash).cloned();
                                if let Some(proposal) = proposal_snapshot {
                                    let mut tentative_params = gov_params.clone();
                                    if gov.activate(&proposal, &mut tentative_params).is_ok() {
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

                                        let genesis_acct = store_gov.account_store().get_account(&genesis_addr);
                                        let (brn_bal, trst_bal) = match genesis_acct {
                                            Ok(acct) => (0u128, acct.trst_balance),
                                            Err(_) => (0, 0),
                                        };

                                        let mut block = StateBlock {
                                            version: CURRENT_BLOCK_VERSION,
                                            block_type: BlockType::GovernanceActivation,
                                            account: genesis_addr,
                                            previous,
                                            representative: WalletAddress::new("brst_genesis"),
                                            brn_balance: brn_bal,
                                            trst_balance: trst_bal,
                                            link: BlockHash::new(*proposal_hash.as_bytes()),
                                            origin: TxHash::ZERO,
                                            transaction: TxHash::new(*new_params_hash.as_bytes()),
                                            timestamp: now,
                                            params_hash: gov_params.params_hash(),
                                            merge_sources: Vec::new(),
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
                                        match generator.generate(&block.hash, threshold) {
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
                            // Drain pending changes/amendments so they don't accumulate
                            // (actual application happens in block processing loop).
                            let _ = gov.drain_pending_changes();
                            let _ = gov.drain_activated_amendments();
                        }

                        // Update adaptive quorum EMA with current participation data.
                        let total_verified = store_gov
                            .account_store()
                            .verified_account_count()
                            .unwrap_or(0) as u32;

                        if total_verified > 0 {
                            for hash in gov.active_proposal_hashes() {
                                if let Some(proposal) = gov.get_proposal(&hash) {
                                    let participation_bps = burst_governance::GovernanceEngine::compute_participation_bps(
                                        proposal.exploration_votes_yea,
                                        proposal.exploration_votes_nay,
                                        proposal.exploration_votes_abstain,
                                        total_verified,
                                    );
                                    if participation_bps > 0 {
                                        burst_governance::GovernanceEngine::update_ema(&mut gov_params, participation_bps);
                                    }
                                }
                            }
                        }

                        // Persist GovernanceEngine state to LMDB so in-flight
                        // proposals survive restarts.
                        if let Ok(bytes) = bincode::serialize(&*gov) {
                            let brn_store = store_gov.brn_store();
                            if let Err(e) = brn_store.put_meta(b"governance_engine", &bytes) {
                                tracing::trace!(error = %e, "failed to persist GovernanceEngine state");
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
        let orch_expiry = Arc::clone(&self.verification_orchestrator);
        let brn_engine_expiry = Arc::clone(&self.brn_engine);
        let challenge_duration_secs = self.config.params.challenge_duration_secs;
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

                        // 1. Flush expired tokens in the engine so cached
                        //    balances and account-facing state stay honest, and
                        //    surface the newly-expired amount into each account's
                        //    `expired_trst` counter. Per whitepaper §Expiry, the
                        //    total `trst_balance` is NOT reduced — it stays as a
                        //    permanent record ("virtue points"); only the
                        //    transferable portion (trst_balance − expired − revoked)
                        //    shrinks.
                        {
                            let newly_expired = {
                                let mut trst = trst_engine_expiry.lock().await;
                                trst.flush_all_expired_by_wallet(now)
                            };
                            for (holder, amount) in newly_expired {
                                if let Ok(mut acct) = store_expiry.account_store().get_account(&holder)
                                {
                                    // Accrue the holder's expired-TRST contribution
                                    // reputation. This feeds the hybrid weight's
                                    // (bounded) contribution multiplier — picked up
                                    // by the periodic rebuild in the online-weight
                                    // task, not patched here.
                                    acct.expired_trst = acct.expired_trst.saturating_add(amount);
                                    if let Err(e) = store_expiry.account_store().put_account(&acct) {
                                        tracing::warn!(%holder, "failed to persist expired_trst: {e}");
                                    }
                                }
                            }
                        }

                        // 2. Return expired pending TRST to senders (6.16a).
                        //    A send whose token expired before the receiver
                        //    claimed it auto-cancels back to the sender —
                        //    expired (non-transferable), counting toward the
                        //    sender's reputation, not the receiver's.
                        match store_expiry.pending_store().get_all_pending() {
                            Ok(all_pending) => {
                                let mut trst = trst_engine_expiry.lock().await;
                                let expiry_secs = trst.expiry_secs;
                                for (destination, send_hash, info) in all_pending {
                                    let expired = info
                                        .provenance
                                        .first()
                                        .map(|p| p.effective_origin_timestamp.has_expired(expiry_secs, now))
                                        .unwrap_or(false);
                                    if !expired {
                                        continue;
                                    }
                                    let returned = crate::ledger_bridge::create_returned_token(
                                        &destination,
                                        &send_hash,
                                        &info,
                                    );
                                    let sender = info.source.clone();
                                    let amount = info.amount;
                                    trst.receive_token(returned, now);
                                    if let Err(e) = store_expiry
                                        .pending_store()
                                        .delete_pending(&destination, &send_hash)
                                    {
                                        tracing::warn!(
                                            %destination,
                                            %send_hash,
                                            "failed to delete returned pending entry: {e}"
                                        );
                                    } else {
                                        tracing::info!(
                                            %sender,
                                            %destination,
                                            amount,
                                            %send_hash,
                                            "expired pending TRST returned to sender"
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!("failed to scan pending entries for expiry returns: {e}");
                            }
                        }

                        // 3. Expire challenges that ran past the governable
                        //    review period without collecting all votes
                        //    (challenge_duration_secs). Resolved in favor of
                        //    the challenged wallet: the challenger forfeits
                        //    half the stake, voters get their stakes back.
                        {
                            let mut orch = orch_expiry.lock().await;
                            let expired_events =
                                orch.cleanup_expired_challenges(now, challenge_duration_secs);
                            drop(orch);
                            for event in expired_events {
                                if let burst_verification::VerificationEvent::ChallengeResolved {
                                    ref wallet,
                                    ref outcome,
                                } = event
                                {
                                    tracing::info!(
                                        challenged = %wallet,
                                        challenger = %outcome.challenger,
                                        "challenge expired without full vote — resolved in favor of the challenged wallet"
                                    );
                                    let mut brn = brn_engine_expiry.lock().await;
                                    if let Some(ws) = brn.get_wallet_mut(&outcome.challenger) {
                                        // Half returned, half burned (time-wasting penalty).
                                        let penalty = outcome.challenger_stake / 2;
                                        ws.total_staked =
                                            ws.total_staked.saturating_sub(outcome.challenger_stake);
                                        ws.total_burned = ws.total_burned.saturating_add(penalty);
                                    }
                                    // Voters on the expired challenge get their
                                    // stakes back (no rewards — no dissenters).
                                    crate::ledger_bridge::resolve_verifier_outcomes(
                                        &mut brn,
                                        &store_expiry.pending_store(),
                                        &outcome.verifier_outcomes,
                                        wallet,
                                        now,
                                    );
                                }
                            }
                        }

                        // 4. Clean up expiry index entries (prevents index bloat).
                        let trst_idx = store_expiry.trst_index_store();
                        match trst_idx.get_expired_before(cutoff) {
                            Ok(expired) if !expired.is_empty() => {
                                tracing::info!(
                                    count = expired.len(),
                                    cutoff = now_secs,
                                    "found expired TRST tokens for index cleanup"
                                );
                                for tx_hash in &expired {
                                    if let Err(e) = trst_idx.delete_expiry_index(cutoff, tx_hash) {
                                        tracing::warn!(token = %tx_hash, "failed to clean up expiry index: {e}");
                                    }
                                }
                            }
                            Ok(_) => {} // no expired tokens
                            Err(e) => {
                                tracing::warn!("failed to query expired TRST indices: {e}");
                            }
                        }
                    }
                }
            }
        });
        self.task_handles.push(expiry_handle);

        // ── Pruning task — periodically removes expired/revoked TRST history ──
        let store_prune = Arc::clone(&self.store);
        let trst_engine_prune = Arc::clone(&self.trst_engine);
        let mut shutdown_rx_prune = self.shutdown.subscribe();
        let pruner = LedgerPruner::new(PruningConfig {
            enabled: true,
            max_expired_age_secs: 30 * 24 * 3600,
            prune_revoked: true,
            batch_size: 1000,
        });

        let prune_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300));
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_prune.recv() => {
                        tracing::info!("pruning task shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                        let now = Timestamp::new(unix_now_secs());
                        let trst_idx = store_prune.trst_index_store();
                        let cutoff = Timestamp::new(now.as_secs().saturating_sub(pruner.config().max_expired_age_secs));
                        let expired_hashes = trst_idx.get_expired_before(cutoff).unwrap_or_default();
                        let revoked_hashes: Vec<burst_types::TxHash> = {
                            let trst = trst_engine_prune.lock().await;
                            trst.merger_graph.revoked_origins().iter().cloned().collect()
                        };
                        let result = pruner.prune(&expired_hashes, &revoked_hashes, now);
                        if result.total_pruned > 0 {
                            let pruneable = pruner.find_pruneable(&expired_hashes, &revoked_hashes, now);
                            for hash in &pruneable {
                                let _ = trst_idx.delete_token(hash);
                            }
                            tracing::info!(
                                expired = result.expired_pruned,
                                revoked = result.revoked_pruned,
                                total = result.total_pruned,
                                "pruned TRST entries"
                            );
                        }
                    }
                }
            }
        });
        self.task_handles.push(prune_handle);

        // ── Unchecked map cleanup — evict entries older than 4 hours ─────
        let block_processor_uc = Arc::clone(&self.block_processor);
        let mut shutdown_rx_uc = self.shutdown.subscribe();
        const UNCHECKED_MAX_AGE_SECS: u64 = 4 * 3600;

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
                        let removed = bp.cleanup_unchecked(UNCHECKED_MAX_AGE_SECS, now);
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
        let mut shutdown_rx_sched = self.shutdown.subscribe();

        let scheduler_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(100));
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_sched.recv() => {
                        tracing::debug!("priority scheduler drain task shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                        let mut sched = priority_scheduler_drain.lock().await;
                        let ae = active_elections_sched.read().await;
                        let capacity = MAX_ACTIVE_ELECTIONS.saturating_sub(ae.election_count());
                        drop(ae);

                        let mut started = 0usize;
                        while started < capacity {
                            match sched.pop() {
                                Some((hash, _account)) => {
                                    let now = Timestamp::new(unix_now_secs());
                                    let mut ae = active_elections_sched.write().await;
                                    if ae.start_election(hash, now).is_ok() {
                                        started += 1;
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
                        // Refresh the hybrid rep-weight cache from the ledger so
                        // newly-verified humans, delegation changes, and freshly
                        // expired TRST are reflected in consensus weight. Cheap at
                        // launch scale; the genesis bootstrap bonus is preserved.
                        rebuild_rep_weights(&store_ow, &rep_weights_bg).await;
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

                        // Update elections with effective weight (max of current, trended, floor)
                        // to prevent quorum collapse on temporary online weight dips.
                        {
                            let mut ae = active_elections_ow.write().await;
                            ae.set_online_weight(effective);
                        }

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

        // Auto-verify the genesis creator so it can endorse during bootstrap
        {
            use burst_store::account::AccountInfo;
            let genesis_addr = genesis_key::genesis_address(self.config.network);
            let acct_store = self.store.account_store();
            let existing = acct_store.get_account(&genesis_addr).ok();

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
                .unwrap_or(BlockHash::ZERO);

            // Write the record when genesis is unverified OR when a stale record
            // still has head==ZERO (the pre-fix bug) while a genesis block
            // exists — repair it in place, preserving any real chain counters.
            let needs_write = match &existing {
                None => true,
                Some(a) => {
                    a.state != burst_types::WalletState::Verified
                        || (a.head.is_zero() && !genesis_head.is_zero())
                }
            };
            if needs_write {
                let base = existing.unwrap_or(AccountInfo {
                    address: genesis_addr.clone(),
                    state: burst_types::WalletState::Verified,
                    verified_at: Some(Timestamp::new(0)),
                    head: genesis_head,
                    block_count: 1,
                    confirmation_height: 0,
                    representative: genesis_addr.clone(),
                    total_brn_burned: 0,
                    total_brn_staked: 0,
                    trst_balance: 0,
                    expired_trst: 0,
                    revoked_trst: 0,
                    epoch: 0,
                    verifier_opted_in_at: None,
                    orv_evicted: false,
                });
                let is_new = base.head.is_zero();
                let info = AccountInfo {
                    state: burst_types::WalletState::Verified,
                    verified_at: base.verified_at.or(Some(Timestamp::new(0))),
                    head: if base.head.is_zero() {
                        genesis_head
                    } else {
                        base.head
                    },
                    block_count: base.block_count.max(1),
                    ..base
                };
                if let Err(e) = acct_store.put_account(&info) {
                    tracing::error!("failed to auto-verify genesis creator: {e}");
                } else {
                    if is_new {
                        self.ledger_cache.inc_account_count();
                    }
                    tracing::info!(%genesis_addr, head = %info.head, "genesis creator auto-verified for bootstrap");
                }
            }

            // Track the genesis account in the BRN engine (accruing from epoch
            // 0, matching its verified_at) so it has BRN to burn when authoring
            // bootstrap endorsements — otherwise the endorse burn and the
            // computed-BRN spend check would reject it.
            {
                let mut brn = self.brn_engine.lock().await;
                if brn.get_wallet(&genesis_addr).is_none() {
                    brn.track_wallet(
                        genesis_addr.clone(),
                        burst_brn::BrnWalletState::new(Timestamp::new(0)),
                    );
                }
            }
        }

        // Re-load frontier after genesis init (in case we just created it)
        {
            let new_frontier = Self::load_frontier_from_store(&self.store)?;
            let mut f = self.frontier.write().await;
            *f = new_frontier;
        }

        // Restore the merger graph from LMDB if a previous snapshot exists.
        {
            let meta = self.store.meta_store();
            match meta.get_meta(MERGER_GRAPH_META_KEY) {
                Ok(bytes) => match burst_trst::MergerGraph::from_bytes(&bytes) {
                    Ok(graph) => {
                        let mut trst = self.trst_engine.lock().await;
                        trst.merger_graph = graph;
                        tracing::info!("restored merger graph from LMDB");
                    }
                    Err(e) => {
                        tracing::warn!("failed to deserialize merger graph, starting fresh: {e}");
                    }
                },
                Err(_) => {
                    tracing::info!("no persisted merger graph found — starting fresh");
                }
            }
        }

        // Restore TRST engine per-wallet token portfolios from LMDB.
        // Runs after the merger graph restore: rebuild_indexes needs the graph
        // to classify merged origins (wallet_origins must only hold burn txs).
        {
            let meta = self.store.meta_store();
            match meta.get_meta(TrstEngine::meta_key()) {
                Ok(bytes) => {
                    let mut trst = self.trst_engine.lock().await;
                    let restored =
                        TrstEngine::load_wallets(&bytes, self.config.params.trst_expiry_secs);
                    trst.wallets = restored.wallets;
                    trst.rebuild_indexes();
                    tracing::info!("TRST engine wallet portfolios restored from LMDB");
                }
                Err(_) => {
                    tracing::info!("no persisted TRST wallet portfolios — starting fresh");
                }
            }
        }

        // Restore delegation engine state from LMDB.
        {
            let meta = self.store.meta_store();
            match meta.get_meta(DelegationEngine::meta_key()) {
                Ok(bytes) => {
                    let restored = DelegationEngine::load_state(&bytes);
                    let mut de = self.delegation_engine.lock().await;
                    *de = restored;
                    tracing::info!("delegation engine state restored from LMDB");
                }
                Err(_) => {
                    tracing::info!("no persisted delegation engine state — starting fresh");
                }
            }
        }

        // Restore verification orchestrator state from LMDB.
        {
            let meta = self.store.meta_store();
            match meta.get_meta(VERIFICATION_ORCHESTRATOR_META_KEY) {
                Ok(bytes) => {
                    match bincode::deserialize::<burst_verification::OrchestratorSnapshot>(&bytes) {
                        Ok(snapshot) => {
                            let restored =
                                burst_verification::VerificationOrchestrator::restore(snapshot);
                            let mut vo = self.verification_orchestrator.lock().await;
                            *vo = restored;
                            tracing::info!("verification orchestrator state restored from LMDB");
                        }
                        Err(e) => {
                            tracing::warn!(
                                "failed to deserialize orchestrator snapshot, starting fresh: {e}"
                            );
                        }
                    }
                }
                Err(_) => {
                    tracing::info!("no persisted verification orchestrator state — starting fresh");
                }
            }
        }

        // Rebuild representative weights from the confirmed account set on every
        // startup (and periodically thereafter — see the online-weight task).
        // Weight is the HYBRID: one vote per VERIFIED human delegating to a rep,
        // amplified by that rep's expired-TRST contribution up to a cap (see
        // RepWeightCache docs). It is a pure function of account state, so we
        // rebuild it rather than persist a snapshot: a snapshot would need
        // migration off the old numbers and, worse, used to be persisted *with*
        // the genesis bootstrap baked in, so reloading it and re-seeding the
        // bootstrap double-counted genesis per restart — diverging nodes by
        // restart count. Rebuild + seed-bootstrap-once is deterministic.
        rebuild_rep_weights(&self.store, &self.rep_weights).await;

        // Give the GENESIS representative a bootstrap voting-weight bridge — a
        // raw additive weight (NOT spendable TRST, so "no premine" holds), the
        // analog of Nano's genesis premine. At launch few humans run voting
        // nodes, so without this the one online genesis node couldn't clear the
        // adaptive online-weight floor. Seeded for the WELL-KNOWN genesis account
        // on EVERY node (deterministic), so all nodes agree on genesis's weight
        // and its votes aggregate. `set_bonus` is preserved across the periodic
        // rebuilds. As real verified humans delegate to online reps, this fixed
        // bridge becomes a shrinking fraction and consensus decentralises.
        {
            let genesis_rep = genesis_key::genesis_address(self.config.network);
            let mut rw = self.rep_weights.write().await;
            rw.set_bonus(&genesis_rep, GENESIS_BOOTSTRAP_WEIGHT);
            tracing::info!(
                representative = %genesis_rep,
                weight = GENESIS_BOOTSTRAP_WEIGHT,
                "seeded genesis bootstrap voting weight (consistent across nodes)"
            );
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

        // ── P2P TCP listener ──────────────────────────────────────────────
        let p2p_port = self.config.port;
        let peer_manager = Arc::clone(&self.peer_manager);
        let mut shutdown_rx_p2p = self.shutdown.subscribe();
        let metrics_p2p = Arc::clone(&self.metrics);
        let conn_registry_p2p = Arc::clone(&self.connection_registry);
        let block_queue_p2p = Arc::clone(&self.block_queue);
        let active_elections_p2p = Arc::clone(&self.active_elections);
        let rep_weights_p2p = Arc::clone(&self.rep_weights);
        let syn_cookies_p2p = Arc::clone(&self.syn_cookies);
        let message_dedup_p2p = Arc::clone(&self.message_dedup);
        let online_weight_sampler_p2p = Arc::clone(&self.online_weight_sampler);
        let store_p2p = Arc::clone(&self.store);
        let node_address_p2p = self.node_address.clone();
        let config_params_p2p = self.config.params.clone();
        let network_p2p = self.config.network;
        let peering_port_p2p = self.config.port;
        let bootstrap_feedback_p2p = bootstrap_feedback_tx.clone();

        let p2p_handle = tokio::spawn(async move {
            let listener = match tokio::net::TcpListener::bind(format!("0.0.0.0:{p2p_port}")).await
            {
                Ok(l) => {
                    tracing::info!(port = p2p_port, "P2P listener started");
                    l
                }
                Err(e) => {
                    tracing::error!("failed to bind P2P listener on port {p2p_port}: {e}");
                    return;
                }
            };

            let params_hash_p2p = config_params_p2p.params_hash();

            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_p2p.recv() => {
                        tracing::info!("P2P listener shutting down");
                        break;
                    }
                    result = listener.accept() => {
                        match result {
                            Ok((stream, addr)) => {
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
                                let message_dedup_c = Arc::clone(&message_dedup_p2p);
                                let online_weight_sampler_c = Arc::clone(&online_weight_sampler_p2p);
                                let store_c = Arc::clone(&store_p2p);
                                let our_node_id = node_address_p2p.clone();
                                let params_hash_c = params_hash_p2p;
                                let network_c = network_p2p;
                                let peering_port_c = peering_port_p2p;
                                let bootstrap_feedback_c = bootstrap_feedback_p2p.clone();

                                tokio::spawn(async move {
                                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                                    let now_secs = unix_now_secs();
                                    let peer_ip = addr.ip().to_string();
                                    let peer_addr = PeerAddress { ip: peer_ip.clone(), port: addr.port() };
                                    let peer_id = format!("{}:{}", addr.ip(), addr.port());

                                    // Generate + send the SYN cookie challenge.
                                    // Bind in a separate `let` so the mutex guard drops
                                    // before the match (a match scrutinee's temporaries
                                    // otherwise live until the whole match ends).
                                    let cookie_opt = syn_cookies_c.lock().await.generate(&peer_ip);
                                    let cookie = match cookie_opt {
                                        Some(c) => c,
                                        None => return,
                                    };
                                    let (mut read_half, mut write_half) = stream.into_split();
                                    let challenge = WireMessage::Handshake(crate::wire_message::HandshakeMsg {
                                        node_id: our_node_id.clone(),
                                        cookie: Some(cookie),
                                        cookie_signature: None,
                                        params_hash: params_hash_c,
                                        network_id: network_c,
                                        peering_port: peering_port_c,
                                    });
                                    match bincode::serialize(&challenge) {
                                        Ok(bytes) => {
                                            let len_bytes = (bytes.len() as u32).to_be_bytes();
                                            if write_half.write_all(&len_bytes).await.is_err()
                                                || write_half.write_all(&bytes).await.is_err()
                                            {
                                                return;
                                            }
                                            let _ = write_half.flush().await;
                                        }
                                        Err(_) => return,
                                    }

                                    // Read + verify the peer's signed cookie response so we
                                    // learn their node_id BEFORE registering — reading directly
                                    // off read_half leaves no buffered bytes for the read loop.
                                    let mut len_buf = [0u8; 4];
                                    match tokio::time::timeout(Duration::from_secs(10), read_half.read_exact(&mut len_buf)).await {
                                        Ok(Ok(_)) => {}
                                        _ => return,
                                    }
                                    let body_len = u32::from_be_bytes(len_buf) as usize;
                                    if body_len == 0 || body_len > 65536 { return; }
                                    let mut body = vec![0u8; body_len];
                                    match tokio::time::timeout(Duration::from_secs(10), read_half.read_exact(&mut body)).await {
                                        Ok(Ok(_)) => {}
                                        _ => return,
                                    }
                                    let (remote_node_id, remote_peering_port) = match bincode::deserialize::<WireMessage>(&body) {
                                        Ok(WireMessage::Handshake(hs)) => {
                                            // Reject peers on a different network before verifying
                                            // the cookie — hard Dev/Test/Live isolation.
                                            if hs.network_id != network_c {
                                                tracing::debug!(
                                                    peer = %peer_id,
                                                    ours = ?network_c,
                                                    theirs = ?hs.network_id,
                                                    "dropping inbound peer on different network"
                                                );
                                                return;
                                            }
                                            match &hs.cookie_signature {
                                                Some(sig) => {
                                                    let ok = { syn_cookies_c.lock().await.verify(&peer_ip, &hs.node_id, sig) };
                                                    if !ok {
                                                        tracing::warn!(peer = %peer_id, "SYN cookie verification failed");
                                                        return;
                                                    }
                                                    (hs.node_id, hs.peering_port)
                                                }
                                                None => return,
                                            }
                                        }
                                        _ => return,
                                    };

                                    // Re-key this peer by its *dialable* listening address
                                    // (ip:peering_port) instead of the ephemeral TCP source
                                    // port. This makes an inbound connection collapse onto the
                                    // outbound connection to the same node (one peer_manager
                                    // entry, correct count) and ensures gossip only advertises
                                    // dialable addresses. Fall back to the ephemeral address if
                                    // the peer didn't advertise a port (0).
                                    let (peer_addr, peer_id) = if remote_peering_port != 0 {
                                        (
                                            PeerAddress { ip: peer_ip.clone(), port: remote_peering_port },
                                            format!("{}:{}", peer_ip, remote_peering_port),
                                        )
                                    } else {
                                        (peer_addr, peer_id)
                                    };

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
                                        let mut registry = conn_registry_c.write().await;
                                        match registry.register_dedup(
                                            peer_id.clone(),
                                            peer_nid,
                                            crate::connection_registry::Direction::Inbound,
                                            write_half,
                                            &our_nid,
                                        ) {
                                            Some(id) => {
                                                metrics_c.peer_count.set(registry.len() as i64);
                                                id
                                            }
                                            None => {
                                                tracing::debug!(peer = %peer_id, "inbound superseded by our outbound (tie-break)");
                                                return;
                                            }
                                        }
                                    };

                                    {
                                        let mut pm = peer_manager_c.write().await;
                                        pm.add_peer(peer_addr);
                                        pm.mark_connected(&peer_id, now_secs);
                                    }

                                    // Handshake already validated here — read loop needs no
                                    // SYN cookie step (syn_cookies = None).
                                    spawn_peer_read_loop(
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
                                        None,
                                        peer_ip,
                                        store_c,
                                        params_hash_c,
                                        bootstrap_feedback_c,
                                    );

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
        });
        self.task_handles.push(p2p_handle);

        // ── UPnP port mapping (NAT traversal) ────────────────────────────
        let is_dev_network = matches!(self.config.network, burst_types::NetworkId::Dev);
        if self.config.enable_upnp && !is_dev_network {
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
                                    // Don't clear — preserve advertise_address from config
                                    // (cloud VPSes have no UPnP IGD).
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

        if !bootstrap_peers.is_empty() {
            let bs_ctx = crate::peer_connector::PeerConnectorContext {
                peer_manager: Arc::clone(&self.peer_manager),
                connection_registry: Arc::clone(&self.connection_registry),
                block_queue: Arc::clone(&self.block_queue),
                metrics: Arc::clone(&self.metrics),
                active_elections: Arc::clone(&self.active_elections),
                rep_weights: Arc::clone(&self.rep_weights),
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
                let mut interval = tokio::time::interval(Duration::from_secs(2));

                loop {
                    tokio::select! {
                        biased;
                        _ = shutdown_rx_bs.recv() => {
                            tracing::debug!("bootstrap task shutting down");
                            break;
                        }
                        Some(fb) = bs_feedback_rx.recv() => match fb {
                            BootstrapFeedback::Blocks { id, last, full_batch } => {
                                bootstrapper.on_blocks_ack(id, last, full_batch);
                            }
                            BootstrapFeedback::Frontiers(entries) => {
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
                            bootstrapper.tick_timeouts(now);

                            // Pull from any connected peer.
                            let peer_id = {
                                let pm = peer_manager_bs.read().await;
                                let first = pm.iter_connected().next().map(|(id, _)| id.clone());
                                first
                            };
                            if let Some(peer_id) = peer_id {
                                // Periodic frontier-scan discovery (~every 30s).
                                if ticks % 15 == 1 {
                                    let msg = WireMessage::Bootstrap(
                                        BootstrapMessage::AscPullReq {
                                            id: 0,
                                            payload: AscPullReqPayload::Frontiers {
                                                start_account: genesis_addr_bs.clone(),
                                                count: ASC_PULL_MAX_FRONTIERS,
                                            },
                                        },
                                    );
                                    if let Ok(bytes) = bincode::serialize(&msg) {
                                        conn_registry_bs.read().await.send(&peer_id, bytes);
                                    }
                                }
                                // Send queued block pulls.
                                for (id, payload) in bootstrapper.next_requests(now) {
                                    let msg = WireMessage::Bootstrap(
                                        BootstrapMessage::AscPullReq { id, payload },
                                    );
                                    if let Ok(bytes) = bincode::serialize(&msg) {
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
        // Gated OFF by default: enabling it triggers simultaneous-connect
        // churn between mutually-dialing public nodes (connection dedup has no
        // deterministic initiator tie-break, and NAT reachability isn't
        // tracked). Enable once those land; until then use bootstrap_peers.
        if self.config.enable_peer_reachout {
            let target_out = self.config.max_peers.clamp(3, 8);
            let reach_ctx = crate::peer_connector::PeerConnectorContext {
                peer_manager: Arc::clone(&self.peer_manager),
                connection_registry: Arc::clone(&self.connection_registry),
                block_queue: Arc::clone(&self.block_queue),
                metrics: Arc::clone(&self.metrics),
                active_elections: Arc::clone(&self.active_elections),
                rep_weights: Arc::clone(&self.rep_weights),
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
        // Alternates between two flood types each period (matching rsnano):
        //   - "self" keepalive: includes own external address in slot 0, sent to ~25% of peers
        //   - "random" keepalive: all 8 slots are random connected peers, sent to ~75%
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
                                .random_known_peers(8)
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

                            let self_bytes = bincode::serialize(&self_msg).ok();
                            let random_bytes = bincode::serialize(&random_msg).ok();

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
                        let bytes = match bincode::serialize(&req) {
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
                rep_weight_cache: self.rep_weights.clone(),
                work_generator: Arc::new(WorkGenerator),
                params: Arc::new(self.config.params.clone()),
                block_processor: Arc::new(NodeBlockProcessor {
                    block_queue: Arc::clone(&self.block_queue),
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

            let rpc_handle = tokio::spawn(async move {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_rpc.recv() => {
                        tracing::info!("RPC server shutting down");
                    }
                    result = rpc_server.start() => {
                        match result {
                            Ok(()) => tracing::info!("RPC server exited"),
                            Err(e) => tracing::error!("RPC server error: {e}"),
                        }
                    }
                }
            });
            self.task_handles.push(rpc_handle);
        }

        // ── WebSocket server (optional) ───────────────────────────────────
        if self.config.enable_websocket {
            let ws_port = self.config.websocket_port;
            let ws_state_clone = Arc::clone(&self.ws_state);
            let ws_server = WebSocketServer::with_state(ws_port, ws_state_clone);
            let mut shutdown_rx_ws = self.shutdown.subscribe();

            let ws_handle = tokio::spawn(async move {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_ws.recv() => {
                        tracing::info!("WebSocket server shutting down");
                    }
                    result = ws_server.start() => {
                        match result {
                            Ok(()) => tracing::info!("WebSocket server exited"),
                            Err(e) => tracing::error!("WebSocket server error: {e}"),
                        }
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
    /// 3. Flushes pending writes to LMDB.
    /// 4. Waits for background tasks to complete (with timeout).
    pub async fn stop(&mut self) -> Result<(), NodeError> {
        tracing::info!("BURST node stopping");

        // Signal all tasks
        self.shutdown.shutdown();

        // Remove UPnP port mapping (be a good citizen for the router)
        if let Some(ref mut mapper) = self.port_mapper {
            tracing::info!("UPnP: removing port mapping");
            mapper.stop().await;
        }

        // Drop all TCP write halves (causes peer read loops to terminate)
        {
            let mut registry = self.connection_registry.write().await;
            *registry = ConnectionRegistry::new();
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

        // Persist BRN engine state to LMDB.
        {
            let brn = self.brn_engine.lock().await;
            let brn_store = self.store.brn_store();
            if let Err(e) = brn.save_to_store(&brn_store) {
                tracing::error!(error = %e, "failed to persist BRN engine state");
            } else {
                tracing::info!(
                    wallets = brn.wallets.len(),
                    "BRN engine state persisted to LMDB"
                );
            }
        }

        // Persist the merger graph to LMDB before flushing.
        {
            let trst = self.trst_engine.lock().await;
            let bytes = trst.merger_graph.to_bytes();
            let meta = self.store.meta_store();
            if let Err(e) = meta.put_meta(MERGER_GRAPH_META_KEY, &bytes) {
                tracing::warn!("failed to persist merger graph: {e}");
            } else {
                tracing::info!(bytes = bytes.len(), "merger graph persisted to LMDB");
            }
        }

        // Persist TRST engine per-wallet token portfolios to LMDB.
        {
            let trst = self.trst_engine.lock().await;
            let bytes = trst.save_wallets();
            let meta = self.store.meta_store();
            if let Err(e) = meta.put_meta(TrstEngine::meta_key(), &bytes) {
                tracing::warn!("failed to persist TRST wallet portfolios: {e}");
            } else {
                tracing::info!(
                    bytes = bytes.len(),
                    "TRST wallet portfolios persisted to LMDB"
                );
            }
        }

        // Persist delegation engine state to LMDB.
        {
            let de = self.delegation_engine.lock().await;
            let bytes = de.save_state();
            let meta = self.store.meta_store();
            if let Err(e) = meta.put_meta(DelegationEngine::meta_key(), &bytes) {
                tracing::warn!("failed to persist delegation engine state: {e}");
            } else {
                tracing::info!(
                    bytes = bytes.len(),
                    "delegation engine state persisted to LMDB"
                );
            }
        }

        // Persist verification orchestrator state to LMDB.
        {
            let vo = self.verification_orchestrator.lock().await;
            let snapshot = vo.snapshot();
            match bincode::serialize(&snapshot) {
                Ok(bytes) => {
                    let meta = self.store.meta_store();
                    if let Err(e) = meta.put_meta(VERIFICATION_ORCHESTRATOR_META_KEY, &bytes) {
                        tracing::warn!("failed to persist verification orchestrator: {e}");
                    } else {
                        tracing::info!(
                            bytes = bytes.len(),
                            "verification orchestrator persisted to LMDB"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!("failed to serialize verification orchestrator: {e}");
                }
            }
        }

        // NB: representative weights are intentionally NOT persisted here. They
        // are a *derived* view of the account set (Σ expired TRST) plus the fixed
        // genesis bootstrap weight, and are rebuilt deterministically from the
        // ledger on every startup. Persisting them risked reloading a stale
        // snapshot and double-counting the bootstrap weight across restarts.

        // Flush LMDB
        if let Err(e) = self.store.force_sync() {
            tracing::warn!("LMDB force_sync failed: {e}");
        } else {
            tracing::info!("LMDB flushed to disk");
        }

        // Wait for all spawned tasks with a timeout
        let handles: Vec<JoinHandle<()>> = std::mem::take(&mut self.task_handles);
        let wait_all = async {
            for handle in handles {
                let _ = handle.await;
            }
        };

        if tokio::time::timeout(SHUTDOWN_TIMEOUT, wait_all)
            .await
            .is_err()
        {
            tracing::warn!(
                "shutdown timeout ({:?}) — some tasks may still be running",
                SHUTDOWN_TIMEOUT
            );
        }

        // Final metrics snapshot
        self.refresh_metrics().await;

        tracing::info!("BURST node stopped");
        Ok(())
    }

    /// Process an incoming block through the full pipeline (synchronous path).
    ///
    /// Runs the block through the multi-stage block processor, writes accepted
    /// blocks to LMDB, updates the frontier, and processes any unchecked
    /// dependents that become unblocked (both gap-previous and gap-source).
    pub fn process_block(&self, block: &StateBlock) -> Result<ProcessResult, NodeError> {
        // We can't hold the async locks from sync code, so use try_lock.
        let mut processor = self
            .block_processor
            .try_lock()
            .map_err(|_| NodeError::Other("block processor is busy".into()))?;

        // We need a mutable reference to frontier. try_write for RwLock.
        // Since this is sync context and the lock is tokio, we use
        // blocking_write (available in tokio).
        let mut frontier = self
            .frontier
            .try_write()
            .map_err(|_| NodeError::Other("frontier is locked".into()))?;

        // Pre-validate balance transition against previous block in the store.
        if !block.previous.is_zero() {
            if let Ok(prev_bytes) = self.store.block_store().get_block(&block.previous) {
                if let Ok(prev_block) = bincode::deserialize::<StateBlock>(&prev_bytes) {
                    if let Err(reason) = BlockProcessor::validate_balance_transition(
                        block,
                        prev_block.brn_balance,
                        prev_block.trst_balance,
                    ) {
                        return Ok(ProcessResult::Rejected(reason));
                    }
                }
            }
        }

        let result = processor.process(block, &mut frontier);

        if result == ProcessResult::Accepted {
            // Process gap-previous dependents
            let dependents = processor.process_unchecked(&block.hash);
            for dep_block in &dependents {
                let dep_result = processor.process(dep_block, &mut frontier);
                tracing::debug!(
                    hash = %dep_block.hash,
                    ?dep_result,
                    "re-processed unchecked block (gap-previous)"
                );
            }

            // Process gap-source dependents — blocks waiting on this block as their
            // linked send block
            let source_deps = processor.process_unchecked_source(&block.hash);
            for dep_block in &source_deps {
                let dep_result = processor.process(dep_block, &mut frontier);
                tracing::debug!(
                    hash = %dep_block.hash,
                    ?dep_result,
                    "re-processed unchecked block (gap-source)"
                );
            }
        }

        Ok(result)
    }

    /// Roll back a block from the frontier after fork resolution.
    ///
    /// When the confirmation task determines a winner in a fork, the losing
    /// block must be rolled back so the winning block can be applied.
    pub fn rollback_block(
        &self,
        block: &StateBlock,
    ) -> Result<crate::block_processor::RollbackResult, NodeError> {
        let mut processor = self
            .block_processor
            .try_lock()
            .map_err(|_| NodeError::Other("block processor is busy".into()))?;

        let mut frontier = self
            .frontier
            .try_write()
            .map_err(|_| NodeError::Other("frontier is locked".into()))?;

        let result = processor.rollback(block, &mut frontier);

        if result == crate::block_processor::RollbackResult::Success {
            // Also remove the block from LMDB so stale data cannot be read.
            let block_store = self.store.block_store();
            if let Err(e) = block_store.delete_block(&block.hash) {
                tracing::warn!(
                    hash = %block.hash,
                    "failed to delete rolled-back block from store: {e}"
                );
            }

            tracing::info!(
                hash = %block.hash,
                account = %block.account,
                "block rolled back from frontier and store"
            );
        }

        Ok(result)
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
        if let Ok(msg_bytes) = bincode::serialize(&block) {
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
            let available = brn.compute_balance(state, now);
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
                let new_brn = brn_balance.saturating_add(burn.amount);
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
                    let mut trst = self.trst_engine.lock().await;
                    if let Some(transferable) = trst.transferable_balance(&sender, now) {
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
                    brn_balance.saturating_add(endorse.burn_amount),
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
                    brn_balance.saturating_add(challenge.stake_amount),
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
                    brn_balance.saturating_add(vote.stake_amount),
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
            version: CURRENT_BLOCK_VERSION,
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

        let bp = self.block_processor.lock().await;
        self.metrics
            .unchecked_count
            .set(bp.unchecked_count() as i64);

        let pm = self.peer_manager.read().await;
        self.metrics.peer_count.set(pm.connected_count() as i64);

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

/// Deterministic expiry-grouped auto-merge selection (whitepaper §Merging:
/// Derive the eligible verifier pool from CONFIRMED account state: every wallet
/// that (a) is Verified, (b) has opted in to the verifier pool
/// (`verifier_opted_in_at`), and (c) has been verified for at least
/// `min_verification_age_secs`. Because it is computed purely from committed
/// account state that every node agrees on, all nodes derive the SAME pool — a
/// prerequisite for deterministic VRF verifier selection. Excluded/penalized
/// filtering is applied afterwards by the orchestrator.
fn eligible_verifiers(
    store: &LmdbStore,
    params: &burst_types::ProtocolParams,
    now_secs: u64,
) -> Vec<burst_types::WalletAddress> {
    let accounts = match store.account_store().iter_verified_accounts() {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("failed to list verified accounts for verifier pool: {e}");
            return Vec::new();
        }
    };
    accounts
        .into_iter()
        .filter(|a| {
            let opted_in = a.verifier_opted_in_at.is_some();
            let old_enough = a.verified_at.is_some_and(|v| {
                now_secs.saturating_sub(v.as_secs()) >= params.min_verification_age_secs
            });
            opted_in && old_enough
        })
        .map(|a| a.address)
        .collect()
}

/// Rebuild the hybrid rep-weight cache from the confirmed account set: one vote
/// per VERIFIED human delegating to a rep, plus that rep's expired-TRST
/// contribution. Preserves the genesis bootstrap bonus (set via `set_bonus`).
/// Called at startup and periodically by the online-weight task — weight is a
/// pure function of account state, so it is derived, never patched per event.
async fn rebuild_rep_weights(store: &LmdbStore, rep_weights: &RwLock<RepWeightCache>) {
    match store.account_store().iter_accounts() {
        Ok(accounts) => {
            // Reps evicted from the ORV set by governance contribute zero weight.
            let evicted: std::collections::HashSet<burst_types::WalletAddress> = accounts
                .iter()
                .filter(|a| a.orv_evicted)
                .map(|a| a.address.clone())
                .collect();
            let mut rw = rep_weights.write().await;
            rw.rebuild_from_accounts(accounts.into_iter().map(|a| {
                (
                    a.state == burst_types::WalletState::Verified,
                    a.representative.clone(),
                    a.expired_trst,
                )
            }));
            rw.set_evicted(evicted);
            tracing::debug!(
                reps = rw.rep_count(),
                "rep weights rebuilt from account store"
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to rebuild rep weights from account store");
        }
    }
}

/// Apply per-holder TRST revocation to the ledger. `revoke_by_origin` returns
/// one [`burst_trst::RevocationEvent`] per affected live token, keyed by the
/// CURRENT holder (which — via the merger graph — may be any downstream account,
/// not just the fraudulent originator). This debits each affected holder's
/// `AccountInfo` (transferable down, `revoked_trst` up). It deliberately does
/// NOT touch consensus (rep) weight: ORV weight is EXPIRED TRST, a separate
/// axis from spendable balance, and the remedy for a bad-acting ORV participant
/// is governance eviction from the verified-node set, not value-layer
/// revocation. Groups by holder so each account is written once. Returns the
/// total revoked across all holders.
fn apply_revocations_to_holders(
    store: &LmdbStore,
    revocations: &[burst_trst::RevocationEvent],
) -> u128 {
    use std::collections::HashMap;
    let mut per_holder: HashMap<burst_types::WalletAddress, u128> = HashMap::new();
    for ev in revocations {
        let e = per_holder.entry(ev.holder.clone()).or_insert(0);
        *e = e.saturating_add(ev.revoked_amount);
    }
    let mut total = 0u128;
    for (holder, amount) in per_holder {
        total = total.saturating_add(amount);
        if let Ok(mut acct) = store.account_store().get_account(&holder) {
            acct.trst_balance = acct.trst_balance.saturating_sub(amount);
            acct.revoked_trst = acct.revoked_trst.saturating_add(amount);
            if let Err(e) = store.account_store().put_account(&acct) {
                tracing::error!(%holder, "failed to persist revocation to holder: {e}");
            }
        }
    }
    total
}

/// Inverse of [`apply_revocations_to_holders`]: restore un-revoked TRST to each
/// current holder (transferable up, `revoked_trst` down). Used when a revoked
/// originator is re-verified (decision §6.15b). Like revocation it does not
/// touch rep weight — any restored tokens that are past expiry re-accrue the
/// holder's expired-TRST consensus stake through the normal expiry flush.
/// Returns the total restored.
fn apply_unrevocations_to_holders(
    store: &LmdbStore,
    restored: &[burst_trst::UnRevocationResult],
) -> u128 {
    use std::collections::HashMap;
    let mut per_holder: HashMap<burst_types::WalletAddress, u128> = HashMap::new();
    for r in restored {
        let e = per_holder.entry(r.holder.clone()).or_insert(0);
        *e = e.saturating_add(r.amount);
    }
    let mut total = 0u128;
    for (holder, amount) in per_holder {
        total = total.saturating_add(amount);
        if let Ok(mut acct) = store.account_store().get_account(&holder) {
            acct.trst_balance = acct.trst_balance.saturating_add(amount);
            acct.revoked_trst = acct.revoked_trst.saturating_sub(amount);
            if let Err(e) = store.account_store().put_account(&acct) {
                tracing::error!(%holder, "failed to persist un-revocation to holder: {e}");
            }
        }
    }
    total
}

/// wallets group "tokens with similar expiry dates to maximize retained
/// value"). Under the earliest-expiry floor rule, merging a fresh token with
/// an old one destroys the fresh token's remaining lifetime — so the group is
/// the oldest token plus every live token whose effective timestamp falls
/// within 10% of the expiry period of it. Deterministic across nodes: it
/// depends only on portfolio state and the current expiry period.
fn select_expiry_merge_group(
    portfolio: &burst_trst::WalletPortfolio,
    now: Timestamp,
    expiry_secs: u64,
) -> Vec<burst_trst::TrstToken> {
    let mut live: Vec<&burst_trst::TrstToken> = portfolio
        .tokens
        .iter()
        .filter(|t| {
            t.state == burst_types::TrstState::Active
                && t.revoked_origin.is_none()
                && !t.is_expired(now, expiry_secs)
        })
        .collect();
    if live.len() < 2 {
        return Vec::new();
    }
    live.sort_by_key(|t| (t.effective_origin_timestamp.as_secs(), *t.id.as_bytes()));

    let window = expiry_secs / 10;
    let oldest = live[0].effective_origin_timestamp.as_secs();
    let cutoff = oldest.saturating_add(window);
    let group: Vec<burst_trst::TrstToken> = live
        .into_iter()
        .filter(|t| t.effective_origin_timestamp.as_secs() <= cutoff)
        .take(burst_trst::MAX_MERGE_SOURCES)
        .cloned()
        .collect();
    if group.len() < 2 {
        Vec::new()
    } else {
        group
    }
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
        if !ip.is_loopback() && !ip.is_private() && !ip.is_link_local() {
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
