//! Controlled RPC operations and state assembly for the node.
use super::{unix_now_secs, BurstNode};
use crate::{genesis_key, priority_queue::BlockPriorityQueue};
use burst_ledger::StateBlock;
use burst_rpc::{BlockProcessorCallback, ProcessResult as RpcProcessResult, RpcState};
use burst_store::meta::MetaStore;
use burst_store_lmdb::LmdbStore;
use burst_types::WalletAddress;
use burst_work::WorkGenerator;
use std::sync::Arc;

/// Expose node admission queues and read views through the RPC callback trait.
struct NodeBlockProcessor {
    checkpoint_sync_status: Arc<crate::checkpoint_sync::tip::Published>,
    historical_lookup: crate::historical_lookup::Reader,
    recovery_store: Arc<LmdbStore>,
    exclusion_reader: crate::payment_exclusion::Reader,
    appeal_view: crate::appeal_view::Reader,
    delegation_view: crate::delegation_view::Reader,
    block_queue: Arc<BlockPriorityQueue>,
    submission_results: Arc<crate::submission_results::SubmissionResults>,
    governance_transfers: Arc<crate::governance_content::transfer::Transfers>,
}

impl BlockProcessorCallback for NodeBlockProcessor {
    fn bootstrap_creator(&self) -> Result<burst_types::WalletAddress, String> {
        Ok(crate::genesis_key::genesis_address(
            self.delegation_view.network,
        ))
    }

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
    ) -> burst_rpc::state::HistoricalAccountPageFuture<'_> {
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
        Box::pin(self.historical_lookup.read_many(hashes))
    }
    fn confirmed_account_slot(
        &self,
        query: burst_types::payment_recovery::SlotQuery,
    ) -> Result<burst_types::payment_recovery::SlotReview, String> {
        crate::payment_exclusion::slot(&self.recovery_store, query)
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
        Box::pin(self.historical_lookup.confirmed_slot(query))
    }
    fn payment_exclusion(
        &self,
        query: burst_types::payment_recovery::ExclusionQuery,
    ) -> Result<burst_types::payment_recovery::ExclusionReview, String> {
        self.exclusion_reader.read(query)
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
        query: burst_governance::signing_delegation::view::Query,
    ) -> Result<burst_governance::signing_delegation::view::Page, String> {
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
        let hash = block.hash;
        self.block_queue
            .try_push_with_reason(block)
            .map_err(|reason| {
                tracing::debug!(%hash, %reason, "RPC queue admission rejected");
                // Preserve the retryable error prefix used by existing clients.
                format!("block queue full — try again later ({reason})")
            })?;
        Ok(RpcProcessResult::Queued)
    }

    fn process_block_async<'a>(
        &'a self,
        block_bytes: &'a [u8],
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<RpcProcessResult, String>> + Send + 'a>,
    > {
        Box::pin(async move {
            let block: StateBlock = bincode::deserialize(block_bytes)
                .map_err(|error| format!("failed to deserialize block: {error}"))?;
            self.submission_results.track(block_bytes);
            let hash = block.hash;
            self.block_queue
                .push_with_reason(block)
                .await
                .map_err(|reason| {
                    tracing::debug!(%hash, %reason, "RPC queue admission rejected");
                    format!("block queue full — try again later ({reason})")
                })?;
            Ok(RpcProcessResult::Queued)
        })
    }

    fn wait_for_result<'a>(
        &'a self,
        block_bytes: &'a [u8],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<RpcProcessResult>> + Send + 'a>>
    {
        Box::pin(self.submission_results.wait(block_bytes))
    }
}

impl BurstNode {
    pub(super) async fn rpc_endpoint(
        &self,
        display_clock: crate::checkpoint_service::Clock,
    ) -> crate::services::RpcEndpoint {
        if self.config.enable_rpc {
            let rpc_port = self.config.rpc_port;
            let rpc_bind = self.config.rpc_bind.clone();
            let metrics_registry = if self.config.enable_metrics {
                Some(self.metrics.registry.clone())
            } else {
                None
            };

            let rpc_state = Arc::new(RpcState {
                governance_reader: Some(Arc::new(crate::governance_read::GovernanceReader {
                    store: self.store.clone(),
                    fallback_params: self.config.params.clone(),
                })),
                read_executor: Default::default(),
                governance_projection_cache: Default::default(),
                merger_stats_cache: Default::default(),
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
                signing_clock: self.signing_clock.clone(),
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
                        preparation: Default::default(),
                        store: self.store.clone(),
                        registry: self.connection_registry.clone(),
                        network: self.config.network,
                        initial_params: self.initial_params.clone(),
                        active: Arc::new(tokio::sync::Mutex::new(())),
                        admitted: Arc::new(tokio::sync::Semaphore::new(3)),
                    },
                    recovery_store: self.store.clone(),
                    exclusion_reader: crate::payment_exclusion::Reader::new(self.store.clone()),
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
                // Dev and ordinary Test use public fixture seeds. Live and private
                // Test require a matching configured seed; bootstrap permission
                // and RPC authorization are checked separately.
                genesis_seed: genesis_key::genesis_seed(self.config.network),
            });

            Some((rpc_port, rpc_bind, rpc_state))
        } else {
            None
        }
    }
}
