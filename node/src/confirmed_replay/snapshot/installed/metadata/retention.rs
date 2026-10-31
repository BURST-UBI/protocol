use super::*;
use burst_store_lmdb::retention::{ReferencePage, TrstReferenceReader};
use burst_types::TrstReference;

mod receipts;
mod verification;
mod governance;
mod delegation;
mod rounds;
mod signers;
mod bodies;
mod payment_bodies;
pub(crate) use bodies::OwnedRetirement;
#[cfg(test)]
use bodies::BodyRequirements;
#[cfg(test)]
pub(crate) use signers::SignerRecord;
#[cfg(test)]
use signers::SignerRecordReader;
pub(crate) use crate::confirmed_replay::snapshot::reconciliation::promises::PaymentPromise;
pub(crate) use receipts::ReceiptReferenceReader;
pub(crate) use verification::VerificationReferenceReader;
pub(crate) use governance::GovernanceReferenceReader;
pub(crate) use delegation::DelegationReferenceReader;
pub(crate) use rounds::RoundJournalReader;

/// TRST roots from the exact storage view checked against recovered authority
/// and applied economics. This is one part of retention closure, not permission
/// to delete rows. Other economic roots and authenticated resolution are required.
pub struct RecoveredTrstReferences<'proof, 'db> {
    metadata: &'proof RecoveredInstalledMetadata,
    reader: TrstReferenceReader<'db>,
    store: &'db LmdbStore,
    _confirmed_schedule: burst_store_lmdb::confirmed_governance::Snapshot,
}

impl RecoveredInstalledMetadata {
    /// Build direct body roots from a fresh, checked storage view. No absence
    /// result permits deletion: transitive resolution and archives remain needed.
    #[cfg(test)]
    pub(crate) fn body_requirements<'proof, 'db>(&'proof self, store: &'db LmdbStore) -> Result<BodyRequirements<'proof, 'db>, StoreError> {
        BodyRequirements::collect(self.trst_references(store)?)
    }

    /// Authenticate and traverse the caller's captured view without replacing it.
    pub(crate) fn trst_references_view<'proof, 'db>(
        &'proof self, store: &'db LmdbStore,
        captured: &'db burst_store_lmdb::LmdbReadTransaction<'db>,
    ) -> Result<RecoveredTrstReferences<'proof, 'db>, StoreError> {
        let view = store.read_view(captured)?;
        self.verify_view(&view)?;
        let confirmed_schedule = view.read_confirmed_governance()?;
        let reader = store.trst_reference_reader_view(captured)?;
        Ok(RecoveredTrstReferences { metadata: self, reader, store, _confirmed_schedule: confirmed_schedule })
    }

    /// Recheck recovered state under the writer, then pin its committed view
    /// before releasing the writer. Page reads do not repeat this verification.
    /// The page budget does not bound acquisition memory or writer duration.
    pub fn trst_references<'proof, 'db>(
        &'proof self,
        store: &'db LmdbStore,
    ) -> Result<RecoveredTrstReferences<'proof, 'db>, StoreError> {
        let batch = store.tx_begin_write()?;
        self.verify_unchanged(&batch)?;
        // verify_unchanged authenticates this cached persistent snapshot against
        // the certified baseline plus cemented tail, not provisional economics.
        let confirmed_schedule = batch.read_confirmed_governance()?;
        let reader = batch.trst_reference_reader()?;
        drop(batch);
        Ok(RecoveredTrstReferences { metadata: self, reader, store, _confirmed_schedule: confirmed_schedule })
    }
}

impl<'proof, 'db> RecoveredTrstReferences<'proof, 'db> {
    pub fn metadata(&self) -> &RecoveredInstalledMetadata { self.metadata }

    /// Journal authority may extend beyond installed economic authority. These
    /// retained certificates are reauthenticated as they are streamed; retaining
    /// them does not install their economics or authorize a new signature.
    #[cfg(test)]
    pub(crate) fn signer_authority(&self) -> (
        &burst_consensus::checkpoint::CheckpointAnchor,
        impl Iterator<Item = Result<burst_consensus::snapshot::SnapshotAuthorityStep, StoreError>> + '_,
    ) {
        let authority = &self.metadata.journals.selections().authority;
        (authority.base(), authority.steps())
    }

    /// Includes accepted selection receipts and beacons used to authenticate
    /// local journals, not just the selections installed into economic state.
    /// Use SelectionSource::iter_selections for one authenticated ordered pass.
    pub(crate) fn signer_selections(&self) -> &crate::confirmed_replay::snapshot::installed::SelectionHistory {
        self.metadata.journals.selections()
    }

    /// Recheck this exact checked storage cut under the publication writer.
    /// Does not certify scan completion or dependency closure. Any intervening
    /// commit requires a new cut; newer signer sequences are never overwritten.
    #[cfg(test)]
    pub(crate) fn check_unchanged_cut(&self, batch: &LmdbWriteTransaction<'_>) -> Result<(), StoreError> {
        batch.check_retention_cut(&self.reader)
    }

    /// This cut has already authenticated the complete signer journal. Body
    /// extraction can therefore skip closed-epoch prefixes without treating
    /// the skipped records as unvalidated or deleting their signing evidence.
    fn payment_bodies(&self) -> payment_bodies::PaymentBodyReader<'_, '_> {
        payment_bodies::PaymentBodyReader::new(self.store, self.reader.view())
    }

    /// Account afterimages checked by applied replay and the local confirmation
    /// work at the same pinned view. A queue item is not a finality certificate.
    pub(crate) fn account_work(&self) -> Result<burst_store_lmdb::retention::AccountWorkReader<'_, 'db>, StoreError> {
        self.reader.account_work()
    }

    /// Exact local signing promises/counters from the checked pinned cut. These
    /// observations cannot replace newer sequence high-water marks at publication.
    #[cfg(test)]
    pub(crate) fn signer_records(&self) -> SignerRecordReader<'_, '_> {
        let anchor = self.metadata.recovered().continuation.anchor();
        SignerRecordReader::new(self.store, self.reader.view(), anchor.network(), anchor.genesis_hash())
    }

    /// Complete local checkpoint choices, locks and replay manifests, read from
    /// the same pinned cut whose signer context was authenticated at acquisition.
    pub(crate) fn checkpoint_rounds(&self) -> RoundJournalReader<'_, '_> {
        let anchor = self.metadata.recovered().continuation.anchor();
        RoundJournalReader::new(self.store, self.reader.view(), anchor.network(), anchor.genesis_hash())
    }

    /// Encoding-checked local rollback bytes observed by JournalGuard at this
    /// cut. Before/after economic claims have not been authenticated. Retention
    /// closure must use replay-derived transitions, not infer rollback rights
    /// from these images. Replacement allocates local counters at publication.
    pub(crate) fn rollback_records(&self) -> Result<burst_store_lmdb::retention::RollbackReader<'_, 'db>, StoreError> {
        self.reader.rollback_records()
    }


    /// Applied constitutional records already checked against replay. Borrow
    /// amendments, vote buckets, conflicts and the complete document/history;
    /// terminal phases and repealed articles remain retained state.
    #[cfg(test)]
    pub(crate) fn constitution(&self) -> Result<&burst_consti::ConstiEngine, StoreError> {
        self.metadata.economics.state.governance.as_ref()
            .map(|governance| &governance.constitution)
            .ok_or_else(|| invalid("applied replay lacks constitutional state"))
    }

    /// Confirmed schedule from the same verified cut: epoch, parameter values,
    /// proposal IDs, proposers, timing and all support/withdrawal boundaries.
    /// Iterate the persistent proposal map by reference and retain its iterator
    /// between batches; neither this accessor nor iteration reconstructs rows.
    #[cfg(test)]
    pub(crate) fn confirmed_governance_schedule(&self) -> &burst_store_lmdb::confirmed_governance::Snapshot {
        &self._confirmed_schedule
    }

    /// Exact ordered accrual segments from the replay-authenticated applied cut.
    /// Preserve rate, start and end (including zero-duration segments); boundary
    /// references alone cannot reconstruct accrual. Retain the iterator between
    /// batches. This borrows the existing persistent sequence without decoding it.
    #[cfg(test)]
    pub(crate) fn brn_rate_segments(&self) -> impl Iterator<Item = &burst_brn::RateSegment> {
        self.metadata.economics.state.brn.rate_history.segments().iter()
    }

    /// Borrow the already replayed graph bound by this reader's initial recheck.
    /// No aggregate decode, snapshot clone, or inventory vector is needed. Keep
    /// the iterator between batches; initial recovery still materializes engines.
    #[cfg(test)]
    pub(crate) fn legacy_delegation_references(&self) -> Result<
        impl Iterator<Item = burst_governance::record_references::Reference<'_>> + '_, StoreError,
    > {
        let governance = self.metadata.economics.state.governance.as_ref()
            .ok_or_else(|| invalid("applied replay lacks governance state"))?;
        Ok(governance.delegation.retention_references())
    }

    pub(crate) fn delegation_references(&self) -> Result<DelegationReferenceReader<'_, '_>, StoreError> {
        let anchor = self.metadata.recovered().continuation.anchor();
        DelegationReferenceReader::new(self.store, self.reader.view(), anchor.network(), anchor.genesis_hash())
    }

    pub(crate) fn governance_references(&self) -> Result<GovernanceReferenceReader<'_, '_>, StoreError> {
        GovernanceReferenceReader::new(self.store, self.reader.view())
    }

    pub(crate) fn verification_references(&self) -> Result<VerificationReferenceReader<'_, '_>, StoreError> {
        VerificationReferenceReader::new(self.store, self.reader.view())
    }

    pub(crate) fn receipt_references(&self) -> ReceiptReferenceReader<'_, '_> {
        ReceiptReferenceReader::new(self.store, self.reader.view())
    }

    /// Borrow BRN wallet roots from this same authenticated view. This covers
    /// headers/pauses/releases only; stakes, rates and other roots remain separate.
    pub fn brn_wallet_references(&self) -> Result<burst_store_lmdb::retention::BrnWalletReferenceReader<'_, 'db>, StoreError> {
        self.reader.brn_wallet_references()
    }

    pub fn visit_next_page(
        &mut self,
        max_rows: usize,
        max_bytes: usize,
        visit: impl FnMut(TrstReference<'_>) -> Result<(), StoreError>,
    ) -> Result<ReferencePage, StoreError> {
        self.reader.visit_next_page(max_rows, max_bytes, visit)
    }
}
