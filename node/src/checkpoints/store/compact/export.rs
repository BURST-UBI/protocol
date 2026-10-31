//! Observed, one-record-at-a-time input for historical economic export.
//! The caller must recheck all observations under the writer after replay.
use super::*;
use crate::checkpoint_store::record_storage;

pub(crate) struct ExportRecords<'a> {
    store: &'a LmdbStore,
    genesis: CheckpointAnchor,
    inventory: std::collections::BTreeMap<u64, burst_types::BlockHash>,
    height: u64,
    tip: u64,
    observations: RecordObservations,
}

impl<'a> ExportRecords<'a> {
    pub(crate) fn new(store: &'a LmdbStore, genesis: CheckpointAnchor) -> Result<Self, StoreError> {
        let txn = store.tx_begin_read()?;
        let prefix = prefix(&genesis);
        let head_key = format!("{prefix}/head");
        let head = record_storage::get_txn(&store.meta_store(), &txn, &head_key)?;
        let tip = decode_tip(head.as_deref())?;
        let inventory = super::super::inventory::fingerprints(store, &txn, &prefix, tip)?;
        let mut observations = RecordObservations::new(store.env().path())?;
        observations.push(RecordObservation::inventory(
            &prefix,
            inventory.keys().copied(),
        ))?;
        observations.push(RecordObservation::new(None, head_key, head.as_deref()))?;
        Ok(Self {
            store,
            genesis,
            inventory,
            height: 0,
            tip,
            observations,
        })
    }

    pub(crate) fn next(&mut self) -> Result<Option<RecoveryRecord>, StoreError> {
        if self.height == self.tip {
            return Ok(None);
        }
        let txn = self.store.tx_begin_read()?;
        self.height += 1;
        let active_key = format!("{}/{}", prefix(&self.genesis), self.height);
        let expected = self.inventory.remove(&self.height);
        let active = record_storage::get_txn(&self.store.meta_store(), &txn, &active_key)?;
        let actual = active
            .as_deref()
            .map(|bytes| burst_types::BlockHash::new(burst_crypto::blake2b_256(bytes)));
        if actual != expected {
            return Err(invalid(
                "historical checkpoint inventory changed during export",
            ));
        }
        self.observations.push(RecordObservation::new(
            Some(self.height),
            active_key,
            active.as_deref(),
        ))?;
        let bytes = match active {
            Some(bytes) => bytes,
            None => {
                let key = archived_record_key(&self.genesis, self.height);
                let bytes = record_storage::get_txn(&self.store.meta_store(), &txn, &key)?;
                self.observations.push(RecordObservation::new(
                    Some(self.height),
                    key,
                    bytes.as_deref(),
                ))?;
                bytes.ok_or_else(|| invalid("historical checkpoint record is missing"))?
            }
        };
        let record = chain::decode_record(&bytes)?;
        let certificate = record.certificate().clone();
        if certificate.checkpoint.height != self.height {
            return Err(invalid("historical checkpoint record has the wrong height"));
        }
        let manifest = record.manifest();
        let mut beacon = match record {
            chain::ParsedRecord::Selection(record) => record.beacon,
            _ => None,
        };
        if matches!(manifest, ReplayManifest::Selection(_)) {
            let key = super::super::archive_selection::beacon_key(certificate.checkpoint.hash());
            let bytes = record_storage::get_txn(&self.store.meta_store(), &txn, &key)?;
            self.observations.push(RecordObservation::new(
                Some(self.height),
                key,
                bytes.as_deref(),
            ))?;
            if let Some(bytes) = bytes {
                let archived = super::super::archive_selection::decode_beacon(&bytes)?;
                if let Some(existing) = &beacon {
                    if bincode::serialize(existing).map_err(invalid)?
                        != bincode::serialize(&archived).map_err(invalid)?
                    {
                        return Err(invalid(
                            "historical selection has conflicting beacon proofs",
                        ));
                    }
                }
                beacon = Some(archived);
            }
        }
        Ok(Some(RecoveryRecord {
            certificate,
            manifest,
            beacon,
        }))
    }

    pub(crate) fn finish(self) -> RecordObservations {
        self.observations
    }
}
