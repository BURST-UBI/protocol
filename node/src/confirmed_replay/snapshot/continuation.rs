//! Authenticated economic state carried through certified epoch transitions.

use super::{
    archive::AuthenticatedSnapshot,
    history::{HistoryContext, HistoryCursor},
    tail::SnapshotTail,
    CertifiedEvent, ConfirmedReplay, ReplayError,
};
pub use crate::confirmed_replay::selections::source::SelectionSource;
use crate::{
    checkpoint_agreement::ReplayManifest,
    checkpoint_replay::{CheckpointManifest, ReplayedCheckpoint},
    checkpoint_store::CertifiedCheckpoint,
    confirmed_history::HistoryHead,
    confirmed_replay::{
        blocks::{source::Source, AppliedBlocks},
        events::{Boundary, EpochInput, InputMode},
        LifecycleHistory,
    },
};
use burst_consensus::checkpoint::{CheckpointAnchor, CheckpointCertificate};
use burst_store_lmdb::LmdbStore;
use burst_types::{BlockHash, WalletAddress};

mod activation;
mod committed;
mod coverage;
mod governance;
pub use activation::CompactActivationCoverage;
pub use governance::CompactGovernanceCoverage;
mod cut;
mod deadlines;
mod discovery;
mod installation;
pub use deadlines::CompactDeadlineCoverage;
mod outcomes;
pub use outcomes::CompactOutcomeCoverage;
mod roster;
pub use roster::CompactRosterCoverage;

pub(super) struct Base<'a> {
    pub anchor: &'a CheckpointAnchor,
    pub context: HistoryContext,
    pub state: &'a ConfirmedReplay,
}

/// Constructed from configured genesis, an authenticated archive or certified replay.
/// This is neither an installed ledger nor a quorum signature over a new archive.
#[derive(Clone)]
pub struct SnapshotContinuation {
    anchor: CheckpointAnchor,
    context: HistoryContext,
    state: burst_utils::Shared<ConfirmedReplay>,
}

/// Membership coverage derived from a replayed selection and its authenticated
/// base. This is not a complete history inventory or permission to sign.
pub struct CompactSelectionCoverage {
    replay: ReplayedCheckpoint,
    cut: CompactCutCoverage,
}

pub(super) struct CompactCutCoverage {
    archive: super::tail::TailArchive,
    parent: BlockHash,
    included: std::collections::HashSet<BlockHash>,
    authority: Vec<u8>,
    cutoff: u64,
    vote_limit: Vec<u8>,
    floors: std::collections::HashMap<WalletAddress, u64>,
    positions: std::collections::HashMap<(WalletAddress, u64), BlockHash>,
}

impl CompactSelectionCoverage {
    pub(crate) fn archive_rows(&self) -> super::tail::ArchiveRows<'_> {
        self.cut.archive.rows()
    }
    pub fn replay(&self) -> &ReplayedCheckpoint {
        &self.replay
    }

    /// Recheck current promises without changing any fence or signer journal.
    /// A later signing transaction must repeat this check atomically.
    pub fn check_local_promises(
        &self,
        batch: &burst_store_lmdb::LmdbWriteTransaction<'_>,
        representative: &WalletAddress,
    ) -> Result<(), burst_store::StoreError> {
        batch.check_consensus_vote_coverage(
            representative,
            &self.cut.parent,
            &self.cut.parent,
            &self.cut.included,
        )
    }
}

impl AuthenticatedSnapshot {
    pub(super) fn base(&self) -> Base<'_> {
        Base {
            anchor: self.anchor(),
            context: self.history_context(),
            state: &self.state,
        }
    }

    pub fn into_continuation(self) -> SnapshotContinuation {
        SnapshotContinuation {
            anchor: self.anchor().clone(),
            context: self.history_context(),
            state: self.into_state().into(),
        }
    }
}

impl SnapshotContinuation {
    pub(in crate::confirmed_replay::snapshot) fn from_genesis(
        network: burst_types::NetworkId,
        initial_params: &burst_types::ProtocolParams,
    ) -> Result<Self, ReplayError> {
        let anchor = ReplayedCheckpoint::genesis_anchor(network, initial_params)
            .map_err(|error| super::invalid(error.to_string()))?;
        let genesis = crate::genesis_state::block(network, initial_params);
        let head = HistoryHead {
            account: genesis.account.clone(),
            hash: genesis.hash,
            height: 1,
        };
        let history = crate::confirmed_history::ConfirmedHistory::from_parts(
            network,
            genesis.hash,
            anchor.cutoff(),
            &[head],
            &[],
            vec![genesis],
            vec![],
        )
        .map_err(|error| super::invalid(error.to_string()))?;
        let state = ConfirmedReplay::replay(&history, network, initial_params)?;
        let manifest = state.snapshot_manifest(&anchor)?;
        Ok(Self {
            context: HistoryContext {
                network,
                genesis: anchor.genesis_hash(),
                epoch: anchor.hash(),
                state_hash: manifest.state_hash,
            },
            anchor,
            state: state.into(),
        })
    }

    fn base(&self) -> Base<'_> {
        Base {
            anchor: &self.anchor,
            context: self.context,
            state: &self.state,
        }
    }
    pub fn anchor(&self) -> &CheckpointAnchor {
        &self.anchor
    }
    pub fn state(&self) -> &ConfirmedReplay {
        &self.state
    }
    pub fn history_context(&self) -> HistoryContext {
        self.context
    }

    pub fn plan_cemented_tail(
        &self,
        store: &LmdbStore,
    ) -> Result<super::tail::cemented::CementedTailPlan, ReplayError> {
        super::tail::cemented::plan(self.base(), store)
    }

    /// Capture both local cuts from one read view, authenticating their shared
    /// snapshot prefix once. Each tail retains its own suffix and read checks.
    pub(crate) fn plan_installed_tails(
        &self,
        store: &LmdbStore,
    ) -> Result<
        (
            super::tail::cemented::CementedTailPlan,
            super::tail::cemented::AppliedTailPlan,
            Vec<super::history::HistoricalBlock>,
        ),
        ReplayError,
    > {
        super::tail::cemented::plan_pair(self.base(), store)
    }

    pub(crate) fn plan_installed_tails_view(
        &self,
        store: &LmdbStore,
        txn: &burst_store_lmdb::LmdbReadTransaction<'_>,
    ) -> Result<
        (
            super::tail::cemented::CementedTailPlan,
            super::tail::cemented::AppliedTailPlan,
            Vec<super::history::HistoricalBlock>,
        ),
        ReplayError,
    > {
        super::tail::cemented::plan_pair_view(self.base(), store, txn)
    }

    /// Includes provisional heads without granting them confirmation or
    /// economic validity. The returned type cannot stand in for a cemented cut.
    pub fn plan_applied_tail(
        &self,
        store: &LmdbStore,
    ) -> Result<super::tail::cemented::AppliedTailPlan, ReplayError> {
        super::tail::cemented::plan_applied(self.base(), store)
    }

    /// Authenticate installed markers and the cemented governance schedule.
    /// This deliberately does not authorize startup: provisional economics,
    /// caches, sparse retained history and signer journals need separate checks.
    pub fn verify_installed_metadata(
        &self,
        store: &LmdbStore,
        cemented: &super::tail::cemented::CementedTail,
    ) -> Result<(), burst_store::StoreError> {
        let batch = store.tx_begin_write()?;
        self.verify_installed_metadata_in(&batch, cemented)
    }

    pub(crate) fn verify_installed_metadata_in(
        &self,
        batch: &burst_store_lmdb::LmdbWriteTransaction<'_>,
        cemented: &super::tail::cemented::CementedTail,
    ) -> Result<(), burst_store::StoreError> {
        self.verify_installed_metadata_read(
            &batch.read_view(),
            cemented,
            |schedule| schedule.verify_compact_tail(batch, cemented.tail()),
            || cemented.verify_view(&batch.read_view()),
        )
    }

    pub(crate) fn verify_installed_metadata_view_with_poll(
        &self,
        batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
        cemented: &super::tail::cemented::CementedTail,
        poll: &dyn Fn(&'static str) -> Result<(), burst_store::StoreError>,
    ) -> Result<(), burst_store::StoreError> {
        self.verify_installed_metadata_read(
            batch,
            cemented,
            |schedule| schedule.verify_compact_tail_view_with_poll(batch, cemented.tail(), poll),
            || cemented.verify_view_with_poll(batch, poll),
        )
    }

    /// Candidate discovery tolerates provisional extensions only. Confirmed
    /// heads, retained bodies, installed authority and governance remain checked.
    pub(crate) fn verify_discovery_metadata_in(
        &self,
        batch: &burst_store_lmdb::LmdbWriteTransaction<'_>,
        cemented: &super::tail::cemented::CementedTail,
    ) -> Result<(), burst_store::StoreError> {
        self.verify_installed_metadata_read(
            &batch.read_view(),
            cemented,
            |schedule| schedule.verify_compact_tail(batch, cemented.tail()),
            || cemented.verify_confirmed_view(&batch.read_view()),
        )
    }

    fn verify_installed_metadata_read(
        &self,
        batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
        cemented: &super::tail::cemented::CementedTail,
        verify_schedule: impl FnOnce(
            crate::confirmed_governance::ConfirmedGovernance,
        ) -> Result<(), burst_store::StoreError>,
        verify_cut: impl FnOnce() -> Result<(), burst_store::StoreError>,
    ) -> Result<(), burst_store::StoreError> {
        let invalid = |message: &str| burst_store::StoreError::Corruption(message.into());
        if cemented.tail().context() != self.context {
            return Err(invalid(
                "cemented tail belongs to a different authenticated state",
            ));
        }
        verify_cut()?;
        self.verify_installed_authority_view(batch)?;
        verify_schedule(self.state.confirmed_governance_schedule()?)
    }

    pub(crate) fn verify_installed_authority_view(
        &self,
        batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    ) -> Result<(), burst_store::StoreError> {
        use crate::confirmed_replay::installation::{authority_bytes, CUTOFF_KEY};
        let invalid = |message: &str| burst_store::StoreError::Corruption(message.into());
        if batch.payment_epoch()? != self.anchor.hash() {
            return Err(burst_store::StoreError::StaleRead(
                "installed authority epoch changed".into(),
            ));
        }
        if crate::payment_authority::storage::read_at(|key| batch.get_meta(key))?
            != Some(authority_bytes(&self.anchor, self.anchor.cutoff())?)
            || batch.get_meta(CUTOFF_KEY)?.as_deref()
                != Some(self.anchor.cutoff().to_be_bytes().as_slice())
            || batch
                .get_meta(&burst_store::meta::closed_payment_epoch_key(
                    &self.anchor.hash(),
                ))?
                .is_some()
            || batch.get_meta(burst_store::meta::PAYMENT_VOTE_LIMIT_KEY)?
                != Some(self.state.certified_vote_limit_bytes()?)
        {
            return Err(invalid(
                "installed metadata differs from authenticated snapshot continuation",
            ));
        }
        Ok(())
    }
    pub fn into_state(self) -> ConfirmedReplay {
        self.state.into_inner()
    }

    pub fn history_cursor(
        &self,
        account: &WalletAddress,
    ) -> Result<Option<HistoryCursor>, ReplayError> {
        HistoryCursor::new(
            &self.anchor,
            self.context.state_hash,
            account,
            self.state.account(account),
        )
    }

    /// Materialize account roots in authenticated private scratch without a
    /// second in-memory copy of every account cursor.
    pub(crate) fn stored_history_roots(
        &self,
        directory: &std::path::Path,
    ) -> Result<super::history::StoredHistoryRoots, ReplayError> {
        let mut roots =
            super::history::StoredHistoryRoots::new(directory, self.context, self.anchor.cutoff())?;
        for account in self.state.accounts.keys() {
            if let Some(cursor) = self.history_cursor(account)? {
                roots.insert(cursor)?;
            }
        }
        Ok(roots)
    }

    pub fn ordering_dependencies(&self) -> Vec<BlockHash> {
        let mut hashes: Vec<_> = self.state.replay_dependencies.iter().copied().collect();
        hashes.sort_by_key(|hash| *hash.as_bytes());
        hashes
    }

    /// Exact heads committed by this authenticated state, not caller-selected
    /// shortcuts into an account chain. Unknown ancestors still need a walk.
    #[cfg(test)]
    pub(crate) fn committed_history_cursor(
        &self,
        hash: BlockHash,
    ) -> Result<Option<HistoryCursor>, ReplayError> {
        Ok(self.committed_history_cursors(&[hash])?.pop())
    }

    pub(crate) fn committed_history_cursors(
        &self,
        hashes: &[BlockHash],
    ) -> Result<Vec<HistoryCursor>, ReplayError> {
        Self::committed_cursors_for(&self.anchor, self.context, &self.state, hashes)
    }

    pub(super) fn committed_cursors_for(
        anchor: &CheckpointAnchor,
        context: HistoryContext,
        state: &ConfirmedReplay,
        hashes: &[BlockHash],
    ) -> Result<Vec<HistoryCursor>, ReplayError> {
        let wanted: std::collections::HashSet<_> = hashes.iter().copied().collect();
        let mut committed = std::collections::HashMap::new();
        if wanted.contains(&anchor.genesis_hash()) {
            committed.insert(
                anchor.genesis_hash(),
                HistoryHead {
                    account: crate::genesis_key::genesis_address(anchor.network()),
                    height: 1,
                    hash: anchor.genesis_hash(),
                },
            );
        }
        for receipt in state.selection_receipts.iter() {
            let receipt = receipt?;
            let mut checked = false;
            for head in receipt
                .heads()
                .iter()
                .filter(|head| head.hash != anchor.genesis_hash() && wanted.contains(&head.hash))
            {
                if !checked {
                    receipt
                        .validate_at(anchor)
                        .map_err(|error| super::invalid(error.to_string()))?;
                    checked = true;
                }
                if committed.get(&head.hash).is_some_and(|old| old != head) {
                    return Err(super::invalid("conflicting committed history heads"));
                }
                committed.insert(head.hash, head.clone());
            }
        }
        let mut cursors = Vec::with_capacity(committed.len());
        for hash in hashes {
            let Some(head) = committed.remove(hash) else {
                continue;
            };
            cursors.push(Self::cursor_for_committed_head(
                anchor, context, state, &head,
            )?);
        }
        Ok(cursors)
    }

    fn cursor_for_committed_head(
        anchor: &CheckpointAnchor,
        context: HistoryContext,
        state: &ConfirmedReplay,
        head: &HistoryHead,
    ) -> Result<HistoryCursor, ReplayError> {
        let mut account = state
            .account(&head.account)
            .cloned()
            .ok_or_else(|| super::invalid("committed history account is absent"))?;
        if head.height == 0
            || head.height > account.block_count
            || (head.height == account.block_count && head.hash != account.head)
        {
            return Err(super::invalid(
                "committed history exceeds authenticated account",
            ));
        }
        account.head = head.hash;
        account.block_count = head.height;
        account.confirmation_height = head.height;
        let cursor = HistoryCursor::new(anchor, context.state_hash, &head.account, Some(&account))?
            .ok_or_else(|| super::invalid("committed history has no account cursor"))?;
        Ok(cursor)
    }

    /// Visit selected committed heads without collecting a cursor per body.
    /// Repeated receipts may emit the same hash more than once. The consumer
    /// must bind each emitted account/height to its exact observed position;
    /// conflicting heads must never be silently deduplicated.
    #[cfg(test)]
    pub(in crate::confirmed_replay) fn visit_committed_history(
        &self,
        mut wanted: impl FnMut(BlockHash) -> Result<bool, ReplayError>,
        mut visit: impl FnMut(HistoryCursor) -> Result<(), ReplayError>,
    ) -> Result<(), ReplayError> {
        let mut walk = self.committed_history_walk();
        while !walk.advance(128, &mut wanted, &mut visit)? {}
        Ok(())
    }

    pub(in crate::confirmed_replay) fn committed_history_walk(
        &self,
    ) -> committed::CommittedHistoryWalk<'_> {
        committed::CommittedHistoryWalk::new(self)
    }

    pub fn read_tail(
        &self,
        store: &LmdbStore,
        cutoff: u64,
        heads: &[HistoryHead],
        retained: Vec<super::history::HistoricalBlock>,
    ) -> Result<SnapshotTail, ReplayError> {
        super::tail::read_for(self.base(), store, cutoff, heads, retained)
    }

    pub fn plan_tail(
        &self,
        store: &LmdbStore,
        cutoff: u64,
        heads: &[HistoryHead],
    ) -> Result<super::tail::SnapshotTailPlan, ReplayError> {
        super::tail::plan_for(self.base(), store, cutoff, heads)
    }

    pub(super) fn read_tail_retaining(
        &self,
        store: &LmdbStore,
        cutoff: u64,
        heads: &[HistoryHead],
        previous: &SnapshotTail,
    ) -> Result<SnapshotTail, ReplayError> {
        self.plan_tail(store, cutoff, heads)?
            .finish_staged(previous.retained_storage())
    }

    pub(crate) fn plan_transferred_tail(
        &self,
        tail: super::tail::TransferredTail,
        cutoff: u64,
        heads: &[HistoryHead],
        directory: &std::path::Path,
    ) -> Result<super::tail::SnapshotTailPlan, ReplayError> {
        tail.plan(self.base(), cutoff, heads, directory)
    }

    fn execute(
        &mut self,
        tail: &SnapshotTail,
        certificates: &(impl SelectionSource + ?Sized),
        deadline: bool,
    ) -> Result<Source<'static>, ReplayError> {
        self.execute_input(
            tail,
            certificates,
            InputMode::Certified {
                deadline_candidate: deadline,
            },
        )
    }

    /// Stream the starting state and each successfully executed block. The
    /// observer must keep results private until this method succeeds: later
    /// blocks or the final head check can still invalidate the entire tail.
    /// No per-block state clone is retained by replay.
    #[cfg(test)]
    pub(in crate::confirmed_replay) fn replay_applied_observed(
        &self,
        tail: &super::tail::cemented::AppliedTail,
        certificates: &(impl SelectionSource + ?Sized),
        observe: &mut impl FnMut(
            Option<&burst_ledger::StateBlock>,
            &ConfirmedReplay,
        ) -> Result<(), ReplayError>,
    ) -> Result<ConfirmedReplay, ReplayError> {
        self.replay_applied_ordered(tail, certificates, &[], observe)
    }

    pub(in crate::confirmed_replay) fn replay_applied_ordered(
        &self,
        tail: &super::tail::cemented::AppliedTail,
        certificates: &(impl SelectionSource + ?Sized),
        ordering: &[(BlockHash, BlockHash)],
        observe: &mut impl FnMut(
            Option<&burst_ledger::StateBlock>,
            &ConfirmedReplay,
        ) -> Result<(), ReplayError>,
    ) -> Result<ConfirmedReplay, ReplayError> {
        self.replay_applied_with_recording(tail, certificates, ordering, false, false, observe)
    }

    /// Observe state before and after each transition. Consumers may skip delta
    /// capture for confirmed blocks, but replay still checks every block and the
    /// final heads. Keep all observations private until the method succeeds.
    pub(in crate::confirmed_replay) fn replay_applied_recorded_transitions(
        &self,
        tail: &super::tail::cemented::AppliedTail,
        certificates: &(impl SelectionSource + ?Sized),
        ordering: &[(BlockHash, BlockHash)],
        record_wallets: bool,
        observe: &mut impl FnMut(
            bool,
            Option<&burst_ledger::StateBlock>,
            &ConfirmedReplay,
        ) -> Result<(), ReplayError>,
    ) -> Result<ConfirmedReplay, ReplayError> {
        let mut candidate = Self {
            anchor: self.anchor.clone(),
            context: self.context,
            state: self.state.clone(),
        };
        candidate.state.mutations = Some(Default::default());
        candidate
            .state
            .brn
            .wallets
            .record_transition(record_wallets);
        candidate.execute_input_transitions(
            tail.tail(),
            certificates,
            InputMode::Applied,
            ordering,
            observe,
        )?;
        candidate.state.mutations = None;
        candidate.state.brn.wallets.record_transition(false);
        Ok(candidate.state.into_inner())
    }

    fn replay_applied_with_recording(
        &self,
        tail: &super::tail::cemented::AppliedTail,
        certificates: &(impl SelectionSource + ?Sized),
        ordering: &[(BlockHash, BlockHash)],
        recording: bool,
        record_wallets: bool,
        observe: &mut impl FnMut(
            Option<&burst_ledger::StateBlock>,
            &ConfirmedReplay,
        ) -> Result<(), ReplayError>,
    ) -> Result<ConfirmedReplay, ReplayError> {
        let mut candidate = Self {
            anchor: self.anchor.clone(),
            context: self.context,
            state: self.state.clone(),
        };
        candidate.state.mutations = recording.then(Default::default);
        candidate
            .state
            .brn
            .wallets
            .record_transition(recording && record_wallets);
        candidate.execute_input_observed(
            tail.tail(),
            certificates,
            InputMode::Applied,
            ordering,
            observe,
        )?;
        candidate.state.mutations = None;
        candidate.state.brn.wallets.record_transition(false);
        Ok(candidate.state.into_inner())
    }

    fn execute_input(
        &mut self,
        tail: &SnapshotTail,
        certificates: &(impl SelectionSource + ?Sized),
        mode: InputMode,
    ) -> Result<Source<'static>, ReplayError> {
        self.execute_input_observed(tail, certificates, mode, &[], &mut |_, _| Ok(()))
    }

    fn execute_input_observed(
        &mut self,
        tail: &SnapshotTail,
        certificates: &(impl SelectionSource + ?Sized),
        mode: InputMode,
        ordering: &[(BlockHash, BlockHash)],
        observe: &mut impl FnMut(
            Option<&burst_ledger::StateBlock>,
            &ConfirmedReplay,
        ) -> Result<(), ReplayError>,
    ) -> Result<Source<'static>, ReplayError> {
        self.execute_input_transitions(
            tail,
            certificates,
            mode,
            ordering,
            &mut |before, block, state| {
                if before {
                    Ok(())
                } else {
                    observe(block, state)
                }
            },
        )
    }

    fn execute_input_transitions(
        &mut self,
        tail: &SnapshotTail,
        certificates: &(impl SelectionSource + ?Sized),
        mode: InputMode,
        ordering: &[(BlockHash, BlockHash)],
        observe: &mut impl FnMut(
            bool,
            Option<&burst_ledger::StateBlock>,
            &ConfirmedReplay,
        ) -> Result<(), ReplayError>,
    ) -> Result<Source<'static>, ReplayError> {
        if tail.context() != self.context {
            return Err(super::invalid(
                "tail belongs to a different authenticated base",
            ));
        }
        let retained_blocks = tail.retained().hashes().collect();
        let blocks = Source::from_tail(tail);
        self.state.append_input_transitions(
            EpochInput {
                directory: Some(&tail.directory),
                network: self.context.network,
                cutoff: tail.cutoff(),
                heads: tail.heads(),
                blocks: &blocks,
                contents: tail.contents(),
                retained_blocks,
            },
            self.context.epoch,
            certificates,
            mode,
            ordering,
            observe,
        )?;
        Ok(blocks)
    }

    pub fn replay_tail(
        mut self,
        tail: SnapshotTail,
        certificates: &(impl SelectionSource + ?Sized),
    ) -> Result<ConfirmedReplay, ReplayError> {
        self.execute(&tail, certificates, false)?;
        self.state
            .require_closed_governance(burst_types::Timestamp::new(tail.cutoff()))?;
        Ok(self.state.into_inner())
    }

    /// Replay a proposed selection without requiring its decision certificate.
    /// The base remains unchanged. Use selection_vote_coverage to derive the
    /// membership coverage needed for subsequent signing checks.
    pub fn replay_selection_candidate(
        &self,
        tail: SnapshotTail,
        manifest: &CheckpointManifest,
        selections: &(impl SelectionSource + ?Sized),
    ) -> Result<ReplayedCheckpoint, ReplayError> {
        let mut candidate = Self {
            anchor: self.anchor.clone(),
            context: self.context,
            state: self.state.clone(),
        };
        candidate
            .selection_candidate(&tail, manifest, selections)
            .map(|(_, replay)| replay)
    }

    /// Include only hashes proven in the authenticated base or the replayed
    /// suffix. Extra historical witnesses must belong to this exact context.
    pub fn selection_vote_coverage(
        &self,
        tail: SnapshotTail,
        manifest: &CheckpointManifest,
        selections: &(impl SelectionSource + ?Sized),
    ) -> Result<CompactSelectionCoverage, ReplayError> {
        self.check_selection_receipts(selections)?;
        let mut candidate = Self {
            anchor: self.anchor.clone(),
            context: self.context,
            state: self.state.clone(),
        };
        let (_, replay) = candidate.selection_candidate(&tail, manifest, selections)?;
        self.finish_selection_coverage(tail, replay)
    }

    /// Discover an eligible, not-yet-certified selection from this cut. The
    /// caller must schedule due lifecycle/governance events before selections,
    /// as on the full-history path. Discovery grants no signing authority.
    pub fn prepare_next_selection(
        &self,
        tail: SnapshotTail,
        selections: &(impl SelectionSource + ?Sized),
    ) -> Result<Option<CompactSelectionCoverage>, ReplayError> {
        self.check_selection_receipts(selections)?;
        let mut candidate = Self {
            anchor: self.anchor.clone(),
            context: self.context,
            state: self.state.clone(),
        };
        candidate.execute(&tail, selections, false)?;
        candidate
            .state
            .require_closed_governance(burst_types::Timestamp::new(tail.cutoff()))?;
        for target in candidate.state.selection_targets() {
            let lifecycle = candidate.state.selection_lifecycle(&target)?;
            let attempt = candidate
                .state
                .verification(&target)
                .ok_or_else(|| super::invalid("missing selection state"))?
                .revote_count;
            if selections
                .find_selection(|selection| {
                    selection.replayed().request().target() == &target
                        && selection.replayed().lifecycle() == lifecycle
                        && selection.replayed().request().attempt() >= attempt
                })?
                .is_some()
            {
                continue;
            }
            let replay = ReplayedCheckpoint::from_compact(
                &tail,
                &self.anchor,
                &target,
                &mut candidate.state,
                selections,
            )
            .map_err(|error| super::invalid(error.to_string()))?;
            return self.finish_selection_coverage(tail, replay).map(Some);
        }
        Ok(None)
    }

    fn check_selection_receipts(
        &self,
        selections: &(impl SelectionSource + ?Sized),
    ) -> Result<(), ReplayError> {
        if self.state.selection_receipts.len() != selections.len() {
            return Err(super::invalid(
                "compact signing requires every authenticated selection receipt",
            ));
        }
        for (receipt, selection) in self
            .state
            .selection_receipts
            .iter()
            .zip(selections.iter_selections())
        {
            let receipt = receipt?;
            let selection = selection?;
            if !receipt.matches_replay(selection.replayed()) {
                return Err(super::invalid(
                    "compact signing selection receipt differs from its base",
                ));
            }
        }
        Ok(())
    }

    fn finish_selection_coverage(
        &self,
        tail: SnapshotTail,
        replay: ReplayedCheckpoint,
    ) -> Result<CompactSelectionCoverage, ReplayError> {
        if bincode::serialized_size(&replay.manifest())
            .map_err(|error| super::invalid(error.to_string()))?
            > crate::checkpoint_store::MAX_RECORD_BYTES
        {
            return Err(super::invalid("selection manifest exceeds byte limit"));
        }
        Ok(CompactSelectionCoverage {
            replay,
            cut: self.cut_coverage(&tail)?,
        })
    }

    pub(in crate::confirmed_replay::snapshot) fn cut_coverage(
        &self,
        tail: &SnapshotTail,
    ) -> Result<CompactCutCoverage, ReplayError> {
        if tail.context() != self.context {
            return Err(super::invalid(
                "compact coverage has a different authenticated base",
            ));
        }
        let mut included: std::collections::HashSet<_> = self
            .state
            .accounts
            .values()
            .filter(|account| account.block_count > 0)
            .map(|account| account.head)
            .collect();
        included.extend(tail.block_hashes());
        for witness in tail.retained().iter() {
            let witness = witness.map_err(|error| super::invalid(error.to_string()))?;
            if witness.context() != self.context {
                return Err(super::invalid(
                    "selection coverage witness has a different base",
                ));
            }
            included.insert(witness.block().hash);
        }
        if included.len() > 131_072 {
            return Err(super::invalid("selection vote coverage exceeds its bound"));
        }
        let positions = coverage::positions(self, tail)?;
        let archive = tail.archive();
        Ok(CompactCutCoverage {
            archive,
            parent: self.anchor.hash(),
            included,
            authority: crate::confirmed_replay::installation::authority_bytes(
                &self.anchor,
                self.anchor.cutoff(),
            )
            .map_err(|error| super::invalid(error.to_string()))?,
            cutoff: self.anchor.cutoff(),
            vote_limit: self
                .state
                .certified_vote_limit_bytes()
                .map_err(|error| super::invalid(error.to_string()))?,
            floors: self
                .state
                .accounts
                .values()
                .map(|account| (account.address.clone(), account.block_count))
                .collect(),
            positions,
        })
    }

    fn selection_candidate(
        &mut self,
        tail: &SnapshotTail,
        manifest: &CheckpointManifest,
        selections: &(impl SelectionSource + ?Sized),
    ) -> Result<(Source<'static>, ReplayedCheckpoint), ReplayError> {
        if bincode::serialized_size(manifest).map_err(|error| super::invalid(error.to_string()))?
            > crate::checkpoint_store::MAX_RECORD_BYTES
        {
            return Err(super::invalid("selection manifest exceeds byte limit"));
        }
        self.anchor
            .validate_successor(&manifest.checkpoint)
            .map_err(|error| super::invalid(error.to_string()))?;
        let checkpoint = &manifest.checkpoint;
        if manifest.heads != tail.heads()
            || checkpoint.cutoff != tail.cutoff()
            || checkpoint.ledger_root != tail.commitment()
            || checkpoint.next_quorum_bps != self.anchor.quorum_bps()
        {
            return Err(super::invalid(
                "selection differs from compact history or quorum rules",
            ));
        }
        let blocks = self.execute(tail, selections, false)?;
        self.state
            .require_closed_governance(burst_types::Timestamp::new(tail.cutoff()))?;
        let replay = ReplayedCheckpoint::from_compact(
            tail,
            &self.anchor,
            &manifest.target,
            &mut self.state,
            selections,
        )
        .map_err(|error| super::invalid(error.to_string()))?;
        if replay.manifest() != *manifest {
            return Err(super::invalid(
                "selection proposal differs from compact replay",
            ));
        }
        Ok((blocks, replay))
    }

    /// Reconstruct an already-certified selection from the authenticated base
    /// and suffix. The returned record can serve later replay and exact beacon
    /// verification, but has no full-prefix inventory for local vote signing.
    pub fn replay_selection(
        mut self,
        tail: SnapshotTail,
        certificate: &CheckpointCertificate,
        manifest: &CheckpointManifest,
        selections: &(impl SelectionSource + ?Sized),
    ) -> Result<(Self, CertifiedCheckpoint), ReplayError> {
        self.anchor
            .verify_certificate(certificate)
            .map_err(|error| super::invalid(error.to_string()))?;
        if certificate.checkpoint != manifest.checkpoint {
            return Err(super::invalid(
                "selection manifest differs from certificate",
            ));
        }
        let (blocks, replay) = self.selection_candidate(&tail, manifest, selections)?;
        let selection =
            CertifiedCheckpoint::from_compact(self.anchor.clone(), certificate.clone(), replay)
                .map_err(|error| super::invalid(error.to_string()))?;
        let cut = CompactCut {
            cutoff: tail.cutoff(),
            commitment: tail.commitment(),
            blocks: &blocks,
        };
        self.state.apply_boundary_source(
            &cut,
            &Boundary::from_selection(&selection),
            selections,
        )?;
        let anchor = self
            .anchor
            .advance(certificate, selection.replayed().roster().clone())
            .map_err(|error| super::invalid(error.to_string()))?;
        Ok((self.advance(anchor)?, selection))
    }

    /// Authenticate an untrusted non-selection manifest without reading its
    /// historical prefix. Earlier selection records remain required for panels.
    /// This derives state only; it cannot sign, persist or install a checkpoint.
    pub fn replay_manifest(
        mut self,
        tail: SnapshotTail,
        certificate: &CheckpointCertificate,
        manifest: &ReplayManifest,
        selections: &(impl SelectionSource + ?Sized),
    ) -> Result<Self, ReplayError> {
        if bincode::serialized_size(manifest).map_err(|error| super::invalid(error.to_string()))?
            > crate::checkpoint_store::MAX_RECORD_BYTES
        {
            return Err(super::invalid("checkpoint manifest exceeds byte limit"));
        }
        self.anchor
            .verify_certificate(certificate)
            .map_err(|error| super::invalid(error.to_string()))?;
        let checkpoint = &certificate.checkpoint;
        if checkpoint.next_quorum_bps != self.anchor.quorum_bps()
            || checkpoint.cutoff != tail.cutoff()
            || checkpoint.ledger_root != tail.commitment()
            || manifest.heads() != tail.heads()
        {
            return Err(super::invalid(
                "manifest differs from compact history or quorum rules",
            ));
        }
        // Validate the variant and certificate before executing the private tail.
        Boundary::from_manifest(certificate, self.anchor.roster(), manifest)?;
        let blocks = self.execute(
            &tail,
            selections,
            matches!(
                manifest,
                ReplayManifest::Deadline(_) | ReplayManifest::Governance(_)
            ),
        )?;
        if matches!(manifest, ReplayManifest::Roster(_))
            && self.state.human_roster()? == *self.anchor.roster()
            && !crate::checkpoint_recovery::valid_cut(self.anchor.cutoff(), checkpoint.cutoff)
        {
            return Err(super::invalid("recovery checkpoint cut is too early"));
        }
        let cut = CompactCut {
            cutoff: tail.cutoff(),
            commitment: tail.commitment(),
            blocks: &blocks,
        };
        self.state.apply_boundary_source(
            &cut,
            &Boundary::from_manifest(certificate, self.anchor.roster(), manifest)?,
            selections,
        )?;
        let anchor = self
            .anchor
            .advance(certificate, self.state.human_roster()?)
            .map_err(|error| super::invalid(error.to_string()))?;
        self.advance(anchor)
    }

    /// Replay a certified successor against this base, checking its cut and
    /// economic effects. Missing witnesses or mismatches consume the candidate
    /// without installing ledger state; replay may write private scratch files.
    pub fn replay_checkpoint(
        mut self,
        tail: SnapshotTail,
        event: &CertifiedEvent,
        certificates: &(impl SelectionSource + ?Sized),
    ) -> Result<Self, ReplayError> {
        let checkpoint = &event.certificate().checkpoint;
        let anchor = self
            .anchor
            .advance(event.certificate(), event.next_roster().clone())
            .map_err(|error| super::invalid(error.to_string()))?;
        let mut heads = event.heads().to_vec();
        heads.sort_by(|a, b| a.account.as_str().cmp(b.account.as_str()));
        if checkpoint.cutoff != tail.cutoff()
            || checkpoint.ledger_root != tail.commitment()
            || heads != tail.heads()
        {
            return Err(super::invalid(
                "certified successor differs from compact history",
            ));
        }
        let deadline = matches!(
            event,
            CertifiedEvent::Deadline(_) | CertifiedEvent::Governance(_)
        );
        let blocks = self.execute(&tail, certificates, deadline)?;
        let cut = CompactCut {
            cutoff: tail.cutoff(),
            commitment: tail.commitment(),
            blocks: &blocks,
        };
        self.state
            .apply_boundary_source(&cut, &Boundary::from(event), certificates)?;
        self.advance(anchor)
    }

    fn advance(mut self, anchor: CheckpointAnchor) -> Result<Self, ReplayError> {
        let manifest = self.state.snapshot_manifest(&anchor)?;
        self.context = HistoryContext {
            network: anchor.network(),
            genesis: anchor.genesis_hash(),
            epoch: anchor.hash(),
            state_hash: manifest.state_hash,
        };
        self.anchor = anchor;
        Ok(self)
    }
}

struct CompactCut<'a> {
    cutoff: u64,
    commitment: BlockHash,
    blocks: &'a Source<'a>,
}
impl LifecycleHistory for CompactCut<'_> {
    fn cutoff(&self) -> u64 {
        self.cutoff
    }
    fn commitment(&self) -> BlockHash {
        self.commitment
    }
    fn applied_blocks(&self) -> impl AppliedBlocks {
        CompletedInput(self.blocks)
    }
    fn first_block(&self) -> Result<std::borrow::Cow<'_, burst_ledger::StateBlock>, ReplayError> {
        self.blocks
            .iter()
            .next()
            .ok_or_else(|| super::invalid("empty lifecycle history"))?
            .map_err(|error| super::invalid(error.to_string()))
    }
}

/// Applied input exposed after the full tail has replayed successfully.
/// Checkpoint boundary effects are applied separately.
struct CompletedInput<'a>(&'a Source<'a>);

impl AppliedBlocks for CompletedInput<'_> {
    fn get(
        &self,
        hash: &BlockHash,
    ) -> Result<Option<std::borrow::Cow<'_, burst_ledger::StateBlock>>, burst_store::StoreError>
    {
        match self.0.get(hash) {
            Ok(block) => Ok(Some(block)),
            Err(burst_store::StoreError::NotFound(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod sharing_tests {
    use super::*;

    #[test]
    fn continuation_forks_share_state_until_mutation_and_preserve_parent() {
        let original = SnapshotContinuation::from_genesis(
            burst_types::NetworkId::Dev,
            &burst_types::ProtocolParams::default(),
        )
        .unwrap();
        let mut changed = original.clone();
        let untouched = original.clone();
        assert!(std::ptr::eq(original.state(), changed.state()));
        assert!(std::ptr::eq(original.state(), untouched.state()));
        let address = original.state.accounts.keys().next().unwrap().clone();
        let account = original.state.accounts[&address].clone();
        changed
            .state
            .accounts
            .get_mut(&address)
            .unwrap()
            .block_count += 1;
        assert!(!std::ptr::eq(original.state(), changed.state()));
        assert!(std::ptr::eq(original.state(), untouched.state()));
        assert_eq!(original.state.accounts[&address], account);
        assert_eq!(untouched.state.accounts[&address], account);
        assert_eq!(
            changed.state.accounts[&address].block_count,
            account.block_count + 1
        );
        assert_eq!(changed.anchor.hash(), original.anchor.hash());
        let unique = changed.state.into_inner();
        assert_eq!(
            unique.accounts[&address].block_count,
            account.block_count + 1
        );
    }
}
