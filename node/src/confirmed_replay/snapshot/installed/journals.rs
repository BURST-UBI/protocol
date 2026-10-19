use super::*;
use blake2::{digest::consts::U32, Blake2b, Digest};
use burst_consensus::checkpoint::CheckpointAnchor;
use burst_types::BlockHash;

mod epoch_index;
mod records;
mod sequences;

const PREFIXES: &[&[u8]] = &[
    b"block_undo/",
    b"checkpoint_round/",
    b"consensus_final/",
    b"consensus_fence/",
    b"payment_epoch/v1/closed/",
    b"consensus_time/",
    b"consensus_governance/",
    b"snapshot_attestation/",
    b"checkpoint_vote/",
];

pub(super) struct JournalGuard {
    fingerprint: BlockHash,
    sequences: sequences::SequenceGuard,
    accepted_records: Option<crate::checkpoint_store::compact::RecordObservations>,
}

fn observe(batch: &LmdbWriteTransaction<'_>) -> Result<BlockHash, StoreError> {
    let mut digest = Blake2b::<U32>::new();
    digest.update(b"BURST/recovery-journals/v1");
    for prefix in PREFIXES {
        digest.update((prefix.len() as u64).to_be_bytes());
        digest.update(prefix);
        for row in batch.checkpoint_metadata_entries(prefix)? {
            let (key, bytes) = row?;
            digest.update([1]);
            digest.update((key.len() as u64).to_be_bytes());
            digest.update(key);
            digest.update((bytes.len() as u64).to_be_bytes());
            digest.update(bytes);
        }
        digest.update([0]);
    }
    Ok(BlockHash::new(digest.finalize().into()))
}

pub(super) fn has_rounds(store: &LmdbStore) -> Result<bool, StoreError> {
    let batch = store.tx_begin_write()?;
    Ok(batch
        .checkpoint_metadata_entries(b"checkpoint_round/")?
        .next()
        .transpose()?
        .is_some()
        || batch
            .checkpoint_metadata_entries(b"snapshot_attestation/")?
            .next()
            .transpose()?
            .is_some())
}

impl JournalGuard {
    pub(super) fn capture(
        batch: &LmdbWriteTransaction<'_>,
        installed: &RecoveredSnapshot,
        accepted: Option<RecoveredSnapshot>,
    ) -> Result<Self, StoreError> {
        let anchors = accepted
            .as_ref()
            .map_or(&installed.selections.authority, |state| &state.selections.authority);
        let fingerprint = observe(batch)?;
        validate(batch, || anchors.iter(), installed.continuation.anchor())?;
        Ok(Self {
            fingerprint,
            sequences: sequences::SequenceGuard::capture(batch)?,
            accepted_records: accepted.map(RecoveredSnapshot::into_record_observations),
        })
    }

    pub(super) fn verify_unchanged(
        &self,
        batch: &LmdbWriteTransaction<'_>,
    ) -> Result<(), StoreError> {
        if observe(batch)? != self.fingerprint {
            return Err(invalid("local signer journals changed during recovery"));
        }
        self.sequences.verify(batch)?;
        if let Some(record) = &self.accepted_records {
            record.verify(batch)?;
        }
        Ok(())
    }
}

fn validate<I>(
    batch: &LmdbWriteTransaction<'_>,
    mut authority: impl FnMut() -> I,
    installed: &CheckpointAnchor,
) -> Result<(), StoreError>
where
    I: IntoIterator<Item = Result<CheckpointAnchor, StoreError>>,
{
    batch.verify_checkpoint_undo_encoding()?;
    records::validate(batch, authority(), installed)?;
    crate::checkpoint_agreement::reconcile_recovery_journals(batch, authority())?;
    let mut anchors = authority().into_iter();
    let genesis = anchors
        .next()
        .ok_or_else(|| invalid("missing journal authority"))??;
    let round_prefix = format!(
        "checkpoint_round/v1/{}/{}/",
        genesis.network().as_str(),
        genesis.hash()
    );
    for row in batch.checkpoint_metadata_entries(b"checkpoint_round/")? {
        let (key, _) = row?;
        if !key.starts_with(round_prefix.as_bytes()) {
            return Err(invalid(
                "checkpoint journal has an unsupported version or network",
            ));
        }
    }
    let mut promises = 0u64;
    for namespace in ["consensus_final", "consensus_fence"] {
        let prefix = format!("{namespace}/");
        for row in batch.checkpoint_metadata_entries(prefix.as_bytes())? {
            let (key, _) = row?;
            let key = std::str::from_utf8(key).map_err(invalid)?;
            key.strip_prefix(&format!("{namespace}/v2/"))
                .and_then(|suffix| suffix.split_once('/'))
                .ok_or_else(|| invalid("payment promise has an unsupported version or key"))?;
            promises = promises
                .checked_add(1)
                .ok_or_else(|| invalid("payment promise count overflow"))?;
        }
    }
    let has_closed = batch
        .checkpoint_metadata_entries(b"payment_epoch/v1/closed/")?
        .next()
        .transpose()?
        .is_some();
    let epochs = std::iter::once(Ok(genesis))
        .chain(anchors)
        .filter_map(|anchor| {
            let checked = (|| {
                let anchor = anchor?;
                if anchor.height() > installed.height() {
                    return Ok(None);
                }
                super::super::reconciliation::visit_retained_promises(
                    batch,
                    anchor.hash(),
                    |_, _| {
                        promises = promises
                            .checked_sub(1)
                            .ok_or_else(|| invalid("duplicate payment promise authority"))?;
                        Ok(())
                    },
                )?;
                Ok(Some((anchor.hash(), anchor.height())))
            })();
            checked.transpose()
        });
    if has_closed {
        let mut index = epoch_index::EpochIndex::build(batch.storage_directory(), epochs)?;
        for row in batch.checkpoint_metadata_entries(b"payment_epoch/v1/closed/")? {
            let (key, value) = row?;
            if value.len() != 64 {
                return Err(invalid("invalid closed payment epoch encoding"));
            }
            let from = BlockHash::new(value[32..].try_into().map_err(invalid)?);
            let to = BlockHash::new(value[..32].try_into().map_err(invalid)?);
            if key != burst_store::meta::closed_payment_epoch_key(&from).as_bytes() {
                return Err(invalid("closed payment epoch differs from its key"));
            }
            match (index.height(from)?, index.height(to)?) {
                (Some(from), Some(to)) if from < to => (),
                _ => {
                    return Err(invalid(
                        "closed payment epoch differs from authenticated descent",
                    ))
                }
            }
        }
        index.verify()?;
    } else {
        for epoch in epochs {
            epoch?;
        }
    }
    if promises != 0 {
        return Err(invalid(
            "payment promise belongs to an unauthenticated or uninstalled epoch",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_consensus::checkpoint::{
        Checkpoint, CheckpointCertificate, CheckpointSignature, HumanDelegation, HumanRoster,
    };
    use burst_consensus::checkpoint_round::Ballot;

    #[test]
    fn closed_epochs_require_forward_installed_authority_without_rewriting_records() {
        let key = burst_crypto::keypair_from_seed(&[41; 32]);
        let representative = burst_crypto::derive_address(&key.public);
        let roster = HumanRoster::new(vec![HumanDelegation {
            human: representative.clone(),
            representative: representative.clone(),
        }])
        .unwrap();
        let genesis = CheckpointAnchor::genesis(
            burst_types::NetworkId::Dev,
            BlockHash::new([9; 32]),
            100,
            roster,
            6700,
        )
        .unwrap();
        let mut anchors = vec![genesis];
        for height in 1..=3 {
            let parent = anchors.last().unwrap();
            let checkpoint = Checkpoint {
                network: parent.network(),
                genesis: parent.genesis_hash(),
                parent: parent.hash(),
                height,
                cutoff: 100 + height,
                ledger_root: BlockHash::new([1; 32]),
                params_hash: BlockHash::new([2; 32]),
                next_roster: parent.roster().commitment(),
                selection_root: BlockHash::new([3; 32]),
                next_quorum_bps: 6700,
            };
            let payload = Ballot::precommit(&checkpoint, 0).signing_payload();
            let certificate = CheckpointCertificate {
                checkpoint,
                round: 0,
                signatures: vec![CheckpointSignature {
                    representative: representative.clone(),
                    signature: burst_crypto::sign_message(&payload, &key.private),
                }],
            };
            anchors.push(
                parent
                    .advance(&certificate, parent.roster().clone())
                    .unwrap(),
            );
        }
        let unknown = BlockHash::new([77; 32]);
        let h = |height: usize| anchors[height].hash();
        for (from, to, expected) in [
            (h(0), h(1), true),
            (h(0), h(2), true),
            (h(1), h(2), true),
            (h(1), h(1), false),
            (h(2), h(1), false),
            (h(0), h(3), false),
            (h(3), h(2), false),
            (unknown, h(2), false),
            (h(0), unknown, false),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
            let mut batch = store.tx_begin_write().unwrap();
            let mut value = to.as_bytes().to_vec();
            value.extend_from_slice(from.as_bytes());
            batch
                .put_meta(&burst_store::meta::closed_payment_epoch_key(&from), &value)
                .unwrap();
            let before = batch.checkpoint_metadata(b"").unwrap();
            assert_eq!(
                validate(&batch, || anchors.iter().cloned().map(Ok), &anchors[2]).is_ok(),
                expected,
                "closure {from} -> {to}"
            );
            assert_eq!(batch.checkpoint_metadata(b"").unwrap(), before);
        }
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        for (from, to) in [(h(0), h(2)), (h(1), h(3))] {
            let mut value = to.as_bytes().to_vec();
            value.extend_from_slice(from.as_bytes());
            batch
                .put_meta(&burst_store::meta::closed_payment_epoch_key(&from), &value)
                .unwrap();
        }
        let before = batch.checkpoint_metadata(b"").unwrap();
        validate(&batch, || anchors.iter().cloned().map(Ok), &anchors[3]).unwrap();
        assert_eq!(batch.checkpoint_metadata(b"").unwrap(), before);
    }
}
