//! Block processing pipeline.
//!
//! Validates incoming blocks through a multi-stage pipeline: dedup, PoW, signature,
//! gap detection, fork detection, and finally ledger application. Inspired by the
//! rsnano-node block processor architecture.

use crate::unchecked::UncheckedMap;
use burst_crypto::{decode_address, verify_signature};
use burst_ledger::{BlockType, DagFrontier, StateBlock};
use burst_store::block::BlockStore;
use burst_store::delegation::DelegationStore;
use burst_types::{BlockHash, PublicKey, Signature, Timestamp, WalletAddress};
use burst_work::{WorkBlockKind, WorkThresholds};
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

mod authentication;
pub(crate) use authentication::{signing_key, AuthenticatedBlock, AuthenticationRules};

/// Result of processing a single block through the pipeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessResult {
    /// Block was accepted and applied to the ledger.
    Accepted,
    /// Block references an unknown previous block — queued as unchecked.
    Gap,
    /// Block's linked source (send) block is unknown — queued as gap-source.
    GapSource,
    /// Block conflicts with an existing block — election started.
    Fork,
    /// Block was rejected as invalid.
    Rejected(String),
    /// Block is a duplicate (already in ledger).
    Duplicate,
    /// Block was queued for later processing (e.g. signature verification backlog).
    Queued,
}

/// Result of rolling back a block from the frontier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RollbackResult {
    /// Block was successfully rolled back.
    Success,
    /// Block is not the current frontier head — cannot roll back.
    NotHead,
    /// Account does not exist in the frontier.
    AccountNotFound,
    /// Persistent state requires a transactional economic rollback.
    RequiresLedgerRollback,
}

/// Where an incoming block originated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockSource {
    /// Block received from a peer over the network.
    Network,
    /// Block submitted locally (via RPC or wallet).
    Local,
    /// Block from bootstrap/sync process.
    Bootstrap,
    /// Block re-queued from unchecked/gap storage.
    Unchecked,
}

/// A block together with ingestion metadata, used by the processing queue.
#[derive(Clone, Debug)]
pub struct BlockContext {
    pub block: StateBlock,
    pub source: BlockSource,
    pub received_at: Timestamp,
}

/// Priority queue with backpressure for incoming blocks.
///
/// Local blocks are dequeued before network/bootstrap/unchecked blocks so that
/// user-initiated operations are never starved by flood traffic.
pub struct ProcessingQueue {
    local_queue: VecDeque<BlockContext>,
    network_queue: VecDeque<BlockContext>,
    max_capacity: usize,
}

impl ProcessingQueue {
    pub fn new(max_capacity: usize) -> Self {
        Self {
            local_queue: VecDeque::new(),
            network_queue: VecDeque::new(),
            max_capacity,
        }
    }

    /// Enqueue a block. Returns `false` if backpressure is active (queue full).
    pub fn enqueue(&mut self, ctx: BlockContext) -> bool {
        if self.len() >= self.max_capacity {
            return false;
        }
        match ctx.source {
            BlockSource::Local => self.local_queue.push_back(ctx),
            _ => self.network_queue.push_back(ctx),
        }
        true
    }

    /// Dequeue the next block (local queue has priority).
    pub fn dequeue(&mut self) -> Option<BlockContext> {
        self.local_queue
            .pop_front()
            .or_else(|| self.network_queue.pop_front())
    }

    pub fn len(&self) -> usize {
        self.local_queue.len() + self.network_queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.local_queue.is_empty() && self.network_queue.is_empty()
    }
}

/// Maximum number of recently processed hashes to keep in the dedup cache.
const MAX_RECENTLY_PROCESSED: usize = 65_536;

/// Multi-stage block processing pipeline.
///
/// Processes blocks synchronously through validation stages. The node calls this
/// from an async context, but the pipeline itself is sync to keep reasoning simple.
/// Max seconds a fresh block's timestamp may sit ahead of a node's own clock.
pub const FUTURE_TIMESTAMP_TOLERANCE_SECS: u64 = 60;
/// Max seconds a fresh block's timestamp may lag a node's own clock.
/// Decision 23.7 applies the same 60-second tolerance in both directions.
pub const PAST_TIMESTAMP_TOLERANCE_SECS: u64 = 60;

pub struct BlockProcessor {
    network: burst_types::NetworkId,
    payment_epoch: BlockHash,
    payment_cutoff: u64,
    /// Blocks waiting for a gap to be filled (previous block unknown).
    unchecked: UncheckedMap,
    /// Minimum proof-of-work difficulty (legacy single-threshold fallback).
    min_work_difficulty: u64,
    /// Per-block-type PoW thresholds.
    work_thresholds: WorkThresholds,
    /// Hashes of recently processed blocks (dedup cache).
    recently_processed: HashSet<BlockHash>,
    /// FIFO eviction order for bounded dedup cache.
    recently_processed_order: VecDeque<BlockHash>,
    /// The genesis account — only this account may sign epoch blocks.
    genesis_account: WalletAddress,
    /// Whether to verify Ed25519 signatures. Disabled in tests with synthetic addresses.
    verify_signatures: bool,
    /// Whether to validate block timestamps against wall-clock time.
    /// Disabled in tests to avoid flaky time-dependent failures.
    validate_timestamps: bool,
    pub(crate) historical_admissions:
        Arc<crate::checkpoint_store::historical::HistoricalAdmissions>,
    pub(crate) historical_finality: Arc<crate::historical_finality::HistoricalFinality>,
    #[cfg(test)]
    pub(crate) test_time: Option<Arc<std::sync::atomic::AtomicU64>>,
    /// Legacy records are retained, but cannot authorize signatures: they do
    /// not encode the proposal/category scope required by decision 20.8.
    pub delegation_store: Option<Arc<dyn DelegationStore + Send + Sync>>,
    /// Optional persistent block store for dedup fallback after cache eviction.
    pub block_store: Option<Arc<dyn BlockStore + Send + Sync>>,
    /// Rules for the installed epoch. Certified mode requires an exact match;
    /// the legacy bootstrap path retains warning-only validation.
    current_params_hash: BlockHash,
    enforce_params_hash: bool,
}

/// Map a ledger `BlockType` to the work-crate's `WorkBlockKind`.
fn block_type_to_work_kind(bt: &BlockType) -> WorkBlockKind {
    match bt {
        BlockType::Receive | BlockType::Open => WorkBlockKind::ReceiveOrOpen,
        BlockType::Epoch | BlockType::GovernanceActivation => WorkBlockKind::Epoch,
        _ => WorkBlockKind::Base,
    }
}

impl BlockProcessor {
    pub(crate) fn admission_time(&self) -> Timestamp {
        #[cfg(test)]
        if let Some(time) = &self.test_time {
            return Timestamp::new(time.load(std::sync::atomic::Ordering::SeqCst));
        }
        Timestamp::now()
    }

    pub(crate) fn install_payment_epoch(&mut self, epoch: BlockHash, cutoff: u64) {
        self.payment_epoch = epoch;
        self.payment_cutoff = cutoff;
        self.clear_recently_processed();
        self.unchecked.clear();
    }
    /// Create a new block processor.
    ///
    /// * `min_work_difficulty` — minimum PoW threshold blocks must meet.
    pub fn new(min_work_difficulty: u64, payment_epoch: BlockHash) -> Self {
        let kp = burst_crypto::keypair_from_seed(&[0u8; 32]);
        let genesis_addr = burst_crypto::derive_address(&kp.public);
        Self::with_genesis_account(min_work_difficulty, genesis_addr, payment_epoch)
    }

    /// Create a block processor with a specific genesis account.
    ///
    /// Epoch blocks are only accepted when signed by this account.
    pub fn with_genesis_account(
        min_work_difficulty: u64,
        genesis_account: WalletAddress,
        payment_epoch: BlockHash,
    ) -> Self {
        let max_unchecked = 65_536;
        Self {
            network: burst_types::NetworkId::Live,
            payment_epoch,
            payment_cutoff: 0,
            unchecked: UncheckedMap::new(max_unchecked),
            min_work_difficulty,
            work_thresholds: WorkThresholds::with_base(min_work_difficulty),
            recently_processed: HashSet::with_capacity(MAX_RECENTLY_PROCESSED),
            recently_processed_order: VecDeque::with_capacity(MAX_RECENTLY_PROCESSED),
            genesis_account,
            verify_signatures: true,
            validate_timestamps: true,
            historical_admissions: Arc::default(),
            historical_finality: Arc::default(),
            #[cfg(test)]
            test_time: None,
            delegation_store: None,
            block_store: None,
            current_params_hash: BlockHash::ZERO,
            enforce_params_hash: false,
        }
    }

    /// Set once by node construction from its validated network configuration.
    pub(crate) fn with_network(mut self, network: burst_types::NetworkId) -> Self {
        self.network = network;
        self
    }

    /// Set the current protocol params hash for validation.
    pub fn set_params_hash(&mut self, hash: BlockHash) {
        self.current_params_hash = hash;
    }

    /// Get the current protocol params hash.
    pub fn params_hash(&self) -> BlockHash {
        self.current_params_hash
    }

    /// Disable Ed25519 signature verification (for testing with synthetic addresses).
    pub fn set_verify_signatures(&mut self, verify: bool) {
        self.verify_signatures = verify;
    }

    /// Disable block timestamp validation (for testing without wall-clock dependency).
    pub fn set_validate_timestamps(&mut self, validate: bool) {
        self.validate_timestamps = validate;
    }

    /// Insert a hash into the bounded dedup cache, evicting the oldest if full.
    fn mark_processed(&mut self, hash: BlockHash) {
        if self.recently_processed.contains(&hash) {
            return;
        }
        if self.recently_processed.len() >= MAX_RECENTLY_PROCESSED {
            if let Some(old) = self.recently_processed_order.pop_front() {
                self.recently_processed.remove(&old);
            }
        }
        self.recently_processed.insert(hash);
        self.recently_processed_order.push_back(hash);
    }

    /// Process a single incoming block through the full pipeline.
    ///
    /// Pipeline stages:
    /// 1. **Dedup** — reject if already processed
    /// 2. **PoW** — verify proof-of-work meets minimum difficulty
    /// 3. **Signature** — basic signature sanity check (non-zero)
    /// 4. **Gap** — queue as unchecked if previous block is unknown
    /// 5. **Fork** — detect conflicting blocks for the same account position
    /// 6. **Open block** — validate first-block-in-chain semantics
    /// 7. **Chain append** — accept if block extends the frontier
    pub fn process(&mut self, block: &StateBlock, frontier: &mut DagFrontier) -> ProcessResult {
        self.process_inner(block, frontier, false)
    }

    fn process_inner(
        &mut self,
        block: &StateBlock,
        frontier: &mut DagFrontier,
        authenticated: bool,
    ) -> ProcessResult {
        if !authenticated {
            if let Err(reason) = block.validate_format() {
                return ProcessResult::Rejected(reason.into());
            }
        }
        let appeals_enabled = crate::release_policy::appeals(self.network);
        #[cfg(test)]
        let appeals_enabled = appeals_enabled || self.test_time.is_some();
        if block.block_type == BlockType::Challenge
            && !block.target_head.is_zero()
            && !appeals_enabled
        {
            return ProcessResult::Rejected(
                "meta-challenge admission requires certified appeal support".into(),
            );
        }
        if block.block_type == BlockType::GovernanceProposal
            && block.origin.is_zero()
            && !crate::governance_content::enabled(self.network)
            && matches!(
                burst_governance::ProposalContent::decode_compact(block.link.as_bytes()),
                Ok(burst_governance::ProposalContent::Emergency { .. })
            )
        {
            return ProcessResult::Rejected(
                "emergency governance is behind the release gate".into(),
            );
        }
        if !authenticated && !block.fits_p2p_message() {
            return ProcessResult::Rejected("block exceeds P2P message limit".into());
        }
        // Signatures and work authenticate the computed contents, not an
        // arbitrary hash supplied alongside a modified payload.
        if !authenticated && block.hash != block.compute_hash() {
            return ProcessResult::Rejected("block hash does not match its contents".into());
        }
        // Stage 1: Dedup check (in-memory cache + persistent store fallback)
        if self.recently_processed.contains(&block.hash) {
            return ProcessResult::Duplicate;
        }
        if let Some(ref store) = self.block_store {
            if let Ok(true) = store.exists(&block.hash) {
                self.mark_processed(block.hash);
                return ProcessResult::Duplicate;
            }
        }
        if self.payment_epoch.is_zero() || block.payment_epoch != self.payment_epoch {
            return ProcessResult::Rejected("block payment epoch is not installed".into());
        }
        if block.timestamp.as_secs() < self.payment_cutoff {
            return ProcessResult::Rejected(
                "block predates its installed checkpoint cutoff".into(),
            );
        }

        // Stage 2: every block kind uses the configured work threshold.
        let work_threshold = self
            .work_thresholds
            .threshold_for(block_type_to_work_kind(&block.block_type));
        if !block.verify_work(work_threshold) {
            return ProcessResult::Rejected(
                "proof-of-work does not meet minimum difficulty".into(),
            );
        }

        // Stage 2.5: Timestamp validation
        // Two-sided timestamp bound, judged against THIS node's own clock —
        // a sender's claimed timestamp is never taken as literal. Once a
        // timestamp passes every node's own-clock check, the (bounded) claim
        // is what the economic arithmetic uses, so all nodes compute
        // identical expiry/accrual results. The maximum lie is the tolerance
        // window is 60 seconds in either direction (decision 23.7).
        // Historical exceptions require a certificate
        // and semantic replay of the exact prefix, never a peer/source label.
        if self.validate_timestamps {
            let now_secs = self.admission_time().as_secs();
            let block_ts = block.timestamp.as_secs();
            if block_ts > now_secs.saturating_add(FUTURE_TIMESTAMP_TOLERANCE_SECS) {
                return ProcessResult::Rejected("block timestamp is too far in the future".into());
            }
            if block_ts.saturating_add(PAST_TIMESTAMP_TOLERANCE_SECS) < now_secs
                && !self.historical_admissions.contains(&block.hash)
                && !self.historical_finality.contains(&block.hash)
            {
                return ProcessResult::Rejected(
                    "block timestamp is too far in the past (backdating rejected)".into(),
                );
            }
        }

        // Stage 3: Signature verification
        if !authenticated {
            if let Err(reason) = self.authentication_rules().check_signature(block) {
                return ProcessResult::Rejected(reason);
            }
        }

        if self.enforce_params_hash && block.params_hash != self.current_params_hash {
            return ProcessResult::Rejected(
                "block parameters do not match the installed payment epoch".into(),
            );
        }

        // Stage 3.5: legacy params_hash validation
        // GovernanceActivation blocks carry the *new* params_hash in their
        // `transaction` field; their own `params_hash` stamp is the pre-activation
        // hash, so we skip validation for them. Bootstrap blocks (old blocks
        // replayed) may carry an earlier params_hash, so we only warn rather
        // than hard-reject to allow gradual migration.
        if !block.params_hash.is_zero()
            && !self.current_params_hash.is_zero()
            && block.params_hash != self.current_params_hash
            && block.block_type != BlockType::GovernanceActivation
        {
            tracing::warn!(
                block_hash = %block.hash,
                block_params = %block.params_hash,
                our_params = %self.current_params_hash,
                "params_hash mismatch — block was created under different protocol parameters"
            );
        }

        if block.is_open() && !block.previous.is_zero() {
            return ProcessResult::Rejected("open block must have zero previous hash".into());
        }
        if block.block_type == BlockType::Epoch {
            return self.process_epoch(block, frontier);
        }
        if !block.is_open()
            && block.block_type != BlockType::GovernanceActivation
            && block.previous.is_zero()
        {
            return ProcessResult::Rejected("non-open block has zero previous hash".into());
        }

        // Check the same dependency set used by cementation and history replay.
        // Address-valued links (send destinations, verification targets) are
        // deliberately absent from StateBlock::dependencies().
        if !block.previous.is_zero()
            && frontier.get_head(&block.account) != Some(&block.previous)
            && !self.source_known(&block.previous)
        {
            self.queue_unchecked(block.previous, block.clone());
            return ProcessResult::Gap;
        }
        if let Some(source) = block
            .dependencies()
            .find(|dependency| *dependency != block.previous && !self.source_known(dependency))
        {
            self.queue_unchecked_source(source, block.clone());
            return ProcessResult::GapSource;
        }

        // Stage 3.6: Epoch / GovernanceActivation block validation
        if block.block_type == BlockType::GovernanceActivation {
            return self.process_governance_activation(block, frontier);
        }

        // Stage 4–8: Account-state–dependent checks
        let account_head = frontier.get_head(&block.account).copied();

        match account_head {
            Some(frontier_head) => {
                // Account exists in the frontier

                // A different opening hash competes at the zero predecessor.
                // Exact duplicates were removed before signature validation.
                if block.is_open() {
                    return ProcessResult::Fork;
                }

                if block.previous == frontier_head {
                    // Stage 8: Chain append — block extends the frontier
                    frontier.update(block.account.clone(), block.hash);
                    self.mark_processed(block.hash);
                    return ProcessResult::Accepted;
                }

                // block.previous != frontier_head: either a gap or a fork.
                if block.previous.is_zero() {
                    return ProcessResult::Rejected("non-open block has zero previous hash".into());
                }

                // Distinguish gap from fork by whether `previous` is KNOWN:
                //
                // - Unknown previous → gap: we're simply missing ancestors (the
                //   common case during bootstrap, where a PoW-priority queue can
                //   deliver a chain out of order). Park it in the unchecked map so
                //   it's retried once its predecessor lands — NOT a fork.
                // - Known previous that isn't the tip → genuine fork: two blocks
                //   compete for the same account slot; resolve via election.
                //
                // The old code flagged every non-tip block as a fork, which sent
                // out-of-order bootstrap blocks to the fork cache (never retried)
                // instead of the unchecked map — so a reordered chain could never
                // converge.
                if self.source_known(&block.previous) {
                    ProcessResult::Fork
                } else {
                    self.queue_unchecked(block.previous, block.clone());
                    ProcessResult::Gap
                }
            }
            None => {
                // Account does not exist in the frontier

                // Stage 7: New account — must be an open block
                if block.is_open() {
                    // Open block with zero previous is valid for a new account
                    if !block.previous.is_zero() {
                        return ProcessResult::Rejected(
                            "open block must have zero previous hash".into(),
                        );
                    }
                    frontier.update(block.account.clone(), block.hash);
                    self.mark_processed(block.hash);
                    return ProcessResult::Accepted;
                }

                // Non-open block for unknown account — the account's chain hasn't arrived yet
                // Stage 4: Gap — queue as unchecked
                self.queue_unchecked(block.previous, block.clone());
                ProcessResult::Gap
            }
        }
    }

    /// Validate a candidate without publishing its frontier or dedup entry.
    /// The node must call `publish_committed` only after durable persistence.
    /// Gap handling is retained, but a rejected/failed write can be retried.
    pub fn validate_candidate(
        &mut self,
        block: &StateBlock,
        frontier: &DagFrontier,
    ) -> ProcessResult {
        self.validate_candidate_inner(block, frontier, false)
    }

    pub(crate) fn authentication_rules(&self) -> AuthenticationRules {
        AuthenticationRules::new(
            self.genesis_account.clone(),
            self.verify_signatures,
            self.work_thresholds.base,
        )
    }

    pub(crate) fn validate_authenticated_candidate(
        &mut self,
        candidate: &AuthenticatedBlock,
        frontier: &DagFrontier,
    ) -> ProcessResult {
        self.unchecked.admission_slot = candidate.retention.clone();
        let result = self.validate_authenticated_inner(candidate, frontier);
        self.unchecked.admission_slot = None;
        result
    }

    fn validate_authenticated_inner(
        &mut self,
        candidate: &AuthenticatedBlock,
        frontier: &DagFrontier,
    ) -> ProcessResult {
        // Only immutable authentication is reusable. Work thresholds, clock,
        // epoch, source availability and frontier checks always run below.
        if !candidate.matches(&self.authentication_rules()) {
            return self.validate_candidate(candidate.block(), frontier);
        }
        if let Err(reason) = candidate.result() {
            return ProcessResult::Rejected(reason.clone());
        }
        self.validate_candidate_inner(candidate.block(), frontier, true)
    }

    fn validate_candidate_inner(
        &mut self,
        block: &StateBlock,
        frontier: &DagFrontier,
        authenticated: bool,
    ) -> ProcessResult {
        let mut candidate_frontier = DagFrontier::new();
        if let Some(head) = frontier.get_head(&block.account) {
            candidate_frontier.update(block.account.clone(), *head);
        }
        let result = self.process_inner(block, &mut candidate_frontier, authenticated);
        if result == ProcessResult::Accepted {
            self.recently_processed.remove(&block.hash);
            // Acceptance just appended this candidate; avoid scanning the
            // entire bounded dedup history for every block.
            let removed = self.recently_processed_order.pop_back();
            debug_assert_eq!(removed, Some(block.hash));
        }
        result
    }

    /// Publish a successfully persisted candidate to the in-memory indexes.
    pub fn publish_committed(&mut self, block: &StateBlock, frontier: &mut DagFrontier) {
        frontier.update(block.account.clone(), block.hash);
        self.mark_processed(block.hash);
    }

    pub(crate) fn forget_replaced(&mut self, hashes: &HashSet<BlockHash>) {
        self.recently_processed
            .retain(|hash| !hashes.contains(hash));
        self.recently_processed_order
            .retain(|hash| !hashes.contains(hash));
    }

    /// Check if a source/link block is known, first in the in-memory dedup
    /// cache, then falling back to the persistent block store.
    fn source_known(&self, hash: &BlockHash) -> bool {
        if self.recently_processed.contains(hash) {
            return true;
        }
        if let Some(ref store) = self.block_store {
            if let Ok(true) = store.exists(hash) {
                return true;
            }
        }
        false
    }

    /// Queue a block whose previous block hasn't been seen yet (gap-previous).
    pub fn queue_unchecked(&mut self, previous: BlockHash, block: StateBlock) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.unchecked.insert(previous, block, now);
    }

    /// Queue a block whose linked source (send) block hasn't been seen yet (gap-source).
    pub fn queue_unchecked_source(&mut self, source_hash: BlockHash, block: StateBlock) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.unchecked.insert_source(source_hash, block, now);
    }

    /// When a block is confirmed, check if any unchecked blocks were waiting for it
    /// as their previous block (gap-previous).
    ///
    /// Returns the blocks that are now ready for processing.
    pub fn process_unchecked(&mut self, confirmed_hash: &BlockHash) -> Vec<StateBlock> {
        self.unchecked.take_ready(
            confirmed_hash,
            crate::unchecked::GapType::Previous,
            burst_types::Timestamp::now().as_secs(),
        )
    }

    /// When a block is confirmed, check if any unchecked blocks were waiting for it
    /// as their source/link block (gap-source).
    ///
    /// Returns the blocks that are now ready for processing.
    pub fn process_unchecked_source(&mut self, confirmed_hash: &BlockHash) -> Vec<StateBlock> {
        self.unchecked.take_ready(
            confirmed_hash,
            crate::unchecked::GapType::Source,
            burst_types::Timestamp::now().as_secs(),
        )
    }

    /// Number of blocks in the unchecked queue (both gap-previous and gap-source).
    pub(crate) fn set_retention(&mut self, retention: Arc<crate::block_retention::BlockRetention>) {
        assert!(self.unchecked.is_empty());
        self.unchecked = UncheckedMap::with_retention(
            crate::block_retention::MAX_RETAINED_BLOCKS - crate::block_retention::INGRESS_RESERVE,
            retention,
        );
    }

    pub(crate) fn release_retained(
        &mut self,
        hash: &BlockHash,
    ) -> Vec<crate::block_retention::RetainedBlock> {
        let now = Some(burst_types::Timestamp::now().as_secs());
        let mut ready =
            self.unchecked
                .take_retained(hash, crate::unchecked::GapType::Previous, now);
        ready.extend(
            self.unchecked
                .take_retained(hash, crate::unchecked::GapType::Source, now),
        );
        ready
    }

    pub fn unchecked_count(&self) -> usize {
        self.unchecked.len()
    }

    /// Remove unchecked entries older than `max_age_secs`. Returns how many were removed.
    pub fn cleanup_unchecked(&mut self, max_age_secs: u64, now: u64) -> usize {
        self.unchecked.clear_expired(max_age_secs, now)
    }

    /// Roll back a block from the frontier.
    ///
    /// In-memory simulation only. Persistent nodes must use a transactional
    /// ledger rollback that restores economic and cross-account effects.
    pub fn rollback(&mut self, block: &StateBlock, frontier: &mut DagFrontier) -> RollbackResult {
        if self.block_store.is_some() {
            return RollbackResult::RequiresLedgerRollback;
        }
        match frontier.get_head(&block.account) {
            Some(head) if *head == block.hash => {
                if block.previous.is_zero() {
                    frontier.remove(&block.account);
                } else {
                    frontier.update(block.account.clone(), block.previous);
                }
                self.recently_processed.remove(&block.hash);

                RollbackResult::Success
            }
            Some(_) => RollbackResult::NotHead,
            None => RollbackResult::AccountNotFound,
        }
    }

    /// Roll back all blocks in an account chain from the head down to (and
    /// including) the target block.
    ///
    /// Walks the chain from the current head backwards using the block store to
    /// look up `previous` pointers. Each block is rolled back in head-first
    /// order so every removal is always at the frontier.
    ///
    /// Returns the list of rolled-back block hashes (head first, target last).
    pub fn rollback_to(
        &mut self,
        account: &WalletAddress,
        target_hash: &BlockHash,
        frontier: &mut DagFrontier,
        store: &dyn BlockStore,
    ) -> Result<Vec<BlockHash>, String> {
        if self.block_store.is_some() {
            return Err("persistent rollback requires transactional ledger effects".into());
        }
        let head = frontier
            .get_head(account)
            .copied()
            .ok_or_else(|| "account not found in frontier".to_string())?;

        let mut chain: Vec<StateBlock> = Vec::new();
        let mut current = head;

        loop {
            let block_bytes = store
                .get_block(&current)
                .map_err(|e| format!("block lookup failed: {e}"))?;
            let block: StateBlock = bincode::deserialize(&block_bytes)
                .map_err(|e| format!("block deserialization failed: {e}"))?;
            let prev = block.previous;
            chain.push(block);

            if current == *target_hash {
                break;
            }
            current = prev;

            if current.is_zero() && *target_hash != current {
                return Err("target block not found in account chain".to_string());
            }
            if chain.len() > 10_000 {
                return Err("rollback chain too deep".to_string());
            }
        }

        let mut rolled_back = Vec::with_capacity(chain.len());
        for block in &chain {
            let result = self.rollback(block, frontier);
            if result != RollbackResult::Success {
                return Err(format!(
                    "rollback of block {} failed: {:?}",
                    block.hash, result
                ));
            }
            rolled_back.push(block.hash);
        }

        Ok(rolled_back)
    }

    /// Roll back a block and all blocks on other account chains that depend on
    /// it (cascading rollback).
    ///
    /// Currently handles the primary chain: rolls back from the account's head
    /// down to (and including) the target block. Cross-chain dependent receives
    /// are not yet tracked (requires a reverse-link index); callers should
    /// handle those at a higher layer when a reverse index is available.
    ///
    /// Returns the list of rolled-back block hashes.
    pub fn cascade_rollback(
        &mut self,
        block_hash: &BlockHash,
        account: &WalletAddress,
        frontier: &mut DagFrontier,
        store: &dyn BlockStore,
    ) -> Result<Vec<BlockHash>, String> {
        self.rollback_to(account, block_hash, frontier, store)
    }

    /// Clear the dedup cache (call periodically to free memory).
    pub fn clear_recently_processed(&mut self) {
        self.recently_processed.clear();
    }

    /// Number of recently processed block hashes in the dedup cache.
    pub fn recently_processed_count(&self) -> usize {
        self.recently_processed.len()
    }

    /// Process an epoch block.
    ///
    /// Epoch blocks are special protocol-upgrade markers. Rules:
    /// - Only the genesis account may sign epoch blocks.
    /// - The target account must already exist in the frontier.
    /// - `previous` must reference the current head of the target account.
    /// - Epoch blocks don't transfer any value (balances must remain unchanged).
    /// - The epoch block is appended to the target account's chain.
    fn process_epoch(&mut self, block: &StateBlock, frontier: &mut DagFrontier) -> ProcessResult {
        // process() already verified the signature against the genesis key.
        if block.previous.is_zero() {
            return ProcessResult::Rejected(
                "epoch block must reference an existing account chain".into(),
            );
        }

        let account_head = frontier.get_head(&block.account).copied();

        match account_head {
            Some(frontier_head) => {
                // The epoch block must chain from the current head.
                if block.previous != frontier_head {
                    return ProcessResult::Rejected(
                        "epoch block previous does not match account head".into(),
                    );
                }

                // Epoch blocks must not *change* balances — they carry the
                // account's current balance forward unchanged. Full balance
                // continuity validation requires account-state lookup (LMDB),
                // which happens at the node layer. Here at the structural level
                // we accept the balances as-is and update the frontier.

                frontier.update(block.account.clone(), block.hash);
                self.mark_processed(block.hash);
                ProcessResult::Accepted
            }
            None => {
                // Target account doesn't exist — epoch blocks can't create accounts.
                ProcessResult::Rejected("epoch block targets account that does not exist".into())
            }
        }
    }

    /// Process a governance activation block.
    ///
    /// GovernanceActivation blocks record on-chain parameter changes (Tezos-style).
    /// Rules:
    /// - Must be signed by the genesis account.
    /// - The block's `account` field is the genesis account (placed on its chain).
    /// - `link` = proposal hash, `transaction` = new params hash.
    /// - Must chain from the genesis account's current head.
    /// - Balances must remain unchanged.
    fn process_governance_activation(
        &mut self,
        block: &StateBlock,
        frontier: &mut DagFrontier,
    ) -> ProcessResult {
        if block.account != self.genesis_account {
            return ProcessResult::Rejected(
                "governance activation block must be on the genesis account chain".into(),
            );
        }

        if block.previous.is_zero() {
            return ProcessResult::Rejected(
                "governance activation block must reference existing genesis chain".into(),
            );
        }

        let account_head = frontier.get_head(&block.account).copied();

        match account_head {
            Some(frontier_head) => {
                if block.previous != frontier_head {
                    return ProcessResult::Rejected(
                        "governance activation block previous does not match genesis chain head"
                            .into(),
                    );
                }

                frontier.update(block.account.clone(), block.hash);
                self.mark_processed(block.hash);
                ProcessResult::Accepted
            }
            None => ProcessResult::Rejected(
                "genesis account not found in frontier for governance activation".into(),
            ),
        }
    }

    /// Publish governed admission rules with the installed checkpoint epoch.
    pub(crate) fn install_params(&mut self, params: &burst_types::ProtocolParams) {
        self.enforce_params_hash = true;
        self.set_params_hash(params.params_hash());
        self.min_work_difficulty = params.min_work_difficulty;
        self.work_thresholds = WorkThresholds::with_base(params.min_work_difficulty);
    }

    /// Current minimum work difficulty.
    pub fn min_work_difficulty(&self) -> u64 {
        self.min_work_difficulty
    }

    /// Per-block-type work thresholds.
    pub fn work_thresholds(&self) -> &WorkThresholds {
        &self.work_thresholds
    }

    /// The genesis account (authorized to sign epoch blocks).
    pub fn genesis_account(&self) -> &WalletAddress {
        &self.genesis_account
    }

    /// Validate that balance transitions are consistent with the block type.
    ///
    /// Given the previous block's BRN and TRST balances, checks that the new
    /// block's balances are valid for its operation type. Returns `Ok(())` if
    /// valid, or `Err(reason)` if the transition violates invariants.
    ///
    /// This is intended to be called by the node layer when the previous block
    /// is available (i.e., the block is not an open block).
    pub fn validate_balance_transition(
        block: &StateBlock,
        prev_brn: u128,
        prev_trst: u128,
    ) -> Result<(), String> {
        // `brn_balance` is an ascending odometer of cumulative BRN spent
        // (burned + staked). BRN itself never lives on the ledger — the
        // odometer delta is the amount, and the node validates it against
        // the independently computed BRN(w) before accepting the block.
        match block.block_type {
            BlockType::Send => {
                if block.trst_balance > prev_trst {
                    return Err("send block cannot increase TRST balance".into());
                }
                if block.brn_balance != prev_brn {
                    return Err("send block cannot change spent BRN".into());
                }
                let send_amount = prev_trst.saturating_sub(block.trst_balance);
                if send_amount == 0 {
                    return Err("send amount must be non-zero".into());
                }
            }
            BlockType::Receive => {
                if block.trst_balance < prev_trst {
                    return Err("receive block cannot decrease TRST balance".into());
                }
                if block.brn_balance != prev_brn {
                    return Err("receive block cannot change spent BRN".into());
                }
            }
            BlockType::Burn => {
                if block.brn_balance < prev_brn {
                    return Err("burn: cumulative spent BRN cannot decrease".into());
                }
                let burn_amount = block.brn_balance.saturating_sub(prev_brn);
                if burn_amount == 0 {
                    return Err("burn amount must be non-zero".into());
                }
                if block.trst_balance != prev_trst {
                    return Err("burn: sender's TRST balance must not change".into());
                }
            }
            BlockType::Merge => {
                if block.brn_balance != prev_brn {
                    return Err("merge block cannot change spent BRN".into());
                }
                if block.trst_balance != prev_trst {
                    return Err("merge block cannot change TRST balance".into());
                }
            }
            BlockType::Endorse | BlockType::GovernanceEndorse => {
                if block.brn_balance <= prev_brn {
                    return Err("endorse block must burn BRN (spent BRN must increase)".into());
                }
                if block.trst_balance != prev_trst {
                    return Err("endorse block cannot change TRST balance".into());
                }
            }
            BlockType::Challenge => {
                if block.brn_balance <= prev_brn {
                    return Err("challenge block must stake BRN (spent BRN must increase)".into());
                }
                if block.trst_balance != prev_trst {
                    return Err("challenge block cannot change TRST balance".into());
                }
            }
            BlockType::GovernanceProposal
            | BlockType::GovernanceVote
            | BlockType::GovernanceWithdraw
            | BlockType::Delegate
            | BlockType::RevokeDelegation
            | BlockType::VerifierOptIn
            | BlockType::VerifierOptOut => {
                if block.brn_balance != prev_brn {
                    return Err(format!(
                        "{:?} block cannot change BRN balance",
                        block.block_type
                    ));
                }
                if block.trst_balance != prev_trst {
                    return Err(format!(
                        "{:?} block cannot change TRST balance",
                        block.block_type
                    ));
                }
            }
            BlockType::ChangeRepresentative | BlockType::ChangeSigner => {
                if block.brn_balance != prev_brn {
                    return Err("change-representative block cannot change BRN balance".into());
                }
                if block.trst_balance != prev_trst {
                    return Err("change-representative block cannot change TRST balance".into());
                }
            }
            BlockType::Epoch => {
                if block.brn_balance != prev_brn || block.trst_balance != prev_trst {
                    return Err("epoch block cannot change balances".into());
                }
            }
            BlockType::GovernanceActivation => {
                if block.brn_balance != prev_brn || block.trst_balance != prev_trst {
                    return Err("governance activation block cannot change balances".into());
                }
            }
            BlockType::RejectReceive => {
                if block.brn_balance != prev_brn || block.trst_balance != prev_trst {
                    return Err("reject-receive block cannot change balances".into());
                }
            }
            BlockType::VerificationVote => {
                // Legitimate/Illegitimate votes stake BRN (odometer increases);
                // "Neither" votes carry no stake (odometer unchanged).
                if block.brn_balance < prev_brn {
                    return Err("verification-vote: cumulative spent BRN cannot decrease".into());
                }
                if block.trst_balance != prev_trst {
                    return Err("verification-vote block cannot change TRST balance".into());
                }
            }
            BlockType::Open => {
                // Open blocks have no previous — caller should not invoke this for them.
            }
        }
        Ok(())
    }

    /// Validate an Open block (the first block in an account chain).
    ///
    /// Balances on open blocks must not be self-reported:
    /// - `brn_balance` must be 0 — BRN is a computed birthright, equal for
    ///   everyone; nothing has been spent before the chain exists.
    /// - `trst_balance` must be 0 unless this is a receive-open, in which
    ///   case it must equal exactly the pending send's amount (checked by the
    ///   caller against the pending store via `expected_pending_amount`).
    pub fn validate_open_block(
        block: &StateBlock,
        expected_pending_amount: Option<u128>,
    ) -> Result<(), String> {
        if block.block_type != BlockType::Open {
            return Ok(());
        }
        if block.brn_balance != 0 {
            return Err(format!(
                "open block must have zero spent BRN (birthright is computed, not claimed), got {}",
                block.brn_balance
            ));
        }
        match expected_pending_amount {
            Some(amount) => {
                if block.trst_balance != amount {
                    return Err(format!(
                        "receive-open TRST balance {} does not match pending send amount {}",
                        block.trst_balance, amount
                    ));
                }
            }
            None => {
                if block.trst_balance != 0 {
                    return Err(format!(
                        "open block claims {} TRST with no matching pending send",
                        block.trst_balance
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_crypto::{derive_address, generate_keypair, keypair_from_seed, sign_message};
    use burst_ledger::{BlockType, DagFrontier, CURRENT_BLOCK_VERSION};
    use burst_store::block::BlockStore;
    use burst_types::{BlockHash, Signature, Timestamp, TxHash, WalletAddress};
    use burst_work::WorkGenerator;

    #[test]
    fn unsupported_versions_are_rejected_before_publication() {
        for version in [0, CURRENT_BLOCK_VERSION + 1, u8::MAX] {
            let mut processor = BlockProcessor::new(0, burst_types::BlockHash::new([1; 32]));
            processor.set_verify_signatures(false);
            processor.set_validate_timestamps(false);
            let mut frontier = DagFrontier::new();
            let mut block = make_open_block(0);
            block.version = version;
            block.hash = block.compute_hash();
            assert_eq!(
                processor.process(&block, &mut frontier),
                ProcessResult::Rejected("unsupported block version".into())
            );
            assert!(frontier.get_head(&block.account).is_none());
            assert_eq!(processor.unchecked_count(), 0);
        }
    }

    #[test]
    fn oversized_blocks_are_rejected_before_publication() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();
        let mut block = make_open_block(0);
        block.merge_sources = vec![TxHash::ZERO; burst_types::MAX_P2P_MESSAGE_BYTES / 32];
        block.hash = block.compute_hash();
        assert_eq!(
            processor.process(&block, &mut frontier),
            ProcessResult::Rejected("too many merge inputs".into())
        );
        assert!(frontier.get_head(&block.account).is_none());
        assert_eq!(processor.unchecked_count(), 0);
    }

    #[test]
    fn certified_admission_requires_exact_parameters_even_when_hash_is_zero() {
        let params = burst_types::ProtocolParams {
            min_work_difficulty: 0,
            ..Default::default()
        };
        let mut processor = test_processor(0);
        processor.install_params(&params);
        let mut frontier = DagFrontier::new();
        for hash in [BlockHash::ZERO, BlockHash::new([7; 32])] {
            let mut block = make_open_block(0);
            block.params_hash = hash;
            block.hash = block.compute_hash();
            assert_eq!(
                processor.process(&block, &mut frontier),
                ProcessResult::Rejected(
                    "block parameters do not match the installed payment epoch".into()
                )
            );
            assert!(frontier.get_head(&block.account).is_none());
        }
        let mut block = make_open_block(0);
        block.params_hash = params.params_hash();
        block.hash = block.compute_hash();
        assert_eq!(
            processor.process(&block, &mut frontier),
            ProcessResult::Accepted
        );
    }

    fn test_account() -> WalletAddress {
        WalletAddress::new(
            "brst_1111111111111111111111111111111111111111111111111111111111111111111",
        )
    }

    fn test_representative() -> WalletAddress {
        WalletAddress::new(
            "brst_2222222222222222222222222222222222222222222222222222222222222222222",
        )
    }

    /// Create a valid open block with proper hash and work.
    fn make_open_block(difficulty: u64) -> StateBlock {
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Open,
            account: test_account(),
            previous: BlockHash::ZERO,
            representative: test_representative(),
            brn_balance: 1000,
            trst_balance: 0,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_000),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([1u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();

        // Generate valid PoW
        if difficulty > 0 {
            let generator = WorkGenerator;
            let nonce = generator.generate(&block.work_root(), difficulty).unwrap();
            block.work = nonce.0;
        }

        block
    }

    /// Create a valid send block that chains after `previous_hash`.
    fn make_send_block(previous_hash: BlockHash, difficulty: u64) -> StateBlock {
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Send,
            account: test_account(),
            previous: previous_hash,
            representative: test_representative(),
            brn_balance: 900,
            trst_balance: 100,
            link: BlockHash::new([0xAA; 32]),
            origin: TxHash::ZERO,
            transaction: TxHash::new([0xBB; 32]),
            timestamp: Timestamp::new(1_000_001),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([2u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();

        if difficulty > 0 {
            let generator = WorkGenerator;
            let nonce = generator.generate(&block.work_root(), difficulty).unwrap();
            block.work = nonce.0;
        }

        block
    }

    /// Create a conflicting (fork) send block for the same account and position.
    fn make_fork_block(previous_hash: BlockHash, difficulty: u64) -> StateBlock {
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Send,
            account: test_account(),
            previous: previous_hash,
            representative: test_representative(),
            brn_balance: 800,
            trst_balance: 200,
            link: BlockHash::new([0xCC; 32]),
            origin: TxHash::ZERO,
            transaction: TxHash::new([0xDD; 32]),
            timestamp: Timestamp::new(1_000_002),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([3u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();

        if difficulty > 0 {
            let generator = WorkGenerator;
            let nonce = generator.generate(&block.work_root(), difficulty).unwrap();
            block.work = nonce.0;
        }

        block
    }

    fn test_processor(difficulty: u64) -> BlockProcessor {
        let mut p = BlockProcessor::new(difficulty, burst_types::BlockHash::new([1; 32]));
        p.set_verify_signatures(false);
        p.set_validate_timestamps(false);
        p
    }

    #[test]
    fn fresh_admission_obeys_the_sixty_second_window_in_both_directions() {
        for (timestamp, accepted) in [
            (939, false),
            (940, true),
            (1000, true),
            (1060, true),
            (1061, false),
        ] {
            let mut processor = test_processor(0);
            processor.set_validate_timestamps(true);
            processor.test_time = Some(Arc::new(std::sync::atomic::AtomicU64::new(1000)));
            let mut block = make_open_block(0);
            block.timestamp = Timestamp::new(timestamp);
            block.hash = block.compute_hash();
            let mut frontier = DagFrontier::new();
            let result = processor.process(&block, &mut frontier);
            if accepted {
                assert_eq!(result, ProcessResult::Accepted, "timestamp {timestamp}");
            } else {
                assert!(
                    matches!(result, ProcessResult::Rejected(ref reason) if reason.contains("timestamp")),
                    "{result:?}"
                );
                assert!(frontier.get_head(&block.account).is_none());
            }
        }
    }

    #[test]
    fn appeal_admission_is_closed_before_gap_or_final_vote_reservation() {
        let mut block = make_send_block(BlockHash::new([9; 32]), 0);
        block.block_type = BlockType::Challenge;
        block.target_head = BlockHash::new([8; 32]);
        block.hash = block.compute_hash();
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();
        assert_eq!(
            processor.process(&block, &mut frontier),
            ProcessResult::Rejected(
                "meta-challenge admission requires certified appeal support".into()
            )
        );
    }

    // ── Normal block acceptance ──────────────────────────────────────────

    #[test]
    fn legacy_delegation_record_cannot_bypass_signature_verification() {
        use burst_store::delegation::DelegationRecord;
        let owner = burst_crypto::keypair_from_seed(&[31; 32]);
        let delegate = burst_crypto::keypair_from_seed(&[32; 32]);
        let account = burst_crypto::derive_address(&owner.public);
        let store = Arc::new(burst_nullables::NullDelegationStore::new());
        store
            .put_delegation(&DelegationRecord {
                delegator: account.clone(),
                delegate: burst_crypto::derive_address(&delegate.public),
                delegation_public_key: delegate.public.0,
                created_at: Timestamp::new(0),
                revoked: false,
            })
            .unwrap();
        let mut processor = BlockProcessor::new(0, burst_types::BlockHash::new([1; 32]));
        processor.set_validate_timestamps(false);
        processor.delegation_store = Some(store);
        let mut frontier = DagFrontier::new();
        let mut block = make_send_block(BlockHash::new([7; 32]), 0);
        block.account = account.clone();
        block.block_type = BlockType::ChangeRepresentative;
        block.hash = block.compute_hash();
        // The former fallback treated this public key as authorization without
        // checking a signature or binding the record to the account.
        block.signature.0[32..].copy_from_slice(&delegate.public.0);
        assert_eq!(
            processor.process(&block, &mut frontier),
            ProcessResult::Rejected("invalid signature".into())
        );
        assert!(frontier.get_head(&account).is_none());
        block.signature = burst_crypto::sign_message(block.hash.as_bytes(), &owner.private);
        assert_eq!(processor.process(&block, &mut frontier), ProcessResult::Gap);
    }

    #[test]
    fn candidate_is_retryable_until_published_after_commit() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();
        let open = make_open_block(0);
        for _ in 0..3 {
            assert_eq!(
                processor.validate_candidate(&open, &frontier),
                ProcessResult::Accepted
            );
            assert!(frontier.get_head(&open.account).is_none());
            assert!(!processor.source_known(&open.hash));
            assert!(!processor.recently_processed_order.contains(&open.hash));
        }
        processor.publish_committed(&open, &mut frontier);
        assert_eq!(frontier.get_head(&open.account), Some(&open.hash));
        assert!(processor.source_known(&open.hash));
        assert_eq!(
            processor.validate_candidate(&open, &frontier),
            ProcessResult::Duplicate
        );
        let send = make_send_block(open.hash, 0);
        assert_eq!(
            processor.validate_candidate(&send, &frontier),
            ProcessResult::Accepted
        );
        assert_eq!(frontier.get_head(&open.account), Some(&open.hash));
        assert!(!processor.source_known(&send.hash));
        processor.publish_committed(&send, &mut frontier);
        assert_eq!(frontier.get_head(&open.account), Some(&send.hash));
    }

    #[test]
    fn rejects_contents_changed_without_rehashing() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();
        let mut block = make_open_block(0);
        block.trst_balance = 1;
        assert_eq!(
            processor.process(&block, &mut frontier),
            ProcessResult::Rejected("block hash does not match its contents".into())
        );
        assert!(frontier.get_head(&block.account).is_none());
    }

    #[test]
    fn accept_valid_open_block() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();
        let block = make_open_block(0);

        let result = processor.process(&block, &mut frontier);
        assert_eq!(result, ProcessResult::Accepted);
        assert_eq!(frontier.get_head(&test_account()), Some(&block.hash));
    }

    #[test]
    fn accept_valid_send_after_open() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );

        let send = make_send_block(open.hash, 0);
        let result = processor.process(&send, &mut frontier);
        assert_eq!(result, ProcessResult::Accepted);
        assert_eq!(frontier.get_head(&test_account()), Some(&send.hash));
    }

    #[test]
    fn verification_target_is_not_a_missing_source_block() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();
        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );
        let mut vote = make_send_block(open.hash, 0);
        vote.block_type = BlockType::VerificationVote;
        vote.trst_balance = open.trst_balance;
        vote.brn_balance = open.brn_balance + 10;
        let target = keypair_from_seed(&[99; 32]);
        vote.link = BlockHash::new(target.public.0);
        vote.hash = vote.compute_hash();
        assert!(!processor.source_known(&vote.link));
        assert_eq!(
            processor.validate_candidate(&vote, &frontier),
            ProcessResult::Accepted
        );
        assert_eq!(processor.unchecked_count(), 0);
        // A receive really does depend on its linked block and must still wait.
        vote.block_type = BlockType::Receive;
        vote.hash = vote.compute_hash();
        assert_eq!(
            processor.validate_candidate(&vote, &frontier),
            ProcessResult::GapSource
        );
    }

    #[test]
    fn endorsement_waits_for_the_signed_target_head_not_the_target_address() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();
        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );
        let mut block = make_send_block(open.hash, 0);
        block.block_type = BlockType::Endorse;
        block.link = BlockHash::new(keypair_from_seed(&[99; 32]).public.0);
        block.target_head = BlockHash::new([77; 32]);
        block.hash = block.compute_hash();
        assert_eq!(
            processor.validate_candidate(&block, &frontier),
            ProcessResult::GapSource
        );
        assert_eq!(processor.unchecked_count(), 1);
        assert!(processor.process_unchecked_source(&block.link).is_empty());
        processor.mark_processed(block.target_head);
        let released = processor.process_unchecked_source(&block.target_head);
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].hash, block.hash);
        assert_eq!(
            processor.validate_candidate(&block, &frontier),
            ProcessResult::Accepted
        );
    }

    #[test]
    fn receive_open_and_governance_links_wait_for_the_same_replay_dependencies() {
        for kind in [
            BlockType::Open,
            BlockType::GovernanceEndorse,
            BlockType::GovernanceWithdraw,
            BlockType::GovernanceVote,
            BlockType::GovernanceActivation,
        ] {
            let mut processor = test_processor(0);
            let mut frontier = DagFrontier::new();
            let mut source = make_open_block(0);
            source.account = processor.genesis_account().clone();
            source.hash = source.compute_hash();
            let source_hash = source.hash;
            let mut candidate = make_open_block(0);
            if kind != BlockType::Open {
                let mut previous = make_open_block(0);
                if kind == BlockType::GovernanceActivation {
                    previous.account = processor.genesis_account().clone();
                    previous.timestamp = Timestamp::new(999_999);
                    previous.hash = previous.compute_hash();
                }
                assert_eq!(
                    processor.process(&previous, &mut frontier),
                    ProcessResult::Accepted
                );
                candidate.account = previous.account;
                candidate.previous = previous.hash;
            }
            candidate.block_type = kind.clone();
            candidate.link = source_hash;
            candidate.hash = candidate.compute_hash();
            assert_eq!(
                processor.validate_candidate(&candidate, &frontier),
                ProcessResult::GapSource,
                "{kind:?}"
            );
            assert_eq!(processor.unchecked_count(), 1);
            // A committed dependency releases exactly this candidate. It must
            // still pass ledger economics before its own frontier is published.
            processor.mark_processed(source_hash);
            let released = processor.process_unchecked_source(&source_hash);
            assert_eq!(released.len(), 1);
            assert_eq!(released[0].hash, candidate.hash);
            assert_eq!(
                processor.validate_candidate(&released[0], &frontier),
                ProcessResult::Accepted
            );
            assert_eq!(processor.unchecked_count(), 0);
        }
    }

    #[test]
    fn returned_stake_spending_waits_for_each_signed_panel_source() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();
        let previous = make_open_block(0);
        assert_eq!(
            processor.process(&previous, &mut frontier),
            ProcessResult::Accepted
        );
        let mut candidate = previous.clone();
        candidate.block_type = BlockType::ChangeRepresentative;
        candidate.previous = previous.hash;
        candidate.release_sources = vec![BlockHash::new([31; 32]), BlockHash::new([32; 32])];
        candidate.hash = candidate.compute_hash();
        for source in &candidate.release_sources {
            assert_eq!(
                processor.validate_candidate(&candidate, &frontier),
                ProcessResult::GapSource
            );
            assert_eq!(processor.unchecked_count(), 1);
            processor.mark_processed(*source);
            let released = processor.process_unchecked_source(source);
            assert_eq!(released.len(), 1);
            assert_eq!(released[0].hash, candidate.hash);
        }
        assert_eq!(
            processor.validate_candidate(&candidate, &frontier),
            ProcessResult::Accepted
        );
        assert_eq!(processor.unchecked_count(), 0);
    }

    #[test]
    fn unauthenticated_or_malformed_gap_blocks_never_enter_unchecked() {
        let key = keypair_from_seed(&[42; 32]);
        let mut processor = BlockProcessor::new(0, burst_types::BlockHash::new([1; 32]));
        processor.set_validate_timestamps(false);
        let mut frontier = DagFrontier::new();
        let mut candidate = make_send_block(BlockHash::new([9; 32]), 0);
        candidate.account = burst_crypto::derive_address(&key.public);
        candidate.hash = candidate.compute_hash();
        assert_eq!(
            processor.process(&candidate, &mut frontier),
            ProcessResult::Rejected("invalid signature".into())
        );
        assert_eq!(processor.unchecked_count(), 0);
        candidate.signature = burst_crypto::sign_message(candidate.hash.as_bytes(), &key.private);
        assert_eq!(
            processor.process(&candidate, &mut frontier),
            ProcessResult::Gap
        );
        assert_eq!(processor.unchecked_count(), 1);
    }

    #[test]
    fn stored_sources_and_fork_ancestors_remain_known_without_a_warm_cache() {
        let dir = tempfile::tempdir().unwrap();
        let store = burst_store_lmdb::LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let open = make_open_block(0);
        let send = make_send_block(open.hash, 0);
        for block in [&open, &send] {
            store
                .block_store()
                .put_block(&block.hash, &bincode::serialize(block).unwrap())
                .unwrap();
        }
        let mut processor = test_processor(0);
        processor.block_store = Some(Arc::new(store.block_store()));
        let mut frontier = DagFrontier::new();
        frontier.update(test_account(), send.hash);
        let fork = make_fork_block(open.hash, 0);
        assert_eq!(
            processor.validate_candidate(&fork, &frontier),
            ProcessResult::Fork
        );
        let receive = make_receive_block_bp(send.hash, open.hash, 0);
        assert_eq!(
            processor.validate_candidate(&receive, &frontier),
            ProcessResult::Accepted
        );
        assert_eq!(processor.unchecked_count(), 0);
        assert_eq!(processor.recently_processed_count(), 0);
        assert_eq!(frontier.get_head(&test_account()), Some(&send.hash));
    }

    #[test]
    fn accept_chain_of_three_blocks() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );

        let send1 = make_send_block(open.hash, 0);
        assert_eq!(
            processor.process(&send1, &mut frontier),
            ProcessResult::Accepted
        );

        let send2 = make_send_block(send1.hash, 0);
        assert_eq!(
            processor.process(&send2, &mut frontier),
            ProcessResult::Accepted
        );

        assert_eq!(frontier.get_head(&test_account()), Some(&send2.hash));
    }

    // ── Duplicate detection ─────────────────────────────────────────────

    #[test]
    fn duplicate_block_detected() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();
        let block = make_open_block(0);

        assert_eq!(
            processor.process(&block, &mut frontier),
            ProcessResult::Accepted
        );
        assert_eq!(
            processor.process(&block, &mut frontier),
            ProcessResult::Duplicate
        );
    }

    #[test]
    fn dedup_cache_cleared() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();
        let block = make_open_block(0);

        assert_eq!(
            processor.process(&block, &mut frontier),
            ProcessResult::Accepted
        );
        assert_eq!(processor.recently_processed_count(), 1);

        processor.clear_recently_processed();
        assert_eq!(processor.recently_processed_count(), 0);
    }

    // ── PoW validation ──────────────────────────────────────────────────

    #[test]
    fn reject_block_with_insufficient_work() {
        let min_difficulty = u64::MAX; // impossibly high
        let mut processor = test_processor(min_difficulty);
        let mut frontier = DagFrontier::new();

        // Block with work=0 won't meet u64::MAX difficulty
        let block = make_open_block(0);
        let result = processor.process(&block, &mut frontier);
        assert_eq!(
            result,
            ProcessResult::Rejected("proof-of-work does not meet minimum difficulty".into())
        );
    }

    #[test]
    fn accept_block_with_valid_work() {
        let min_difficulty = 1000;
        let mut processor = test_processor(min_difficulty);
        let mut frontier = DagFrontier::new();
        // Open blocks use a higher threshold (receive multiplier), so generate
        // work meeting the actual threshold the processor will check.
        let open_threshold = processor
            .work_thresholds()
            .threshold_for(burst_work::WorkBlockKind::ReceiveOrOpen);
        let block = make_open_block(open_threshold);

        let result = processor.process(&block, &mut frontier);
        assert_eq!(result, ProcessResult::Accepted);
    }

    // ── Signature validation ────────────────────────────────────────────

    #[test]
    fn reject_block_with_zero_signature() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let mut block = make_open_block(0);
        block.signature = Signature([0u8; 64]);

        let result = processor.process(&block, &mut frontier);
        assert_eq!(
            result,
            ProcessResult::Rejected("signature is zero (unsigned block)".into())
        );
    }

    #[test]
    fn accept_block_with_nonzero_signature() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();
        let block = make_open_block(0); // has Signature([1u8; 64])

        let result = processor.process(&block, &mut frontier);
        assert_eq!(result, ProcessResult::Accepted);
    }

    // ── Gap detection and unchecked queueing ────────────────────────────

    #[test]
    fn gap_detected_for_unknown_account() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        // A send block for an account not in the frontier
        let unknown_prev = BlockHash::new([0xFF; 32]);
        let block = make_send_block(unknown_prev, 0);

        let result = processor.process(&block, &mut frontier);
        assert_eq!(result, ProcessResult::Gap);
        assert_eq!(processor.unchecked_count(), 1);
    }

    #[test]
    fn unchecked_blocks_replayed_when_dependency_arrives() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        // Process open block
        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );

        // Create a send that references the open block's hash (valid chain)
        let send = make_send_block(open.hash, 0);

        // Create a second send that references the first send (gap — send not confirmed yet)
        let _send2 = make_send_block(send.hash, 0);

        // Process send2 first — should gap because send.hash isn't the frontier head
        // Actually, since the account exists and send2.previous != frontier_head, this is a fork.
        // Instead, let's test with an unknown account scenario.
        let mut processor2 = test_processor(0);
        let mut frontier2 = DagFrontier::new();

        let unknown_prev = BlockHash::new([0xEE; 32]);
        let gap_block = make_send_block(unknown_prev, 0);
        assert_eq!(
            processor2.process(&gap_block, &mut frontier2),
            ProcessResult::Gap
        );
        assert_eq!(processor2.unchecked_count(), 1);

        // Now the dependency "arrives" — retrieve the waiting blocks
        let dependents = processor2.process_unchecked(&unknown_prev);
        assert_eq!(dependents.len(), 1);
        assert_eq!(dependents[0].hash, gap_block.hash);
        assert_eq!(processor2.unchecked_count(), 0);
    }

    #[test]
    fn multiple_blocks_waiting_on_same_dependency() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let dep = BlockHash::new([0xAB; 32]);

        // Two different blocks both reference the same unknown previous
        let mut b1 = make_send_block(dep, 0);
        b1.trst_balance = 10;
        b1.hash = b1.compute_hash();

        let mut b2 = make_send_block(dep, 0);
        b2.trst_balance = 20;
        b2.hash = b2.compute_hash();

        assert_eq!(processor.process(&b1, &mut frontier), ProcessResult::Gap);
        assert_eq!(processor.process(&b2, &mut frontier), ProcessResult::Gap);
        assert_eq!(processor.unchecked_count(), 2);

        let dependents = processor.process_unchecked(&dep);
        assert_eq!(dependents.len(), 2);
        assert_eq!(processor.unchecked_count(), 0);
    }

    // ── Fork detection ──────────────────────────────────────────────────

    #[test]
    fn fork_detected_when_previous_mismatches_frontier() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );

        // Advance the head past `open` so that `open.hash` is a KNOWN block that
        // is no longer the frontier tip.
        let send1 = make_send_block(open.hash, 0);
        assert_eq!(
            processor.process(&send1, &mut frontier),
            ProcessResult::Accepted
        );

        // A block whose previous is a known block that isn't the current tip is
        // a genuine fork (two blocks competing for the same slot). Contrast with
        // an unknown previous, which is a gap — see `gap_when_previous_unknown`.
        let fork_block = make_fork_block(open.hash, 0);
        assert_eq!(
            processor.process(&fork_block, &mut frontier),
            ProcessResult::Fork
        );
    }

    #[test]
    fn gap_when_previous_unknown() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );

        // A block referencing an unknown previous (never seen) is a gap, not a
        // fork — we're simply missing ancestors. It must be parked as unchecked
        // so bootstrap can retry it once the predecessor arrives.
        let unknown_prev = BlockHash::new([0x99; 32]);
        let orphan = make_fork_block(unknown_prev, 0);
        assert_eq!(
            processor.process(&orphan, &mut frontier),
            ProcessResult::Gap
        );
        assert_eq!(processor.unchecked_count(), 1);
    }

    #[test]
    fn two_blocks_competing_for_same_position() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );

        // First send (accepted, extends frontier)
        let send1 = make_send_block(open.hash, 0);
        assert_eq!(
            processor.process(&send1, &mut frontier),
            ProcessResult::Accepted
        );

        // Second block also tries to extend from open.hash (now behind frontier)
        let send2 = make_fork_block(open.hash, 0);
        let result = processor.process(&send2, &mut frontier);
        assert_eq!(result, ProcessResult::Fork);
    }

    // ── Open block validation ───────────────────────────────────────────

    #[test]
    fn different_opening_blocks_compete_at_the_zero_predecessor() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let open1 = make_open_block(0);
        assert_eq!(
            processor.process(&open1, &mut frontier),
            ProcessResult::Accepted
        );

        // Create a different open block for the same account
        let mut open2 = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Open,
            account: test_account(),
            previous: BlockHash::ZERO,
            representative: test_representative(),
            brn_balance: 2000,
            trst_balance: 0,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::new([0xFF; 32]),
            timestamp: Timestamp::new(2_000_000),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([5u8; 64]),
            hash: BlockHash::ZERO,
        };
        open2.hash = open2.compute_hash();

        let result = processor.process(&open2, &mut frontier);
        assert_eq!(result, ProcessResult::Fork);
    }

    #[test]
    fn reject_open_block_with_nonzero_previous() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Open,
            account: test_account(),
            previous: BlockHash::new([0x11; 32]), // should be zero for open
            representative: test_representative(),
            brn_balance: 1000,
            trst_balance: 0,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_000),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([1u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();

        let result = processor.process(&block, &mut frontier);
        assert_eq!(
            result,
            ProcessResult::Rejected("open block must have zero previous hash".into())
        );
    }

    // ── Non-open block with zero previous for existing account ──────────

    #[test]
    fn reject_non_open_block_with_zero_previous() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );

        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Send,
            account: test_account(),
            previous: BlockHash::ZERO,
            representative: test_representative(),
            brn_balance: 900,
            trst_balance: 100,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_001),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([4u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();

        let mut empty_frontier = DagFrontier::new();
        assert_eq!(
            processor.process(&block, &mut empty_frontier),
            ProcessResult::Rejected("non-open block has zero previous hash".into())
        );
        assert_eq!(processor.unchecked_count(), 0);

        let result = processor.process(&block, &mut frontier);
        assert_eq!(
            result,
            ProcessResult::Rejected("non-open block has zero previous hash".into())
        );
    }

    // ── Frontier state ──────────────────────────────────────────────────

    #[test]
    fn frontier_not_updated_on_rejection() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        // Zero-sig block is rejected
        let mut block = make_open_block(0);
        block.signature = Signature([0u8; 64]);

        processor.process(&block, &mut frontier);
        assert!(frontier.get_head(&test_account()).is_none());
    }

    #[test]
    fn frontier_not_updated_on_fork() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );

        // Advance past `open` so a fork off it (known, non-head previous) is a
        // genuine fork rather than a gap.
        let send1 = make_send_block(open.hash, 0);
        assert_eq!(
            processor.process(&send1, &mut frontier),
            ProcessResult::Accepted
        );
        let head_after = *frontier.get_head(&test_account()).unwrap();

        let fork = make_fork_block(open.hash, 0);
        assert_eq!(processor.process(&fork, &mut frontier), ProcessResult::Fork);

        // Frontier should be unchanged after a fork
        assert_eq!(frontier.get_head(&test_account()), Some(&head_after));
    }

    // ── Integration-style: process block, then replay unchecked ─────────

    #[test]
    fn end_to_end_gap_then_fill() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        // Process an open block
        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );

        // Create send1 (chains from open) and send2 (chains from send1)
        let send1 = make_send_block(open.hash, 0);
        let send2 = make_send_block(send1.hash, 0);

        // Process send2 first — its previous (send1) is unknown, so this is a
        // GAP (missing ancestor), not a fork. It is parked as unchecked.
        assert_eq!(processor.process(&send2, &mut frontier), ProcessResult::Gap);
        assert_eq!(processor.unchecked_count(), 1);

        // Process send1 — chains from open.hash (the tip) → accepted.
        assert_eq!(
            processor.process(&send1, &mut frontier),
            ProcessResult::Accepted
        );
        assert_eq!(frontier.get_head(&test_account()), Some(&send1.hash));

        // send1 unblocks send2: drain the unchecked dependents and process them.
        let dependents = processor.process_unchecked(&send1.hash);
        assert_eq!(dependents.len(), 1);
        assert_eq!(dependents[0].hash, send2.hash);
        assert_eq!(
            processor.process(&dependents[0], &mut frontier),
            ProcessResult::Accepted
        );
        assert_eq!(frontier.get_head(&test_account()), Some(&send2.hash));
    }

    // ── Epoch block processing ───────────────────────────────────────────

    fn make_epoch_block(previous: BlockHash, account: WalletAddress) -> StateBlock {
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Epoch,
            account,
            previous,
            representative: test_representative(),
            brn_balance: 1000,
            trst_balance: 0,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(2_000_000),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([7u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block
    }

    #[test]
    fn epoch_block_accepted_on_existing_chain() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );

        let epoch = make_epoch_block(open.hash, test_account());
        let result = processor.process(&epoch, &mut frontier);
        assert_eq!(result, ProcessResult::Accepted);
        assert_eq!(frontier.get_head(&test_account()), Some(&epoch.hash));
    }

    #[test]
    fn epoch_block_rejected_for_nonexistent_account() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let epoch = make_epoch_block(BlockHash::new([0xAA; 32]), test_account());
        let result = processor.process(&epoch, &mut frontier);
        assert_eq!(
            result,
            ProcessResult::Rejected("epoch block targets account that does not exist".into())
        );
    }

    #[test]
    fn epoch_block_rejected_with_wrong_previous() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );

        let wrong_prev = BlockHash::new([0xBB; 32]);
        let epoch = make_epoch_block(wrong_prev, test_account());
        let result = processor.process(&epoch, &mut frontier);
        assert_eq!(
            result,
            ProcessResult::Rejected("epoch block previous does not match account head".into())
        );
    }

    #[test]
    fn epoch_block_rejected_with_zero_previous() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let epoch = make_epoch_block(BlockHash::ZERO, test_account());
        let result = processor.process(&epoch, &mut frontier);
        assert_eq!(
            result,
            ProcessResult::Rejected("epoch block must reference an existing account chain".into())
        );
    }

    #[test]
    fn send_block_after_epoch_accepted() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );

        let epoch = make_epoch_block(open.hash, test_account());
        assert_eq!(
            processor.process(&epoch, &mut frontier),
            ProcessResult::Accepted
        );

        let send = make_send_block(epoch.hash, 0);
        let result = processor.process(&send, &mut frontier);
        assert_eq!(result, ProcessResult::Accepted);
        assert_eq!(frontier.get_head(&test_account()), Some(&send.hash));
    }

    // ── Real Ed25519 signature verification ──────────────────────────────

    #[test]
    fn accept_block_with_valid_ed25519_signature() {
        let kp = generate_keypair();
        let address = derive_address(&kp.public);

        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Open,
            account: address,
            previous: BlockHash::ZERO,
            representative: test_representative(),
            brn_balance: 1000,
            trst_balance: 0,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_000),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([0u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block.signature = sign_message(block.hash.as_bytes(), &kp.private);

        let mut processor = BlockProcessor::new(0, burst_types::BlockHash::new([1; 32]));
        processor.set_validate_timestamps(false); // fixtures use epoch-era timestamps
        let mut frontier = DagFrontier::new();
        let result = processor.process(&block, &mut frontier);
        assert_eq!(result, ProcessResult::Accepted);
    }

    #[test]
    fn reject_block_with_wrong_ed25519_signature() {
        let kp = generate_keypair();
        let kp2 = generate_keypair();
        let address = derive_address(&kp.public);

        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Open,
            account: address,
            previous: BlockHash::ZERO,
            representative: test_representative(),
            brn_balance: 1000,
            trst_balance: 0,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_000),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([0u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block.signature = sign_message(block.hash.as_bytes(), &kp2.private);

        let mut processor = BlockProcessor::new(0, burst_types::BlockHash::new([1; 32]));
        processor.set_validate_timestamps(false); // fixtures use epoch-era timestamps
        let mut frontier = DagFrontier::new();
        let result = processor.process(&block, &mut frontier);
        assert_eq!(result, ProcessResult::Rejected("invalid signature".into()));
    }

    #[test]
    fn epoch_block_signature_verified_against_genesis_key() {
        let genesis_kp = keypair_from_seed(&[42u8; 32]);
        let genesis_address = derive_address(&genesis_kp.public);

        let account_kp = generate_keypair();
        let account_address = derive_address(&account_kp.public);

        let mut processor = BlockProcessor::with_genesis_account(
            0,
            genesis_address.clone(),
            burst_types::BlockHash::new([1; 32]),
        );
        processor.set_validate_timestamps(false); // fixtures use epoch-era timestamps
        let mut frontier = DagFrontier::new();

        // Open block for the account (signed by account key)
        let mut open = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Open,
            account: account_address.clone(),
            previous: BlockHash::ZERO,
            representative: account_address.clone(),
            brn_balance: 1000,
            trst_balance: 0,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_000),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([0u8; 64]),
            hash: BlockHash::ZERO,
        };
        open.hash = open.compute_hash();
        open.signature = sign_message(open.hash.as_bytes(), &account_kp.private);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );

        // Epoch block targeting the account, signed by genesis key
        let mut epoch = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Epoch,
            account: account_address.clone(),
            previous: open.hash,
            representative: account_address.clone(),
            brn_balance: 1000,
            trst_balance: 0,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(2_000_000),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([0u8; 64]),
            hash: BlockHash::ZERO,
        };
        epoch.hash = epoch.compute_hash();
        epoch.signature = sign_message(epoch.hash.as_bytes(), &genesis_kp.private);
        assert_eq!(
            processor.process(&epoch, &mut frontier),
            ProcessResult::Accepted
        );
    }

    #[test]
    fn epoch_block_rejected_when_signed_by_wrong_key() {
        let genesis_kp = keypair_from_seed(&[42u8; 32]);
        let genesis_address = derive_address(&genesis_kp.public);

        let account_kp = generate_keypair();
        let account_address = derive_address(&account_kp.public);

        let mut processor = BlockProcessor::with_genesis_account(
            0,
            genesis_address,
            burst_types::BlockHash::new([1; 32]),
        );
        processor.set_validate_timestamps(false); // fixtures use epoch-era timestamps
        let mut frontier = DagFrontier::new();

        // Open block for account
        let mut open = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Open,
            account: account_address.clone(),
            previous: BlockHash::ZERO,
            representative: account_address.clone(),
            brn_balance: 1000,
            trst_balance: 0,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_000),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([0u8; 64]),
            hash: BlockHash::ZERO,
        };
        open.hash = open.compute_hash();
        open.signature = sign_message(open.hash.as_bytes(), &account_kp.private);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );

        // Epoch block signed by the ACCOUNT key (wrong — should be genesis)
        let mut epoch = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Epoch,
            account: account_address.clone(),
            previous: open.hash,
            representative: account_address.clone(),
            brn_balance: 1000,
            trst_balance: 0,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(2_000_000),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([0u8; 64]),
            hash: BlockHash::ZERO,
        };
        epoch.hash = epoch.compute_hash();
        epoch.signature = sign_message(epoch.hash.as_bytes(), &account_kp.private);
        assert_eq!(
            processor.process(&epoch, &mut frontier),
            ProcessResult::Rejected("invalid signature".into())
        );
    }

    // ── Balance validation ───────────────────────────────────────────────

    #[test]
    fn balance_validation_send_valid() {
        let mut block = make_send_block(BlockHash::new([0x11; 32]), 0);
        block.brn_balance = 1000;
        block.trst_balance = 50;
        block.hash = block.compute_hash();

        assert!(BlockProcessor::validate_balance_transition(&block, 1000, 100).is_ok());
    }

    #[test]
    fn balance_validation_send_rejects_trst_increase() {
        let mut block = make_send_block(BlockHash::new([0x11; 32]), 0);
        block.brn_balance = 1000;
        block.trst_balance = 200;
        block.hash = block.compute_hash();

        let result = BlockProcessor::validate_balance_transition(&block, 1000, 100);
        assert_eq!(
            result,
            Err("send block cannot increase TRST balance".into())
        );
    }

    #[test]
    fn balance_validation_send_rejects_brn_change() {
        let mut block = make_send_block(BlockHash::new([0x11; 32]), 0);
        block.brn_balance = 999;
        block.trst_balance = 50;
        block.hash = block.compute_hash();

        let result = BlockProcessor::validate_balance_transition(&block, 1000, 100);
        assert_eq!(result, Err("send block cannot change spent BRN".into()));
    }

    #[test]
    fn balance_validation_receive_valid() {
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Receive,
            account: test_account(),
            previous: BlockHash::new([0x11; 32]),
            representative: test_representative(),
            brn_balance: 1000,
            trst_balance: 200,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_001),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([1u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();

        assert!(BlockProcessor::validate_balance_transition(&block, 1000, 100).is_ok());
    }

    #[test]
    fn balance_validation_receive_rejects_trst_decrease() {
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Receive,
            account: test_account(),
            previous: BlockHash::new([0x11; 32]),
            representative: test_representative(),
            brn_balance: 1000,
            trst_balance: 50,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_001),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([1u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();

        let result = BlockProcessor::validate_balance_transition(&block, 1000, 100);
        assert_eq!(
            result,
            Err("receive block cannot decrease TRST balance".into())
        );
    }

    #[test]
    fn open_block_rejects_self_reported_brn() {
        // The birthright is computed, never claimed: open blocks must not
        // carry any spent BRN.
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Open,
            account: test_account(),
            previous: BlockHash::ZERO,
            representative: test_representative(),
            brn_balance: 12345,
            trst_balance: 0,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_001),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([1u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        assert!(BlockProcessor::validate_open_block(&block, None).is_err());

        block.brn_balance = 0;
        block.hash = block.compute_hash();
        assert!(BlockProcessor::validate_open_block(&block, None).is_ok());
    }

    #[test]
    fn open_block_rejects_self_reported_trst() {
        // TRST only enters a chain from a real pending send: an open block
        // claiming TRST must match a pending entry exactly.
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Open,
            account: test_account(),
            previous: BlockHash::ZERO,
            representative: test_representative(),
            brn_balance: 0,
            trst_balance: 777,
            link: BlockHash::new([9u8; 32]),
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_001),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([1u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();

        // No pending → any claimed TRST is rejected.
        assert!(BlockProcessor::validate_open_block(&block, None).is_err());
        // Pending with a different amount → rejected.
        assert!(BlockProcessor::validate_open_block(&block, Some(500)).is_err());
        // Pending matching exactly → receive-open accepted.
        assert!(BlockProcessor::validate_open_block(&block, Some(777)).is_ok());
    }

    #[test]
    fn balance_validation_burn_valid() {
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Burn,
            account: test_account(),
            previous: BlockHash::new([0x11; 32]),
            representative: test_representative(),
            // Ascending odometer: prev 1000 spent + 500 burned.
            brn_balance: 1500,
            trst_balance: 100,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_001),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([1u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();

        assert!(BlockProcessor::validate_balance_transition(&block, 1000, 100).is_ok());
    }

    #[test]
    fn balance_validation_burn_rejects_spent_brn_decrease() {
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Burn,
            account: test_account(),
            previous: BlockHash::new([0x11; 32]),
            representative: test_representative(),
            // Odometer going backwards (un-spending BRN) must be rejected.
            brn_balance: 500,
            trst_balance: 100,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_001),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([1u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();

        let result = BlockProcessor::validate_balance_transition(&block, 1000, 100);
        assert!(result.is_err());
    }

    #[test]
    fn balance_validation_epoch_must_preserve_balances() {
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Epoch,
            account: test_account(),
            previous: BlockHash::new([0x11; 32]),
            representative: test_representative(),
            brn_balance: 1000,
            trst_balance: 100,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_001),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([1u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();

        // Same balances → OK
        assert!(BlockProcessor::validate_balance_transition(&block, 1000, 100).is_ok());

        // Changed BRN → fail
        let result = BlockProcessor::validate_balance_transition(&block, 999, 100);
        assert_eq!(result, Err("epoch block cannot change balances".into()));

        // Changed TRST → fail
        let result = BlockProcessor::validate_balance_transition(&block, 1000, 99);
        assert_eq!(result, Err("epoch block cannot change balances".into()));
    }

    // ── Rollback tests ───────────────────────────────────────────────────

    #[test]
    fn persistent_rollback_cannot_delete_a_block_or_rewrite_the_frontier() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            burst_store_lmdb::LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap();
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();
        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );
        let blocks = Arc::new(store.block_store());
        blocks
            .put_block(&open.hash, &bincode::serialize(&open).unwrap())
            .unwrap();
        processor.block_store = Some(blocks.clone());
        assert_eq!(
            processor.rollback(&open, &mut frontier),
            RollbackResult::RequiresLedgerRollback
        );
        assert!(processor
            .rollback_to(&open.account, &open.hash, &mut frontier, &*blocks)
            .is_err());
        assert!(processor
            .cascade_rollback(&open.hash, &open.account, &mut frontier, &*blocks)
            .is_err());
        assert_eq!(frontier.get_head(&open.account), Some(&open.hash));
        assert_eq!(processor.recently_processed_count(), 1);
        assert!(blocks.get_block(&open.hash).is_ok());
    }

    #[test]
    fn rollback_removes_head_and_restores_previous() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );

        let send = make_send_block(open.hash, 0);
        assert_eq!(
            processor.process(&send, &mut frontier),
            ProcessResult::Accepted
        );
        assert_eq!(frontier.get_head(&test_account()), Some(&send.hash));

        let result = processor.rollback(&send, &mut frontier);
        assert_eq!(result, RollbackResult::Success);
        assert_eq!(frontier.get_head(&test_account()), Some(&open.hash));
    }

    #[test]
    fn rollback_open_block_removes_account() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );

        let result = processor.rollback(&open, &mut frontier);
        assert_eq!(result, RollbackResult::Success);
        assert!(frontier.get_head(&test_account()).is_none());
        assert_eq!(frontier.account_count(), 0);
    }

    #[test]
    fn rollback_not_head_fails() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );

        let send = make_send_block(open.hash, 0);
        assert_eq!(
            processor.process(&send, &mut frontier),
            ProcessResult::Accepted
        );

        // Try to roll back the open block (not the head)
        let result = processor.rollback(&open, &mut frontier);
        assert_eq!(result, RollbackResult::NotHead);
        assert_eq!(frontier.get_head(&test_account()), Some(&send.hash));
    }

    #[test]
    fn rollback_unknown_account_fails() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let open = make_open_block(0);
        // Don't process it — account not in frontier
        let result = processor.rollback(&open, &mut frontier);
        assert_eq!(result, RollbackResult::AccountNotFound);
    }

    #[test]
    fn rollback_clears_from_recently_processed() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );
        assert_eq!(processor.recently_processed_count(), 1);

        processor.rollback(&open, &mut frontier);
        assert_eq!(processor.recently_processed_count(), 0);
    }

    // ── Gap-source tests ─────────────────────────────────────────────────

    fn make_receive_block_bp(
        previous: BlockHash,
        source: BlockHash,
        difficulty: u64,
    ) -> StateBlock {
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Receive,
            account: test_account(),
            previous,
            representative: test_representative(),
            brn_balance: 1000,
            trst_balance: 200,
            link: source,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_010),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([6u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();

        if difficulty > 0 {
            let generator = WorkGenerator;
            let nonce = generator.generate(&block.work_root(), difficulty).unwrap();
            block.work = nonce.0;
        }

        block
    }

    #[test]
    fn receive_block_gap_source_when_send_unknown() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );

        // Create a receive block referencing an unknown source send block
        let unknown_source = BlockHash::new([0xAA; 32]);
        let recv = make_receive_block_bp(open.hash, unknown_source, 0);

        let result = processor.process(&recv, &mut frontier);
        assert_eq!(result, ProcessResult::GapSource);
        assert_eq!(processor.unchecked_count(), 1);

        // Frontier should NOT have been updated
        assert_eq!(frontier.get_head(&test_account()), Some(&open.hash));
    }

    #[test]
    fn receive_block_accepted_when_source_known() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        // Process open and send blocks to populate recently_processed
        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );

        let send = make_send_block(open.hash, 0);
        assert_eq!(
            processor.process(&send, &mut frontier),
            ProcessResult::Accepted
        );

        // Create a receive block referencing the send block as source
        let recv = make_receive_block_bp(send.hash, open.hash, 0);
        // open.hash is in recently_processed, so the source check passes
        let result = processor.process(&recv, &mut frontier);
        assert_eq!(result, ProcessResult::Accepted);
    }

    #[test]
    fn gap_source_dependents_released_when_source_arrives() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );

        // Queue a receive block waiting for an unknown source
        let unknown_source = BlockHash::new([0xBB; 32]);
        let recv = make_receive_block_bp(open.hash, unknown_source, 0);

        assert_eq!(
            processor.process(&recv, &mut frontier),
            ProcessResult::GapSource
        );
        assert_eq!(processor.unchecked_count(), 1);

        // Source "arrives" — drain the gap-source dependents
        let source_deps = processor.process_unchecked_source(&unknown_source);
        assert_eq!(source_deps.len(), 1);
        assert_eq!(source_deps[0].hash, recv.hash);
        assert_eq!(processor.unchecked_count(), 0);
    }

    #[test]
    fn receive_with_zero_link_accepted_without_source_check() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();

        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );

        // Receive block with zero link — no source check needed
        let recv = make_receive_block_bp(open.hash, BlockHash::ZERO, 0);
        let result = processor.process(&recv, &mut frontier);
        assert_eq!(result, ProcessResult::Accepted);
    }

    // ── ProcessingQueue tests ───────────────────────────────────────────

    fn make_block_context(source: BlockSource) -> BlockContext {
        let block = make_open_block(0);
        BlockContext {
            block,
            source,
            received_at: Timestamp::new(1_000_000),
        }
    }

    #[test]
    fn queue_local_priority_over_network() {
        let mut q = ProcessingQueue::new(100);

        let net = make_block_context(BlockSource::Network);
        let local = make_block_context(BlockSource::Local);
        let net_hash = net.block.hash;
        let local_hash = local.block.hash;

        assert!(q.enqueue(net));
        assert!(q.enqueue(local));
        assert_eq!(q.len(), 2);

        let first = q.dequeue().unwrap();
        assert_eq!(first.block.hash, local_hash);

        let second = q.dequeue().unwrap();
        assert_eq!(second.block.hash, net_hash);

        assert!(q.is_empty());
    }

    #[test]
    fn queue_backpressure_rejects_when_full() {
        let mut q = ProcessingQueue::new(2);

        assert!(q.enqueue(make_block_context(BlockSource::Network)));
        assert!(q.enqueue(make_block_context(BlockSource::Local)));
        assert!(!q.enqueue(make_block_context(BlockSource::Bootstrap)));
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn queue_empty_dequeue_returns_none() {
        let mut q = ProcessingQueue::new(10);
        assert!(q.dequeue().is_none());
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn queue_bootstrap_and_unchecked_go_to_network_queue() {
        let mut q = ProcessingQueue::new(100);

        assert!(q.enqueue(make_block_context(BlockSource::Bootstrap)));
        assert!(q.enqueue(make_block_context(BlockSource::Unchecked)));
        assert_eq!(q.len(), 2);

        let first = q.dequeue().unwrap();
        assert_eq!(first.source, BlockSource::Bootstrap);

        let second = q.dequeue().unwrap();
        assert_eq!(second.source, BlockSource::Unchecked);
    }

    #[test]
    fn queue_interleaved_local_always_first() {
        let mut q = ProcessingQueue::new(100);

        for _ in 0..3 {
            q.enqueue(make_block_context(BlockSource::Network));
        }
        q.enqueue(make_block_context(BlockSource::Local));

        let first = q.dequeue().unwrap();
        assert_eq!(first.source, BlockSource::Local);
    }

    // ── Cascade rollback tests ──────────────────────────────────────────

    fn store_block(store: &burst_nullables::NullStore, block: &StateBlock) {
        let bytes = bincode::serialize(block).unwrap();
        store.put_block(&block.hash, &bytes).unwrap();
    }

    #[test]
    fn rollback_to_rolls_back_chain() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();
        let store = burst_nullables::NullStore::new();

        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );
        store_block(&store, &open);

        let send1 = make_send_block(open.hash, 0);
        assert_eq!(
            processor.process(&send1, &mut frontier),
            ProcessResult::Accepted
        );
        store_block(&store, &send1);

        let send2 = make_send_block(send1.hash, 0);
        assert_eq!(
            processor.process(&send2, &mut frontier),
            ProcessResult::Accepted
        );
        store_block(&store, &send2);

        assert_eq!(frontier.get_head(&test_account()), Some(&send2.hash));

        // Roll back to send1 (should remove send2)
        let rolled = processor
            .rollback_to(&test_account(), &send1.hash, &mut frontier, &store)
            .unwrap();
        assert_eq!(rolled.len(), 2);
        assert_eq!(rolled[0], send2.hash);
        assert_eq!(rolled[1], send1.hash);
        assert_eq!(frontier.get_head(&test_account()), Some(&open.hash));
    }

    #[test]
    fn rollback_to_single_block() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();
        let store = burst_nullables::NullStore::new();

        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );
        store_block(&store, &open);

        let send = make_send_block(open.hash, 0);
        assert_eq!(
            processor.process(&send, &mut frontier),
            ProcessResult::Accepted
        );
        store_block(&store, &send);

        // Roll back just the head (target == head)
        let rolled = processor
            .rollback_to(&test_account(), &send.hash, &mut frontier, &store)
            .unwrap();
        assert_eq!(rolled.len(), 1);
        assert_eq!(rolled[0], send.hash);
        assert_eq!(frontier.get_head(&test_account()), Some(&open.hash));
    }

    #[test]
    fn rollback_to_open_removes_account() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();
        let store = burst_nullables::NullStore::new();

        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );
        store_block(&store, &open);

        let send = make_send_block(open.hash, 0);
        assert_eq!(
            processor.process(&send, &mut frontier),
            ProcessResult::Accepted
        );
        store_block(&store, &send);

        // Roll back to (and including) the open block — account should be removed
        let rolled = processor
            .rollback_to(&test_account(), &open.hash, &mut frontier, &store)
            .unwrap();
        assert_eq!(rolled.len(), 2);
        assert!(frontier.get_head(&test_account()).is_none());
    }

    #[test]
    fn rollback_to_unknown_account_fails() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();
        let store = burst_nullables::NullStore::new();

        let result = processor.rollback_to(
            &test_account(),
            &BlockHash::new([0xFF; 32]),
            &mut frontier,
            &store,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("account not found"));
    }

    #[test]
    fn rollback_to_missing_target_fails() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();
        let store = burst_nullables::NullStore::new();

        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );
        store_block(&store, &open);

        // Target hash that's not in the chain
        let result = processor.rollback_to(
            &test_account(),
            &BlockHash::new([0xFF; 32]),
            &mut frontier,
            &store,
        );
        assert!(result.is_err());
    }

    #[test]
    fn cascade_rollback_delegates_to_rollback_to() {
        let mut processor = test_processor(0);
        let mut frontier = DagFrontier::new();
        let store = burst_nullables::NullStore::new();

        let open = make_open_block(0);
        assert_eq!(
            processor.process(&open, &mut frontier),
            ProcessResult::Accepted
        );
        store_block(&store, &open);

        let send1 = make_send_block(open.hash, 0);
        assert_eq!(
            processor.process(&send1, &mut frontier),
            ProcessResult::Accepted
        );
        store_block(&store, &send1);

        let send2 = make_send_block(send1.hash, 0);
        assert_eq!(
            processor.process(&send2, &mut frontier),
            ProcessResult::Accepted
        );
        store_block(&store, &send2);

        let rolled = processor
            .cascade_rollback(&send1.hash, &test_account(), &mut frontier, &store)
            .unwrap();
        assert_eq!(rolled.len(), 2);
        assert_eq!(rolled[0], send2.hash);
        assert_eq!(rolled[1], send1.hash);
        assert_eq!(frontier.get_head(&test_account()), Some(&open.hash));
    }
}
