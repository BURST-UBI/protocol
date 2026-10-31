use super::*;
use burst_store_lmdb::LmdbWriteTransaction;

pub(crate) struct ArchiveSelection {
    pub parameter_hash: Option<burst_types::BlockHash>,
    pub heads: Vec<HistoryHead>,
    pub beacon: Option<RandomOutput>,
}

pub(crate) fn beacon_key(epoch: burst_types::BlockHash) -> String {
    format!("snapshot_selection_beacon/v1/{epoch}")
}

pub(crate) fn decode_beacon(bytes: &[u8]) -> Result<RandomOutput, StoreError> {
    if bytes.len() > MAX_BEACON_BYTES {
        return Err(invalid("snapshot selection beacon exceeds byte limit"));
    }
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .reject_trailing_bytes()
        .with_limit(MAX_BEACON_BYTES as u64)
        .deserialize(bytes)
        .map_err(invalid)
}

/// Combining storage locations does not authenticate the proof. The caller
/// still verifies it against the replayed selection's exact round.
pub(super) fn merge_beacon(
    stored: &mut Option<RandomOutput>,
    incoming: RandomOutput,
) -> Result<(), StoreError> {
    if let Some(existing) = stored {
        if bincode::serialize(existing).map_err(invalid)?
            != bincode::serialize(&incoming).map_err(invalid)?
        {
            return Err(invalid("selection has conflicting beacon proofs"));
        }
    }
    *stored = Some(incoming);
    Ok(())
}

pub(super) fn persist_beacon(
    batch: &mut LmdbWriteTransaction<'_>,
    checkpoint: &CertifiedCheckpoint,
    beacon: &RandomOutput,
) -> Result<(), StoreError> {
    persist_beacon_for_epoch(batch, checkpoint.certificate().checkpoint.hash(), beacon)
}

/// The caller must authenticate the proof against the certified selection.
pub(super) fn persist_beacon_for_epoch(
    batch: &mut LmdbWriteTransaction<'_>,
    epoch: burst_types::BlockHash,
    beacon: &RandomOutput,
) -> Result<(), StoreError> {
    let key = beacon_key(epoch);
    let bytes = bincode::serialize(beacon).map_err(invalid)?;
    if let Some(old) = batch.get_meta(&key)? {
        if old != bytes {
            return Err(invalid("snapshot selection already has a different beacon"));
        }
    } else {
        batch.put_meta(&key, &bytes)?;
    }
    Ok(())
}

impl CertifiedCheckpoint {
    pub(crate) fn publication_epoch_ready(
        &self,
        batch: &LmdbWriteTransaction<'_>,
        params: &ProtocolParams,
    ) -> Result<bool, StoreError> {
        match self.archive_selection(batch, params)? {
            Some(archive) => Ok(archive.parameter_hash.is_some()),
            None => self.live_epoch_ready(batch),
        }
    }

    pub(crate) fn archive_selection(
        &self,
        batch: &LmdbWriteTransaction<'_>,
        initial_params: &ProtocolParams,
    ) -> Result<Option<ArchiveSelection>, StoreError> {
        use crate::confirmed_replay::snapshot::installed;
        let Some((archive, _)) = installed::read_base(
            |key| batch.get_meta(key),
            self.parent.network(),
            initial_params,
            batch.storage_directory(),
        )?
        else {
            return Ok(None);
        };
        if self.certificate().checkpoint.height > archive.anchor().height() {
            return Ok(None);
        }
        let mut record = archive
            .recover_selection(self.certificate().checkpoint.height)
            .map_err(invalid)?
            .ok_or_else(|| invalid("selection is absent from authenticated snapshot"))?;
        if record.certificate() != self.certificate()
            || record.parent().hash() != self.parent().hash()
            || record.replayed().manifest() != self.replayed().manifest()
        {
            return Err(invalid(
                "selection differs from authenticated snapshot receipt",
            ));
        }
        if let Some(bytes) = batch.get_meta(&beacon_key(self.certificate().checkpoint.hash()))? {
            record.attach_compact_beacon(decode_beacon(&bytes)?)?;
        }
        if let Some(beacon) = self.beacon() {
            // A retained full record may carry a beacon learned before the
            // archive was installed. It remains independently checked.
            if record.beacon().is_none() {
                self.require_persisted(batch)?;
                record.attach_compact_beacon(beacon.clone())?;
            }
            if bincode::serialize(&record.beacon()).map_err(invalid)?
                != bincode::serialize(&Some(beacon)).map_err(invalid)?
            {
                return Err(invalid("snapshot selection beacon changed"));
            }
        }
        let base = archive.anchor().clone();
        let heads = archive.confirmed_heads();
        let state = archive.into_state();
        let installed = batch.payment_epoch()?;
        let parameter_hash = if batch
            .get_meta(crate::confirmed_replay::installation::INSTALLED_KEY)?
            .is_none()
        {
            None
        } else if installed == base.hash() {
            let expected =
                crate::confirmed_replay::installation::authority_bytes(&base, base.cutoff())?;
            if crate::payment_authority::storage::read_at(|key| batch.get_meta(key))?
                != Some(expected)
            {
                return Err(invalid(
                    "installed snapshot authority differs from its archive",
                ));
            }
            Some(state.params().params_hash())
        } else {
            self.live_parameter_hash_after(batch, base.height() + 1)?
        };
        Ok(Some(ArchiveSelection {
            parameter_hash,
            heads,
            beacon: record.beacon().cloned(),
        }))
    }
}
