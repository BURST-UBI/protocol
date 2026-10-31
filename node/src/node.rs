//! The main BURST node struct — wires all protocol subsystems together.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;

use burst_brn::BrnEngine;
use burst_consensus::{
    ActiveElections, OnlineWeightSampler, PriorityScheduler, RepWeightCache, VoteGenerator,
};
use burst_governance::vote_delegation::DelegationEngine;
use burst_governance::GovernanceEngine;
use burst_ledger::{BlockType, DagFrontier, StateBlock, CURRENT_BLOCK_VERSION};
use burst_messages::PeerAddress;
use burst_network::{Broadcaster, ClockSync, PeerManager, PortMapper, UpnpState};
use burst_store::block::BlockStore;
use burst_rpc::ProcessResult as RpcProcessResult;
use burst_store::brn::BrnStore;
use burst_store::frontier::FrontierStore;
use burst_store_lmdb::LmdbStore;
use burst_trst::TrstEngine;
use burst_types::{BlockHash, ProtocolParams, Timestamp, TxHash, WalletAddress};
use burst_websocket::WsState;

use burst_store::account::AccountStore;
use burst_store::delegation::DelegationStore;
use burst_store::meta::MetaStore;
use burst_store::pending::PendingStore;

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
use crate::wire_message::{WireMessage, WireVote};

/// Default LMDB map size: 1 GiB.
pub(crate) const DEFAULT_MAP_SIZE: usize = 1 << 30;
/// Number of named LMDB databases.
pub(crate) const MAX_DBS: u32 = burst_store_lmdb::REQUIRED_DATABASES;
/// Channel capacity for the block-processing pipeline.
const BLOCK_CHANNEL_CAPACITY: usize = 4096;
/// Channel capacity for outbound peer messages.
const OUTBOUND_CHANNEL_CAPACITY: usize = 4096;
/// Timeout for waiting on background tasks during shutdown.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// Meta-store key used to persist the serialized MergerGraph.
const MERGER_GRAPH_META_KEY: &str = "merger_graph";

use crate::genesis_key;

mod rpc_adapter;
mod online_weight_worker;
/// Maximum number of recently confirmed hashes to remember.
const RECENTLY_CONFIRMED_CAPACITY: usize = 65_536;
/// Default maximum concurrent elections.
const MAX_ACTIVE_ELECTIONS: usize = 5000;
/// Conservative placeholder; replaced from ledger-derived weight before networking.
const DEFAULT_ONLINE_WEIGHT: u128 = 1_000_000;
/// A running BURST node.
pub struct BurstNode {
    pub config: NodeConfig,
    /// Immutable replay inputs, separate from the currently governed rules.
    initial_params: ProtocolParams,
    recovery_scratch: Option<(crate::confirmed_replay::scratch_file::ScratchLimit, usize)>,
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
    /// Constitutional engine for managing on-chain amendments.
    pub consti_engine: Arc<Mutex<burst_consti::ConstiEngine>>,
    /// Election scheduler ordered by descending recorded TRST balance.
    pub priority_scheduler: Arc<Mutex<PriorityScheduler>>,
    /// Historical online-weight telemetry; consensus uses certified human weight.
    pub online_weight_tracker: Arc<Mutex<OnlineWeightTracker>>,

    /// Priority queue for submitting blocks into the processing pipeline.
    /// Transport origins rotate fairly; higher work goes first within an origin.
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
    #[cfg(test)]
    admission_group_probe: Option<Arc<admission_group::AbortProbe>>,
}

mod admission_group;
mod admission_worker;
mod confirmation_handoff;
mod confirmation_requests;
mod local_votes;
mod recovery_peers;
mod recovery_publication;
mod repair_timer;
mod dependency_waits;
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
    /// subsystems. Call [`Self::start`] to begin accepting connections and
    /// processing blocks.
    pub async fn new(mut config: NodeConfig) -> Result<Self, NodeError> {
        genesis_key::validate_identity(config.network)
            .map_err(|error| NodeError::Config(error.into()))?;
        config.validate_drand_relays()?;
        let database_map_size = config.database_map_size().map_err(NodeError::Config)?;
        let peer_limits = config.effective_peer_limits()?;
        let recovery_budget = config.recovery_scratch_budget().map_err(NodeError::Config)?;
        if config.body_retirement.enabled && !cfg!(test) {
            return Err(NodeError::Config(
                "physical body retirement remains behind the qualification gate".into(),
            ));
        }
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

        let store = LmdbStore::open(&config.data_dir, MAX_DBS, database_map_size)
            .map_err(|e| NodeError::Other(format!("failed to open LMDB: {e}")))?;
        let store = Arc::new(store);
        let recovery_scratch = recovery_budget.map(|bytes| {
            let map_bytes = ((bytes / 4).min(16 << 20) & !4095) as usize;
            let reserved = crate::confirmed_replay::snapshot::history::DurableHistoryInputs::reservation(map_bytes)?;
            let limit = crate::confirmed_replay::scratch_file::ScratchLimit::acquire_shared_reserved(
                store.env().path(), bytes, reserved,
            ).map_err(|error| burst_store::StoreError::Corruption(error.to_string()))?;
            Ok::<_, burst_store::StoreError>((limit, map_bytes))
        }).transpose()?;
        crate::network_identity::verify_network(&store, config.network)?;
        let initial_params =
            crate::network_identity::genesis_params(&store, config.network, &config.params)?;

        let peer_manager = PeerManager::with_config(
            config.max_peers,
            config.bootstrap_peers.clone(),
            15, // keepalive interval (must be < READ_TIMEOUT of 30s)
        );
        let peer_manager = Arc::new(RwLock::new(peer_manager));

        // Shared admission budget with fair origin scheduling and per-origin work priority.
        let retention = crate::block_retention::BlockRetention::new(
            crate::block_retention::MAX_RETAINED_BLOCKS,
        );
        let block_queue = Arc::new(BlockPriorityQueue::with_retention(
            BLOCK_CHANNEL_CAPACITY,
            retention.clone(),
        ));

        let (outbound_tx, outbound_rx) =
            mpsc::channel::<(String, Vec<u8>)>(OUTBOUND_CHANNEL_CAPACITY);
        let broadcaster = Broadcaster::new(outbound_tx);

        let shutdown = Arc::new(ShutdownController::new());

        let metrics = Arc::new(NodeMetrics::new());

        // WebSocket shared state (always created; only served if enabled)
        let ws_state = Arc::new(WsState::new(256));

        // Connection registry (maps peer_id -> TCP write half)
        let mut registry = ConnectionRegistry::for_recovery();
        registry.configure_network_limits(config.max_peers, peer_limits, &config.bandwidth_limits);
        let connection_registry = Arc::new(RwLock::new(registry));

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
        let recently_confirmed = Arc::new(RwLock::new(RecentlyConfirmed::new(
            RECENTLY_CONFIRMED_CAPACITY,
        )));

        // Vote generator — produce votes when acting as a representative.
        // Configured representatives retain their identity; an unconfigured
        // node uses a transient key with no delegated weight.
        // Weight comes from verified humans delegating to the representative
        // account. Genesis has the same weight per human; extra nodes add none.
        let (vote_generator, representative_key) = {
            let (representative, representative_key) = match config
                .representative_key()?
                .or_else(|| genesis_key::genesis_signing_key(config.network))
            {
                Some(keys) => (burst_crypto::derive_address(&keys.public), keys.private),
                None => {
                    let keys = burst_crypto::generate_keypair();
                    (burst_crypto::derive_address(&keys.public), keys.private)
                }
            };
            let representative_key = Arc::new(representative_key);
            let mut generator = VoteGenerator::with_shared_key(representative.clone(), representative_key.clone());
            if config.enable_representative {
                generator.set_representative(true);
                tracing::info!(representative = %representative, "node voting as representative (enabled)");
            } else {
                tracing::info!(representative = %representative, "node representative voting disabled");
            }
            (Arc::new(Mutex::new(generator)), representative_key)
        };
        let node_keys = burst_crypto::generate_keypair();
        let node_address = burst_crypto::derive_address(&node_keys.public);
        let node_private_key = node_keys.private;

        // Representative weight cache (rebuilt at startup from the account set)
        let rep_weights = Arc::new(RwLock::new(RepWeightCache::new()));

        // Confirming set for batched cementation of confirmed blocks
        let confirming_set = Arc::new(Mutex::new(ConfirmingSet::new(5)));

        // Bounded backlog for DoS-resistant unconfirmed block tracking
        let backlog = Arc::new(Mutex::new(BoundedBacklog::with_default_size()));
        let local_broadcaster = Arc::new(Mutex::new(LocalBroadcaster::with_default()));

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
                        .is_some()
                        || brn_store
                            .get_meta(burst_brn::rate_storage::HEADER)
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
                        let mut batch = store
                            .tx_begin_write()
                            .map_err(|e| NodeError::Other(e.to_string()))?;
                        batch
                            .persist_brn_rate_history(&loaded.rate_history)
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
            let block_count = block_store.block_count().unwrap_or(0);
            let account_count = account_store.account_count().unwrap_or(0);
            let pending_count = pending_store.pending_count().unwrap_or(0);
            tracing::info!(
                blocks = block_count,
                accounts = account_count,
                pending = pending_count,
                "ledger cache initialized"
            );
            Arc::new(LedgerCache::new(block_count, account_count, pending_count))
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
            recovery_scratch,
            brn_engine: Arc::new(Mutex::new(brn_engine)),
            trst_engine: Arc::new(Mutex::new(TrstEngine::with_expiry(trst_expiry))),
            governance: {
                let batch = store.tx_begin_write()?;
                match crate::governance_storage::load(&batch) {
                    Ok(Some(engine)) => {
                        tracing::info!(
                            proposals = engine.all_proposals().count(),
                            "loaded GovernanceEngine state from LMDB"
                        );
                        Arc::new(Mutex::new(engine))
                    }
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
            recently_confirmed,
            vote_generator,
            rep_weights,
            confirming_set,
            backlog,
            local_broadcaster,
            fork_cache,
            vote_spacing,
            request_aggregator,
            syn_cookies,
            online_weight_sampler,
            message_dedup,
            clock_sync,
            delegation_engine,
            vrf_client,
            delegation_store: Arc::new(burst_store::delegation::InMemoryDelegationStore::new()),
            verification_orchestrator,
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
            #[cfg(test)]
            admission_group_probe: None,
        };

        Ok(node)
    }

    /// Load the in-memory frontier from the persistent frontier store.
    fn load_frontier_from_store(store: &LmdbStore) -> Result<DagFrontier, NodeError> {
        let frontier_store = store.frontier_store();
        let mut frontier = DagFrontier::new();

        match frontier_store.all_frontiers() {
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
                let batch = self.store.tx_begin_write()?;
                if crate::governance_storage::present(&batch)?
                    || batch.uses_verification_records()?
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
        batch.initialize_brn_wallet_records_if_empty()?;
        crate::ledger_updater::persist_economics(&mut batch, &brn, &trst)
            .map_err(NodeError::Other)?;
        // A new ledger has no provisional journals to translate. Start with
        // stable holdings so ordinary payments use the bounded storage path.
        crate::trst_storage::stage_replayed_stable(&mut batch, &trst)?;
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
        let diagnostic_port = self.config.port;
        self.connection_registry.write().await.start_vote_dispatch(
            self.active_elections.clone(), self.rep_weights.clone(),
        );
        let fork_install_tx = {
            self.block_processor.lock().await.historical_finality = self
                .connection_registry
                .read()
                .await
                .historical_finality
                .clone();
            let block_processor = Arc::clone(&self.block_processor);
            let frontier = Arc::clone(&self.frontier);
            let store = Arc::clone(&self.store);
            let metrics = Arc::clone(&self.metrics);
            let mut shutdown_rx = self.shutdown.subscribe();
            let shutdown = self.shutdown.clone();
            let block_queue = Arc::clone(&self.block_queue);
            let submission_results = Arc::clone(&self.submission_results);
            let governance_transfers = Arc::clone(&self.governance_transfers);
            let active_elections = Arc::clone(&self.active_elections);
            let broadcaster = self.broadcaster.clone();
            let peer_manager = Arc::clone(&self.peer_manager);

            let backlog_queue = Arc::clone(&self.backlog);
            let brn_engine = Arc::clone(&self.brn_engine);
            let trst_engine = Arc::clone(&self.trst_engine);
            let ledger_cache = Arc::clone(&self.ledger_cache);
            let mut params = self.config.params.clone();
            let network = self.config.network;
            let fork_cache = Arc::clone(&self.fork_cache);
            let websocket = Arc::clone(&self.ws_state);
            let governance = Arc::clone(&self.governance);
            let delegation = Arc::clone(&self.delegation_engine);
            let verification = Arc::clone(&self.verification_orchestrator);
            let certified_selection = self.certified_selection_enabled();
            let priority_scheduler = Arc::clone(&self.priority_scheduler);
            let constitution = Arc::clone(&self.consti_engine);
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

            let chain_identity = (
                self.config.network,
                crate::genesis_state::block(self.config.network, &self.initial_params).hash,
            );
            let fork_governance = crate::fork_resolution::GovernanceRuntime {
                genesis: chain_identity.1,
                governance: self.governance.clone(),
                delegation: self.delegation_engine.clone(),
                constitution: self.consti_engine.clone(),
            };
            let mut validation_queue = crate::block_validation::BlockValidationQueue::new(
                block_queue.clone(),
                block_processor.lock().await.authentication_rules(),
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
            self.track_task("historical admissions", history_task);
            if self.certified_selection_enabled() {
                let cleanup = crate::checkpoint_store::record_storage::maintenance::start(
                    self.store.clone(), self.shutdown.clone(),
                );
                self.track_task("shared checkpoint cleanup", cleanup);
            }
            let local_votes = self.spawn_local_votes();
            #[cfg(test)]
            let admission_group_probe = self.admission_group_probe.clone();
            let block_processor_task = admission_worker::spawn(async move {
                let mut checkpoint_tick = tokio::time::interval(Duration::from_secs(1));
                checkpoint_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                let mut last_checkpoint_error: Option<String> = None;
                let mut serial_pending = std::collections::VecDeque::new();
                let admission_stage_log = std::env::var_os("BURST_FINALITY_STAGE_LOG").is_some();
                loop {
                    // Select the next origin and its highest-work block.
                    // Use select! to remain responsive to shutdown signals.
                    validation_queue.set_preparation(store.clone(), params.clone());
                    let mut authentication = tokio::select! {
                        biased;
                        _ = shutdown_rx.recv() => {
                            tracing::info!("block processor task shutting down");
                            break;
                        }
                        Some(request) = snapshot_install_rx.recv(), if certified_selection => {
                            if request.reply.is_closed() {
                                continue;
                            }
                            let epoch = request.candidate.anchor().hash();
                            let result = match checkpoint_runtime.install_snapshot(request.candidate).await {
                                Ok(installed_params) => {
                                    params = installed_params;
                                    validation_queue.set_rules(block_processor.lock().await.authentication_rules());
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
                        }, if certified_selection => {
                            match checkpoint_runtime.install_next().await {
                                Ok(Some(installed_params)) => {
                                    params = installed_params;
                                    validation_queue.set_rules(block_processor.lock().await.authentication_rules());
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
                            match fork_runtime.install(winner, &params, network,
                                certified_selection.then_some(&verification),
                                certified_selection.then_some(&fork_governance)).await {
                                Ok(released) => for dependent in released {
                                    if !block_queue.try_push_retained(dependent) {
                                        tracing::warn!(%winner, "block queue full after fork replacement");
                                    }
                                },
                                Err(error) => tracing::debug!(%winner, %error, "fork replacement deferred"),
                            }
                            continue;
                        }
                        result = async {
                            match serial_pending.pop_front() {
                                Some(candidate) => Ok(candidate),
                                None => validation_queue.next().await,
                            }
                        } => match result {
                            Ok(block) => block,
                            Err(error) => {
                                tracing::error!(%error, "block authentication worker failed; stopping node");
                                checkpoint_runtime.shutdown.fail("block authentication", &error);
                                break;
                            }
                        },
                    };
                    let block = authentication.block().clone();

                    let start = std::time::Instant::now();
                    let publication_stage = |phase: &str, hash: BlockHash| {
                        if admission_stage_log {
                            tracing::debug!(node_port = diagnostic_port,
                                queue = Arc::as_ptr(&block_queue) as usize, %hash, phase,
                                elapsed_us = start.elapsed().as_micros(), "admission publication stage");
                        }
                    };
                    tracing::debug!(node_port = diagnostic_port, hash = %block.hash,
                        authentication_ms = authentication.authentication_time().as_millis(),
                        preparation_ms = authentication.preparation_time.as_millis(),
                        "block authentication result reached admission");
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
                        let mut processor = block_processor.lock().await;
                        let f = frontier.read().await;
                        processor.validate_candidate_with_authentication(&authentication, &f)
                    };
                    tracing::debug!(node_port = diagnostic_port, hash = %block.hash, elapsed_ms = start.elapsed().as_millis(),
                        "block admission validation finished");
                    match &admission {
                        ProcessResult::Accepted | ProcessResult::Fork => (),
                        ProcessResult::GapSource if certified_selection => {
                            let _ = historical_dependencies.try_send(block.clone());
                            continue;
                        }
                        ProcessResult::Rejected(reason) => {
                            tracing::debug!(hash = %block.hash, account = %block.account, %reason,
                                "block admission rejected");
                            if certified_selection && reason.contains("backdating rejected") {
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
                            governance_transfers.remove(block.hash);
                            if let Ok(payload) = bincode::serialize(&block) {
                                submission_results
                                    .complete(&payload, RpcProcessResult::Rejected(reason.clone()));
                            }
                            continue;
                        }
                        _ => continue,
                    }

                    let proposal_content = if crate::governance_content::is_full(&block) {
                        if !crate::governance_content::enabled(network) {
                            if let Ok(payload) = bincode::serialize(&block) {
                                submission_results.complete(
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
                            governance_transfers.remove(block.hash);
                            if let Ok(payload) = bincode::serialize(&block) {
                                submission_results.complete(
                                    &payload,
                                    RpcProcessResult::Rejected(
                                        "full proposal requires a verified proposer".into(),
                                    ),
                                );
                            }
                            continue;
                        }
                        let ready = authentication.growth_fork_content.clone().map_or_else(
                            || governance_transfers.ready(block.hash), |content| Ok(Some(content)),
                        ).and_then(|ready| {
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
                                if let Err(error) = governance_transfers.defer(&block) {
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
                    let previous_block = if !block.previous.is_zero() {
                        store
                            .block_store()
                            .get_block(&block.previous)
                            .ok()
                            .and_then(|bytes| bincode::deserialize::<StateBlock>(&bytes).ok())
                    } else {
                        None
                    };
                    let previous_brn_spent = previous_block.as_ref().map_or(0, |b| b.brn_spent);

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
                        let admission_time = block_processor.lock().await.admission_time();
                        let _governance = if certified_selection {
                            Some((
                                fork_governance.governance.lock().await,
                                fork_governance.delegation.lock().await,
                                fork_governance.constitution.lock().await,
                            ))
                        } else {
                            None
                        };
                        let _brn = brn_engine.lock().await;
                        let _trst = trst_engine.lock().await;
                        let _verification = if certified_selection {
                            Some(verification.lock().await)
                        } else {
                            None
                        };
                        let mut fork_pressure = None;
                        let evaluated = crate::fork_resolution::evaluate_retry(
                            &store,
                            &block,
                            &params,
                            admission_time,
                            network,
                            certified_selection,
                            certified_selection.then_some(crate::fork_resolution::GovernanceInput {
                                genesis: chain_identity.1,
                                content: proposal_content.as_ref().map(|bytes| bytes.as_slice()),
                            }),
                        )
                        .map_err(|failure| {
                            fork_pressure = failure.pressure;
                            failure.error
                        })
                        .and_then(|target| {
                            if target.is_some() {
                                let bytes = bincode::serialize(&block).map_err(|e| {
                                    burst_store::StoreError::Serialization(e.to_string())
                                })?;
                                new_fork_payload =
                                    store.meta_store().fork_candidate(&block.hash)?.is_none();
                                store.meta_store().put_fork_candidate_with_content_retry(
                                    &block.hash,
                                    &bytes,
                                    proposal_content.as_ref().map(|bytes| bytes.as_slice()),
                                ).map_err(|failure| {
                                    fork_pressure = failure.pressure;
                                    failure.error
                                })?;
                            }
                            Ok(target.is_some())
                        });
                        match evaluated {
                            Ok(fork) => admitted_fork = fork,
                            Err(error) => {
                                if let Some((watch, permit)) = fork_pressure {
                                    drop(_verification);
                                    drop(_trst);
                                    drop(_brn);
                                    drop(_governance);
                                    if !admission_group::wait_growth_release(watch, &shutdown).await {
                                        break;
                                    }
                                    authentication._growth_retry = Some(permit);
                                    authentication.growth_fork_content = proposal_content;
                                    serial_pending.push_front(authentication);
                                    continue;
                                }
                                tracing::debug!(hash = %block.hash, %error, "fork not admitted");
                                if let Ok(payload) = bincode::serialize(&block) {
                                    submission_results.complete(
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
                    let balance_rejected = previous_block.as_ref().and_then(|prev| {
                        BlockProcessor::validate_balance_transition(
                            &block,
                            prev.brn_spent,
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
                    // increase must equal its amount exactly. Provenance and
                    // economic validity are checked by the candidate transition.
                    let receive_rejected = if block.block_type == BlockType::Receive {
                        let send_hash = burst_types::TxHash::new(*block.link.as_bytes());
                        match store
                            .pending_store()
                            .get_pending(&block.account, &send_hash)
                        {
                            Ok(pending) => {
                                let prev_trst = previous_block.as_ref().map_or(0, |b| b.trst_balance);
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
                        if block.block_type == BlockType::VerificationVote && certified_selection {
                            let orchestrator = verification.lock().await;
                            crate::verification_vote::validate(
                                &block,
                                previous_brn_spent,
                                &orchestrator,
                                &params,
                            )
                            .err()
                        } else if block.block_type == BlockType::Challenge && certified_selection {
                            None
                        } else {
                            crate::verification_vote::availability_error(
                                &block.block_type,
                                network,
                            )
                        };

                    let result = if block.block_type == BlockType::GovernanceActivation
                    {
                        ProcessResult::Rejected(
                            "governance activation requires its certified boundary".into(),
                        )
                    } else if !certified_selection && matches!(block.block_type,
                        BlockType::GovernanceProposal | BlockType::GovernanceVote |
                        BlockType::GovernanceEndorse | BlockType::GovernanceWithdraw | BlockType::GovernanceVeto) {
                        ProcessResult::Rejected("governance requires certified activation; unavailable behind the release gate".into())
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
                            submission_results
                                .complete(&payload, RpcProcessResult::Rejected(reason.clone()));
                        }
                    }

                    match &result {
                        ProcessResult::Accepted => {
                            #[cfg(test)]
                            if let Some(probe) = &admission_group_probe {
                                probe.install_ready(block.hash, &mut validation_queue);
                            }
                            let mut grouped = Vec::new();
                            let mut group_gap_positions = Vec::new();
                            if serial_pending.is_empty() && admission_group::eligible(&authentication) {
                                let mut budget = admission_group::Budget::new(&block);
                                while budget.has_capacity() {
                                    let Some(next) = validation_queue.try_next_ready() else {
                                        break;
                                    };
                                    let candidate = match next {
                                        Ok(candidate) => candidate,
                                        Err(error) => {
                                            checkpoint_runtime
                                                .shutdown
                                                .fail("block authentication", &error);
                                            break;
                                        }
                                    };
                                    let admission = if admission_group::eligible(&candidate)
                                        && budget.include(candidate.block())
                                    {
                                        let mut processor = block_processor.lock().await;
                                        let current = frontier.read().await;
                                        Some(
                                            processor
                                                .validate_candidate_with_authentication(&candidate, &current),
                                        )
                                    } else {
                                        None
                                    };
                                    match admission {
                                        Some(ProcessResult::Accepted) => grouped.push(candidate),
                                        Some(ProcessResult::Gap | ProcessResult::GapSource) => {
                                            // The budget includes this examined account and payload.
                                            // Preserve ordinary gap handling while considering only
                                            // already-ready, independent candidates behind it.
                                            group_gap_positions.push(grouped.len());
                                            serial_pending.push_back(candidate);
                                        }
                                        _ => {
                                            serial_pending.push_back(candidate);
                                            break;
                                        }
                                    }
                                }
                            }
                            let group_attempt = !grouped.is_empty();
                            let mut grouped_economics = Vec::new();
                            // Keep awaits outside the LMDB transaction. Representative
                            // weights are rebuilt separately; this batch holds no weight lock.
                            // Governance tick takes governance -> delegation. Keep
                            // that order here before acquiring economic state.
                            let mut live_governance =
                                if crate::governance_commit::CandidateGovernance::applies_to(
                                    &block.block_type,
                                ) {
                                    Some((
                                        governance.lock().await,
                                        delegation.lock().await,
                                        constitution.lock().await,
                                    ))
                                } else {
                                    None
                                };
                            let mut staged_governance = None;
                            let mut live_brn = brn_engine.lock().await;
                            let mut live_trst = trst_engine.lock().await;
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
                                Some(verification.lock().await)
                            } else {
                                None
                            };
                            let mut staged_orchestrator = None;
                            let mut commit_attempted = false;
                            let mut growth_outcome = None;
                            let mut growth_retry = None;
                            tracing::debug!(node_port = diagnostic_port, hash = %block.hash, elapsed_ms = start.elapsed().as_millis(),
                                "block commit locks acquired");
                            let commit = (|| -> Result<crate::economic_candidate::StagedBlockEffects, String> {
                                if let Some(error) = authentication.preparation_fault.take() {
                                    return Err(error);
                                }
                                let mut batch = if group_attempt { store.tx_begin_block_group() }
                                    else { store.tx_begin_write() }.map_err(|e| e.to_string())?;
                                growth_outcome = batch.growth_outcome();
                                growth_retry = batch.reserve_growth_retry().map_err(|e| e.to_string())?;
                                tracing::debug!(node_port = diagnostic_port, hash = %block.hash,
                                    elapsed_us = start.elapsed().as_micros(), "block writer acquired");
                                if live_orchestrator.is_some() {
                                    staged_orchestrator = Some(crate::endorsement_commit::restore(&batch)?);
                                }
                                if live_governance.is_some() {
                                    staged_governance = Some(crate::governance_commit::CandidateGovernance::restore(
                                        &batch, params.clone(), chain_identity.0, chain_identity.1,
                                    )?);
                                }
                                let prepared = match authentication.prepared.take() {
                                    Some(prepared) => match prepared.stage_with_economics(&mut batch, &params) {
                                        Ok(staged) => staged,
                                        Err(crate::economic_candidate::prepared::PreparedStageError::BeforeWrite(error)) => {
                                            tracing::debug!(hash = %block.hash, %error, "prepared payment dependencies changed; validating current state");
                                            None
                                        }
                                        Err(crate::economic_candidate::prepared::PreparedStageError::Abort(error)) => return Err(error),
                                    },
                                    None => None,
                                };
                                let prepared_persisted = prepared.is_some();
                                let economics = if let Some(prepared) = prepared {
                                    engines.adopt_persisted(prepared.state.brn, prepared.state.trst);
                                    prepared.economics
                                } else {
                                    if crate::economic_candidate::prepared::CapturedPayment::eligible(&block.block_type) {
                                        // Busy-account candidates skipped speculative capture. Their
                                        // serial path must still detect authoritative snapshot faults.
                                        let current = crate::payment_transition::restore(&batch, &params)
                                            .map_err(|error| error.to_string())?;
                                        let (brn, trst) = engines.mutable();
                                        *brn = current.brn;
                                        *trst = current.trst;
                                    }
                                    crate::economic_candidate::stage(&mut batch, &block, &params, &mut engines)?
                                };
                                if let Some(bytes) = &proposal_content {
                                    crate::governance_content::stage(&mut batch, &block, bytes)?;
                                }
                                if let Some(orch) = staged_orchestrator.as_mut() {
                                    if crate::verification_transition::supports(&block.block_type) {
                                        crate::verification_transition::apply_staged(&mut batch, &block, &economics.effect,
                                            &params, network, orch, &mut engines)?;
                                    }
                                }
                                if let Some(governance) = staged_governance.as_mut() {
                                    governance.apply_staged(&mut batch, &block, &economics.effect, &mut engines)?;
                                    if block.block_type == BlockType::GovernanceActivation {
                                        let orch = staged_orchestrator.as_mut().ok_or("missing activation verification state")?;
                                        let before = orch.storage_clone();
                                        orch.begin_record_changes();
                                        orch.apply_governed_rules(&governance.params);
                                        crate::verification_storage::persist(&mut batch, &before, orch)
                                            .map_err(|e| e.to_string())?;
                                    }
                                }
                                crate::bootstrap_phase::observe(&mut batch,
                                    staged_governance.as_ref().map_or(&params, |governance| &governance.params))?;
                                if !prepared_persisted || staged_orchestrator.is_some() || staged_governance.is_some() {
                                    crate::ledger_updater::refresh_token_counters(&mut batch, &block.account, crate::economic_candidate::Engines::trst(&engines), block.timestamp)?;
                                }
                                engines.persist(&mut batch)?;
                                for candidate in &mut grouped {
                                    let staged = admission_group::stage(&mut batch, candidate, &params)?;
                                    engines.adopt_persisted(staged.state.brn, staged.state.trst);
                                    grouped_economics.push(staged.economics);
                                }
                                #[cfg(test)]
                                if group_attempt {
                                    if let Some(probe) = &admission_group_probe {
                                        probe.staged(block.hash, &mut batch, &grouped)?;
                                    }
                                }
                                commit_attempted = true;
                                tracing::debug!(node_port = diagnostic_port, hash = %block.hash,
                                    elapsed_us = start.elapsed().as_micros(), "block writes staged");
                                batch.commit().map_err(|e|e.to_string())?;
                                tracing::debug!(node_port = diagnostic_port, hash = %block.hash,
                                    count = grouped.len() + 1,
                                    elapsed_us = start.elapsed().as_micros(), "block transaction committed");
                                governance_transfers.remove(block.hash);
                                Ok(economics)
                            })();
                            let economics = match commit {
                                Ok(economics) => economics,
                                Err(error) => {
                                    if let Some(outcome) = growth_outcome.filter(|outcome| outcome.was_rejected()) {
                                        let permit = growth_retry.take().expect("guarded writer reserved retry before staging");
                                        drop(engines);
                                        drop(staged_governance);
                                        drop(staged_orchestrator);
                                        drop(grouped_economics);
                                        drop(live_orchestrator);
                                        drop(live_trst);
                                        drop(live_brn);
                                        drop(live_governance);
                                        #[cfg(test)]
                                        if let Some(probe) = &admission_group_probe { probe.aborted(block.hash, commit_attempted); }
                                        if !admission_group::wait_growth_release(outcome.watch(), &shutdown).await {
                                            break;
                                        }
                                        authentication._growth_retry = Some(permit.clone());
                                        for candidate in &mut grouped { candidate._growth_retry = Some(permit.clone()); }
                                        tracing::debug!(hash = %block.hash, "capture pressure aborted admission; retrying after owned reader release");
                                        admission_group::retry_in_order(authentication, grouped, group_gap_positions, &mut serial_pending);
                                        continue;
                                    }
                                    if group_attempt && !commit_attempted {
                                        #[cfg(test)]
                                        if let Some(probe) = &admission_group_probe {
                                            probe.aborted(block.hash, commit_attempted);
                                        }
                                        tracing::debug!(hash = %block.hash, %error,
                                            "admission group aborted; retrying in serial order");
                                        admission_group::retry_in_order(
                                            authentication,
                                            grouped,
                                            group_gap_positions,
                                            &mut serial_pending,
                                        );
                                        continue;
                                    }
                                    tracing::error!(hash = %block.hash, %error, "block persistence failed; candidate not published");
                                    if !commit_attempted {
                                        if let Ok(payload) = bincode::serialize(&block) {
                                            submission_results
                                                .complete(&payload, RpcProcessResult::Rejected(error));
                                        }
                                    }
                                    continue;
                                }
                            };
                            tracing::debug!(node_port = diagnostic_port, hash = %block.hash, elapsed_ms = start.elapsed().as_millis(),
                                "block durable commit finished");
                            if let (Some((gov, del, consti)), Some(staged)) =
                                (live_governance.as_mut(), staged_governance)
                            {
                                **gov = staged.governance;
                                **del = staged.delegation;
                                **consti = staged.constitution;
                                params = staged.params;
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
                            drop(live_trst);
                            publication_stage("engines_published", block.hash);
                            let publications: Vec<_> = std::iter::once((&block, economics))
                                .chain(
                                    grouped
                                        .iter()
                                        .map(|candidate| candidate.block())
                                        .zip(grouped_economics),
                                )
                                .collect();
                            for (block, economics) in &publications {
                                let deferred_pending = &economics.pending;
                                if block.hash != authentication.block().hash {
                                    metrics.blocks_processed.inc();
                                }
                                {
                                    let mut processor = block_processor.lock().await;
                                    let mut f = frontier.write().await;
                                    processor.set_params_hash(params.params_hash());
                                    processor.publish_committed(&block, &mut f);
                                }
                                publication_stage("frontier_published", block.hash);
                                ledger_cache.inc_block_count();
                                if block.block_type == BlockType::Open {
                                    ledger_cache.inc_account_count();
                                }
                                if deferred_pending.is_some() {
                                    ledger_cache.inc_pending_count();
                                }
                                if block.block_type == BlockType::Receive
                                    || (block.block_type == BlockType::Open && block.trst_balance > 0)
                                {
                                    ledger_cache.dec_pending_count();
                                }
                            }
                            drop(live_brn);
                            for (block, _) in publications {
                                // Publish scheduler work and release dependencies only
                                // after all durable state for this block has committed.
                                {
                                    let mut backlog = backlog_queue.lock().await;
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
                                publication_stage("backlog_tracked", block.hash);
                                priority_scheduler.lock().await.push(
                                    block.hash,
                                    block.account.clone(),
                                    block.trst_balance,
                                );
                                publication_stage("scheduler_queued", block.hash);
                                let released = {
                                    let mut processor = block_processor.lock().await;
                                    processor.release_retained(&block.hash)
                                };
                                for dep in released {
                                    if !block_queue.push_retained(dep).await {
                                        tracing::warn!(parent = %block.hash, "block queue full re-enqueuing unchecked dependent");
                                    }
                                }

                                publication_stage("dependencies_released", block.hash);
                                // Track acceptance (NOT confirmation — that happens via consensus)
                                if let Err(error) = crate::backlog_scan::register_candidate(
                                    &store,
                                    &mut *active_elections.write().await,
                                    &block,
                                    Timestamp::new(unix_now_secs()),
                                ) {
                                    tracing::debug!(hash = %block.hash, %error, "election deferred to scheduler");
                                }
                                publication_stage("election_registered", block.hash);
                                metrics.blocks_accepted.inc();
                                tracing::debug!(node_port = diagnostic_port, hash = %block.hash, elapsed_ms = start.elapsed().as_millis(),
                                    "block accepted and persisted");
                                if let Ok(payload) = bincode::serialize(&block) {
                                    submission_results
                                        .complete(&payload, RpcProcessResult::Accepted);
                                }

                                // Account update is provisional; acceptance is not finality.
                                websocket.publish_account_update(
                                    &block.account.to_string(),
                                    &block.trst_balance.to_string(),
                                    &format!("{:?}", block.block_type),
                                );

                                // Every node publishes accepted blocks, including nodes
                                // that do not vote or whose vote is currently spaced.
                                if let Ok(bytes) = WireMessage::Block(Box::new(block.clone())).encode()
                                {
                                    let peers: Vec<_> = peer_manager
                                        .read()
                                        .await
                                        .iter_connected()
                                        .map(|(_, peer)| peer.clone())
                                        .collect();
                                    let _ = broadcaster
                                        .broadcast_with_fanout(&bytes, &peers, 4)
                                        .await;
                                }

                                local_votes::enqueue(&local_votes, block.hash);
                            }
                        }
                        ProcessResult::Fork => {
                            let root = crate::ledger_updater::election_root(&block);
                            if let Ok(payload) = bincode::serialize(&block) {
                                submission_results.complete(&payload, RpcProcessResult::Fork);
                            }
                            if new_fork_payload {
                                let peers: Vec<_> = peer_manager
                                    .read()
                                    .await
                                    .iter_connected()
                                    .map(|(_, peer)| peer.clone())
                                    .collect();
                                if let Ok(bytes) = WireMessage::Block(Box::new(block.clone())).encode()
                                {
                                    let _ = broadcaster
                                        .broadcast_with_fanout(&bytes, &peers, 4)
                                        .await;
                                }
                            }
                            // Cache the fork block for election consideration
                            {
                                let mut fc = fork_cache.lock().await;
                                fc.insert(root, block.hash);
                            }
                            // Join the existing election for this account position.
                            let now = Timestamp::new(unix_now_secs());
                            {
                                let mut elections = active_elections.write().await;
                                if let Err(e) = elections.register_candidate(root, block.hash, now) {
                                    tracing::debug!(root = %root, error = %e, "could not start election for fork");
                                } else {
                                    tracing::info!(root = %root, fork_hash = %block.hash, "election started for fork");
                                }
                            }

                            // Queue a local vote for the incumbent at this account
                            // position. Fork arrival does not pass through the normal
                            // accepted-block vote path. Other representatives may vote
                            // independently; this local vote does not establish quorum.
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
                                    let _ = active_elections
                                        .write()
                                        .await
                                        .register_candidate(root, accepted, now);
                                    local_votes::enqueue(&local_votes, accepted);
                                }
                            }
                        }
                        _ => {
                            tracing::debug!(hash = %block.hash, ?result, "block not accepted");
                        }
                    }

                    let elapsed =
                        (start.elapsed() + authentication.worker_time()).as_secs_f64() * 1000.0;
                    metrics.block_process_time_ms.observe(elapsed);
                }
            });
            self.track_task("block processor", block_processor_task);
            fork_install_tx
        };

        let active_elections_confirmation = Arc::clone(&self.active_elections);
        let recently_confirmed_cementation = Arc::clone(&self.recently_confirmed);
        let metrics_confirmation = Arc::clone(&self.metrics);
        let ws_state_cementation = Arc::clone(&self.ws_state);
        let mut shutdown_rx_confirmation = self.shutdown.subscribe();
        let vote_generator_confirmation = Arc::clone(&self.vote_generator);
        let broadcaster_confirmation = self.broadcaster.clone();
        let peer_manager_confirmation = Arc::clone(&self.peer_manager);
        let rep_weights_confirmation = Arc::clone(&self.rep_weights);
        let store_confirmation = Arc::clone(&self.store);
        let backlog_cementation = Arc::clone(&self.backlog);
        let local_broadcaster_cementation = Arc::clone(&self.local_broadcaster);
        let historical_confirmation = self
            .connection_registry
            .read()
            .await
            .historical_finality
            .clone();

        let confirmation_ready = Arc::new(tokio::sync::Notify::new());
        let handoff_ready = Arc::new(tokio::sync::Notify::new());
        let wake = Arc::clone(&confirmation_ready);
        let handoff_wake = Arc::clone(&handoff_ready);
        self.active_elections
            .write()
            .await
            .set_confirmation_wake(Arc::new(move || {
                wake.notify_one();
                handoff_wake.notify_one();
            }));
        let cementation_ready = Arc::new(tokio::sync::Notify::new());
        self.spawn_confirmation_handoff(handoff_ready, cementation_ready.clone(), fork_install_tx);
        let confirmation_handle = tokio::spawn(async move {
            let stage_log = std::env::var_os("BURST_FINALITY_STAGE_LOG").is_some();
            let mut interval = repair_timer::interval(Duration::from_millis(500));
            let mut historical_request_cursor = 0usize;
            let mut last_request = std::time::Instant::now();
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_confirmation.recv() => {
                        tracing::info!("confirmation task shutting down");
                        break;
                    }
                    repair = async {
                        tokio::select! {
                            _ = interval.tick() => true,
                            _ = confirmation_ready.notified() => false,
                        }
                    } => {
                        let phase_started = stage_log.then(std::time::Instant::now);
                        if stage_log {
                            tracing::debug!(node_port = diagnostic_port, repair, "confirmation phase awakened");
                        }
                        if repair && last_request.elapsed() >= Duration::from_secs(1) {
                            if stage_log {
                                tracing::debug!(node_port = diagnostic_port, "confirmation request repair started");
                            }
                            last_request = std::time::Instant::now();
                            let hashes = confirmation_requests::select(
                                &mut *active_elections_confirmation.write().await,
                                historical_confirmation.awaiting_votes(),
                                &mut historical_request_cursor,
                            );
                            if !hashes.is_empty() {
                                let requested = hashes.len();
                                for hash in &hashes {
                                    let vote = {
                                        let generator = vote_generator_confirmation.clone().lock_owned().await;
                                        let elections = active_elections_confirmation.read().await;
                                        let already_voted = elections.candidate_root(hash).and_then(|root| elections.get_election(&root))
                                            .is_some_and(|election| election.last_votes.contains_key(&generator.representative));
                                        drop(elections);
                                        if already_voted { continue; }
                                        crate::durable_votes::worker::respond(store_confirmation.clone(), generator, *hash).await
                                    };
                                    match vote {
                                        Ok(Some(vote)) => {
                                            let weight = rep_weights_confirmation.read().await.weight(&vote.voter);
                                            let mut elections = active_elections_confirmation.write().await;
                                            if let Some(root) = elections.candidate_root(hash) {
                                                let _ = elections.process_signed_vote(&root, burst_consensus::VoteInfo::new(
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
                                    let peers: Vec<_> = peer_manager_confirmation.read().await.iter_connected().map(|(_, peer)| peer.clone()).collect();
                                    let result = broadcaster_confirmation.broadcast_with_fanout(&bytes, &peers, 4).await;
                                    tracing::debug!(requested, queued = result.queued, failed = result.failed, "requested missing consensus votes");
                                }
                            }
                            if let Some(started) = phase_started {
                                tracing::debug!(node_port = diagnostic_port,
                                    elapsed_us = started.elapsed().as_micros(), "confirmation request repair finished");
                            }
                        }
                        let roots = active_elections_confirmation.write().await.confirmation_roots(repair, 256);
                        if let Some(started) = phase_started {
                            tracing::debug!(node_port = diagnostic_port, repair, roots = roots.len(),
                                elapsed_us = started.elapsed().as_micros(), "confirmation roots selected");
                        }
                        // Soft quorum selects the candidate for final voting.
                        // Confirmation still requires a quorum of final votes.
                        {
                            let (our_rep, is_rep) = {
                                let generator = vote_generator_confirmation.lock().await;
                                (generator.representative.clone(), generator.is_representative)
                            };
                            if let Some(started) = phase_started {
                                tracing::debug!(node_port = diagnostic_port,
                                    elapsed_us = started.elapsed().as_micros(), "confirmation signer identity read");
                            }
                            if is_rep {
                                let ready = {
                                    let elections = active_elections_confirmation.read().await;
                                    elections.soft_quorum_for_roots(&roots, &our_rep, repair)
                                };
                                if !ready.is_empty() {
                                    let our_weight =
                                        { rep_weights_confirmation.read().await.weight(&our_rep) };
                                    let now = Timestamp::new(unix_now_secs());
                                    for group in ready.chunks(burst_store_lmdb::meta::LmdbMetaStore::MAX_CONSENSUS_RESERVATIONS) {
                                        if stage_log {
                                            for (root, winner) in group {
                                                tracing::debug!(node_port = diagnostic_port, %root, hash = %winner,
                                                    voter = %our_rep, repair, "final vote candidate ready");
                                            }
                                        }
                                        let generation = active_elections_confirmation.read().await.dependency_generation();
                                        let votes = {
                                            let started = stage_log.then(std::time::Instant::now);
                                            let generator = vote_generator_confirmation.clone().lock_owned().await;
                                            if let Some(started) = started {
                                                tracing::debug!(node_port = diagnostic_port, hash = %group[0].1,
                                                    count = group.len(), elapsed_us = started.elapsed().as_micros(),
                                                    "final vote signer acquired");
                                            }
                                            let hashes: Vec<_> = group.iter().map(|(_, winner)| *winner).collect();
                                            match crate::durable_votes::worker::finals(store_confirmation.clone(), generator, hashes).await {
                                                Ok(votes) => votes,
                                                Err(error) => {
                                                    tracing::error!(%error, "final vote worker failed; elections retained");
                                                    continue;
                                                }
                                            }
                                        };
                                        let mut results = group.iter().copied().zip(votes).peekable();
                                        while let Some(((root, winner), result)) = results.next() {
                                            let final_vote = match result {
                                                Ok(vote) => vote,
                                                Err(crate::durable_votes::VoteGenerationError::DependenciesPending(dependencies)) => {
                                                    let waits = dependency_waits::take_run(
                                                        (root, winner, dependencies), &mut results);
                                                    if stage_log {
                                                        for (_, winner, dependencies) in &waits {
                                                            tracing::debug!(node_port = diagnostic_port, hash = %winner,
                                                                dependencies = ?dependencies, "final vote dependencies pending");
                                                        }
                                                    }
                                                    let mut applied = 0;
                                                    while applied < waits.len() {
                                                        applied += active_elections_confirmation.write().await
                                                            .wait_for_dependencies_batch(&waits[applied..], generation);
                                                    }
                                                    continue;
                                                },
                                                Err(error) => {
                                                    tracing::error!(%winner, %error, "refusing conflicting or non-durable final vote");
                                                    continue;
                                                }
                                            };
                                            if stage_log {
                                                tracing::debug!(node_port = diagnostic_port, hash = %winner,
                                                    voter = %final_vote.voter, "final vote signed durably");
                                            }
                                            // Count our own final weight locally.
                                            {
                                                let mut elections = active_elections_confirmation.write().await;
                                                elections.clear_dependency_wait(&root);
                                                let _ = elections.process_signed_vote(
                                                    &root, burst_consensus::VoteInfo::new(our_rep.clone(), winner,
                                                        our_weight, true, Timestamp::new(final_vote.timestamp / 1000), final_vote.sequence), now,
                                                );
                                            }
                                            let wire = WireMessage::Vote(WireVote {
                                                voter: final_vote.voter,
                                                block_hashes: vec![final_vote.block_hash],
                                                is_final: true,
                                                timestamp: final_vote.timestamp,
                                                sequence: final_vote.sequence,
                                                signature: final_vote.signature,
                                            });
                                            if let Ok(bytes) = wire.encode() {
                                                if stage_log {
                                                    crate::connection_registry::trace_final_vote_transport(
                                                        &bytes, "final_broadcast_attempt", &diagnostic_port.to_string());
                                                }
                                                let peers: Vec<burst_network::PeerState> = {
                                                    let peers = peer_manager_confirmation.read().await;
                                                    peers.iter_connected().map(|(_, s)| s.clone()).collect()
                                                };
                                                let _ = broadcaster_confirmation
                                                    .broadcast_with_fanout(&bytes, &peers, 4)
                                                    .await;
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        // Cleanup expired elections
                        let now = Timestamp::new(unix_now_secs());
                        if repair {
                            let mut elections = active_elections_confirmation.write().await;
                            let expired = elections.cleanup_expired(300_000, now);
                            if !expired.is_empty() {
                                tracing::debug!(
                                    count = expired.len(),
                                    "cleaned up expired elections"
                                );
                            }
                            // Update election count gauge
                            metrics_confirmation.election_count.set(elections.election_count() as i64);
                        }

                        if let Some(started) = phase_started {
                            tracing::debug!(node_port = diagnostic_port, repair,
                                elapsed_us = started.elapsed().as_micros(), "confirmation phase finished");
                        }
                    }
                }
            }
        });
        self.track_task("confirmation", confirmation_handle);

        let confirming_set_cementation = Arc::clone(&self.confirming_set);
        let store_cementation = Arc::clone(&self.store);
        let metrics_cementation = Arc::clone(&self.metrics);
        let rep_weights_cementation = Arc::clone(&self.rep_weights);
        let elections_cementation = Arc::clone(&self.active_elections);
        let mut shutdown_rx_cementation = self.shutdown.subscribe();

        let cementation_handle = tokio::spawn(async move {
            let worker = crate::causal_cementation::worker::Worker::new(store_cementation.clone());
            let stage_log = std::env::var_os("BURST_FINALITY_STAGE_LOG").is_some();
            let mut interval = repair_timer::interval(Duration::from_millis(100));
            let mut cursor: Option<BlockHash> = None;
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_cementation.recv() => {
                        tracing::info!("cementation task shutting down");
                        break;
                    }
                    _ = async {
                        tokio::select! {
                            _ = interval.tick() => {},
                            _ = cementation_ready.notified() => {},
                        }
                    } => {
                        let polled = std::time::Instant::now();
                        let batch = match worker.pending(cursor).await {
                            Ok(pending) => pending,
                            Err(error) => {
                                tracing::error!(%error, "cannot read durable confirmation queue");
                                continue;
                            }
                        };
                        if stage_log {
                            tracing::debug!(node_port = diagnostic_port, count = batch.len(),
                                elapsed_us = polled.elapsed().as_micros(), "cementation queue polled");
                        }
                        if !batch.is_empty() {
                            let cement_started = std::time::Instant::now();
                            tracing::debug!(node_port = diagnostic_port, count = batch.len(), "cementation group started");
                            let mut total_cemented: u64 = 0;
                            let (selected, outcome) = match worker.pending_group(cursor).await {
                                Ok(attempt) => (attempt.targets, attempt.outcome),
                                Err(error) => (batch.clone(), Err(error)),
                            };
                            let results: Vec<_> = match outcome {
                                Ok(Some(results)) => results.into_iter().map(|(hash, result)| (hash, Some(result))).collect(),
                                deferred_or_failed => {
                                    if let Err(error) = deferred_or_failed {
                                        tracing::warn!(%error, "cementation group aborted; isolating targets");
                                    }
                                    selected.iter().map(|&hash| (hash, None)).collect()
                                }
                            };
                            let shared_commit = results.iter().all(|(_, result)| result.is_some());
                            if shared_commit {
                                tracing::debug!(node_port = diagnostic_port, count = results.len(),
                                    elapsed_us = cement_started.elapsed().as_micros(), "cementation group committed");
                                if let Err(error) = rebuild_rep_weights(&store_cementation, &rep_weights_cementation, &elections_cementation).await {
                                    tracing::error!(%error, "cannot publish confirmed authority");
                                }
                            }
                            for (block_hash, committed) in results {
                                cursor = Some(block_hash);
                                let result = match committed {
                                    Some(result) => Ok(result),
                                    None => worker.single(block_hash).await,
                                };
                                match result {
                                    Ok(result) => {
                                        if !shared_commit {
                                            if let Err(error) = rebuild_rep_weights(&store_cementation, &rep_weights_cementation, &elections_cementation).await {
                                                tracing::error!(%error, "cannot publish confirmed authority");
                                            }
                                        }
                                        elections_cementation.write().await.dependencies_cemented(&result.hashes);
                                        if stage_log {
                                            for hash in &result.hashes {
                                                tracing::debug!(node_port = diagnostic_port, %hash,
                                                    "cemented dependency waiters notified");
                                            }
                                        }
                                        total_cemented += result.hashes.len() as u64;
                                        // Publish only after the heights and queue acknowledgment
                                        // have committed together. Causal ancestors are final too.
                                        for hash in &result.hashes {
                                            recently_confirmed_cementation.write().await.insert(*hash);
                                            backlog_cementation.lock().await.remove(hash);
                                            local_broadcaster_cementation.lock().await.confirmed(hash);
                                            metrics_cementation.blocks_confirmed.inc();
                                            if let Ok(bytes) = store_cementation.block_store().get_block(hash) {
                                                if let Ok(block) = bincode::deserialize::<StateBlock>(&bytes) {
                                                    elections_cementation.write().await.remove_root(&crate::ledger_updater::election_root(&block));
                                                    ws_state_cementation.publish_confirmation(block.account.as_str(), &hash.to_string(), &block.trst_balance.to_string());
                                                }
                                            }
                                        }
                                        tracing::debug!(node_port = diagnostic_port, hash = %block_hash, complete = result.complete, "committed causal cementation");
                                    }
                                    Err(error) => {
                                        tracing::error!(hash = %block_hash, %error, "causal cementation failed; retrying");
                                    }
                                }
                            }

                            if total_cemented > 0 {
                                if let Err(error) = worker.prune().await {
                                    tracing::error!(%error, "finalized fork payload cleanup failed; cache retained");
                                }
                                let cs = confirming_set_cementation.lock().await;
                                cs.record_cemented(total_cemented);
                                tracing::debug!(count = total_cemented, "cemented block batch");
                            }
                        }

                    }
                }
            }
        });
        self.track_task("cementation", cementation_handle);

        // Governance activates only at certified boundaries. Creator availability
        // and local wall-clock timers never authorize parameter changes.

        let local_broadcaster_rebroadcast = Arc::clone(&self.local_broadcaster);
        let broadcaster_rebroadcast = self.broadcaster.clone();
        let peer_manager_rebroadcast = Arc::clone(&self.peer_manager);
        let mut shutdown_rx_rebroadcast = self.shutdown.subscribe();

        let rebroadcast_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(500));
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_rebroadcast.recv() => {
                        tracing::info!("local re-broadcaster shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                        let now_ms = unix_now_ms();
                        let blocks = {
                            let mut broadcaster = local_broadcaster_rebroadcast.lock().await;
                            broadcaster.schedule_rebroadcasts(now_ms)
                        };
                        if !blocks.is_empty() {
                            let peers: Vec<burst_network::PeerState> = {
                                let peers = peer_manager_rebroadcast.read().await;
                                peers.iter_connected().map(|(_, s)| s.clone()).collect()
                            };
                            for (hash, block_bytes) in &blocks {
                                let _ = broadcaster_rebroadcast
                                    .broadcast_with_fanout(block_bytes, &peers, 4)
                                    .await;
                                tracing::trace!(%hash, "re-broadcast local block");
                            }
                            tracing::debug!(count = blocks.len(), "re-broadcast local blocks");
                        }
                        // Cleanup blocks that exhausted retries
                        {
                            let mut broadcaster = local_broadcaster_rebroadcast.lock().await;
                            broadcaster.remove_exhausted();
                        }
                    }
                }
            }
        });
        self.track_task("local rebroadcast", rebroadcast_handle);

        let mut outbound_rx = outbound_rx;
        let mut shutdown_rx_drain = self.shutdown.subscribe();
        let conn_registry_drain = Arc::clone(&self.connection_registry);

        let drain_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_drain.recv() => {
                        tracing::info!("outbound message task shutting down");
                        break;
                    }
                    Some((peer_id, msg_bytes)) = outbound_rx.recv() => {
                        crate::connection_registry::trace_final_vote_transport(
                            &msg_bytes, "outbound_dequeued", &peer_id);
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
        self.track_task("outbound messages", drain_handle);

        // Every 60 seconds, commit held-token expiry and clean a bounded batch
        // of expiry lookup rows. Certified mode defers pending ownership returns
        // to an agreed boundary; other modes include them in this task.
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

                        // Remove at most 1,024 expired lookup rows; economic state is separate.
                        let trst_index = store_expiry.trst_index_store();
                        match trst_index.clean_expiry_before(cutoff, 1024) {
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
        self.track_task("TRST expiry", expiry_handle);

        // Expired unchecked blocks are removed on the next one-minute sweep.
        let block_processor_cleanup = Arc::clone(&self.block_processor);
        let mut shutdown_rx_cleanup = self.shutdown.subscribe();

        let unchecked_cleanup_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_cleanup.recv() => {
                        tracing::info!("unchecked cleanup task shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                        let mut processor = block_processor_cleanup.lock().await;
                        let now = unix_now_secs();
                        let removed = processor.cleanup_unchecked(crate::unchecked::MAX_AGE_SECS, now);
                        if removed > 0 {
                            tracing::debug!(
                                removed,
                                remaining = processor.unchecked_count(),
                                "cleaned expired unchecked entries"
                            );
                        }
                    }
                }
            }
        });
        self.track_task("unchecked cleanup", unchecked_cleanup_handle);

        let priority_scheduler_drain = Arc::clone(&self.priority_scheduler);
        let active_elections_scheduler = Arc::clone(&self.active_elections);
        let store_scheduler = Arc::clone(&self.store);
        let rebroadcast_scheduler = Arc::clone(&self.local_broadcaster);
        let mut shutdown_rx_scheduler = self.shutdown.subscribe();

        let scheduler_handle = tokio::spawn(async move {
            let mut interval = repair_timer::interval(Duration::from_millis(100));
            let mut scan = crate::backlog_scan::BacklogScan::default();
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_scheduler.recv() => {
                        tracing::debug!("priority scheduler drain task shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                        // One next-unconfirmed block per account, from a bounded
                        // snapshot page. Elections must acquire fresh votes.
                        match scan.next(&store_scheduler, 256) {
                            Ok(pending) => {
                                for block in pending {
                                    match WireMessage::Block(Box::new(block.clone())).encode() {
                                        Ok(bytes) => rebroadcast_scheduler.lock().await.track_pending(block.hash, bytes, unix_now_ms()),
                                        Err(error) => tracing::error!(hash = %block.hash, %error, "cannot encode pending block for recovery"),
                                    }
                                    if active_elections_scheduler.read().await.candidate_root(&block.hash).is_none() {
                                        priority_scheduler_drain.lock().await.push(
                                            block.hash, block.account, block.trst_balance);
                                    }
                                }
                            }
                            Err(error) => tracing::error!(%error, "cannot scan unconfirmed account backlog"),
                        }
                        let started = crate::backlog_scan::drain_scheduler(
                            &store_scheduler, &priority_scheduler_drain, &active_elections_scheduler,
                            MAX_ACTIVE_ELECTIONS, Timestamp::new(unix_now_secs()),
                        ).await;
                        if started > 0 {
                            tracing::debug!(count = started, "priority scheduler started elections");
                        }
                    }
                }
            }
        });
        self.track_task("priority scheduler", scheduler_handle);

        self.spawn_online_weight_worker();

        Ok(())
    }

    async fn start_recovery_listener(
        &mut self,
        bootstrap_feedback_tx: &mpsc::Sender<crate::bootstrap::BootstrapFeedback>,
    ) -> Result<(), NodeError> {
        let p2p_port = self.config.port;
        let peer_manager = Arc::clone(&self.peer_manager);
        let mut shutdown_rx = self.shutdown.subscribe();
        let metrics = Arc::clone(&self.metrics);
        let conn_registry = Arc::clone(&self.connection_registry);
        let block_queue = Arc::clone(&self.block_queue);
        let active_elections = Arc::clone(&self.active_elections);
        let rep_weights = Arc::clone(&self.rep_weights);
        let vote_generator = Arc::clone(&self.vote_generator);
        let syn_cookies = Arc::clone(&self.syn_cookies);
        let message_dedup = Arc::clone(&self.message_dedup);
        let online_weight_sampler = Arc::clone(&self.online_weight_sampler);
        let store = Arc::clone(&self.store);
        let node_address = self.node_address.clone();
        let node_private_key = Arc::new(burst_types::PrivateKey(self.node_private_key.0));
        let config_params = self.config.params.clone();
        let network = self.config.network;
        let peering_port = self.config.port;
        let bootstrap_feedback = bootstrap_feedback_tx.clone();

        let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{p2p_port}"))
            .await
            .map_err(|error| NodeError::Other(format!("cannot bind P2P listener: {error}")))?;
        tracing::info!(port = p2p_port, "P2P recovery listener started");
        let p2p_handle = tokio::spawn(async move {
            let params_hash = config_params.params_hash();
            let mut handshakes = tokio::task::JoinSet::new();

            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx.recv() => {
                        tracing::info!("P2P listener shutting down");
                        break;
                    }
                    Some(result) = handshakes.join_next(), if !handshakes.is_empty() => {
                        if let Err(error) = result { tracing::debug!(%error, "inbound handshake task ended"); }
                    }
                    result = listener.accept() => {
                        match result {
                            Ok((stream, addr)) => {
                                let Some(admission) = conn_registry.read().await.admit(
                                    addr.ip(), crate::connection_registry::Direction::Inbound,
                                ) else { continue; };
                                if let Err(error) = burst_network::connection::configure_peer_stream(&stream) {
                                    tracing::debug!(%addr, %error, "inbound TCP setup failed");
                                    continue;
                                }
                                // Handle each inbound connection in its own task so a
                                // slow or malicious handshake never head-of-line-blocks
                                // the accept loop (matches rsnano's per-connection model).
                                let syn_cookies = Arc::clone(&syn_cookies);
                                let conn_registry = Arc::clone(&conn_registry);
                                let peer_manager = Arc::clone(&peer_manager);
                                let metrics = Arc::clone(&metrics);
                                let block_queue = Arc::clone(&block_queue);
                                let active_elections = Arc::clone(&active_elections);
                                let rep_weights = Arc::clone(&rep_weights);
                                let vote_generator = Arc::clone(&vote_generator);
                                let message_dedup = Arc::clone(&message_dedup);
                                let online_weight_sampler = Arc::clone(&online_weight_sampler);
                                let store = Arc::clone(&store);
                                let our_node_id = node_address.clone();
                                let node_private_key = Arc::clone(&node_private_key);
                                let bootstrap_feedback = bootstrap_feedback.clone();

                                handshakes.spawn(async move {
                                    let now_secs = unix_now_secs();
                                    let peer_ip = addr.ip().to_string();
                                    let peer_addr = PeerAddress { ip: peer_ip.clone(), port: addr.port() };
                                    let peer_id = format!("{}:{}", addr.ip(), addr.port());

                                    let cookie_id = addr.to_string();
                                    let Some(cookie) = syn_cookies.lock().await.generate_for_connection(&peer_ip, &cookie_id) else {
                                        tracing::debug!(peer = %peer_id, "handshake challenge unavailable");
                                        return;
                                    };
                                    let (mut read_half, mut write_half) = stream.into_split();
                                    let challenge = crate::wire_message::HandshakeMsg {
                                        node_id: our_node_id.clone(),
                                        cookie: Some(cookie),
                                        cookie_signature: None,
                                        params_hash: params_hash,
                                        network_id: network,
                                        peering_port: peering_port,
                                        block_version: CURRENT_BLOCK_VERSION,
                                        wire_version: burst_types::PROTOCOL_VERSION,
                                    };
                                    let exchange = async {
                                        use crate::peer_handshake::{self, Role};
                                        peer_handshake::write(&mut write_half, &challenge).await?;
                                        let remote = peer_handshake::read(&mut read_half).await?;
                                        let expected = syn_cookies.lock().await.take_cookie(&cookie_id)
                                            .ok_or("expired listener challenge")?;
                                        if challenge.cookie != Some(expected) { return Err("listener challenge changed".to_string()); }
                                        peer_handshake::validate(&remote, &challenge)?;
                                        peer_handshake::verify(Role::Dialer, &challenge, &remote,
                                            remote.cookie_signature.as_ref().ok_or("missing dialer proof")?)?;
                                        let mut proof = challenge.clone();
                                        proof.cookie = None;
                                        proof.cookie_signature = Some(peer_handshake::sign(Role::Listener, &challenge, &remote, &node_private_key)?);
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

                                    let local_node_id = our_node_id.as_str().to_string();
                                    let remote_node_id_string = remote_node_id.as_str().to_string();

                                    // Deterministic node_id dedup: if we already dialed this
                                    // peer, keep exactly one (the connection from the lower
                                    // node_id). `None` → this inbound lost; drop it.
                                    let connection_id = {
                                        let mut peers = peer_manager.write().await;
                                        let mut registry = conn_registry.write().await;
                                        match registry.register_admitted(
                                            remote_node_id_string,
                                            crate::connection_registry::Direction::Inbound,
                                            write_half,
                                            &local_node_id,
                                            admission.clone(),
                                        ) {
                                            Some(id) => {
                                                if !peers.register_connection(peer_id.clone(), peer_addr, false, now_secs) {
                                                    registry.remove_if(&peer_id, id);
                                                    metrics.peer_count.set(registry.len() as i64);
                                                    return;
                                                }
                                                metrics.peer_count.set(registry.len() as i64);
                                                id
                                            }
                                            None => {
                                                tracing::debug!(peer = %peer_id, "inbound superseded by our outbound (tie-break)");
                                                return;
                                            }
                                        }
                                    };

                                    // Only authenticated peers reach message dispatch.
                                    let reader_registry = conn_registry.clone();
                                    let mut registry = reader_registry.write().await;
                                    let reader_task = spawn_peer_read_loop(
                                        peer_id.clone(),
                                        connection_id,
                                        read_half,
                                        block_queue,
                                        conn_registry,
                                        peer_manager,
                                        metrics,
                                        active_elections,
                                        rep_weights,
                                        message_dedup,
                                        online_weight_sampler,
                                        store,
                                        params_hash,
                                        bootstrap_feedback,
                                        vote_generator,
                                        admission,
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
        self.track_task("P2P listener", p2p_handle);

        Ok(())
    }

    /// Start the node — begin listening for connections and processing blocks.
    ///
    /// This method:
    /// 1. Initializes the genesis block if the database is empty
    /// 2. Starts the P2P TCP listener
    /// 3. Connects to bootstrap peers
    /// 4. Waits for the shutdown signal
    ///
    /// RPC and WebSocket listeners must be disabled; applications enabling them
    /// use [`Self::start_with_services`].
    pub async fn start(&mut self) -> Result<(), NodeError> {
        if self.config.enable_rpc || self.config.enable_websocket {
            return Err(NodeError::Config(
                "public listeners require start_with_services and an application service factory".into(),
            ));
        }
        self.start_with_services(|_, _, _| Vec::new()).await
    }

    /// Start core workers, then let the application compose public listeners.
    /// Returned tasks join the existing node shutdown/failure supervision.
    pub async fn start_with_services<F>(&mut self, start_services: F) -> Result<(), NodeError>
    where
        F: FnOnce(crate::services::RpcEndpoint, crate::services::WebSocketEndpoint,
            Arc<ShutdownController>) -> crate::services::ServiceTasks + Send,
    {
        if self.outbound_rx.is_none() {
            return Err(NodeError::Other("node has already been started".into()));
        }
        let result = self.start_inner(start_services).await;
        if result.is_err() {
            self.outbound_rx.take();
            if let Err(error) = self.stop().await {
                tracing::error!(%error, "cleanup after failed startup");
            }
        }
        result
    }

    async fn start_inner<F>(&mut self, start_services: F) -> Result<(), NodeError>
    where
        F: FnOnce(crate::services::RpcEndpoint, crate::services::WebSocketEndpoint,
            Arc<ShutdownController>) -> crate::services::ServiceTasks + Send,
    {
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
            self.track_task("governance content", tokio::spawn(crate::governance_content::transfer::run(
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
            use crate::confirmed_replay::snapshot::installed::{
                recover_metadata_with_durable_inputs, StartupRecovery,
            };
            match recover_metadata_with_durable_inputs(
                self.store.clone(),
                self.config.network,
                self.initial_params.clone(),
                self.connection_registry.clone(),
                self.shutdown.clone(),
                self.recovery_scratch.clone(),
            )
            .await?
            {
                StartupRecovery::Ready(recovered) => recovered,
                StartupRecovery::Cancelled => {
                    self.outbound_rx.take();
                    self.stop().await?;
                    return Ok(());
                }
            }
        } else {
            None
        };
        if self.stop_requested_startup().await? {
            return Ok(());
        }
        if let Some(recovered) = compact {
            self.publish_recovered(recovered).await?;
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
                let genesis_account = genesis_key::genesis_address(self.config.network);
                let account_store = self.store.account_store();
                let existing = match account_store.get_account(&genesis_account) {
                    Ok(account) => Some(account),
                    Err(burst_store::StoreError::NotFound(_)) => None,
                    Err(error) => {
                        return Err(NodeError::Other(format!(
                            "cannot read genesis account: {error}"
                        )))
                    }
                };

                // Genesis is already the first account block. A zero head would
                // make wallets build an invalid spending Open instead of its successor.
                let genesis_head = self
                    .store
                    .frontier_store()
                    .get_frontier(&genesis_account)
                    .map_err(|error| {
                        NodeError::Other(format!("cannot read genesis frontier: {error}"))
                    })?;

                let is_new = existing.is_none();
                if let Some(info) = crate::genesis_state::account_to_write(
                    existing,
                    genesis_account.clone(),
                    genesis_head,
                ) {
                    account_store.put_account(&info).map_err(|error| {
                        NodeError::Other(format!("cannot initialize genesis account: {error}"))
                    })?;
                    if is_new {
                        self.ledger_cache.inc_account_count();
                    }
                    tracing::info!(%genesis_account, head = %info.head, "genesis account initialized or frontier repaired");
                }

                // Match genesis verified_at: accrual starts at epoch zero and
                // funds ordinary endorsement burns.
                {
                    let mut brn = self.brn_engine.lock().await;
                    if brn.get_wallet(&genesis_account).is_none() {
                        let account = account_store.get_account(&genesis_account).map_err(|error| {
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
                            batch.initialize_brn_wallet_records_if_empty()?;
                            batch.put_brn_wallet(&genesis_account, &bytes)?;
                            batch.commit()?;
                            brn.track_wallet(genesis_account.clone(), wallet);
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

            // Restore holdings, graph and private causes from one pinned writer
            // cut. Commit its validated cache so later priming and live engines
            // share these roots instead of decoding a second graph copy.
            let restore_store = self.store.clone();
            let restore_expiry = self.config.params.trst_expiry_secs;
            let restored = tokio::task::spawn_blocking(move || {
                let batch = restore_store.tx_begin_write()?;
                let engine = batch.read_trst_engine(restore_expiry)?;
                batch.commit()?;
                Ok::<_, burst_store::StoreError>(engine)
            })
            .await
            .map_err(|error| NodeError::Other(format!("TRST restoration task failed: {error}")))?
            .map_err(|error| NodeError::Other(format!(
                "cannot restore persisted TRST state: {error}; restore a known-good backup or re-sync into a new data directory"
            )))?;
            *self.trst_engine.lock().await = restored;
            tracing::info!("TRST graph and wallet portfolios restored from one LMDB cut");

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

        // Publication must drain its owned worker before shutdown can close
        // recovery. Do not launch live services after that drain requested stop.
        if self.stop_requested_startup().await? {
            return Ok(());
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
            let missing = (batch
                .undo_value(
                    burst_store_lmdb::block_undo::UndoTable::BrnMeta,
                    b"rate_history",
                )
                .map_err(|e| NodeError::Other(e.to_string()))?
                .is_none()
                && !batch
                    .uses_brn_rate_records()
                    .map_err(|e| NodeError::Other(e.to_string()))?)
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
            crate::brn_storage::migrate(&migration_store)?;
            crate::trst_storage::migrate(&migration_store, migration_expiry)?;
            crate::graph_storage::migrate(&migration_store)?;
            crate::governance_storage::migrate(&migration_store)?;
            crate::verification_storage::migrate(&migration_store)
        })
        .await
        .map_err(|error| {
            NodeError::Other(format!("economic storage migration worker failed: {error}"))
        })??;

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
        // Prime the committed writer cut before workers run. Sharing this root
        // with the live engines avoids retaining second decoded wallet copies.
        let cache_store = self.store.clone();
        let cache_expiry = self.config.params.trst_expiry_secs;
        let (restored_brn, restored_trst) = tokio::task::spawn_blocking(move || {
            let mut batch = cache_store.tx_begin_write()?;
            // Startup already restores the full database. Payment admission
            // still enforces its row/byte limits, including on warm cache hits.
            let brn = batch.read_brn_engine(usize::MAX, usize::MAX)?;
            let trst = batch.read_trst_engine(cache_expiry)?;
            batch.prepare_pending_dependencies(&trst.merger_graph)?;
            batch.commit()?;
            Ok::<_, burst_store::StoreError>((brn, trst))
        })
        .await
        .map_err(|error| {
            NodeError::Other(format!("economic cache initialization failed: {error}"))
        })??;
        *self.brn_engine.lock().await = restored_brn;
        *self.trst_engine.lock().await = restored_trst;
        self.connection_registry.write().await.finish_recovery();
        self.spawn_initial_tasks(outbound_rx).await?;
        if self.config.body_retirement.enabled {
            if !self.certified_selection_enabled() {
                return Err(NodeError::Config(
                    "physical body retirement requires certified recovery".into(),
                ));
            }
            let retirement = Arc::new(crate::body_retirement::Worker {
                store: self.store.clone(),
                network: self.config.network,
                params: self.initial_params.clone(),
                registry: self.connection_registry.clone(),
                processor: self.block_processor.clone(),
                counts: self.ledger_cache.clone(),
                shutdown: self.shutdown.clone(),
                #[cfg(test)]
                prepared_probe: None,
            });
            self.track_task("body retirement", retirement.start(self.config.body_retirement.clone()));
        }

        if let Some(service) = self.nts_service.take() {
            let mut stop = self.shutdown.subscribe();
            self.track_task("NTS service", tokio::spawn(async move {
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
            self.track_task("NTP sync", ntp_handle);
        }

        // Update metrics with initial counts
        self.refresh_metrics().await;

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
            let mut peers = self.peer_manager.write().await;
            peers.set_external_address(addr);
            tracing::info!(
                advertise = %addr,
                "advertise address set (cloud VPS / no UPnP)"
            );
        }

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
                                    let mut peers = pm_upnp.write().await;
                                    peers.set_external_address(addr);
                                    tracing::info!(
                                        external = %addr,
                                        "UPnP: external address set on PeerManager"
                                    );
                                }
                                UpnpState::NotFound | UpnpState::NonRoutable | UpnpState::Failed(_) => {
                                    let mut peers = pm_upnp.write().await;
                                    peers.clear_external_address();
                                    if let Some(addr) = external_addr { peers.set_external_address(addr); }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            });
            self.track_task("UPnP sync", upnp_sync_handle);
            self.port_mapper = Some(mapper);
        } else if is_dev_network {
            tracing::debug!("UPnP: disabled on dev network");
        } else {
            tracing::debug!("UPnP: disabled by configuration");
        }

        {
            use burst_store::peer::PeerStore;

            let peer_store = self.store.peer_store();
            match peer_store.all_peers() {
                Ok(cached) if !cached.is_empty() => {
                    tracing::info!(
                        count = cached.len(),
                        "peer cache: loaded cached peers from LMDB"
                    );

                    let connector = crate::peer_connector::PeerConnectorContext {
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
                                &connector.peer_manager,
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
                            match crate::peer_connector::connect_to_peer(&addr, &connector).await {
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
                    self.track_task("cached peer connection", cache_handle);
                }
                Ok(_) => {
                    tracing::debug!("peer cache: no cached peers found");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "peer cache: failed to load cached peers");
                }
            }
        }

        let bootstrap_peers: Vec<String> = {
            let peers = self.peer_manager.read().await;
            peers.bootstrap_peers().to_vec()
        };

        // Configured seeds are optional; inbound peers can have newer history.
        {
            let bootstrap_context = crate::peer_connector::PeerConnectorContext {
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
            let frontier_bootstrap = Arc::clone(&self.frontier);
            let store_bootstrap = Arc::clone(&self.store);
            let block_queue_bootstrap = Arc::clone(&self.block_queue);
            let conn_registry_bootstrap = Arc::clone(&self.connection_registry);
            let peer_manager_bootstrap = Arc::clone(&self.peer_manager);
            let mut shutdown_rx_bootstrap = self.shutdown.subscribe();
            let mut bootstrap_feedback_rx = bootstrap_feedback_rx;
            let genesis_addr_bootstrap = genesis_key::genesis_address(self.config.network);

            let bootstrap_handle = tokio::spawn(async move {
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
                        _ = shutdown_rx_bootstrap.recv() => {
                            tracing::debug!("bootstrap task shutting down");
                            break;
                        }
                        Some(feedback) = bootstrap_feedback_rx.recv() => match feedback {
                            BootstrapFeedback::Blocks { peer_id, id, blocks } => {
                                tracing::debug!(peer = %peer_id, id, count = blocks.len(),
                                    "received bootstrap block response");
                                let Some(source_ip) = conn_registry_bootstrap.read().await.source_ip(&peer_id) else { continue; };
                                match bootstrapper.receive_blocks(id, &peer_id, &blocks) {
                                    Ok(blocks) => for block in blocks {
                                        if !block_queue_bootstrap.push_from_peer(block, source_ip).await {
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
                                let f = frontier_bootstrap.read().await;
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
                                    &bootstrap_context.peer_manager,
                                )
                                .await
                                {
                                    let _ =
                                        crate::peer_connector::connect_to_peer(addr_str, &bootstrap_context)
                                            .await;
                                }
                            }
                            // Seed the genesis account once (always known/baked).
                            if !seeded {
                                let our = frontier_bootstrap
                                    .read()
                                    .await
                                    .get_head(&genesis_addr_bootstrap)
                                    .copied()
                                    .unwrap_or(BlockHash::ZERO);
                                bootstrapper.enqueue(genesis_addr_bootstrap.clone(), our);
                                seeded = true;
                            }
                            let now = unix_now_secs();
                            let blocks = store_bootstrap.block_store();
                            let fronts = store_bootstrap.frontier_store();
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
                                let peers = peer_manager_bootstrap.read().await;
                                let mut peers: Vec<_> = peers.iter_connected().map(|(id, _)| id.clone()).collect();
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
                                        if conn_registry_bootstrap.read().await.send(&peer_id, bytes) {
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
                                        conn_registry_bootstrap.read().await.send(&peer_id, bytes);
                                    }
                                }
                            }
                        }
                    }
                }
            });
            self.track_task("bootstrap", bootstrap_handle);
        }

        // Actively dials peers learned via keepalive gossip but not yet
        // connected, using their advertised listening address, up to a target
        // out-degree — the piece that lets a mesh self-form from one seed.
        //
        // Enabled by default. Identity-based dedup resolves simultaneous dials;
        // inbound ephemeral ports are excluded from the discovery address book.
        if self.config.enable_peer_reachout {
            let target_out = self.config.max_peers.clamp(3, 8);
            let connector = crate::peer_connector::PeerConnectorContext {
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
                            let connected = connector.peer_manager.read().await.connected_count();
                            if connected >= target_out {
                                continue;
                            }
                            let candidates = {
                                let peers = connector.peer_manager.read().await;
                                peers.reachout_candidates()
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
                                    &connector.peer_manager,
                                )
                                .await
                                {
                                    continue;
                                }
                                match crate::peer_connector::connect_to_peer(&addr, &connector).await
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
            self.track_task("peer reachout", reach_handle);
        }

        // Rotate two randomized peer samples. Both reserve slot zero for self
        // (0.0.0.0:0 when unavailable), so receivers never mistake a random peer
        // for the sender's externally mapped listening address.
        let peer_manager_keepalive = Arc::clone(&self.peer_manager);
        let conn_registry_keepalive = Arc::clone(&self.connection_registry);
        let mut shutdown_rx_keepalive = self.shutdown.subscribe();

        let keepalive_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(15));
            let mut round: u64 = 0;
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_keepalive.recv() => {
                        tracing::debug!("keepalive task shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                        let now = unix_now_secs();

                        // Disconnect peers idle for >5 minutes and unban expired bans
                        let idle_peers = {
                            let mut peers = peer_manager_keepalive.write().await;
                            peers.check_bans(now);
                            peers.cleanup_idle(now, 300)
                        };
                        if !idle_peers.is_empty() {
                            tracing::info!(
                                count = idle_peers.len(),
                                "disconnecting idle peers"
                            );
                            let mut registry = conn_registry_keepalive.write().await;
                            for pid in &idle_peers {
                                registry.remove(pid);
                            }
                        }

                        let mut peers = peer_manager_keepalive.write().await;
                        if peers.should_keepalive(now) {
                            peers.record_keepalive(now);
                            round = round.wrapping_add(1);

                            let peer_ids: Vec<String> = {
                                let registry = conn_registry_keepalive.read().await;
                                registry.peer_ids().into_iter().cloned().collect()
                            };

                            if peer_ids.is_empty() {
                                tracing::trace!("keepalive: no connected peers, skipping send");
                            }

                            // Both samples include the advertised local address when
                            // available, plus known peers that need not be connected.
                            let self_peers: Vec<String> = peers
                                .random_peers_with_self(8)
                                .iter()
                                .map(|a| format!("{}:{}", a.ip, a.port))
                                .collect();
                            let random_peers: Vec<String> = peers
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

                            // Rotate recipients between the two address samples.
                            for (i, pid) in peer_ids.iter().enumerate() {
                                let bytes = if i % 4 == (round as usize % 4) {
                                    &self_bytes
                                } else {
                                    &random_bytes
                                };
                                if let Some(ref payload) = bytes {
                                    conn_registry_keepalive.read().await.send(pid, payload.clone());
                                }
                            }

                            tracing::trace!(
                                connected = peers.connected_count(),
                                round = round,
                                "keepalive round"
                            );
                        }
                    }
                }
            }
        });
        self.track_task("keepalive", keepalive_handle);

        {
            let connector = crate::peer_connector::PeerConnectorContext {
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
            let mut shutdown_rx_discovery = self.shutdown.subscribe();

            let discovery_handle = tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(1));
                loop {
                    tokio::select! {
                        biased;
                        _ = shutdown_rx_discovery.recv() => {
                            tracing::debug!("reachout loop shutting down");
                            break;
                        }
                        _ = interval.tick() => {
                            let keepalive_peers = {
                                let mut peers = connector.peer_manager.write().await;
                                peers.pop_random_keepalive()
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
                                    let peers = connector.peer_manager.read().await;
                                    if peers.is_connected(&addr_str) || peers.is_banned(&addr_str) {
                                        continue;
                                    }
                                    // Skip our own advertised address — peers gossip it
                                    // back to us, and dialing it just churns a
                                    // self-connection that the node_id check rejects.
                                    if peers
                                        .external_address()
                                        .is_some_and(|ext| ext.to_string() == addr_str)
                                    {
                                        continue;
                                    }
                                }

                                tracing::debug!(peer = %addr_str, "reachout: attempting connection");
                                match crate::peer_connector::connect_to_peer(
                                    &addr_str,
                                    &connector,
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
            self.track_task("discovered peer connection", discovery_handle);
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
        self.track_task("checkpoint agreement", tokio::spawn(async move {
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
        self.track_task("checkpoint sync", tokio::spawn(crate::checkpoint_sync::run(
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
        self.track_task("checkpoint beacon", tokio::spawn(crate::checkpoint_beacon::run(
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

        let conn_registry_telemetry = Arc::clone(&self.connection_registry);
        let peer_manager_telemetry = Arc::clone(&self.peer_manager);
        let mut shutdown_rx_telemetry = self.shutdown.subscribe();

        let telemetry_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx_telemetry.recv() => {
                        tracing::debug!("telemetry request task shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                        let peer_ids: Vec<String> = {
                            let peers = peer_manager_telemetry.read().await;
                            peers.iter_connected().map(|(id, _)| id.clone()).collect()
                        };
                        if peer_ids.is_empty() {
                            continue;
                        }
                        let req = WireMessage::TelemetryReq;
                        let bytes = match req.encode() {
                            Ok(b) => b,
                            Err(_) => continue,
                        };
                        let registry = conn_registry_telemetry.read().await;
                        for pid in &peer_ids {
                            registry.send(pid, bytes.clone());
                        }
                        tracing::trace!(peers = peer_ids.len(), "sent telemetry requests");
                    }
                }
            }
        });
        self.track_task("telemetry", telemetry_handle);

        {
            let store_peer_cache = Arc::clone(&self.store);
            let peer_manager_peer_cache = Arc::clone(&self.peer_manager);
            let mut shutdown_rx_peer_cache = self.shutdown.subscribe();

            let peer_cache_handle = tokio::spawn(async move {
                use burst_store::peer::PeerStore;

                let mut interval = tokio::time::interval(Duration::from_secs(300));
                loop {
                    tokio::select! {
                        biased;
                        _ = shutdown_rx_peer_cache.recv() => {
                            tracing::debug!("peer cache writer shutting down");
                            break;
                        }
                        _ = interval.tick() => {
                            let connected = {
                                let peers = peer_manager_peer_cache.read().await;
                                peers.connected_peer_addresses()
                            };

                            let peer_store = store_peer_cache.peer_store();
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
            self.track_task("peer cache", peer_cache_handle);
        }

        let rpc_endpoint = self.rpc_endpoint(display_clock).await;
        let websocket_endpoint = self.config.enable_websocket.then(|| (
            self.config.websocket_port,
            self.config.websocket_bind.clone(),
            self.ws_state.clone(),
        ));
        for (name, task) in start_services(rpc_endpoint, websocket_endpoint, self.shutdown.clone()) {
            self.track_task(name, task);
        }

        tracing::info!("BURST node started — all subsystems running");

        // Wait for the shutdown signal
        self.shutdown.wait_for_signal().await;

        Ok(())
    }

    pub(crate) async fn stop_requested_startup(&mut self) -> Result<bool, NodeError> {
        if !self.shutdown.is_requested() {
            return Ok(false);
        }
        self.outbound_rx.take();
        self.stop().await?;
        Ok(true)
    }

    fn track_task(&mut self, worker: &'static str, task: JoinHandle<()>) {
        tracing::debug!(worker, task = %task.id(), node_port = self.config.port,
            "registered node background task");
        self.task_handles.push(task);
    }

    /// Stop workers and connections, then flush committed state.
    pub async fn stop(&mut self) -> Result<(), NodeError> {
        tracing::info!("BURST node stopping");

        // Signal all tasks
        self.shutdown.shutdown();

        // Remove the owned UPnP mapping.
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
            let mut peers = self.peer_manager.write().await;
            let connected_ids: Vec<String> =
                peers.iter_connected().map(|(id, _)| id.clone()).collect();
            for id in connected_ids {
                peers.mark_disconnected(&id);
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
    /// Returns RequiresLedgerRollback; elected replacements use fork_resolution.
    pub fn rollback_block(
        &self,
        _block: &StateBlock,
    ) -> Result<crate::block_processor::RollbackResult, NodeError> {
        Ok(crate::block_processor::RollbackResult::RequiresLedgerRollback)
    }

    /// Convert and enqueue a transaction, then broadcast its block.
    /// Returning a hash does not imply admission or confirmation.
    pub async fn process_transaction(
        &self,
        tx: burst_transactions::Transaction,
    ) -> Result<BlockHash, NodeError> {
        use burst_transactions::validation::validate_transaction_structure;

        let now_secs = unix_now_secs();
        let now = Timestamp::new(now_secs);

        validate_transaction_structure(&tx, now, 300)
            .map_err(|e| NodeError::Other(format!("transaction validation failed: {e}")))?;

        self.metrics.transactions_received.inc();

        let block = self.transaction_to_state_block(&tx, now).await?;
        let block_hash = block.hash;

        if !self.block_queue.push(block.clone()).await {
            return Err(NodeError::Other("block priority queue full".into()));
        }

        if let Ok(msg_bytes) = WireMessage::Block(Box::new(block)).encode() {
            let peers: Vec<burst_network::PeerState> = {
                let peers = self.peer_manager.read().await;
                peers.iter_connected().map(|(_, s)| s.clone()).collect()
            };
            let result = self
                .broadcaster
                .broadcast_with_fanout(&msg_bytes, &peers, 4)
                .await;
            tracing::debug!(
                queued = result.queued,
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
        let (previous, brn_spent, trst_balance, representative, previous_origin) = {
            let frontier = self.frontier.read().await;
            match frontier.get_head(&sender) {
                Some(head) => {
                    // Account exists — load current balances from the block store.
                    let block_store = self.store.block_store();
                    match block_store.get_block(head) {
                        Ok(bytes) => {
                            if let Ok(previous_block) = bincode::deserialize::<StateBlock>(&bytes) {
                                (
                                    *head,
                                    previous_block.brn_spent,
                                    previous_block.trst_balance,
                                    previous_block.representative,
                                    previous_block.origin,
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
        // the inverse of `wallet_from_link` (which derives the
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
        let (block_type, next_brn_spent, next_trst_balance, link) = match tx {
            burst_transactions::Transaction::Burn(burn) => {
                {
                    let brn = self.brn_engine.lock().await;
                    check_brn_spend(&brn, &sender, burn.amount)?;
                }
                // Ascending odometer: spending BRN increases the field.
                let next_brn_spent = brn_spent
                    .checked_add(burn.amount)
                    .ok_or_else(|| NodeError::Other("BRN spend overflow".into()))?;
                (
                    if is_open {
                        BlockType::Open
                    } else {
                        BlockType::Burn
                    },
                    next_brn_spent,
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
                let next_trst_balance = trst_balance - send.amount;
                (
                    if is_open {
                        BlockType::Open
                    } else {
                        BlockType::Send
                    },
                    brn_spent,
                    next_trst_balance,
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
                    brn_spent
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
                    brn_spent
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
                    brn_spent
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
                (BlockType::Merge, brn_spent, trst_balance, BlockHash::ZERO)
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
                (block_type, brn_spent, trst_balance, BlockHash::ZERO)
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
            brn_spent: next_brn_spent,
            trst_balance: next_trst_balance,
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

        let processor = self.block_processor.lock().await;
        self.metrics
            .unchecked_count
            .set(processor.unchecked_count() as i64);
        drop(processor);

        let peers = self.peer_manager.read().await;
        self.metrics.peer_count.set(peers.connected_count() as i64);
        drop(peers);

        if let Ok(count) = self.store.block_store().block_count() {
            self.metrics.block_count.set(count as i64);
        }
    }

    /// Parameters held in this node configuration; not a live governance query.
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
    let mut weights_cache = rep_weights.write().await;
    let mut active = elections.write().await;
    let view = store.tx_begin_read()?;
    let identity = store.read_payment_authority_identity(&view);
    if identity.is_some()
        && weights_cache.publication_identity() == identity
        && active.publication_identity() == identity
    {
        return Ok(());
    }
    let meta = store.meta_store();
    let authority =
        crate::payment_authority::PaymentAuthority::read_at(|key| meta.get_meta_txn(&view, key))?;
    drop(view);
    if let Some(authority) = authority {
        let mut weights = authority.weights();
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
        weights.mark_publication(identity);
        active.mark_publication(identity);
        *weights_cache = weights;
        return Ok(());
    }
    if active.authority_epoch().is_some() {
        return Err(burst_store::StoreError::Corruption(
            "installed payment authority is missing".into(),
        ));
    }
    // Legacy authority can change with cementation and is not cached here.
    crate::confirmed_authority::ConfirmedAuthority::read(store)?.rebuild_weights(&mut weights_cache);
    active
        .set_authority(weights_cache.all_weights())
        .map_err(|e| burst_store::StoreError::Corruption(e.to_string()))?;
    Ok(())
}

/// Detect the public source IP of an outbound TCP connection.
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
