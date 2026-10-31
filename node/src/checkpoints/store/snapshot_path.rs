use super::*;
use crate::checkpoint_store::record_storage;
use burst_store_lmdb::LmdbWriteTransaction;

/// Compare already accepted local certificates with a separately authenticated
/// snapshot path. No ledger replay or new certificate persistence occurs here.
/// The caller authenticates the path; this checks local records against it.
pub(crate) fn reconcile_snapshot_path(
    batch: &LmdbWriteTransaction<'_>,
    anchors: impl IntoIterator<Item = Result<CheckpointAnchor, StoreError>>,
) -> Result<u64, StoreError> {
    reconcile_records(batch, anchors, |_, _, _, _| {}).map(|(tip, _)| tip)
}

fn reconcile_records(
    batch: &LmdbWriteTransaction<'_>,
    anchors: impl IntoIterator<Item = Result<CheckpointAnchor, StoreError>>,
    mut observe: impl FnMut(&str, &[u8], bool, Option<burst_types::BlockHash>),
) -> Result<(u64, CheckpointAnchor), StoreError> {
    let mut anchors = anchors.into_iter();
    let genesis = anchors
        .next()
        .ok_or_else(|| invalid("empty snapshot authority path"))??;
    if genesis.height() != 0 {
        return Err(invalid("snapshot authority must start at genesis"));
    }
    let prefix = format!("{}/", prefix(&genesis));
    let tip = decode_tip(record_storage::get_batch(&batch, &format!("{prefix}head"))?.as_deref())?;
    let base = crate::confirmed_replay::snapshot::installed::read_base_at(
        |key| record_storage::get_batch(&batch, key),
        &genesis,
        batch.storage_directory(),
    )?
    .map(|(base, _)| base.anchor().clone());
    let base_height = base.as_ref().map_or(0, CheckpointAnchor::height);
    if base_height > tip {
        return Err(invalid("stored snapshot base exceeds accepted tip"));
    }
    let mut present = std::collections::HashSet::new();
    let mut used = 0usize;
    batch.visit_checkpoint_metadata(prefix.as_bytes(), |key, bytes| {
        let key = std::str::from_utf8(key).map_err(invalid)?;
        let suffix = key
            .strip_prefix(&prefix)
            .ok_or_else(|| invalid("invalid checkpoint key"))?;
        if suffix == "head" {
            return Ok(());
        }
        let height: u64 = suffix.parse().map_err(invalid)?;
        if height == 0 || height > tip || suffix != height.to_string() {
            return Err(invalid("invalid local checkpoint height"));
        }
        present.insert(height);
        let bytes = record_storage::decode_stored(
            key,
            bytes,
            |name| batch.get_meta(name),
            |hash| batch.read_view().checkpoint_head_node(hash),
            MAX_CHAIN_BYTES - used,
        )?;
        used = used
            .checked_add(bytes.len())
            .filter(|size| *size <= MAX_CHAIN_BYTES)
            .ok_or_else(|| invalid("snapshot checkpoint records exceed byte limit"))?;
        Ok(())
    })?;
    let mut parent = genesis;
    let mut base_seen = base
        .as_ref()
        .is_none_or(|base| base.hash() == parent.hash());
    for next in anchors {
        let next = next?;
        if parent.height().checked_add(1) != Some(next.height())
            || next.network() != parent.network()
            || next.genesis_hash() != parent.genesis_hash()
            || next.cutoff() <= parent.cutoff()
        {
            return Err(invalid("snapshot authority path is not contiguous"));
        }
        let height = next.height();
        if let Some(base) = &base {
            if height == base.height() {
                if next.hash() != base.hash() {
                    return Err(invalid(
                        "snapshot does not extend the stored authenticated base",
                    ));
                }
                base_seen = true;
            }
        }
        let active = present.remove(&height);
        if active || (height > base_height && height <= tip) {
            let key = if active {
                format!("{prefix}{height}")
            } else {
                archived_record_key(&parent, height)
            };
            let bytes = record_storage::get_batch(&batch, &key)?
                .ok_or_else(|| invalid("snapshot cannot discard a missing accepted successor"))?;
            let selection = verify_record(&bytes, &parent, &next)?;
            observe(&key, &bytes, !active, selection);
        }
        parent = next;
    }
    if parent.height() < tip || !present.is_empty() || !base_seen {
        return Err(invalid(
            "snapshot omits or rolls back accepted checkpoint records",
        ));
    }
    Ok((tip, parent))
}

fn verify_record(
    bytes: &[u8],
    parent: &CheckpointAnchor,
    next: &CheckpointAnchor,
) -> Result<Option<burst_types::BlockHash>, StoreError> {
    let record = chain::decode_record(bytes)?;
    let certificate = record.certificate();
    if certificate.checkpoint.hash() != next.hash() {
        return Err(invalid(
            "snapshot conflicts with an accepted local checkpoint",
        ));
    }
    parent.verify_certificate(certificate).map_err(invalid)?;
    if crate::confirmed_history::head_commitment(
        parent.network(),
        parent.genesis_hash(),
        certificate.checkpoint.cutoff,
        record.heads(),
    )? != certificate.checkpoint.ledger_root
    {
        return Err(invalid(
            "retained checkpoint heads differ from their certificate",
        ));
    }
    Ok(record.selection_epoch())
}

pub(crate) fn stage_snapshot_head(
    batch: &mut LmdbWriteTransaction<'_>,
    anchors: impl IntoIterator<Item = Result<CheckpointAnchor, StoreError>>,
) -> Result<SnapshotRecords, StoreError> {
    let mut records = Vec::new();
    let (_, anchor) = reconcile_records(batch, anchors, |key, bytes, archived, selection| {
        records.push(StagedRecord {
            key: key.to_owned(),
            digest: burst_types::BlockHash::new(burst_crypto::blake2b_256(bytes)),
            archived,
            selection,
        });
    })?;
    if anchor.height() == 0 {
        return Err(invalid("invalid snapshot checkpoint height"));
    }
    record_storage::put(
        batch,
        &format!("{}/head", prefix(&anchor)),
        &anchor.height().to_be_bytes(),
    )?;
    Ok(SnapshotRecords {
        epoch: anchor.hash(),
        records,
    })
}

/// Only constructed after local records have been reconciled with the incoming
/// authority path under this writer. Original bytes remain available for audit.
pub(crate) struct SnapshotRecords {
    epoch: burst_types::BlockHash,
    records: Vec<StagedRecord>,
}

struct StagedRecord {
    key: String,
    digest: burst_types::BlockHash,
    archived: bool,
    selection: Option<burst_types::BlockHash>,
}

impl SnapshotRecords {
    pub(crate) fn archive(
        self,
        batch: &mut LmdbWriteTransaction<'_>,
        snapshot: &crate::confirmed_replay::snapshot::archive::AuthenticatedSnapshot,
    ) -> Result<(), StoreError> {
        if self.epoch != snapshot.anchor().hash() {
            return Err(invalid("record retirement belongs to another snapshot"));
        }
        let required = self
            .records
            .iter()
            .filter_map(|record| record.selection)
            .collect();
        let mut selections = snapshot
            .recover_selections_for(&required)
            .map_err(invalid)?;
        for StagedRecord {
            key,
            digest,
            archived: already_archived,
            ..
        } in self.records
        {
            let bytes = record_storage::get_batch(&batch, &key)?
                .ok_or_else(|| invalid("checkpoint record missing before retirement"))?;
            if burst_crypto::blake2b_256(&bytes) != *digest.as_bytes() {
                return Err(invalid("checkpoint record changed before retirement"));
            }
            let record = chain::decode_record(&bytes)?;
            let proof = match &record {
                chain::ParsedRecord::Selection(selection) => {
                    let restored = selections
                        .get(&selection.certificate.checkpoint.hash())
                        .ok_or_else(|| invalid("snapshot omits a retained selection receipt"))?;
                    let crate::checkpoint_agreement::ReplayManifest::Selection(manifest) =
                        record.manifest()
                    else {
                        unreachable!("selection record has selection manifest")
                    };
                    if restored.replayed().manifest() != manifest {
                        return Err(invalid(
                            "snapshot selection receipt differs from retained record",
                        ));
                    }
                    selection
                        .beacon
                        .clone()
                        .map(|beacon| (selection.certificate.checkpoint.hash(), beacon))
                }
                chain::ParsedRecord::Activation(_) => {
                    let crate::checkpoint_agreement::ReplayManifest::Activation(manifest) =
                        record.manifest()
                    else {
                        unreachable!("activation record has activation manifest")
                    };
                    Some((manifest.activation.selection, manifest.activation.beacon()))
                }
                _ => None,
            };
            if let Some((epoch, beacon)) = proof {
                let selection = selections.get_mut(&epoch).ok_or_else(|| {
                    invalid("retired beacon has no authenticated selection receipt")
                })?;
                selection.attach_compact_beacon(beacon.clone())?;
                super::archive_selection::persist_beacon(batch, selection, &beacon)?;
            }
            let archived =
                archived_record_key(snapshot.anchor(), record.certificate().checkpoint.height);
            match record_storage::matches_batch(&batch, &archived, &bytes)? {
                Some(false) => {
                    return Err(invalid(
                        "retired checkpoint archive conflicts with local record",
                    ))
                }
                Some(_) => (),
                None => record_storage::put(batch, &archived, &bytes)?,
            }
            if !already_archived {
                record_storage::delete(batch, &key)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_consensus::{
        checkpoint::{Checkpoint, CheckpointSignature},
        snapshot::SnapshotAuthorityStep,
    };
    use burst_store::meta::MetaStore;
    use burst_types::BlockHash;

    #[test]
    fn compact_head_and_records_accept_later_authenticated_heights_without_full_replay() {
        let params = ProtocolParams::default();
        let genesis = ReplayedCheckpoint::genesis_anchor(NetworkId::Dev, &params).unwrap();
        let mut anchors = vec![genesis];
        let key = burst_crypto::keypair_from_seed(&[0; 32]);
        let representative = burst_crypto::derive_address(&key.public);
        let mut last = None;
        for _ in 0..129 {
            let parent = anchors.last().unwrap();
            let checkpoint = Checkpoint {
                network: parent.network(),
                genesis: parent.genesis_hash(),
                parent: parent.hash(),
                height: parent.height() + 1,
                cutoff: parent.cutoff() + 1,
                ledger_root: BlockHash::new([2; 32]),
                params_hash: params.params_hash(),
                next_roster: parent.roster().commitment(),
                selection_root: BlockHash::new([4; 32]),
                next_quorum_bps: parent.quorum_bps(),
            };
            let payload = burst_consensus::checkpoint_round::Ballot::precommit(&checkpoint, 0)
                .signing_payload();
            let certificate = CheckpointCertificate {
                checkpoint,
                round: 0,
                signatures: vec![CheckpointSignature {
                    representative: representative.clone(),
                    signature: burst_crypto::sign_message(&payload, &key.private),
                }],
            };
            let step = SnapshotAuthorityStep {
                certificate: certificate.clone(),
                roster: parent.roster().humans().to_vec(),
            };
            anchors.push(step.authenticate(parent).unwrap());
            last = Some(certificate);
        }
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let before = batch.checkpoint_metadata(b"").unwrap();
        let interrupted = anchors
            .iter()
            .cloned()
            .map(Ok)
            .chain(std::iter::once(Err(invalid("authority page unavailable"))));
        assert!(stage_snapshot_head(&mut batch, interrupted)
            .err()
            .unwrap()
            .to_string()
            .contains("authority page unavailable"));
        assert!(
            stage_snapshot_head(&mut batch, [Ok(anchors[0].clone()), Ok(anchors[0].clone())],)
                .is_err()
        );
        assert!(stage_snapshot_head(&mut batch, anchors[1..].iter().cloned().map(Ok),).is_err());
        assert_eq!(batch.checkpoint_metadata(b"").unwrap(), before);
        stage_snapshot_head(&mut batch, anchors.iter().cloned().map(Ok)).unwrap();
        batch.commit().unwrap();
        assert_eq!(
            CertifiedCheckpoint::stored_tip(&store, NetworkId::Dev, &params).unwrap(),
            129
        );
        let records =
            super::super::compact::read_after(&store, anchors.iter().cloned().map(Ok), &[])
                .unwrap();
        assert_eq!(records.successors.len(), 0);
        assert_eq!(records.observations.len(), 2);
        let error = chain::load(&store, NetworkId::Dev, &params).err().unwrap();
        assert!(error.to_string().contains("incremental replay"));

        let mut record = Record {
            version: 1,
            certificate: last.unwrap(),
            heads: vec![],
            target: representative,
            beacon: None,
        };
        assert!(chain::decode_record(&encode(&record).unwrap()).is_ok());
        record.certificate.checkpoint.height = 0;
        assert!(chain::decode_record(&encode(&record).unwrap()).is_err());
        let head_key = format!("{}/head", prefix(&anchors[0]));
        store
            .meta_store()
            .put_meta(&head_key, &u64::MAX.to_be_bytes())
            .unwrap();
        assert!(chain::load(&store, NetworkId::Dev, &params).is_err());
        assert!(
            super::super::compact::read_after(&store, anchors.iter().cloned().map(Ok), &[])
                .is_err()
        );
    }
}
