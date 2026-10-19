//! Read checkpoint records relative to an independently authenticated base.
//! Returned manifests still require economic replay; decoding grants no authority.

use super::*;
use crate::checkpoint_agreement::{CompactReplayInput, ReplayManifest};
use blake2::{digest::consts::U32, Blake2b, Digest};

mod export;
mod observations;
mod retirement;
mod scratch;
mod successors;
pub(crate) use export::ExportRecords;
pub(crate) use observations::RecordObservations;
pub(crate) use successors::RecoveryReferences;

/// Persist a semantically replayed compact selection and its authenticated
/// suffix atomically. This does not install economics or clear local promises.
#[cfg(test)]
pub(crate) fn accept_selection(
    store: &LmdbStore,
    anchor: &CheckpointAnchor,
    coverage: &crate::confirmed_replay::snapshot::CompactSelectionCoverage,
    certificate: &CheckpointCertificate,
) -> Result<(), StoreError> {
    accept_candidate(
        store,
        anchor,
        CompactReplayInput::Selection(coverage),
        certificate,
    )
}

/// Accept replay-checked compact effects without installing their economic state.
pub(crate) fn accept_candidate(
    store: &LmdbStore,
    anchor: &CheckpointAnchor,
    coverage: CompactReplayInput<'_>,
    certificate: &CheckpointCertificate,
) -> Result<(), StoreError> {
    accept_candidate_checked(store, anchor, coverage, certificate, |_| Ok(()))
}

pub(crate) fn accept_candidate_checked(
    store: &LmdbStore,
    anchor: &CheckpointAnchor,
    coverage: CompactReplayInput<'_>,
    certificate: &CheckpointCertificate,
    check: impl FnOnce(&burst_store_lmdb::LmdbWriteTransaction<'_>) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    accept_candidate_with_beacon_checked(store, anchor, coverage, certificate, None, check)
}

pub(crate) fn accept_candidate_with_beacon_checked(
    store: &LmdbStore,
    anchor: &CheckpointAnchor,
    coverage: CompactReplayInput<'_>,
    certificate: &CheckpointCertificate,
    beacon: Option<&RandomOutput>,
    check: impl FnOnce(&burst_store_lmdb::LmdbWriteTransaction<'_>) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    if !cfg!(test) && !burst_verification::checkpoint_selection_enabled(anchor.network()) {
        return Err(invalid(
            "compact checkpoint persistence is behind the release gate",
        ));
    }
    coverage.validate_parent(anchor)?;
    anchor.verify_certificate(certificate).map_err(invalid)?;
    if coverage.checkpoint() != &certificate.checkpoint {
        return Err(invalid("compact decision differs from replay"));
    }
    if let Some(beacon) = beacon {
        let CompactReplayInput::Selection(selection) = coverage else {
            return Err(invalid("only selection records carry a separate beacon"));
        };
        if bincode::serialized_size(beacon).map_err(invalid)? > MAX_BEACON_BYTES as u64 {
            return Err(invalid("checkpoint beacon exceeds byte limit"));
        }
        selection
            .replay()
            .request()
            .select(anchor, certificate, beacon)
            .map_err(invalid)?;
    }
    let height = certificate.checkpoint.height;
    if height == 0 {
        return Err(invalid("invalid checkpoint height"));
    }
    let manifest = coverage.manifest();
    let bytes = match &manifest {
        ReplayManifest::Selection(manifest) => encode(&Record {
            version: 1,
            certificate: certificate.clone(),
            heads: manifest.heads.clone(),
            target: manifest.target.clone(),
            beacon: beacon.cloned(),
        })?,
        ReplayManifest::Deadline(manifest) => {
            chain::encode_deadline_manifest(certificate, manifest)?
        }
        ReplayManifest::ChallengeOutcome(manifest) => {
            chain::encode_outcome_manifest(certificate, manifest)?
        }
        ReplayManifest::Roster(manifest) => chain::encode_roster_manifest(certificate, manifest)?,
        ReplayManifest::Activation(manifest) => {
            chain::encode_activation_manifest(certificate, manifest)?
        }
        ReplayManifest::Governance(manifest) => {
            chain::encode_governance_manifest(certificate, manifest)?
        }
    };
    let prefix = prefix(anchor);
    let key = format!("{prefix}/{height}");
    let tip_key = format!("{prefix}/head");
    let mut batch = store.tx_begin_write()?;
    check(&batch)?;
    let tip = decode_tip(batch.get_meta(&tip_key)?.as_deref())?;
    if let Some(existing) = batch.get_meta(&key)? {
        let mut existing = chain::decode_record(&existing)?;
        anchor
            .verify_certificate(existing.certificate())
            .map_err(invalid)?;
        if tip < height
            || existing.certificate().checkpoint != certificate.checkpoint
            || existing.manifest() != manifest
        {
            return Err(invalid("compact checkpoint conflicts with stored decision"));
        }
        let mut update = None;
        if let (CompactReplayInput::Selection(coverage), chain::ParsedRecord::Selection(existing)) =
            (coverage, &mut existing)
        {
            if let Some(beacon) = &existing.beacon {
                if bincode::serialized_size(beacon).map_err(invalid)? > MAX_BEACON_BYTES as u64 {
                    return Err(invalid("checkpoint beacon exceeds byte limit"));
                }
                coverage
                    .replay()
                    .request()
                    .select(anchor, &existing.certificate, beacon)
                    .map_err(invalid)?;
            }
            if let Some(incoming) = beacon {
                if let Some(stored) = &existing.beacon {
                    if bincode::serialize(stored).map_err(invalid)?
                        != bincode::serialize(incoming).map_err(invalid)?
                    {
                        return Err(invalid("checkpoint already has a different beacon"));
                    }
                } else {
                    existing.beacon = Some(incoming.clone());
                    update = Some(encode(existing)?);
                }
            }
        }
        for row in coverage.archive_rows() {
            let (key, bytes) = row?;
            if batch.get_meta(&key)?.as_deref() != Some(bytes.as_slice()) {
                return Err(invalid("compact certified history is missing or changed"));
            }
        }
        let activation = if let ReplayManifest::Activation(manifest) = &manifest {
            // Replay checked the proof against the exact selection receipt.
            // Keep it durable even when that receipt has been retired.
            archive_selection::persist_beacon_for_epoch(
                &mut batch,
                manifest.activation.selection,
                &manifest.activation.beacon(),
            )?;
            true
        } else {
            false
        };
        if let Some(bytes) = update {
            batch.put_meta(&key, &bytes)?;
            batch.commit()?;
        } else if activation {
            batch.commit()?;
        } else {
            drop(batch);
        }
        return store.force_sync().map_err(invalid);
    }
    coverage.check_signing_parent(&batch, anchor)?;
    if tip != anchor.height() {
        return Err(invalid("compact checkpoint parent is not the accepted tip"));
    }
    retirement::make_room(&mut batch, anchor, tip, bytes.len())?;
    for row in coverage.archive_rows() {
        let (key, bytes) = row?;
        match batch.get_meta(&key)? {
            Some(existing) if existing != bytes => {
                return Err(invalid("compact certified history bytes changed"))
            }
            Some(_) => (),
            None => batch.put_meta(&key, &bytes)?,
        }
    }
    if let ReplayManifest::Activation(manifest) = &manifest {
        archive_selection::persist_beacon_for_epoch(
            &mut batch,
            manifest.activation.selection,
            &manifest.activation.beacon(),
        )?;
    }
    batch.put_meta(&key, &bytes)?;
    batch.put_meta(&tip_key, &height.to_be_bytes())?;
    batch.commit()?;
    store.force_sync().map_err(invalid)
}

pub(crate) struct RecoveryRecord {
    pub certificate: CheckpointCertificate,
    pub manifest: ReplayManifest,
    pub beacon: Option<RandomOutput>,
}

pub(crate) struct RecoveryRecords {
    pub successors: RecoveryReferences,
    pub beacons: RecoveryBeacons,
    pub observations: RecordObservations,
}

// The existing beacon limit plus the fixed-width checkpoint hash. This bounds
// each scratch frame, not the number of historical activations.
type RecoveryBeacons =
    scratch::ScratchRows<(burst_types::BlockHash, RandomOutput), { MAX_BEACON_BYTES as u64 + 32 }>;

/// Identifies a checked record without retaining its decoded manifest or body.
#[derive(Serialize, Deserialize)]
pub(crate) struct RecoveryRecordRef {
    pub height: u64,
    pub checkpoint: burst_types::BlockHash,
    key: String,
    digest: burst_types::BlockHash,
    beacon: Option<RandomOutput>,
}

impl RecoveryRecordRef {
    pub(crate) fn load(&self, store: &LmdbStore) -> Result<RecoveryRecord, StoreError> {
        let txn = store.tx_begin_read()?;
        let bytes = store
            .meta_store()
            .get_meta_txn(&txn, &self.key)?
            .ok_or_else(|| {
                StoreError::StaleRead("checkpoint record disappeared during recovery".into())
            })?;
        if burst_crypto::blake2b_256(&bytes) != *self.digest.as_bytes() {
            return Err(StoreError::StaleRead(
                "checkpoint record changed during recovery".into(),
            ));
        }
        let record = chain::decode_record(&bytes)?;
        let certificate = record.certificate().clone();
        let manifest = record.manifest();
        Ok(RecoveryRecord {
            certificate,
            manifest,
            beacon: self.beacon.clone(),
        })
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct RecordObservation {
    pub height: Option<u64>,
    key: String,
    value: ObservedValue,
}

#[derive(Serialize, Deserialize)]
enum ObservedValue {
    Record(Option<burst_types::BlockHash>),
    Inventory(burst_types::BlockHash),
}

struct InventoryDigest(Blake2b<U32>);

impl InventoryDigest {
    fn new() -> Self {
        Self(Blake2b::new().chain_update(b"BURST/checkpoint-inventory/v1\0"))
    }

    fn add(&mut self, key: &[u8]) {
        self.0.update((key.len() as u64).to_be_bytes());
        self.0.update(key);
    }

    fn finish(self) -> burst_types::BlockHash {
        burst_types::BlockHash::new(self.0.finalize().into())
    }
}

impl RecordObservation {
    pub(crate) fn verify_relocated(
        &mut self,
        batch: &burst_store_lmdb::LmdbWriteTransaction<'_>,
        anchor: &CheckpointAnchor,
        selections: &(impl crate::confirmed_replay::snapshot::SelectionSource + ?Sized),
    ) -> Result<Option<Self>, StoreError> {
        match self.verify(batch) {
            Ok(()) => return Ok(None),
            Err(StoreError::StaleRead(_)) => (),
            Err(error) => return Err(error),
        }
        // Activation can persist a second copy of a beacon already authenticated
        // in the selection receipt. Only that exact absent-to-identical change
        // preserves replay; a new proof or changed existing value stays stale.
        if matches!(self.value, ObservedValue::Record(None)) {
            if let Some(selection) = selections
                .find_selection(|selection| {
                    self.height == Some(selection.certificate().checkpoint.height)
                        && self.key
                            == archive_selection::beacon_key(
                                selection.certificate().checkpoint.hash(),
                            )
                })
                .map_err(invalid)?
            {
                if let Some(beacon) = selection.beacon() {
                    let expected = bincode::serialize(beacon).map_err(invalid)?;
                    if batch.get_meta(&self.key)?.as_deref() == Some(expected.as_slice()) {
                        self.value = ObservedValue::Record(Some(burst_types::BlockHash::new(
                            burst_crypto::blake2b_256(&expected),
                        )));
                        return Ok(None);
                    }
                }
            }
        }
        if let (Some(height), ObservedValue::Record(Some(expected))) = (self.height, &self.value) {
            // A late beacon may enrich an otherwise byte-identical selection.
            // Authenticate the proof with the replayed receipt before accepting
            // this mutation; every original record field remains pinned.
            if self.key == format!("{}/{height}", prefix(anchor))
                || self.key == archived_record_key(anchor, height)
            {
                if let Some(bytes) = batch.get_meta(&self.key)? {
                    if let Ok(chain::ParsedRecord::Selection(mut record)) =
                        chain::decode_record(&bytes)
                    {
                        if let Some(beacon) = record.beacon.take() {
                            let original = encode(&record)?;
                            if burst_types::BlockHash::new(burst_crypto::blake2b_256(&original))
                                == *expected
                            {
                                if let Some(receipt) = selections
                                    .find_selection(|receipt| {
                                        receipt.certificate().checkpoint
                                            == record.certificate.checkpoint
                                    })
                                    .map_err(invalid)?
                                {
                                    receipt.into_owned().attach_compact_beacon(beacon)?;
                                    self.value =
                                        ObservedValue::Record(Some(burst_types::BlockHash::new(
                                            burst_crypto::blake2b_256(&bytes),
                                        )));
                                    return Ok(None);
                                }
                            }
                        }
                    }
                }
            }
            if self.key == format!("{}/{height}", prefix(anchor))
                && batch.get_meta(&self.key)?.is_none()
            {
                let archive = archived_record_key(anchor, height);
                if let Some(bytes) = batch.get_meta(&archive)? {
                    if burst_types::BlockHash::new(burst_crypto::blake2b_256(&bytes)) == *expected {
                        let absence = Self::new(self.height, self.key.clone(), None);
                        self.key = archive;
                        return Ok(Some(absence));
                    }
                }
            }
        }
        Err(StoreError::StaleRead(format!(
            "cached checkpoint bytes changed or disappeared: {}",
            self.key,
        )))
    }

    fn new(height: Option<u64>, key: String, bytes: Option<&[u8]>) -> Self {
        Self {
            height,
            key,
            value: ObservedValue::Record(
                bytes.map(|bytes| burst_types::BlockHash::new(burst_crypto::blake2b_256(bytes))),
            ),
        }
    }

    fn inventory(prefix: &str, heights: impl Iterator<Item = u64>) -> Self {
        let key = format!("{prefix}/");
        let mut keys: Vec<_> = heights
            .map(|height| format!("{key}{height}").into_bytes())
            .collect();
        keys.sort();
        let mut digest = InventoryDigest::new();
        for key in keys {
            digest.add(&key);
        }
        Self {
            height: None,
            key,
            value: ObservedValue::Inventory(digest.finish()),
        }
    }

    pub(crate) fn verify(
        &self,
        batch: &burst_store_lmdb::LmdbWriteTransaction<'_>,
    ) -> Result<(), StoreError> {
        if let ObservedValue::Inventory(expected) = &self.value {
            let head = format!("{}head", self.key);
            let mut actual = InventoryDigest::new();
            batch.visit_checkpoint_metadata(self.key.as_bytes(), |key, _| {
                if key != head.as_bytes() {
                    actual.add(key);
                }
                Ok(())
            })?;
            if actual.finish() != *expected {
                return Err(StoreError::StaleRead(
                    "checkpoint inventory changed during installed recovery".into(),
                ));
            }
            return Ok(());
        }
        let current = batch.get_meta(&self.key)?;
        let digest = current
            .as_deref()
            .map(|bytes| burst_types::BlockHash::new(burst_crypto::blake2b_256(bytes)));
        if !matches!(&self.value, ObservedValue::Record(expected) if digest == *expected) {
            return Err(StoreError::StaleRead(format!(
                "checkpoint record changed during installed recovery: {}",
                self.key
            )));
        }
        Ok(())
    }
}

pub(crate) fn read_after(
    store: &LmdbStore,
    anchors: impl IntoIterator<Item = Result<CheckpointAnchor, StoreError>>,
    selections: &(impl crate::confirmed_replay::snapshot::SelectionSource + ?Sized),
) -> Result<RecoveryRecords, StoreError> {
    let mut anchors = anchors.into_iter();
    let genesis = anchors
        .next()
        .ok_or_else(|| invalid("missing recovery genesis"))??;
    if genesis.height() != 0 {
        return Err(invalid(
            "recovery requires the complete authenticated authority path",
        ));
    }
    let prefix = prefix(&genesis);
    let txn = store.tx_begin_read()?;
    let meta = store.meta_store();
    let head_key = format!("{prefix}/head");
    let head_bytes = meta.get_meta_txn(&txn, &head_key)?;
    let tip = decode_tip(head_bytes.as_deref())?;
    let inventory = super::inventory::fingerprints(store, &txn, &prefix, tip)?;
    let mut observations = RecordObservations::new(store.env().path())?;
    observations.push(RecordObservation::inventory(
        &prefix,
        inventory.keys().copied(),
    ))?;
    observations.push(RecordObservation::new(
        None,
        head_key,
        head_bytes.as_deref(),
    ))?;
    let mut result = RecoveryRecords {
        successors: RecoveryReferences::new(store.env().path())?,
        beacons: RecoveryBeacons::new(store.env().path())?,
        observations,
    };
    // Only active pre-base records need their parent electorate below. Archived
    // successors are authenticated by semantic replay, not by this inventory read.
    let mut retained_authorities = std::collections::HashMap::new();
    let mut base = genesis.clone();
    let mut pending_selections = selections.read_all();
    let mut pending_selection = pending_selections.next().transpose().map_err(invalid)?;
    for next in anchors {
        let next = next?;
        if base.height().checked_add(1) != Some(next.height())
            || next.network() != base.network()
            || next.genesis_hash() != base.genesis_hash()
            || next.cutoff() <= base.cutoff()
        {
            return Err(invalid("recovery authority path is not contiguous"));
        }
        if let Some(selection) = &pending_selection {
            let checkpoint = &selection.certificate().checkpoint;
            if checkpoint.height < next.height() {
                return Err(invalid(
                    "beacon selection differs from authenticated authority",
                ));
            }
            if checkpoint.height == next.height() {
                if checkpoint.hash() != next.hash()
                    || selection.parent().hash() != base.hash()
                    || checkpoint.parent != base.hash()
                {
                    return Err(invalid(
                        "beacon selection differs from authenticated authority",
                    ));
                }
                let key = super::archive_selection::beacon_key(checkpoint.hash());
                let bytes = meta.get_meta_txn(&txn, &key)?;
                if let Some(bytes) = &bytes {
                    result.beacons.push((
                        checkpoint.hash(),
                        super::archive_selection::decode_beacon(bytes)?,
                    ))?;
                }
                result.observations.push(RecordObservation::new(
                    Some(checkpoint.height),
                    key,
                    bytes.as_deref(),
                ))?;
                pending_selection = pending_selections.next().transpose().map_err(invalid)?;
            }
        }
        if inventory.contains_key(&next.height()) {
            retained_authorities.insert(next.height(), (base, next.clone()));
        }
        base = next;
    }
    if pending_selection.is_some() {
        return Err(invalid(
            "beacon selection differs from authenticated authority",
        ));
    }
    // Pre-base records may be absent. Successors must exist either in the
    // active set or as original archived bytes; both still require replay.
    let heights = inventory
        .keys()
        .copied()
        .filter(|height| *height <= base.height())
        .chain((base.height()..tip).map(|height| height + 1));
    for height in heights {
        let active_key = format!("{prefix}/{height}");
        let (key, bytes, digest) = if let Some(digest) = inventory.get(&height) {
            let bytes = meta
                .get_meta_txn(&txn, &active_key)?
                .ok_or_else(|| invalid("checkpoint inventory record is missing"))?;
            (active_key, bytes, *digest)
        } else {
            result
                .observations
                .push(RecordObservation::new(Some(height), active_key, None))?;
            let key = archived_record_key(&genesis, height);
            let bytes = meta
                .get_meta_txn(&txn, &key)?
                .ok_or_else(|| invalid("compact checkpoint chain has a missing successor"))?;
            let digest = burst_types::BlockHash::new(burst_crypto::blake2b_256(&bytes));
            (key, bytes, digest)
        };
        result.observations.push(RecordObservation::new(
            Some(height),
            key.clone(),
            Some(&bytes),
        ))?;
        let record = chain::decode_record(&bytes)?;
        let certificate = record.certificate();
        if certificate.checkpoint.height != height {
            return Err(invalid("checkpoint record is stored at a different height"));
        }
        if crate::confirmed_history::head_commitment(
            genesis.network(),
            genesis.genesis_hash(),
            certificate.checkpoint.cutoff,
            record.heads(),
        )? != certificate.checkpoint.ledger_root
        {
            return Err(invalid(
                "checkpoint heads differ from their certified commitment",
            ));
        }
        let mut beacon = match &record {
            chain::ParsedRecord::Selection(record) => record.beacon.clone(),
            _ => None,
        };
        if height <= base.height() {
            let (parent, anchor) = retained_authorities
                .remove(&height)
                .ok_or_else(|| invalid("retained checkpoint has no authenticated authority"))?;
            if certificate.checkpoint.hash() != anchor.hash() {
                return Err(invalid(
                    "retained checkpoint conflicts with snapshot authority",
                ));
            }
            parent.verify_certificate(certificate).map_err(invalid)?;
            if let Some(beacon) = beacon {
                result
                    .beacons
                    .push((certificate.checkpoint.hash(), beacon))?;
            }
            if let ReplayManifest::Activation(manifest) = record.manifest() {
                result
                    .beacons
                    .push((manifest.activation.selection, manifest.activation.beacon()))?;
            }
        } else {
            if matches!(record, chain::ParsedRecord::Selection(_)) {
                let key = super::archive_selection::beacon_key(certificate.checkpoint.hash());
                let bytes = meta.get_meta_txn(&txn, &key)?;
                result.observations.push(RecordObservation::new(
                    Some(height),
                    key,
                    bytes.as_deref(),
                ))?;
                if let Some(bytes) = bytes {
                    super::archive_selection::merge_beacon(
                        &mut beacon,
                        super::archive_selection::decode_beacon(&bytes)?,
                    )?;
                }
            }
            if base
                .height()
                .checked_add(result.successors.len())
                .and_then(|height| height.checked_add(1))
                != Some(height)
            {
                return Err(invalid("compact checkpoint chain has a missing successor"));
            }
            result.successors.push(RecoveryRecordRef {
                height,
                checkpoint: certificate.checkpoint.hash(),
                key,
                digest,
                beacon,
            })?;
        }
    }
    if result.successors.len() != tip.saturating_sub(base.height()) {
        return Err(invalid("compact checkpoint chain has a missing successor"));
    }
    Ok(result)
}

#[cfg(test)]
mod inventory_tests {
    use super::*;
    use burst_store::meta::MetaStore;

    #[test]
    fn inventory_commitment_detects_equal_count_substitution_and_hidden_keys() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let prefix = "checkpoint_chain/v1/dev/inventory-test";
        let observed = RecordObservation::inventory(prefix, 1..=256);
        let mut batch = store.tx_begin_write().unwrap();
        for height in 1..=256 {
            batch
                .put_meta(&format!("{prefix}/{height}"), b"record")
                .unwrap();
        }
        observed.verify(&batch).unwrap();
        batch.delete_meta(&format!("{prefix}/1")).unwrap();
        batch
            .put_meta(&format!("{prefix}/257"), b"replacement")
            .unwrap();
        assert!(observed.verify(&batch).is_err());
        batch.delete_meta(&format!("{prefix}/257")).unwrap();
        batch.put_meta(&format!("{prefix}/1"), b"record").unwrap();
        observed.verify(&batch).unwrap();
        batch.put_meta(&format!("{prefix}/01"), b"hidden").unwrap();
        assert!(observed.verify(&batch).is_err());
        batch.delete_meta(&format!("{prefix}/01")).unwrap();
        // The head has its own value observation, not inventory membership.
        batch.put_meta(&format!("{prefix}/head"), b"head").unwrap();
        observed.verify(&batch).unwrap();
        let mut first = InventoryDigest::new();
        let mut second = InventoryDigest::new();
        for key in [b"a".as_slice(), b"bc"] {
            first.add(key);
        }
        for key in [b"ab".as_slice(), b"c"] {
            second.add(key);
        }
        assert_ne!(first.finish(), second.finish());
    }

    #[test]
    fn compact_inventory_rejects_records_beyond_the_old_scan() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let anchor =
            ReplayedCheckpoint::genesis_anchor(NetworkId::Dev, &ProtocolParams::default()).unwrap();
        let anchors = [anchor];
        assert!(read_after(&store, std::iter::empty(), &[]).is_err());
        assert!(read_after(
            &store,
            [Ok(anchors[0].clone()), Ok(anchors[0].clone())],
            &[],
        )
        .is_err());
        assert!(read_after(
            &store,
            [
                Ok(anchors[0].clone()),
                Err(invalid("authority read failed"))
            ],
            &[],
        )
        .err()
        .unwrap()
        .to_string()
        .contains("authority read failed"));
        let observed = read_after(&store, anchors.iter().cloned().map(Ok), &[]).unwrap();
        {
            let batch = store.tx_begin_write().unwrap();
            observed.observations.verify(&batch).unwrap();
        }
        let key = format!("{}/999", prefix(&anchors[0]));
        store.meta_store().put_meta(&key, b"orphan").unwrap();
        assert!(read_after(&store, anchors.iter().cloned().map(Ok), &[]).is_err());
        let batch = store.tx_begin_write().unwrap();
        assert!(observed.observations.verify(&batch).is_err());
    }

    #[test]
    fn inventory_observation_detects_insertions_and_deletions_in_writer_view() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let prefix = "checkpoint_chain/v1/dev/test";
        for height in [1, 2, 10] {
            store
                .meta_store()
                .put_meta(&format!("{prefix}/{height}"), b"record")
                .unwrap();
        }
        let observed = RecordObservation::inventory(prefix, [1, 2, 10].into_iter());
        let mut batch = store.tx_begin_write().unwrap();
        observed.verify(&batch).unwrap();
        batch.put_meta(&format!("{prefix}/1000"), b"late").unwrap();
        assert!(observed.verify(&batch).is_err());
        drop(batch);
        store
            .meta_store()
            .delete_meta(&format!("{prefix}/2"))
            .unwrap();
        assert!(observed.verify(&store.tx_begin_write().unwrap()).is_err());
    }
}
