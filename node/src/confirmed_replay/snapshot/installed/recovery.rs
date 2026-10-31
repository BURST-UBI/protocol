use super::*;
use super::metadata::RecoveryControl;
use crate::confirmed_replay::snapshot::SelectionSource;
use crate::{
    checkpoint_agreement::ReplayManifest,
    checkpoint_store::compact,
    confirmed_replay::snapshot::{
        history::HistoricalBlock,
        tail::{SnapshotTail, SnapshotTailPlan},
        SnapshotContinuation,
    },
};
use burst_consensus::checkpoint::{CheckpointAnchor, CheckpointCertificate};
use burst_types::BlockHash;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Replayed certified state, not an installed node. Historical selections have
/// no full-prefix signing inventory. Provisional state and local signer journals
/// still require separate reconciliation before admission can start.
pub struct RecoveredSnapshot {
    pub continuation: SnapshotContinuation,
    pub selections: super::SelectionHistory,
    observations: compact::RecordObservations,
    last_decision: Option<(CheckpointAnchor, CheckpointCertificate, ReplayManifest)>,
    from_archive: bool,
}

pub(crate) enum CandidateDiscovery {
    Ready(Option<crate::checkpoint_agreement::CompactReplayCandidate>),
    Advanced,
}

pub(crate) struct ReplayCache {
    pub(crate) base: Option<BaseCommitment>,
    pub(crate) recovered: RecoveredSnapshot,
}

impl ReplayCache {
    pub(crate) fn refresh_known_selection(
        &self,
        store: &LmdbStore,
        base: Option<BaseCommitment>,
        certificate: &CheckpointCertificate,
        manifest: &ReplayManifest,
        beacon: Option<&burst_vrf::RandomOutput>,
        params: &ProtocolParams,
    ) -> Result<bool, StoreError> {
        if self.base != base {
            return Ok(false);
        }
        let Some(receipt) = self
            .recovered
            .selections
            .find_selection(|receipt| {
                receipt.certificate().checkpoint.height == certificate.checkpoint.height
            })
            .map_err(invalid)?
        else {
            return Ok(false);
        };
        let batch = store.tx_begin_write()?;
        match self.recovered.verify_records_unchanged(&batch) {
            Ok(()) => (),
            Err(StoreError::StaleRead(_)) => return Ok(false),
            Err(error) => return Err(error),
        }
        match base {
            Some(base) => base.verify(|key| batch.get_meta(key))?,
            None if batch.get_meta(super::BASE_KEY)?.is_some() => return Ok(false),
            None => (),
        }
        receipt
            .parent()
            .verify_certificate(certificate)
            .map_err(invalid)?;
        if certificate.checkpoint != receipt.certificate().checkpoint
            || *manifest != ReplayManifest::Selection(receipt.replayed().manifest())
        {
            return Err(invalid(
                "transferred checkpoint differs from replayed selection",
            ));
        }
        drop(batch);
        if let Some(beacon) = beacon {
            receipt
                .into_owned()
                .attach_beacon_checked(store, params, beacon.clone(), |batch| {
                    self.recovered.verify_records_unchanged(batch)?;
                    match base {
                        Some(base) => base.verify(|key| batch.get_meta(key)),
                        None if batch.get_meta(super::BASE_KEY)?.is_some() => {
                            Err(StoreError::StaleRead(
                                "snapshot base appeared during selection refresh".into(),
                            ))
                        }
                        None => Ok(()),
                    }
                })?;
        }
        Ok(true)
    }

    pub(crate) fn fork(&self, directory: &std::path::Path) -> Result<Self, StoreError> {
        Ok(Self {
            base: self.base,
            recovered: self.recovered.fork(directory)?,
        })
    }

    pub(crate) async fn recover(
        self,
        base: Option<BaseCommitment>,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        target: BlockHash,
    ) -> Result<Option<RecoveredSnapshot>, StoreError> {
        if self.base != base {
            tracing::debug!(%target, cached_height = self.recovered.continuation.anchor().height(),
                "replay cache rejected changed snapshot base");
            return Ok(None);
        }
        let cached_height = self.recovered.continuation.anchor().height();
        match self
            .recovered
            .advance_from_peers(store, registry, target)
            .await
        {
            Ok(recovered) => Ok(Some(recovered)),
            Err(error @ StoreError::StaleRead(_)) => {
                tracing::debug!(%error, %target, cached_height, "replay cache rejected stale observations");
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
}

impl AuthenticatedSnapshot {
    pub(in crate::confirmed_replay::snapshot) fn recover_export(
        self,
        store: &LmdbStore,
        epoch: BlockHash,
    ) -> Result<RecoveredSnapshot, StoreError> {
        let (mut recovered, records) = self.begin_recovery(store, Some(epoch))?;
        for record in records.into_cursor()? {
            let record = record?.load(store)?;
            let plan = recovered.plan(store, &record)?;
            let retained =
                crate::checkpoint_sync::witness::read_plan(store, &recovered.continuation, &plan)?;
            recovered = recovered.apply(record, plan, retained)?;
        }
        Ok(recovered)
    }

    /// Recover retained checkpoint records without replaying the pre-base ledger.
    /// The provider supplies cursor-authenticated membership witnesses. Missing
    /// dependencies or beacons required by replay cause an error. Scratch files
    /// may be written; the ledger database is unchanged.
    pub fn recover_stored_successors(
        self,
        store: &LmdbStore,
        mut witnesses: impl FnMut(
            &SnapshotContinuation,
            &SnapshotTailPlan,
            &super::SelectionHistory,
        ) -> Result<Vec<HistoricalBlock>, StoreError>,
    ) -> Result<RecoveredSnapshot, StoreError> {
        self.recover_stored_target(store, None, &mut witnesses)
    }

    /// Recover an exact certified epoch, leaving any later accepted records
    /// unapplied. The target must be the base or one of its retained successors.
    pub fn recover_stored_epoch(
        self,
        store: &LmdbStore,
        epoch: BlockHash,
        mut witnesses: impl FnMut(
            &SnapshotContinuation,
            &SnapshotTailPlan,
            &super::SelectionHistory,
        ) -> Result<Vec<HistoricalBlock>, StoreError>,
    ) -> Result<RecoveredSnapshot, StoreError> {
        self.recover_stored_target(store, Some(epoch), &mut witnesses)
    }

    fn recover_stored_target(
        self,
        store: &LmdbStore,
        target: Option<BlockHash>,
        witnesses: &mut impl FnMut(
            &SnapshotContinuation,
            &SnapshotTailPlan,
            &super::SelectionHistory,
        ) -> Result<Vec<HistoricalBlock>, StoreError>,
    ) -> Result<RecoveredSnapshot, StoreError> {
        let (mut recovered, records) = self.begin_recovery(store, target)?;
        for record in records.into_cursor()? {
            let record = record?.load(store)?;
            let plan = recovered.plan(store, &record)?;
            let retained = witnesses(&recovered.continuation, &plan, &recovered.selections)?;
            recovered = recovered.apply(record, plan, retained)?;
        }
        Ok(recovered)
    }

    /// Recover stored successors using the running peer proof service. Planning
    /// and replay run off the async worker. No writer or runtime lock crosses a
    /// network wait, and cancellation cannot publish partially recovered state.
    pub async fn recover_stored_successors_from_peers(
        self,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
    ) -> Result<RecoveredSnapshot, StoreError> {
        self.recover_from_peers(store, registry, None).await
    }

    pub async fn recover_stored_epoch_from_peers(
        self,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        epoch: BlockHash,
    ) -> Result<RecoveredSnapshot, StoreError> {
        self.recover_from_peers(store, registry, Some(epoch)).await
    }

    async fn recover_from_peers(
        self,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        target: Option<BlockHash>,
    ) -> Result<RecoveredSnapshot, StoreError> {
        self.recover_from_peers_until(store, registry, target, RecoveryControl::default()).await?
            .ok_or_else(|| invalid("uncancelled tip recovery stopped"))
    }

    pub(super) async fn recover_from_peers_until(
        self,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        target: Option<BlockHash>,
        control: RecoveryControl,
    ) -> Result<Option<RecoveredSnapshot>, StoreError> {
        if control.checkpoint("tip-recovery:before-plan") { return Ok(None); }
        let source = Arc::clone(&store);
        let (recovered, records) =
            tokio::task::spawn_blocking(move || self.begin_recovery(&source, target))
                .await
                .map_err(invalid)??;
        recovered
            .recover_records_from_peers_until(records, store, registry, control)
            .await
    }

    fn begin_recovery(
        mut self,
        store: &LmdbStore,
        target: Option<BlockHash>,
    ) -> Result<(RecoveredSnapshot, compact::RecoveryReferences), StoreError> {
        let mut selections = super::SelectionHistory::new(
            super::authority_history::AuthorityHistory::new(
                self.authority_base.clone(),
                store.env().path(),
            )?,
            store.env().path(),
        )?;
        self.try_visit_selections(|selection| {
            selections
                .push(selection)
                .map_err(|e| super::super::super::invalid(e.to_string()))
        })
        .map_err(invalid)?;
        let authority = std::mem::replace(
            &mut self.authority,
            super::super::archive::Authority::Memory(Vec::new()),
        );
        selections.authority = authority.into_history(&self.authority_base, store.env().path())?;
        let mut records = compact::read_after(store, selections.authority.iter(), &selections)?;
        let height = records.successors.select_through(self.anchor(), target)?;
        records.observations.retain_through(height)?;
        selections.attach_beacons(store.env().path(), records.beacons.into_cursor()?)?;
        Ok((
            RecoveredSnapshot {
                continuation: self.into_continuation(),
                selections,
                observations: records.observations,
                last_decision: None,
                from_archive: true,
            },
            records.successors,
        ))
    }
}

impl RecoveredSnapshot {
    /// Independent scratch files keep advancing accepted authority from changing
    /// the installed view. Both views retain their own publication observations.
    pub(crate) fn fork(&self, directory: &std::path::Path) -> Result<Self, StoreError> {
        Ok(Self {
            continuation: self.continuation.clone(),
            selections: self.selections.fork(directory)?,
            observations: self.observations.fork()?,
            last_decision: self.last_decision.clone(),
            from_archive: self.from_archive,
        })
    }

    pub(crate) fn uses_snapshot_archive(&self) -> bool {
        self.from_archive
    }
    pub(in crate::confirmed_replay) fn genesis(
        store: &LmdbStore,
        network: NetworkId,
        params: &ProtocolParams,
    ) -> Result<Self, StoreError> {
        let continuation = SnapshotContinuation::from_genesis(network, params).map_err(invalid)?;
        let genesis = continuation.anchor().clone();
        Ok(Self {
            continuation,
            selections: super::SelectionHistory::new(
                super::authority_history::AuthorityHistory::new(genesis, store.env().path())?,
                store.env().path(),
            )?,
            observations: compact::RecordObservations::new(store.env().path())?,
            last_decision: None,
            from_archive: false,
        })
    }

    pub(crate) async fn recover_genesis_from_peers(
        store: Arc<LmdbStore>,
        network: NetworkId,
        params: ProtocolParams,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        target: Option<BlockHash>,
    ) -> Result<Self, StoreError> {
        Self::recover_genesis_from_peers_until(store, network, params, registry, target, RecoveryControl::default()).await?
            .ok_or_else(|| invalid("uncancelled tip recovery stopped"))
    }

    pub(super) async fn recover_genesis_from_peers_until(
        store: Arc<LmdbStore>,
        network: NetworkId,
        params: ProtocolParams,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        target: Option<BlockHash>,
        control: RecoveryControl,
    ) -> Result<Option<Self>, StoreError> {
        if control.checkpoint("tip-recovery:before-plan") { return Ok(None); }
        let source = store.clone();
        let (recovered, records) = tokio::task::spawn_blocking(move || {
            let mut recovered = Self::genesis(&source, network, &params)?;
            let mut records =
                compact::read_after(&source, recovered.selections.authority.iter(), &[])?;
            let height = records
                .successors
                .select_through(recovered.continuation.anchor(), target)?;
            records.observations.retain_through(height)?;
            recovered.observations = records.observations;
            Ok::<_, StoreError>((recovered, records.successors))
        })
        .await
        .map_err(invalid)??;
        recovered
            .recover_records_from_peers_until(records, store, registry, control)
            .await
    }

    pub(crate) async fn advance_from_peers(
        self,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        target: BlockHash,
    ) -> Result<Self, StoreError> {
        self.advance_target_from_peers(store, registry, Some(target))
            .await
    }

    pub(super) async fn advance_target_from_peers(
        self,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        target: Option<BlockHash>,
    ) -> Result<Self, StoreError> {
        self.advance_target_from_peers_until(store, registry, target, RecoveryControl::default()).await?
            .ok_or_else(|| invalid("uncancelled tip recovery stopped"))
    }

    pub(super) async fn advance_target_from_peers_until(
        self,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        target: Option<BlockHash>,
        control: RecoveryControl,
    ) -> Result<Option<Self>, StoreError> {
        if control.checkpoint("tip-recovery:before-plan") { return Ok(None); }
        let source = store.clone();
        let (recovered, records) =
            tokio::task::spawn_blocking(move || self.begin_advance(&source, target))
                .await
                .map_err(invalid)??;
        recovered
            .recover_records_from_peers_until(records, store, registry, control)
            .await
    }

    fn begin_advance(
        mut self,
        source: &LmdbStore,
        target: Option<BlockHash>,
    ) -> Result<(Self, compact::RecoveryReferences), StoreError> {
        // Appending changes the head and active inventory. Every previously
        // observed body and beacon must still match; a new durable copy of
        // an already-authenticated identical beacon also preserves replay.
        let batch = source.tx_begin_write()?;
        let relocated = self.observations.verify_relocated(
            &batch,
            self.continuation.anchor(),
            &self.selections,
        )?;
        let mut records =
            compact::read_after(source, self.selections.authority.iter(), &self.selections)?;
        let height = records
            .successors
            .select_through(self.continuation.anchor(), target)?;
        records.observations.retain_through(height)?;
        // read_after omits archived pre-base bodies. Keep their observations
        // so reuse cannot hide corruption in a retired record.
        records
            .observations
            .retain_retired_from(&relocated, self.continuation.anchor())?;
        self.selections
            .attach_beacons(source.env().path(), records.beacons.into_cursor()?)?;
        self.observations = records.observations;
        drop(batch);
        Ok((self, records.successors))
    }

    pub(crate) fn advance_locally(
        self,
        store: &LmdbStore,
        target: BlockHash,
    ) -> Result<Self, StoreError> {
        let (mut recovered, records) = self.begin_advance(store, Some(target))?;
        for record in records.into_cursor()? {
            let record = record?.load(store)?;
            let plan = recovered.plan(store, &record)?;
            let retained =
                crate::checkpoint_sync::witness::read_plan(store, &recovered.continuation, &plan)?;
            recovered = recovered.apply(record, plan, retained)?;
        }
        Ok(recovered)
    }

    fn recover_records_from_peers_until(
        mut self,
        records: compact::RecoveryReferences,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        control: RecoveryControl,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Option<Self>, StoreError>> + Send>>
    {
        // Startup, cached recovery and tip refresh nest this replay loop. Keep
        // its suspension state out of each caller's async frame.
        Box::pin(async move {
            if control.checkpoint("tip-recovery:planned") { return Ok(None); }
            for record in records.into_cursor()? {
                if control.checkpoint("tip-recovery:record") { return Ok(None); }
                let record = record?;
                let source = store.clone();
                let (state, record, plan) = tokio::task::spawn_blocking(move || {
                    let record = record.load(&source)?;
                    let plan = self.plan(&source, &record)?;
                    Ok::<_, StoreError>((self, record, plan))
                })
                .await
                .map_err(invalid)??;
                if control.checkpoint("tip-recovery:record-planned") { return Ok(None); }
                let retained = control.peer_wait(crate::checkpoint_sync::witness::fetch_plan(
                    &registry,
                    &state.continuation,
                    &plan,
                )).await?;
                let Some(retained) = retained else { return Ok(None); };
                if control.checkpoint("tip-recovery:before-apply") { return Ok(None); }
                self = tokio::task::spawn_blocking(move || state.apply(record, plan, retained))
                    .await
                    .map_err(invalid)??;
            }
            if control.checkpoint("tip-recovery:records-complete") { return Ok(None); }
            Ok(Some(self))
        })
    }

    pub(in crate::confirmed_replay::snapshot) fn into_authority(
        self,
    ) -> super::authority_history::AuthorityHistory {
        self.selections.authority
    }

    /// Authenticate local archived history without building a snapshot header.
    /// The caller must recheck record observations before using the result.
    pub(crate) fn recover_genesis_locally(
        store: &LmdbStore,
        network: NetworkId,
        params: &ProtocolParams,
        epoch: BlockHash,
    ) -> Result<Self, StoreError> {
        let mut recovered = Self::genesis(store, network, params)?;
        let genesis = recovered.continuation.anchor().clone();
        let mut records = compact::ExportRecords::new(store, genesis)?;
        while let Some(record) = records.next()? {
            let plan = recovered.plan(store, &record)?;
            let retained =
                crate::checkpoint_sync::witness::read_plan(store, &recovered.continuation, &plan)?;
            recovered = recovered.apply(record, plan, retained)?;
            if recovered.continuation.anchor().hash() == epoch {
                recovered.observations = records.finish();
                return Ok(recovered);
            }
        }
        Err(invalid(
            "snapshot epoch is not in authenticated local history",
        ))
    }

    /// Admit an externally certified decision without a signing key or journal.
    /// The accepted head advances, not payment authority. Reload observations
    /// after success before admitting another decision or retrying this one.
    pub async fn accept_decision_from_peers(
        self: Arc<Self>,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        certificate: CheckpointCertificate,
        manifest: ReplayManifest,
    ) -> Result<(), StoreError> {
        self.accept_decision_with_beacon_from_peers(store, registry, certificate, manifest, None)
            .await
    }

    /// Validate a transferred selection beacon before storing either new record.
    /// A delayed beacon is attached to the stored selection record; its original
    /// certificate is preserved.
    pub async fn accept_decision_with_beacon_from_peers(
        self: Arc<Self>,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        certificate: CheckpointCertificate,
        manifest: ReplayManifest,
        beacon: Option<burst_vrf::RandomOutput>,
    ) -> Result<(), StoreError> {
        self.accept_transferred_decision(store, registry, certificate, manifest, beacon, None)
            .await
    }

    pub(crate) async fn accept_transferred_decision(
        self: Arc<Self>,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        certificate: CheckpointCertificate,
        manifest: ReplayManifest,
        beacon: Option<burst_vrf::RandomOutput>,
        tail: Option<super::super::tail::TransferredTail>,
    ) -> Result<(), StoreError> {
        if !cfg!(test)
            && !burst_verification::checkpoint_selection_enabled(
                self.continuation.anchor().network(),
            )
        {
            return Err(invalid(
                "compact decision admission is behind the release gate",
            ));
        }
        if certificate.checkpoint != *manifest.checkpoint() {
            return Err(invalid("compact decision differs from its manifest"));
        }
        self.continuation
            .anchor()
            .verify_certificate(&certificate)
            .map_err(invalid)?;
        let (base, candidate) = self
            .candidate_coverage_shared_inner(store.clone(), registry, manifest, false, tail)
            .await?;
        tokio::task::spawn_blocking(move || {
            crate::checkpoint_store::compact::accept_candidate_with_beacon_checked(
                &store,
                base.continuation.anchor(),
                candidate.input(),
                &certificate,
                beacon.as_ref(),
                |batch| base.verify_records_unchanged(batch),
            )
        })
        .await
        .map_err(invalid)?
    }

    /// Discover the next lifecycle event from a pinned, authenticated confirmed suffix.
    pub async fn discover_cemented_candidate_from_peers(
        self: Arc<Self>,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        clock: crate::checkpoint_replay::SigningClock,
    ) -> Result<Option<crate::checkpoint_agreement::CompactReplayCandidate>, StoreError> {
        match self.discover_cemented_candidate_attempt(store, registry, clock, |_, _| Ok(())).await? {
            CandidateDiscovery::Ready(candidate) => Ok(candidate),
            CandidateDiscovery::Advanced => Err(StoreError::StaleRead("confirmed cut advanced during compact discovery".into())),
        }
    }

    pub(crate) async fn discover_cemented_candidate_attempt(
        self: Arc<Self>,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        clock: crate::checkpoint_replay::SigningClock,
        before_validation: impl FnOnce(&LmdbStore, &CheckpointAnchor) -> Result<(), StoreError> + Send + 'static,
    ) -> Result<CandidateDiscovery, StoreError> {
        let source = Arc::clone(&store);
        let (base, plan) = tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            let _timing = DiscoveryTiming::new("plan", source.clone());
            let plan = self
                .continuation
                .plan_cemented_tail(&source)
                .map_err(invalid)?;
            Ok::<_, StoreError>((self, plan))
        })
        .await
        .map_err(invalid)??;
        #[cfg(test)]
        let timing = DiscoveryTiming::new("witness", store.clone());
        let retained = crate::checkpoint_sync::witness::fetch_plan(
            &registry,
            &base.continuation,
            plan.witness_plan(),
        )
        .await?;
        #[cfg(test)]
        drop(timing);
        tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            let timing = DiscoveryTiming::new("replay", store.clone());
            let cemented = plan.finish(retained).map_err(invalid)?;
            let candidate = base
                .continuation
                .prepare_next_cemented_candidate(&store, &cemented, &base.selections, clock)
                .map_err(invalid)?;
            #[cfg(test)]
            drop(timing);
            #[cfg(test)]
            let timing = DiscoveryTiming::new("writer-wait", store.clone());
            if candidate.is_some() {
                before_validation(&store, base.continuation.anchor())?;
            }
            let batch = store.tx_begin_write()?;
            #[cfg(test)]
            drop(timing);
            #[cfg(test)]
            let _timing = DiscoveryTiming::new("validation", store.clone());
            base.verify_records_unchanged(&batch)?;
            match base.continuation.verify_discovery_metadata_in(&batch, &cemented) {
                Err(StoreError::StaleRead(_)) => return Ok(CandidateDiscovery::Advanced),
                result => result?,
            }
            if let Some(candidate) = &candidate {
                match candidate.input().check_signing_parent(&batch, base.continuation.anchor()) {
                    Err(StoreError::StaleRead(_)) => return Ok(CandidateDiscovery::Advanced),
                    result => result?,
                }
            }
            drop(batch);
            Ok(CandidateDiscovery::Ready(candidate))
        })
        .await
        .map_err(invalid)?
    }

    /// Replay the applied suffix to cover local final promises whose blocks are
    /// not yet cemented. Capture confirmed and applied tails together, and check
    /// local metadata against the confirmed tail before returning a candidate.
    pub(crate) async fn discover_promised_candidate_from_peers(
        self: Arc<Self>, store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        clock: crate::checkpoint_replay::SigningClock,
        representative: burst_types::WalletAddress,
    ) -> Result<CandidateDiscovery, StoreError> {
        let source = store.clone();
        let (base, confirmed_plan, applied_plan, authenticated) = tokio::task::spawn_blocking(move || {
            let (confirmed, applied, proofs) = self.continuation.plan_installed_tails(&source).map_err(invalid)?;
            Ok::<_, StoreError>((self, confirmed, applied, proofs))
        }).await.map_err(invalid)??;
        let retained = crate::checkpoint_sync::witness::fetch_plan_reusing(
            &registry, &base.continuation, confirmed_plan.witness_plan(), &authenticated,
        ).await?;
        let applied_retained = if confirmed_plan.witness_plan().witness_hashes()
            == applied_plan.witness_plan().witness_hashes() {
            retained.clone()
        } else {
            crate::checkpoint_sync::witness::fetch_plan_reusing(
                &registry, &base.continuation, applied_plan.witness_plan(), retained.clone(),
            ).await?
        };
        tokio::task::spawn_blocking(move || {
            let confirmed = confirmed_plan.finish(retained).map_err(invalid)?;
            let applied = applied_plan.finish(applied_retained).map_err(invalid)?;
            let candidate = base.continuation.prepare_next_replayed_candidate(
                &store, applied.tail(), &base.selections, clock,
            ).map_err(invalid)?;
            let batch = store.tx_begin_write()?;
            base.verify_records_unchanged(&batch)?;
            match base.continuation.verify_discovery_metadata_in(&batch, &confirmed) {
                Err(StoreError::StaleRead(_)) => return Ok(CandidateDiscovery::Advanced),
                result => result?,
            }
            applied.verify_unchanged(&batch)?;
            if let Some(candidate) = &candidate {
                candidate.input().check_signing_parent(&batch, base.continuation.anchor())?;
                if !candidate.input().covers_local_promises(&batch, &representative)? {
                    return Err(StoreError::Backend("replayed cut cannot yet cover local final promises".into()));
                }
            }
            tracing::debug!("replayed applied extension for checkpoint final promises");
            Ok(CandidateDiscovery::Ready(candidate))
        }).await.map_err(invalid)?
    }

    /// Derive a confirmed cut from the installed compact ledger, then replay
    /// selection discovery. The caller must schedule due lifecycle events first.
    pub async fn discover_cemented_selection_from_peers(
        self: Arc<Self>,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        cutoff: u64,
    ) -> Result<Option<super::super::CompactSelectionCoverage>, StoreError> {
        let source = Arc::clone(&store);
        let (base, plan) = tokio::task::spawn_blocking(move || {
            let plan = self
                .continuation
                .plan_cemented_tail(&source)
                .map_err(invalid)?;
            Ok::<_, StoreError>((self, plan))
        })
        .await
        .map_err(invalid)??;
        let retained = crate::checkpoint_sync::witness::fetch_plan(
            &registry,
            &base.continuation,
            plan.witness_plan(),
        )
        .await?;
        tokio::task::spawn_blocking(move || {
            let cemented = plan.finish(retained).map_err(invalid)?;
            let heads = base
                .continuation
                .cemented_heads_at(&cemented, cutoff)
                .map_err(invalid)?;
            let tail = base
                .continuation
                .read_tail_retaining(&store, cutoff, &heads, cemented.tail())
                .map_err(invalid)?;
            let candidate = base
                .continuation
                .prepare_next_selection(tail, &base.selections)
                .map_err(invalid)?;
            let batch = store.tx_begin_write()?;
            base.verify_records_unchanged(&batch)?;
            base.continuation
                .verify_discovery_metadata_in(&batch, &cemented)?;
            if let Some(candidate) = &candidate {
                candidate.check_signing_parent(&batch, base.continuation.anchor())?;
            }
            drop(batch);
            Ok(candidate)
        })
        .await
        .map_err(invalid)?
    }

    /// Discover a selection from a locally chosen cut using authenticated peer
    /// ancestry. Lifecycle/governance scheduling must precede this call. No
    /// signature or state installation occurs here.
    pub async fn discover_selection_from_peers(
        self: Arc<Self>,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        cutoff: u64,
        heads: Vec<crate::confirmed_history::HistoryHead>,
    ) -> Result<Option<super::super::CompactSelectionCoverage>, StoreError> {
        let source = Arc::clone(&store);
        let (base, plan) = tokio::task::spawn_blocking(move || {
            let plan = self
                .continuation
                .plan_tail(&source, cutoff, &heads)
                .map_err(invalid)?;
            Ok::<_, StoreError>((self, plan))
        })
        .await
        .map_err(invalid)??;
        let retained =
            crate::checkpoint_sync::witness::fetch_plan(&registry, &base.continuation, &plan)
                .await?;
        tokio::task::spawn_blocking(move || {
            let tail = plan.finish(retained).map_err(invalid)?;
            let candidate = base
                .continuation
                .prepare_next_selection(tail, &base.selections)
                .map_err(invalid)?;
            let batch = store.tx_begin_write()?;
            base.verify_records_unchanged(&batch)?;
            if let Some(candidate) = &candidate {
                candidate.check_signing_parent(&batch, base.continuation.anchor())?;
            }
            drop(batch);
            Ok(candidate)
        })
        .await
        .map_err(invalid)?
    }

    /// Prepare a proposed selection through the peer proof service. This does
    /// not sign, save a decision, or advance the recovered base. The caller
    /// must still submit coverage to the atomic agreement transition.
    pub async fn selection_coverage_from_peers(
        self,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        manifest: crate::checkpoint_replay::CheckpointManifest,
    ) -> Result<(Self, super::super::CompactSelectionCoverage), StoreError> {
        self.selection_coverage_inner(store, registry, manifest, true)
            .await
    }

    /// A verified terminal journal may recover all its saved choices after
    /// installation advances. This only reconstructs replay; the atomic signer
    /// must still read that terminal journal before returning its decision.
    pub(crate) async fn candidate_coverage_recovery_shared(
        self: Arc<Self>,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        manifest: crate::checkpoint_agreement::ReplayManifest,
        decision: Option<&CheckpointCertificate>,
    ) -> Result<
        (
            Arc<Self>,
            crate::checkpoint_agreement::CompactReplayCandidate,
        ),
        StoreError,
    > {
        if let Some(decision) = decision {
            self.continuation
                .anchor()
                .verify_certificate(decision)
                .map_err(invalid)?;
        }
        self.candidate_coverage_shared_inner(store, registry, manifest, decision.is_none(), None)
            .await
    }

    async fn selection_coverage_inner(
        self,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        manifest: crate::checkpoint_replay::CheckpointManifest,
        require_installed_parent: bool,
    ) -> Result<(Self, super::super::CompactSelectionCoverage), StoreError> {
        let (recovered, coverage) = Arc::new(self)
            .selection_coverage_shared_inner(store, registry, manifest, require_installed_parent)
            .await?;
        let recovered = Arc::try_unwrap(recovered)
            .map_err(|_| invalid("compact recovery unexpectedly retained a shared base"))?;
        Ok((recovered, coverage))
    }

    #[cfg(test)]
    pub(crate) async fn selection_coverage_shared(
        self: Arc<Self>,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        manifest: crate::checkpoint_replay::CheckpointManifest,
    ) -> Result<super::super::CompactSelectionCoverage, StoreError> {
        self.selection_coverage_shared_inner(store, registry, manifest, true)
            .await
            .map(|(_, coverage)| coverage)
    }

    async fn selection_coverage_shared_inner(
        self: Arc<Self>,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        manifest: crate::checkpoint_replay::CheckpointManifest,
        require_installed_parent: bool,
    ) -> Result<(Arc<Self>, super::super::CompactSelectionCoverage), StoreError> {
        let (base, candidate) = self
            .candidate_coverage_shared_inner(
                store,
                registry,
                crate::checkpoint_agreement::ReplayManifest::Selection(manifest),
                require_installed_parent,
                None,
            )
            .await?;
        let crate::checkpoint_agreement::CompactReplayCandidate::Selection(coverage) = candidate
        else {
            return Err(invalid("selection replay returned another event kind"));
        };
        Ok((base, *coverage))
    }

    /// Replay a peer proposal without advancing the base or writing a journal.
    pub async fn candidate_coverage_shared(
        self: Arc<Self>,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        manifest: crate::checkpoint_agreement::ReplayManifest,
    ) -> Result<crate::checkpoint_agreement::CompactReplayCandidate, StoreError> {
        self.candidate_coverage_shared_inner(store, registry, manifest, true, None)
            .await
            .map(|(_, coverage)| coverage)
    }

    async fn candidate_coverage_shared_inner(
        self: Arc<Self>,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
        manifest: crate::checkpoint_agreement::ReplayManifest,
        require_installed_parent: bool,
        transferred: Option<super::super::tail::TransferredTail>,
    ) -> Result<
        (
            Arc<Self>,
            crate::checkpoint_agreement::CompactReplayCandidate,
        ),
        StoreError,
    > {
        use crate::checkpoint_agreement::{CompactReplayCandidate, ReplayManifest};
        if bincode::serialized_size(&manifest).map_err(invalid)?
            > crate::checkpoint_store::MAX_RECORD_BYTES
        {
            return Err(invalid("compact proposal exceeds byte limit"));
        }
        let source = Arc::clone(&store);
        let (recovered, manifest, plan) = tokio::task::spawn_blocking(move || {
            self.continuation
                .anchor()
                .validate_successor(manifest.checkpoint())
                .map_err(invalid)?;
            let cutoff = manifest.checkpoint().cutoff;
            let plan = match transferred {
                Some(tail) => self.continuation.plan_transferred_tail(
                    tail,
                    cutoff,
                    manifest.heads(),
                    source.env().path(),
                ),
                None => self
                    .continuation
                    .plan_tail(&source, cutoff, manifest.heads()),
            }
            .map_err(invalid)?;
            Ok::<_, StoreError>((self, manifest, plan))
        })
        .await
        .map_err(invalid)??;
        let retained =
            crate::checkpoint_sync::witness::fetch_plan(&registry, &recovered.continuation, &plan)
                .await?;
        tokio::task::spawn_blocking(move || {
            let tail = plan.finish(retained).map_err(invalid)?;
            let selections = &recovered.selections;
            let coverage = match &manifest {
                ReplayManifest::Selection(manifest) => CompactReplayCandidate::Selection(Box::new(
                    recovered
                        .continuation
                        .selection_vote_coverage(tail, manifest, selections)
                        .map_err(invalid)?,
                )),
                ReplayManifest::Deadline(manifest) => CompactReplayCandidate::Deadline(Box::new(
                    recovered
                        .continuation
                        .deadline_vote_coverage_for_manifest(&tail, manifest, selections)
                        .map_err(invalid)?,
                )),
                ReplayManifest::ChallengeOutcome(manifest) => {
                    CompactReplayCandidate::Outcome(Box::new(
                        recovered
                            .continuation
                            .outcome_vote_coverage_for_manifest(&tail, manifest, selections)
                            .map_err(invalid)?,
                    ))
                }
                ReplayManifest::Roster(manifest) => CompactReplayCandidate::Roster(Box::new(
                    recovered
                        .continuation
                        .roster_vote_coverage_for_manifest(&tail, manifest, selections)
                        .map_err(invalid)?,
                )),
                ReplayManifest::Activation(manifest) => {
                    CompactReplayCandidate::Activation(Box::new(
                        recovered
                            .continuation
                            .activation_vote_coverage_for_manifest(&tail, manifest, selections)
                            .map_err(invalid)?,
                    ))
                }
                ReplayManifest::Governance(manifest) => {
                    CompactReplayCandidate::Governance(Box::new(
                        recovered
                            .continuation
                            .governance_vote_coverage_for_manifest(&tail, manifest, selections)
                            .map_err(invalid)?,
                    ))
                }
            };
            // Retrieval may have raced a new installed epoch or certificate.
            // Use one writer view, then drop it without committing anything.
            let batch = store.tx_begin_write()?;
            recovered.verify_records_unchanged(&batch)?;
            if require_installed_parent {
                coverage
                    .input()
                    .check_signing_parent(&batch, recovered.continuation.anchor())?;
            }
            drop(batch);
            Ok((recovered, coverage))
        })
        .await
        .map_err(invalid)?
    }

    /// Relay the last semantically replayed successor under its original parent.
    /// An archive alone has no decision manifest, so base-only recovery returns
    /// None. This cache supplies neither candidate replay nor signing authority.
    pub fn replayed_decision_relay(
        &self,
        now: std::time::Instant,
    ) -> Result<
        Option<(
            crate::checkpoint_transport::Context,
            crate::checkpoint_transport::Transport,
        )>,
        StoreError,
    > {
        self.last_decision
            .as_ref()
            .map(|(parent, certificate, manifest)| {
                crate::checkpoint_transport::decision_relay(
                    parent,
                    certificate.clone(),
                    manifest.clone(),
                    now,
                )
            })
            .transpose()
    }

    pub(super) fn into_journal_history(self) -> (super::SelectionHistory, compact::RecordObservations) {
        (self.selections, self.observations)
    }

    pub(crate) fn verify_records_unchanged(
        &self,
        batch: &LmdbWriteTransaction<'_>,
    ) -> Result<(), StoreError> {
        self.verify_records_view(&batch.read_view())
    }

    pub(crate) fn verify_records_view(
        &self,
        batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    ) -> Result<(), StoreError> {
        self.verify_records_view_with_poll(batch, &|_| Ok(()))
    }

    pub(crate) fn verify_records_view_with_poll(
        &self,
        batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.observations.verify_view_with_poll(batch, poll)
    }

    pub(crate) fn verify_replayed_decision(
        &self,
        certificate: &CheckpointCertificate,
        manifest: &ReplayManifest,
    ) -> Result<(), StoreError> {
        let (parent, original, expected) = self
            .last_decision
            .as_ref()
            .ok_or_else(|| invalid("recovered state has no successor decision"))?;
        parent.verify_certificate(certificate).map_err(invalid)?;
        if certificate.checkpoint != original.checkpoint || manifest != expected {
            return Err(invalid(
                "transferred checkpoint differs from replayed decision",
            ));
        }
        Ok(())
    }

    fn plan(
        &mut self,
        store: &LmdbStore,
        record: &compact::RecoveryRecord,
    ) -> Result<SnapshotTailPlan, StoreError> {
        self.continuation
            .anchor()
            .verify_certificate(&record.certificate)
            .map_err(invalid)?;
        self.attach_activation_beacon(record)?;
        self.continuation
            .plan_tail(
                store,
                record.certificate.checkpoint.cutoff,
                record.manifest.heads(),
            )
            .map_err(invalid)
    }

    fn attach_activation_beacon(
        &mut self,
        record: &compact::RecoveryRecord,
    ) -> Result<(), StoreError> {
        // Activation carries the exact old-selection beacon. Authenticate
        // it before executing the suffix, as the full-history loader does.
        if let ReplayManifest::Activation(manifest) = &record.manifest {
            self.selections
                .attach_beacon(manifest.activation.selection, manifest.activation.beacon())?;
        }
        Ok(())
    }

    fn apply(
        self,
        record: compact::RecoveryRecord,
        plan: SnapshotTailPlan,
        retained: Vec<HistoricalBlock>,
    ) -> Result<Self, StoreError> {
        let tail = plan.finish(retained).map_err(invalid)?;
        self.apply_tail(record, tail)
    }

    pub(super) fn installation_successor(
        &self,
        store: &LmdbStore,
        record: compact::RecoveryRecord,
        observations: &compact::RecordObservations,
        tail: SnapshotTail,
        accepted: Option<&Self>,
    ) -> Result<(Self, super::super::continuation::CompactCutCoverage, bool), StoreError> {
        let cut = self.continuation.cut_coverage(&tail).map_err(invalid)?;
        if let Some(accepted) = accepted.filter(|accepted|
            accepted.continuation.anchor().hash() == record.certificate.checkpoint.hash()) {
            // Journal recovery already executed this exact certified successor.
            // Its observations include beacon bytes; neither a later accepted tip
            // nor changed records can replace this immediate installation input.
            let reader = store.tx_begin_read()?;
            accepted.verify_records_view(&store.read_view(&reader)?)?;
            accepted.verify_replayed_decision(&record.certificate, &record.manifest)?;
            drop(reader);
            return Ok((accepted.fork(store.env().path())?, cut, true));
        }
        let mut next = self.fork(store.env().path())?;
        let mut observed = observations.fork()?;
        observed.retain_through(record.certificate.checkpoint.height)?;
        observed.retain_retired_from(&self.observations, self.continuation.anchor())?;
        next.observations = observed;
        next.attach_activation_beacon(&record)?;
        Ok((next.apply_tail(record, tail)?, cut, false))
    }

    fn apply_tail(
        mut self,
        record: compact::RecoveryRecord,
        tail: SnapshotTail,
    ) -> Result<Self, StoreError> {
        let parent = self.continuation.anchor().clone();
        self.continuation = if let ReplayManifest::Selection(manifest) = &record.manifest {
            let (next, mut selection) = self
                .continuation
                .replay_selection(tail, &record.certificate, manifest, &self.selections)
                .map_err(invalid)?;
            if let Some(beacon) = record.beacon {
                selection.attach_compact_beacon(beacon)?;
            }
            self.selections.push(selection)?;
            next
        } else {
            self.continuation
                .replay_manifest(
                    tail,
                    &record.certificate,
                    &record.manifest,
                    &self.selections,
                )
                .map_err(invalid)?
        };
        self.selections
            .authority
            .append(&record.certificate, self.continuation.anchor())?;
        self.last_decision = Some((parent, record.certificate, record.manifest));
        Ok(self)
    }
}

/// Optional diagnostics for the sustained-traffic fixture; absent from production.
#[cfg(test)]
struct DiscoveryTiming {
    stage: &'static str,
    store: Arc<LmdbStore>,
    started: std::time::Instant,
    revision: usize,
    enabled: bool,
}

#[cfg(test)]
impl DiscoveryTiming {
    fn new(stage: &'static str, store: Arc<LmdbStore>) -> Self {
        Self {
            stage,
            revision: store.env().info().last_txn_id,
            store,
            started: std::time::Instant::now(),
            enabled: std::env::var_os("BURST_TRACE_COMPACT_DISCOVERY").is_some(),
        }
    }
}

#[cfg(test)]
impl Drop for DiscoveryTiming {
    fn drop(&mut self) {
        if self.enabled {
            eprintln!("compact discovery timing: store={} stage={} elapsed_us={} revision_before={} revision_after={}",
                self.store.env().path().display(), self.stage, self.started.elapsed().as_micros(),
                self.revision, self.store.env().info().last_txn_id);
        }
    }
}
