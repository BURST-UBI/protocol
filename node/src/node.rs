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
use burst_store::frontier::FrontierStore;
use burst_store_lmdb::LmdbStore;
use burst_trst::TrstEngine;
use burst_types::{BlockHash, Signature, Timestamp, TxHash, WalletAddress};
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
use crate::connection_registry::{spawn_peer_read_loop, write_framed, ConnectionRegistry};
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
const MAX_DBS: u32 = 28;
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

/// Well-known seed for the deterministic genesis keypair (all zeros).
const GENESIS_SEED: [u8; 32] = [0u8; 32];

/// Derive the deterministic genesis Ed25519 keypair from `GENESIS_SEED`.
fn genesis_keypair() -> burst_types::KeyPair {
    burst_crypto::keypair_from_seed(&GENESIS_SEED)
}

/// Derive the genesis wallet address from the genesis public key.
fn genesis_address() -> WalletAddress {
    burst_crypto::derive_address(&genesis_keypair().public)
}

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
    pub async fn new(config: NodeConfig) -> Result<Self, NodeError> {
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
            genesis_address(),
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
        let vote_kp = burst_crypto::generate_keypair();
        let vote_generator = {
            let rep_addr = burst_crypto::derive_address(&vote_kp.public);
            tracing::info!(representative = %rep_addr, "generated node representative key");
            Arc::new(Mutex::new(VoteGenerator::new(rep_addr, vote_kp.private.0)))
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
            governance: Arc::new(Mutex::new(GovernanceEngine::new())),
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
            consti_engine: Arc::new(Mutex::new(burst_consti::ConstiEngine::new())),
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

        let kp = genesis_keypair();
        let genesis_account = genesis_address();
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
            work: 0,
            signature: Signature([0u8; 64]),
            hash: BlockHash::ZERO,
        };
        genesis_block.hash = genesis_block.compute_hash();
        genesis_block.signature =
            burst_crypto::sign_message(genesis_block.hash.as_bytes(), &kp.private);

        // Persist genesis block, frontier, and schema version in a single write batch
        let block_bytes =
            bincode::serialize(&genesis_block).map_err(|e| NodeError::Other(e.to_string()))?;
        let mut batch = self
            .store
            .write_batch()
            .map_err(|e| NodeError::Other(format!("failed to start write batch: {e}")))?;
        batch
            .put_block(&genesis_block.hash, &block_bytes)
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
        let broadcaster_bp = self.broadcaster.clone();
        let peer_manager_bp = Arc::clone(&self.peer_manager);

        let rep_weights_bp = Arc::clone(&self.rep_weights);
        let backlog_bp = Arc::clone(&self.backlog);
        let brn_engine_bp = Arc::clone(&self.brn_engine);
        let trst_engine_bp = Arc::clone(&self.trst_engine);
        let ledger_cache_bp = Arc::clone(&self.ledger_cache);
        let trst_expiry_secs = self.config.params.trst_expiry_secs;
        let config_params_bp = self.config.params.clone();
        let fork_cache_bp = Arc::clone(&self.fork_cache);
        let vote_spacing_bp = Arc::clone(&self.vote_spacing);
        let ws_state_bp = Arc::clone(&self.ws_state);
        let governance_bp = Arc::clone(&self.governance);
        let delegation_bp = Arc::clone(&self.delegation_engine);
        let delegation_store_bp = Arc::clone(&self.delegation_store);
        let vrf_client_bp = Arc::clone(&self.vrf_client);
        let verifier_pool_bp = Arc::clone(&self.verifier_pool);
        let _verification_processor_bp = Arc::clone(&self.verification_processor);
        let verification_orch_bp = Arc::clone(&self.verification_orchestrator);
        let difficulty_adjuster_bp = Arc::clone(&self.difficulty_adjuster);
        let priority_scheduler_bp = Arc::clone(&self.priority_scheduler);

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

                // Enforce verification status for Send/Burn blocks
                let verification_rejected = if matches!(
                    block.block_type,
                    BlockType::Send
                        | BlockType::Burn
                        | BlockType::Split
                        | BlockType::Merge
                        | BlockType::Endorse
                        | BlockType::Challenge
                ) {
                    prev_account
                        .as_ref()
                        .and_then(|acct| {
                            if acct.state != burst_types::WalletState::Verified {
                                Some(format!(
                                "account must be verified to perform {:?} (current state: {:?})",
                                block.block_type, acct.state
                            ))
                            } else {
                                None
                            }
                        })
                        .or_else(|| {
                            // New account (no prev_account) trying to Send/Burn — reject
                            if block.previous.is_zero() {
                                None // Open blocks don't need verification
                            } else {
                                Some("account not found for verification check".to_string())
                            }
                        })
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
                                prev_brn_balance.saturating_sub(block.brn_balance)
                            };
                            let now = Timestamp::new(unix_now_secs());
                            crate::limits::check_wallet_limits(acct, amount, now, &config_params_bp)
                                .err()
                        })
                    } else {
                        None
                    };

                // Reject sends/splits of expired or revoked TRST.
                // The TrstEngine tracks per-wallet token portfolios in memory;
                // if the sender is tracked, verify the send amount doesn't
                // exceed the non-expired, non-revoked (transferable) balance.
                let trst_transferable_rejected = if matches!(
                    block.block_type,
                    BlockType::Send | BlockType::Split
                ) {
                    let send_amount = prev_account
                        .as_ref()
                        .map(|acct| acct.trst_balance.saturating_sub(block.trst_balance))
                        .unwrap_or(0);
                    if send_amount > 0 {
                        let mut trst = trst_engine_bp.lock().await;
                        let now = Timestamp::new(unix_now_secs());
                        match trst.transferable_balance(&block.account, now, trst_expiry_secs) {
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
                            _ => None,
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                let result = if let Some(reason) = balance_rejected {
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
                        // the batch exists. ───────────────────────────────────
                        let mut rw = rep_weights_bp.write().await;
                        let mut brn = brn_engine_bp.lock().await;
                        let mut trst = trst_engine_bp.lock().await;

                        // ── In-memory economics ──────────────────────────────
                        let econ_now = Timestamp::new(unix_now_secs());
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
                            drop(rw);
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
                                trst.track_token(token.clone());
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
                                    let provenance =
                                        trst.debit_wallet_with_provenance(sender, send_amount);
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
                                    trst.track_token(received_token);
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
                                    let active_tokens: Vec<burst_trst::TrstToken> = portfolio
                                        .tokens
                                        .iter()
                                        .filter(|t| t.state == burst_types::TrstState::Active)
                                        .cloned()
                                        .collect();
                                    if active_tokens.len() >= 2 {
                                        let merge_tx =
                                            burst_types::TxHash::new(*block.hash.as_bytes());
                                        match trst.merge(
                                            &active_tokens,
                                            account.clone(),
                                            merge_tx,
                                            econ_now,
                                            trst_expiry_secs,
                                        ) {
                                            Ok(merged) => {
                                                let ids_to_remove: std::collections::HashSet<_> =
                                                    active_tokens.iter().map(|t| t.id).collect();
                                                trst.bulk_untrack(account, &ids_to_remove);
                                                trst.track_token(merged);
                                                tracing::info!(%account, count = active_tokens.len(), "TRST merge: tokens merged in portfolio");
                                            }
                                            Err(e) => {
                                                tracing::warn!(%account, error = %e, "TRST merge failed");
                                            }
                                        }
                                    }
                                }
                            }
                            crate::ledger_bridge::EconomicResult::Split { ref account } => {
                                let split_amount = u128::from_be_bytes({
                                    let b = block.link.as_bytes();
                                    let mut arr = [0u8; 16];
                                    arr.copy_from_slice(&b[..16]);
                                    arr
                                });
                                if split_amount > 0 {
                                    if let Some(portfolio) = trst.get_portfolio(account) {
                                        if let Some(parent) = portfolio.tokens.first().cloned() {
                                            if split_amount > parent.amount {
                                                tracing::warn!(%account, split_amount, parent_amount = parent.amount, "TRST split rejected: split amount exceeds parent token");
                                            } else if split_amount == parent.amount {
                                                tracing::warn!(%account, split_amount, "TRST split rejected: split amount equals parent (no-op)");
                                            } else {
                                                let remainder = parent.amount - split_amount;
                                                let hash_a = burst_types::TxHash::new(
                                                    *block.hash.as_bytes(),
                                                );
                                                let mut hash_b_bytes = *block.hash.as_bytes();
                                                hash_b_bytes[0] ^= 0xFF;
                                                let hash_b = burst_types::TxHash::new(hash_b_bytes);
                                                match trst.split(
                                                    &parent,
                                                    &[
                                                        (account.clone(), split_amount),
                                                        (account.clone(), remainder),
                                                    ],
                                                    &[hash_a, hash_b],
                                                    econ_now,
                                                    trst_expiry_secs,
                                                ) {
                                                    Ok(children) => {
                                                        trst.untrack_token(account, &parent.id);
                                                        for child in children {
                                                            trst.track_token(child);
                                                        }
                                                        tracing::info!(%account, split_amount, remainder, "TRST split: token split in portfolio");
                                                    }
                                                    Err(e) => {
                                                        tracing::warn!(%account, error = %e, "TRST split failed");
                                                    }
                                                }
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
                            let mut batch = match store.write_batch() {
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
                                &mut rw,
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
                        drop(rw);

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

                            let genesis_addr = genesis_address();
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
                                let pool = Arc::clone(&verifier_pool_bp);
                                let orch_vrf = Arc::clone(&verification_orch_bp);
                                let target_for_vrf = target_addr.clone();
                                let params_vrf = config_params_bp.clone();
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

                                            let verifier_addrs = {
                                                let p = pool.lock().await;
                                                p.pool()
                                            };

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
                            let mut orch = verification_orch_bp.lock().await;
                            if let Err(e) = orch.initiate_challenge(
                                target_addr,
                                &block.account,
                                challenger_verified,
                                stake_amount,
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
                        if let crate::ledger_bridge::EconomicResult::BurnOnly {
                            burn_amount,
                            ref burn_result,
                        } = econ_result
                        {
                            match burn_result {
                                Ok(()) => {
                                    tracing::info!(
                                        account = %block.account,
                                        burn_amount,
                                        "BRN burned without TRST mint (no valid receiver)"
                                    );
                                }
                                Err(e) => {
                                    tracing::error!(
                                        account = %block.account,
                                        burn_amount,
                                        error = %e,
                                        "BRN burn-only recording failed"
                                    );
                                }
                            }
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

                        // Split/Merge: balance is handled at the ledger level by
                        // update_account_on_block (trst_balance comes from the block).
                        // Individual token provenance tracking (TrstEngine split/merge)
                        // is deferred until per-token persistence via TrstIndexStore.
                        if let crate::ledger_bridge::EconomicResult::Split { ref account } =
                            econ_result
                        {
                            tracing::info!(%account, "TRST split processed at ledger level");
                        }
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
                                                if let Ok(mut acct) = store.account_store().get_account(wallet) {
                                                    acct.state = burst_types::WalletState::Verified;
                                                    acct.verified_at = Some(Timestamp::now());
                                                    if let Err(e) = store.account_store().put_account(&acct) {
                                                        tracing::error!(%wallet, "failed to update account to Verified: {e}");
                                                    }
                                                }
                                                let mut brn_inner = brn_engine_bp.lock().await;
                                                let ws = burst_brn::BrnWalletState::new(Timestamp::now());
                                                brn_inner.track_wallet(wallet.clone(), ws);
                                                tracing::info!(%wallet, "BRN accrual activated after verification");

                                                // Mint TRST rewards for endorsers
                                                let mut trst_inner = trst_engine_bp.lock().await;
                                                let now_ts = Timestamp::now();
                                                for eo in &outcomes.endorsers {
                                                    if eo.trst_reward > 0 {
                                                        let reward_hash = TxHash::new(
                                                            burst_crypto::blake2b_256_multi(&[
                                                                b"endorser_reward",
                                                                eo.address.as_str().as_bytes(),
                                                                wallet.as_str().as_bytes(),
                                                            ]),
                                                        );
                                                        match trst_inner.mint(
                                                            reward_hash,
                                                            eo.address.clone(),
                                                            eo.trst_reward,
                                                            eo.address.clone(),
                                                            now_ts,
                                                        ) {
                                                            Ok(ref token) => {
                                                                tracing::info!(
                                                                    endorser = %eo.address,
                                                                    reward = eo.trst_reward,
                                                                    "minted TRST reward for endorser"
                                                                );
                                                                let expiry_ts = Timestamp::new(
                                                                    token.effective_origin_timestamp.as_secs()
                                                                        .saturating_add(trst_expiry_secs),
                                                                );
                                                                if let Ok(mut idx_batch) = store.write_batch() {
                                                                    let _ = idx_batch.put_origin_index(&token.origin, &token.id);
                                                                    let _ = idx_batch.put_expiry_index(expiry_ts, &token.id);
                                                                    if let Err(e) = idx_batch.commit() {
                                                                        tracing::warn!(
                                                                            endorser = %eo.address,
                                                                            "failed to persist endorser reward TRST indices: {e}"
                                                                        );
                                                                    }
                                                                }
                                                            }
                                                            Err(e) => {
                                                                tracing::error!(
                                                                    endorser = %eo.address,
                                                                    error = %e,
                                                                    "failed to mint endorser TRST reward"
                                                                );
                                                            }
                                                        }
                                                    }
                                                }
                                            }

                                            // Resolve verifier stakes via BRN engine
                                            for vo in &outcomes.verifiers {
                                                if vo.staked == 0 {
                                                    continue;
                                                }
                                                let mut brn_inner = brn_engine_bp.lock().await;
                                                if let Some(ws) = brn_inner.get_wallet_mut(&vo.address) {
                                                    if vo.voted_correctly {
                                                        ws.total_staked = ws.total_staked.saturating_sub(vo.staked);
                                                        tracing::info!(
                                                            verifier = %vo.address,
                                                            staked = vo.staked,
                                                            "verifier stake returned (correct vote)"
                                                        );
                                                    } else {
                                                        ws.total_staked = ws.total_staked.saturating_sub(vo.staked);
                                                        ws.total_burned = ws.total_burned.saturating_add(vo.staked);
                                                        tracing::info!(
                                                            verifier = %vo.address,
                                                            penalty = vo.penalty,
                                                            "dissenter verifier stake forfeited"
                                                        );
                                                    }
                                                } else {
                                                    tracing::warn!(
                                                        verifier = %vo.address,
                                                        "verifier wallet not tracked in BRN engine, cannot resolve stake"
                                                    );
                                                }
                                            }
                                        }
                                        burst_verification::VerificationEvent::WalletUnverified { ref wallet } => {
                                            tracing::warn!(%wallet, "wallet unverified (fraud confirmed)");
                                            let mut trst_inner = trst_engine_bp.lock().await;
                                            let revocations = trst_inner.revoke_by_origin(wallet);
                                            drop(trst_inner);
                                            let total_revoked: u128 = revocations.iter().map(|r| r.revoked_amount).sum();
                                            if !revocations.is_empty() {
                                                tracing::warn!(
                                                    %wallet,
                                                    revoked_count = revocations.len(),
                                                    total_revoked,
                                                    "TRST revoked via orchestrator fraud confirmation"
                                                );
                                            }
                                            if let Ok(mut acct) = store.account_store().get_account(wallet) {
                                                acct.state = burst_types::WalletState::Revoked;
                                                acct.revoked_trst = acct.revoked_trst.saturating_add(total_revoked);
                                                acct.trst_balance = acct.trst_balance.saturating_sub(total_revoked);
                                                if let Err(e) = store.account_store().put_account(&acct) {
                                                    tracing::error!(%wallet, "failed to persist account Revoked state: {e}");
                                                }
                                            }
                                        }
                                        burst_verification::VerificationEvent::ChallengeResolved { ref wallet, ref outcome } => {
                                            tracing::info!(%wallet, ?outcome.outcome, "challenge resolved via orchestrator");
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
                                if vs.votable(&block.account, &block.hash) {
                                    vs.record(block.account.clone(), block.hash);
                                    drop(vs);
                                    let vote = vg.generate_vote(block.hash);
                                    let wire_msg = WireMessage::Vote(WireVote {
                                        voter: vote.voter,
                                        block_hashes: vec![vote.block_hash],
                                        is_final: false,
                                        timestamp: vote.timestamp,
                                        sequence: vote.sequence,
                                        signature: vote.signature,
                                    });
                                    if let Ok(msg_bytes) = bincode::serialize(&wire_msg) {
                                        let peers: Vec<burst_network::PeerState> = {
                                            let pm = peer_manager_bp.read().await;
                                            pm.iter_connected().map(|(_, s)| s.clone()).collect()
                                        };
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
                        let mut ae = active_elections_bp.write().await;
                        if let Err(e) = ae.start_election(block.previous, now) {
                            tracing::debug!(
                                root = %block.previous,
                                error = %e,
                                "could not start election for fork"
                            );
                        } else {
                            tracing::info!(
                                root = %block.previous,
                                fork_hash = %block.hash,
                                "election started for fork"
                            );
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
                        // Collect confirmed elections
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

                            // TASK 3: Generate and broadcast a FINAL vote for the winner
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

        // ── Governance tick task — periodically advances proposals through phases ──
        let governance_tick = Arc::clone(&self.governance);
        let brn_engine_gov = Arc::clone(&self.brn_engine);
        let consti_engine_gov = Arc::clone(&self.consti_engine);
        let store_gov = Arc::clone(&self.store);
        let mut shutdown_rx_gov = self.shutdown.subscribe();
        let mut gov_params = self.config.params.clone();

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
                        let mut gov = governance_tick.lock().await;
                        let activated = gov.tick(now, &mut gov_params);
                        if !activated.is_empty() {
                            let changes = gov.drain_pending_changes();
                            if !changes.is_empty() {
                                let mut brn = brn_engine_gov.lock().await;
                                for (param, value) in &changes {
                                    match param {
                                        burst_governance::GovernableParam::BrnRate => {
                                            if let Err(e) = brn.apply_rate_change(*value, now) {
                                                tracing::warn!(error = %e, "failed to propagate BRN rate change via governance tick");
                                            } else {
                                                tracing::info!(new_rate = value, "BRN rate change propagated via governance tick");
                                            }
                                        }
                                        other => {
                                            tracing::info!(param = ?other, value = value, "governance parameter activated");
                                        }
                                    }
                                }
                            }

                            // Apply activated constitutional amendments to the ConstiEngine
                            let amendments = gov.drain_activated_amendments();
                            if !amendments.is_empty() {
                                let mut consti = consti_engine_gov.lock().await;
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
                                            created_at: now,
                                            operations: Vec::new(),
                                        };
                                        match consti.activate_amendment_internal(&amendment) {
                                            Ok(()) => tracing::info!(title = %title, "constitutional amendment applied"),
                                            Err(e) => tracing::warn!(title = %title, "failed to apply constitutional amendment: {e}"),
                                        }
                                    }
                                }
                            }

                            for hash in &activated {
                                tracing::info!(%hash, "governance proposal activated");
                            }
                        }

                        // Update adaptive quorum EMA with current participation data.
                        // Compute participation from the most recently finished proposals.
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
        let peer_manager_drain = Arc::clone(&self.peer_manager);
        let metrics_drain = Arc::clone(&self.metrics);

        let drain_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx2.recv() => {
                        tracing::info!("outbound message task shutting down");
                        break;
                    }
                    Some((peer_id, msg_bytes)) = outbound_rx.recv() => {
                        // Check outbound bandwidth throttle (requires write lock)
                        let (writer, throttle_ok) = {
                            let mut registry = conn_registry_drain.write().await;
                            let ok = registry.try_consume_outbound(&peer_id, msg_bytes.len() as u64);
                            (registry.get(&peer_id), ok)
                        };

                        if !throttle_ok {
                            tracing::trace!(
                                peer = %peer_id,
                                bytes = msg_bytes.len(),
                                "outbound message throttled"
                            );
                            continue;
                        }

                        match writer {
                            Some(writer) => {
                                if let Err(e) = write_framed(&writer, &msg_bytes).await {
                                    tracing::warn!(
                                        peer = %peer_id,
                                        error = %e,
                                        "failed to send message, disconnecting peer"
                                    );
                                    {
                                        let mut registry = conn_registry_drain.write().await;
                                        registry.remove(&peer_id);
                                    }
                                    {
                                        let mut pm = peer_manager_drain.write().await;
                                        pm.mark_disconnected(&peer_id);
                                        metrics_drain.peer_count.set(pm.connected_count() as i64);
                                    }
                                }
                            }
                            None => {
                                tracing::trace!(
                                    peer = %peer_id,
                                    "outbound message dropped: no connection for peer"
                                );
                            }
                        }
                    }
                }
            }
        });
        self.task_handles.push(drain_handle);

        // ── Expired TRST cleanup task — returns expired pending tokens ─────
        let store_expiry = Arc::clone(&self.store);
        let trst_expiry_bg = self.config.params.trst_expiry_secs;
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
                        let cutoff = Timestamp::new(now_secs);
                        let trst_idx = store_expiry.trst_index_store();
                        match trst_idx.get_expired_before(cutoff) {
                            Ok(expired) if !expired.is_empty() => {
                                tracing::info!(
                                    count = expired.len(),
                                    cutoff = now_secs,
                                    expiry_secs = trst_expiry_bg,
                                    "found expired TRST tokens for cleanup"
                                );
                                // Clean up expiry index entries. The TRST engine
                                // uses lazy expiry checking via `is_transferable()`
                                // at transfer time, so we don't update engine state
                                // here. This cleanup only prevents index bloat.
                                // Account-level expired_trst counters are updated
                                // when transactions involving expired tokens are
                                // rejected during block processing.
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
                        let rw = rep_weights_bg.read().await;
                        let weight_map = rw.all_weights().clone();
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

        // Initialize genesis if needed
        self.initialize_genesis()?;

        // Auto-verify the genesis creator so it can endorse during bootstrap
        {
            use burst_store::account::AccountInfo;
            let genesis_addr = genesis_address();
            let acct_store = self.store.account_store();
            let already_verified = acct_store
                .get_account(&genesis_addr)
                .map(|a| a.state == burst_types::WalletState::Verified)
                .unwrap_or(false);

            if !already_verified {
                let info = AccountInfo {
                    address: genesis_addr.clone(),
                    state: burst_types::WalletState::Verified,
                    verified_at: Some(Timestamp::new(0)),
                    head: BlockHash::ZERO,
                    block_count: 1,
                    confirmation_height: 0,
                    representative: genesis_addr.clone(),
                    total_brn_burned: 0,
                    total_brn_staked: 0,
                    trst_balance: 0,
                    expired_trst: 0,
                    revoked_trst: 0,
                    epoch: 0,
                };
                if let Err(e) = acct_store.put_account(&info) {
                    tracing::error!("failed to auto-verify genesis creator: {e}");
                } else {
                    tracing::info!(%genesis_addr, "genesis creator auto-verified for bootstrap");
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
        {
            let meta = self.store.meta_store();
            match meta.get_meta(TrstEngine::meta_key()) {
                Ok(bytes) => {
                    let mut trst = self.trst_engine.lock().await;
                    let restored =
                        TrstEngine::load_wallets(&bytes, self.config.params.trst_expiry_secs);
                    trst.wallets = restored.wallets;
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

        // Restore representative weights from LMDB into the in-memory cache.
        // If no persisted weights exist, rebuild from the full account set.
        {
            let rw_store = self.store.rep_weight_store();
            match rw_store.iter_rep_weights() {
                Ok(entries) if !entries.is_empty() => {
                    let mut rw = self.rep_weights.write().await;
                    for (rep, weight) in &entries {
                        rw.add_weight(rep, *weight);
                    }
                    tracing::info!(
                        reps = entries.len(),
                        "representative weights loaded from LMDB"
                    );
                }
                _ => {
                    let acct_store = self.store.account_store();
                    match acct_store.iter_accounts() {
                        Ok(accounts) => {
                            let mut rw = self.rep_weights.write().await;
                            rw.rebuild_from_accounts(accounts.into_iter().map(|a| {
                                (a.address.clone(), a.representative.clone(), a.trst_balance)
                            }));
                            tracing::info!(
                                reps = rw.rep_count(),
                                "representative weights rebuilt from account store"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "failed to rebuild rep weights from accounts — cache starts empty"
                            );
                        }
                    }
                }
            }
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
        let frontier_p2p = Arc::clone(&self.frontier);
        let store_p2p = Arc::clone(&self.store);
        let node_address_p2p = self.node_address.clone();

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
                                let now_secs = unix_now_secs();
                                let peer_addr = PeerAddress {
                                    ip: addr.ip().to_string(),
                                    port: addr.port(),
                                };
                                let peer_id = format!("{}:{}", addr.ip(), addr.port());

                                // SYN cookie validation — generate a cookie challenge for
                                // this peer's IP. The read loop validates the signed
                                // response when the peer completes the handshake.
                                let peer_ip = addr.ip().to_string();
                                let cookie = {
                                    let mut cookies = syn_cookies_p2p.lock().await;
                                    cookies.generate(&peer_ip)
                                };

                                let cookie = match cookie {
                                    Some(c) => {
                                        tracing::trace!(
                                            peer = %peer_id,
                                            "SYN cookie challenge generated"
                                        );
                                        c
                                    }
                                    None => {
                                        tracing::warn!(
                                            peer = %peer_id,
                                            "SYN cookie generation failed, rejecting connection"
                                        );
                                        drop(stream);
                                        continue;
                                    }
                                };

                                // Split the TCP stream into read and write halves
                                let (read_half, mut write_half) = stream.into_split();

                                // Send the cookie challenge before registering
                                let challenge = WireMessage::Handshake(crate::wire_message::HandshakeMsg {
                                    node_id: node_address_p2p.clone(),
                                    cookie: Some(cookie),
                                    cookie_signature: None,
                                });
                                if let Ok(bytes) = bincode::serialize(&challenge) {
                                    use tokio::io::AsyncWriteExt;
                                    let len_bytes = (bytes.len() as u32).to_be_bytes();
                                    if let Err(e) = write_half.write_all(&len_bytes).await {
                                        tracing::warn!(peer = %peer_id, "failed to send cookie challenge length: {e}");
                                        continue;
                                    }
                                    if let Err(e) = write_half.write_all(&bytes).await {
                                        tracing::warn!(peer = %peer_id, "failed to send cookie challenge: {e}");
                                        continue;
                                    }
                                    let _ = write_half.flush().await;
                                }

                                // Register write half in the connection registry
                                {
                                    let mut registry = conn_registry_p2p.write().await;
                                    registry.insert(peer_id.clone(), write_half);
                                }

                                // Register the peer
                                {
                                    let mut pm = peer_manager.write().await;
                                    pm.add_peer(peer_addr);
                                    pm.mark_connected(&peer_id, now_secs);
                                    metrics_p2p.peer_count.set(pm.connected_count() as i64);
                                }

                                // Spawn a read loop for inbound messages (with cookie validation)
                                spawn_peer_read_loop(
                                    peer_id.clone(),
                                    read_half,
                                    Arc::clone(&block_queue_p2p),
                                    Arc::clone(&conn_registry_p2p),
                                    Arc::clone(&peer_manager),
                                    Arc::clone(&metrics_p2p),
                                    Arc::clone(&active_elections_p2p),
                                    Arc::clone(&rep_weights_p2p),
                                    Arc::clone(&message_dedup_p2p),
                                    Arc::clone(&online_weight_sampler_p2p),
                                    Some(Arc::clone(&syn_cookies_p2p)),
                                    peer_ip,
                                    Arc::clone(&frontier_p2p),
                                    Arc::clone(&store_p2p),
                                );

                                tracing::info!(peer = %peer_id, "inbound peer connected");
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
                                    let mut pm = pm_upnp.write().await;
                                    pm.clear_external_address();
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

        // ── Bootstrap peer discovery ──────────────────────────────────────
        let bootstrap_peers: Vec<String> = {
            let pm = self.peer_manager.read().await;
            pm.bootstrap_peers().to_vec()
        };

        if !bootstrap_peers.is_empty() {
            let peer_manager_bs = Arc::clone(&self.peer_manager);
            let metrics_bs = Arc::clone(&self.metrics);
            let conn_registry_bs = Arc::clone(&self.connection_registry);
            let block_queue_bs = Arc::clone(&self.block_queue);
            let active_elections_bs = Arc::clone(&self.active_elections);
            let rep_weights_bs = Arc::clone(&self.rep_weights);
            let message_dedup_bs = Arc::clone(&self.message_dedup);
            let online_weight_sampler_bs = Arc::clone(&self.online_weight_sampler);
            let frontier_bs = Arc::clone(&self.frontier);
            let store_bs = Arc::clone(&self.store);
            let node_private_bs = burst_types::PrivateKey(self.node_private_key.0);
            let node_address_bs = self.node_address.clone();
            let store_bs2 = Arc::clone(&self.store);
            let mut shutdown_rx_bs = self.shutdown.subscribe();

            let bs_handle = tokio::spawn(async move {
                loop {
                    for addr_str in &bootstrap_peers {
                        // Skip peers that are already connected
                        {
                            let pm = peer_manager_bs.read().await;
                            let parts: Vec<&str> = addr_str.rsplitn(2, ':').collect();
                            if parts.len() == 2 {
                                let key = format!("{}:{}", parts[1], parts[0]);
                                if pm.is_connected(&key) {
                                    continue;
                                }
                            }
                        }

                        tracing::info!(peer = %addr_str, "connecting to bootstrap peer");
                        match tokio::net::TcpStream::connect(addr_str).await {
                            Ok(stream) => {
                                let parts: Vec<&str> = addr_str.rsplitn(2, ':').collect();
                                let (port, ip) = if parts.len() == 2 {
                                    (
                                        parts[0].parse::<u16>().unwrap_or(7075),
                                        parts[1].to_string(),
                                    )
                                } else {
                                    (7075, addr_str.clone())
                                };
                                let peer_addr = PeerAddress {
                                    ip: ip.clone(),
                                    port,
                                };
                                let peer_id = format!("{ip}:{port}");
                                let now = unix_now_secs();

                                let (read_half, mut write_half) = stream.into_split();

                                // Read the cookie challenge from the peer
                                let mut reader = tokio::io::BufReader::new(read_half);
                                let cookie_opt = {
                                    use tokio::io::AsyncReadExt;
                                    let mut len_buf = [0u8; 4];
                                    match tokio::time::timeout(
                                        std::time::Duration::from_secs(10),
                                        reader.read_exact(&mut len_buf),
                                    )
                                    .await
                                    {
                                        Ok(Ok(_)) => {
                                            let body_len = u32::from_be_bytes(len_buf) as usize;
                                            if body_len > 0 && body_len < 65536 {
                                                let mut body = vec![0u8; body_len];
                                                if reader.read_exact(&mut body).await.is_ok() {
                                                    if let Ok(WireMessage::Handshake(hs)) =
                                                        bincode::deserialize::<WireMessage>(&body)
                                                    {
                                                        hs.cookie
                                                    } else {
                                                        None
                                                    }
                                                } else {
                                                    None
                                                }
                                            } else {
                                                None
                                            }
                                        }
                                        _ => None,
                                    }
                                };

                                // Sign and send cookie response
                                if let Some(cookie) = cookie_opt {
                                    use tokio::io::AsyncWriteExt;
                                    let sig = burst_crypto::sign_message(&cookie, &node_private_bs);
                                    let response =
                                        WireMessage::Handshake(crate::wire_message::HandshakeMsg {
                                            node_id: node_address_bs.clone(),
                                            cookie: None,
                                            cookie_signature: Some(sig),
                                        });
                                    if let Ok(bytes) = bincode::serialize(&response) {
                                        let len_bytes = (bytes.len() as u32).to_be_bytes();
                                        let _ = write_half.write_all(&len_bytes).await;
                                        let _ = write_half.write_all(&bytes).await;
                                        let _ = write_half.flush().await;
                                        tracing::debug!(
                                            peer = %peer_id,
                                            "sent cookie response to bootstrap peer"
                                        );
                                    }
                                } else {
                                    tracing::warn!(
                                        peer = %peer_id,
                                        "no cookie challenge received from bootstrap peer"
                                    );
                                }

                                let read_half = reader.into_inner();

                                // Register write half in the connection registry
                                {
                                    let mut registry = conn_registry_bs.write().await;
                                    registry.insert(peer_id.clone(), write_half);
                                }

                                // Register the peer
                                {
                                    let mut pm = peer_manager_bs.write().await;
                                    pm.add_peer(peer_addr);
                                    pm.mark_connected(&peer_id, now);
                                    metrics_bs.peer_count.set(pm.connected_count() as i64);
                                }

                                // Spawn a read loop (no SYN cookie for outbound — already done)
                                spawn_peer_read_loop(
                                    peer_id.clone(),
                                    read_half,
                                    Arc::clone(&block_queue_bs),
                                    Arc::clone(&conn_registry_bs),
                                    Arc::clone(&peer_manager_bs),
                                    Arc::clone(&metrics_bs),
                                    Arc::clone(&active_elections_bs),
                                    Arc::clone(&rep_weights_bs),
                                    Arc::clone(&message_dedup_bs),
                                    Arc::clone(&online_weight_sampler_bs),
                                    None,
                                    ip.clone(),
                                    Arc::clone(&frontier_bs),
                                    Arc::clone(&store_bs2),
                                );

                                tracing::info!(peer = %peer_id, "bootstrap peer connected");

                                // Check if we need to bootstrap — if our frontier
                                // is empty, we have no ledger data and need to pull
                                // everything from this peer.
                                let local_frontier_count =
                                    { frontier_bs.read().await.account_count() };

                                if local_frontier_count == 0 {
                                    tracing::info!(
                                        peer = %peer_id,
                                        "local frontier is empty — initiating bootstrap sync"
                                    );

                                    let mut bootstrap_client =
                                        crate::bootstrap::BootstrapClient::new(10_000);

                                    // Step 1: Request frontiers from the peer
                                    let frontier_req = bootstrap_client.start_frontier_scan();
                                    let wire_req = WireMessage::Bootstrap(frontier_req);
                                    let req_bytes = match bincode::serialize(&wire_req) {
                                        Ok(b) => b,
                                        Err(e) => {
                                            tracing::error!(
                                                "failed to serialize frontier req: {e}"
                                            );
                                            continue;
                                        }
                                    };

                                    // Send the frontier request to the peer
                                    {
                                        let registry = conn_registry_bs.read().await;
                                        if let Some(writer) = registry.get(&peer_id) {
                                            if let Err(e) = write_framed(&writer, &req_bytes).await
                                            {
                                                tracing::warn!(
                                                    peer = %peer_id,
                                                    "failed to send frontier request: {e}"
                                                );
                                            }
                                        } else {
                                            tracing::warn!(
                                                peer = %peer_id,
                                                "no writer found in registry for bootstrap peer"
                                            );
                                        }
                                    }

                                    tracing::info!(
                                        peer = %peer_id,
                                        "bootstrap frontier request sent — blocks will arrive via read loop"
                                    );
                                } else {
                                    tracing::debug!(
                                        peer = %peer_id,
                                        frontier_accounts = local_frontier_count,
                                        "frontier not empty — skipping bootstrap for this peer"
                                    );
                                }

                                // Also check LMDB block count for a more thorough
                                // behind-detection (frontier may exist but be stale).
                                if let Ok(block_count) = store_bs.block_store().block_count() {
                                    if block_count < 10 && local_frontier_count > 0 {
                                        tracing::info!(
                                            peer = %peer_id,
                                            block_count = block_count,
                                            "very few blocks in store — may need catch-up sync"
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(peer = %addr_str, "bootstrap connection failed: {e}");
                            }
                        }
                    }

                    // Wait 30s before retrying disconnected bootstrap peers
                    tokio::select! {
                        biased;
                        _ = shutdown_rx_bs.recv() => {
                            tracing::debug!("bootstrap reconnect task shutting down");
                            break;
                        }
                        _ = tokio::time::sleep(Duration::from_secs(30)) => {}
                    }
                } // end loop
            });
            self.task_handles.push(bs_handle);
        }

        // ── Keepalive task ────────────────────────────────────────────────
        let peer_manager_ka = Arc::clone(&self.peer_manager);
        let conn_registry_ka = Arc::clone(&self.connection_registry);
        let mut shutdown_rx_ka = self.shutdown.subscribe();

        let ka_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(15));
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

                            let connected_peers: Vec<String> = pm
                                .random_peers(8)
                                .iter()
                                .map(|a| format!("{}:{}", a.ip, a.port))
                                .collect();

                            let msg = WireMessage::Keepalive(
                                crate::wire_message::KeepaliveMsg {
                                    peers: connected_peers,
                                },
                            );
                            if let Ok(bytes) = bincode::serialize(&msg) {
                                let registry = conn_registry_ka.read().await;
                                let peer_ids: Vec<String> =
                                    registry.peer_ids().into_iter().cloned().collect();
                                drop(registry);

                                for pid in &peer_ids {
                                    let registry = conn_registry_ka.read().await;
                                    if let Some(writer) = registry.get(pid) {
                                        if let Err(e) = write_framed(&writer, &bytes).await {
                                            tracing::debug!(
                                                peer = %pid,
                                                error = %e,
                                                "keepalive send failed"
                                            );
                                        }
                                    }
                                }
                            }
                            tracing::trace!(
                                connected = pm.connected_count(),
                                "keepalive round"
                            );
                        }
                    }
                }
            }
        });
        self.task_handles.push(ka_handle);

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
                            if let Some(writer) = registry.get(pid) {
                                if let Err(e) = crate::connection_registry::write_framed(&writer, &bytes).await {
                                    tracing::trace!(peer = %pid, "failed to send telemetry req: {e}");
                                }
                            }
                        }
                        tracing::trace!(peers = peer_ids.len(), "sent telemetry requests");
                    }
                }
            }
        });
        self.task_handles.push(telem_handle);

        // ── RPC server (optional) ─────────────────────────────────────────
        if self.config.enable_rpc {
            let rpc_port = self.config.rpc_port;
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
            });

            let rpc_server = RpcServer::with_state(rpc_port, rpc_state);
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

        // Persist representative weights to LMDB.
        {
            let rw = self.rep_weights.read().await;
            let rw_store = self.store.rep_weight_store();
            let mut persisted = 0u64;
            for (rep, weight) in rw.all_weights() {
                if let Err(e) = rw_store.put_rep_weight(rep, *weight) {
                    tracing::warn!(rep = %rep, error = %e, "failed to persist rep weight");
                } else {
                    persisted += 1;
                }
            }
            tracing::info!(reps = persisted, "representative weights persisted to LMDB");
        }

        // Flush LMDB
        if let Err(e) = self.store.force_sync() {
            tracing::warn!("LMDB force_sync failed: {e}");
        } else {
            tracing::info!("LMDB flushed to disk");
        }

        // Wait for all spawned tasks with a timeout
        let handles: Vec<JoinHandle<()>> = self.task_handles.drain(..).collect();
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

        let (block_type, new_brn, new_trst, link) = match tx {
            burst_transactions::Transaction::Burn(burn) => {
                if burn.amount > brn_balance {
                    return Err(NodeError::Other(format!(
                        "insufficient BRN: need {}, have {}",
                        burn.amount, brn_balance
                    )));
                }
                let new_brn = brn_balance - burn.amount;
                let new_trst = trst_balance;
                let mut link_bytes = [0u8; 32];
                let addr_bytes = burn.receiver.as_str().as_bytes();
                let copy_len = addr_bytes.len().min(32);
                link_bytes[..copy_len].copy_from_slice(&addr_bytes[..copy_len]);
                (
                    if is_open {
                        BlockType::Open
                    } else {
                        BlockType::Burn
                    },
                    new_brn,
                    new_trst,
                    BlockHash::new(link_bytes),
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
                    let trst_expiry = self.config.params.trst_expiry_secs;
                    if let Some(transferable) = trst.transferable_balance(&sender, now, trst_expiry)
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
                let mut link_bytes = [0u8; 32];
                let addr_bytes = send.receiver.as_str().as_bytes();
                let copy_len = addr_bytes.len().min(32);
                link_bytes[..copy_len].copy_from_slice(&addr_bytes[..copy_len]);
                (
                    if is_open {
                        BlockType::Open
                    } else {
                        BlockType::Send
                    },
                    brn_balance,
                    new_trst,
                    BlockHash::new(link_bytes),
                )
            }
            _ => {
                // For other transaction types, create a generic block
                let block_type = if is_open {
                    BlockType::Open
                } else {
                    match tx {
                        burst_transactions::Transaction::Split(_) => BlockType::Split,
                        burst_transactions::Transaction::Merge(_) => BlockType::Merge,
                        burst_transactions::Transaction::Endorse(_) => BlockType::Endorse,
                        burst_transactions::Transaction::Challenge(_) => BlockType::Challenge,
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
