use super::*;
use blake2::{digest::consts::U32, Blake2b, Digest};
use burst_consensus::checkpoint::CheckpointAnchor;
use burst_types::BlockHash;

mod epoch_index;
mod records;
mod sequences;

const PREFIXES: &[&[u8]] = &[
    b"block_undo/",
    // Local rollback conflict counters: observe exact bytes, not replay authority.
    b"governance_revision/",
    b"checkpoint_round/",
    b"consensus_final/",
    b"consensus_fence/",
    b"payment_epoch/",
    b"consensus_time/",
    b"consensus_governance/",
    b"snapshot_attestation/",
    b"checkpoint_vote/",
];

pub(super) struct JournalGuard {
    fingerprint: BlockHash,
    history: JournalHistory,
    sequences: sequences::SequenceGuard,
}

enum JournalHistory {
    Records {
        selections: super::SelectionHistory,
        observations: Option<crate::checkpoint_store::compact::RecordObservations>,
    },
    Accepted(Box<RecoveredSnapshot>),
}

#[cfg(test)]
fn observe(batch: &LmdbWriteTransaction<'_>) -> Result<BlockHash, StoreError> {
    observe_view(&batch.read_view())
}

#[cfg(test)]
fn observe_view(batch: &burst_store_lmdb::read_view::LedgerReadView<'_>) -> Result<BlockHash, StoreError> {
    observe_view_with_poll(batch, &|_| Ok(()))
}

fn observe_view_with_poll(batch: &burst_store_lmdb::read_view::LedgerReadView<'_>, poll: &dyn Fn(&'static str) -> Result<(), StoreError>) -> Result<BlockHash, StoreError> {
    let mut digest = Blake2b::<U32>::new();
    digest.update(b"BURST/recovery-journals/v1");
    for prefix in PREFIXES {
        digest.update((prefix.len() as u64).to_be_bytes());
        digest.update(prefix);
        for row in batch.checkpoint_metadata_entries(prefix)? {
            let (key, bytes) = row?;
            poll("metadata:journal-row")?;
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
    pub(super) fn capture_view(
        batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
        installed: &RecoveredSnapshot,
        accepted: Option<RecoveredSnapshot>,
    ) -> Result<Self, StoreError> {
        Self::capture_view_with_poll(batch, installed, accepted, &|_| Ok(()))
    }

    pub(super) fn capture_view_with_poll(
        batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
        installed: &RecoveredSnapshot,
        accepted: Option<RecoveredSnapshot>,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<Self, StoreError> {
        let anchors = accepted
            .as_ref()
            .map_or(&installed.selections.authority, |state| {
                &state.selections.authority
            });
        poll("journal:capture-start")?;
        let fingerprint = observe_view_with_poll(batch, poll)?;
        validate_view_with_poll(batch, || anchors.iter(), installed.continuation.anchor(), poll)?;
        let history = match accepted {
            Some(accepted) if installed.continuation.anchor().height().checked_add(1)
                == Some(accepted.continuation.anchor().height()) =>
                JournalHistory::Accepted(Box::new(accepted)),
            Some(accepted) => {
                let (selections, observations) = accepted.into_journal_history();
                JournalHistory::Records { selections, observations: Some(observations) }
            }
            None => JournalHistory::Records {
                selections: installed.selections.fork(batch.storage_directory())?, observations: None,
            },
        };
        let sequences = sequences::SequenceGuard::capture_view_with_poll(batch, poll)?;
        poll("journal:capture-complete")?;
        Ok(Self {
            fingerprint,
            history,
            sequences,
        })
    }

    pub(super) fn selections(&self) -> &super::SelectionHistory {
        match &self.history {
            JournalHistory::Records { selections, .. } => selections,
            JournalHistory::Accepted(accepted) => &accepted.selections,
        }
    }

    pub(super) fn accepted(&self) -> Option<&RecoveredSnapshot> {
        match &self.history {
            JournalHistory::Records { .. } => None,
            JournalHistory::Accepted(accepted) => Some(accepted),
        }
    }

    pub(super) fn verify_unchanged(
        &self,
        batch: &LmdbWriteTransaction<'_>,
    ) -> Result<(), StoreError> {
        self.verify_view(&batch.read_view())
    }

    pub(super) fn verify_view(
        &self,
        batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    ) -> Result<(), StoreError> {
        self.verify_view_with_poll(batch, &|_| Ok(()))
    }

    pub(super) fn verify_view_with_poll(
        &self,
        batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        if observe_view_with_poll(batch, poll)? != self.fingerprint {
            return Err(invalid("local signer journals changed during recovery"));
        }
        self.sequences.verify_view_with_poll(batch, poll)?;
        match &self.history {
            JournalHistory::Accepted(accepted) => {
                poll("metadata:accepted-records")?;
                accepted.verify_records_view_with_poll(batch, poll)?;
            }
            JournalHistory::Records { observations: Some(records), .. } => {
                poll("metadata:accepted-records")?;
                records.verify_view_with_poll(batch, poll)?;
            }
            JournalHistory::Records { observations: None, .. } => (),
        }
        Ok(())
    }
}

// Known current authority fields are authenticated by recovery's economic
// checks. Every other row must be a closure validated against installed ancestry.
#[cfg(test)]
fn payment_epoch_namespace(batch: &LmdbWriteTransaction<'_>) -> Result<bool, StoreError> {
    payment_epoch_namespace_view(&batch.read_view())
}

#[cfg(test)]
fn payment_epoch_namespace_view(batch: &burst_store_lmdb::read_view::LedgerReadView<'_>) -> Result<bool, StoreError> {
    payment_epoch_namespace_view_with_poll(batch, &|_| Ok(()))
}

fn payment_epoch_namespace_view_with_poll(batch: &burst_store_lmdb::read_view::LedgerReadView<'_>, poll: &dyn Fn(&'static str) -> Result<(), StoreError>) -> Result<bool, StoreError> {
    let mut has_closed = false;
    for row in batch.checkpoint_metadata_entries(b"payment_epoch/")? {
        let (key, _) = row?;
        poll("journal:epoch-namespace")?;
        if key == burst_store::meta::PAYMENT_EPOCH_KEY.as_bytes()
            || key == crate::confirmed_replay::installation::CUTOFF_KEY.as_bytes()
            || key == burst_store::meta::PAYMENT_VOTE_LIMIT_KEY.as_bytes()
            || key == burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY.as_bytes()
        {
            continue;
        }
        if !key.starts_with(b"payment_epoch/v1/closed/") {
            return Err(invalid("unsupported payment epoch namespace record"));
        }
        has_closed = true;
    }
    Ok(has_closed)
}

#[cfg(test)]
fn validate<I>(
    batch: &LmdbWriteTransaction<'_>,
    authority: impl FnMut() -> I,
    installed: &CheckpointAnchor,
) -> Result<(), StoreError>
where
    I: IntoIterator<Item = Result<CheckpointAnchor, StoreError>>,
{
    validate_view(&batch.read_view(), authority, installed)
}

#[cfg(test)]
fn validate_view<I>(
    batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    authority: impl FnMut() -> I,
    installed: &CheckpointAnchor,
) -> Result<(), StoreError>
where
    I: IntoIterator<Item = Result<CheckpointAnchor, StoreError>>,
{
    validate_view_with_poll(batch, authority, installed, &|_| Ok(()))
}

fn validate_view_with_poll<I>(
    batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    mut authority: impl FnMut() -> I,
    installed: &CheckpointAnchor,
    poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
) -> Result<(), StoreError>
where
    I: IntoIterator<Item = Result<CheckpointAnchor, StoreError>>,
{
    let has_closed = payment_epoch_namespace_view_with_poll(batch, poll)?;
    poll("journal:undo-encoding")?;
    batch.verify_checkpoint_undo_encoding_with_poll(poll)?;
    poll("journal:undo-encoded")?;
    records::validate_view_with_poll(batch, authority(), installed, poll)?;
    crate::checkpoint_agreement::reconcile_recovery_journals_view_with_poll(batch, authority(), poll)?;
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
        poll("journal:namespace-row")?;
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
            poll("journal:namespace-row")?;
            let key = std::str::from_utf8(key).map_err(invalid)?;
            key.strip_prefix(&format!("{namespace}/v2/"))
                .and_then(|suffix| suffix.split_once('/'))
                .ok_or_else(|| invalid("payment promise has an unsupported version or key"))?;
            promises = promises
                .checked_add(1)
                .ok_or_else(|| invalid("payment promise count overflow"))?;
        }
    }
    let epochs = std::iter::once(Ok(genesis))
        .chain(anchors)
        .filter_map(|anchor| {
            let checked = (|| {
                let anchor = anchor?;
                poll("journal:promise-authority")?;
                if anchor.height() > installed.height() {
                    return Ok(None);
                }
                super::super::reconciliation::visit_retained_promises_view(
                    batch,
                    anchor.hash(),
                    |_, _| {
                        poll("journal:promise-row")?;
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
        let mut index = epoch_index::EpochIndex::build_with_poll(batch.storage_directory(), epochs, poll)?;
        for row in batch.checkpoint_metadata_entries(b"payment_epoch/v1/closed/")? {
            let (key, value) = row?;
            poll("journal:closed-epoch")?;
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
        index.verify_with_poll(poll)?;
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
    fn pinned_journal_view_preserves_validation_across_live_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let anchor = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
            burst_types::NetworkId::Dev, &burst_types::ProtocolParams::default()).unwrap();
        let signer = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[23; 32]).public);
        let sequence = format!("consensus_sequence/v1/{signer}");
        let mut writer = store.tx_begin_write().unwrap();
        writer.put_meta(&sequence, &12u64.to_be_bytes()).unwrap();
        writer.commit().unwrap();
        let reader = store.tx_begin_read().unwrap();
        let view = store.read_view(&reader).unwrap();
        let guard = sequences::SequenceGuard::capture_view(&view).unwrap();
        let fingerprint = observe_view(&view).unwrap();
        validate_view(&view, || [Ok(anchor.clone())], &anchor).unwrap();
        for key in ["block_undo/v2/unknown", "checkpoint_round/v2/unknown",
            "payment_epoch/v1/closed/unknown", "consensus_final/v2/unknown",
            "snapshot_attestation/v2/unknown"] {
            let mut writer = store.tx_begin_write().unwrap();
            writer.put_meta(key, b"malformed").unwrap();
            writer.commit().unwrap();
            validate_view(&view, || [Ok(anchor.clone())], &anchor).unwrap();
            assert_eq!(observe_view(&view).unwrap(), fingerprint);
            let mut writer = store.tx_begin_write().unwrap();
            assert!(validate(&writer, || [Ok(anchor.clone())], &anchor).is_err());
            assert_ne!(observe(&writer).unwrap(), fingerprint);
            writer.delete_meta(key).unwrap();
            writer.commit().unwrap();
        }
        for value in [Some(13u64), Some(11), None] {
            let mut writer = store.tx_begin_write().unwrap();
            match value {
                Some(value) => writer.put_meta(&sequence, &value.to_be_bytes()).unwrap(),
                None => writer.delete_meta(&sequence).unwrap(),
            }
            writer.commit().unwrap();
            guard.verify_view(&view).unwrap();
            let writer = store.tx_begin_write().unwrap();
            assert_eq!(guard.verify(&writer).is_ok(), value == Some(13));
        }
    }

    #[test]
    fn journal_observation_pins_all_local_governance_revision_versions() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(),32,16 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let initial = observe(&batch).unwrap();
        for version in ["v1", "v2"] {
            let key = format!("governance_revision/{version}/{}", BlockHash::new([8;32]));
            batch.put_meta(&key,&1u128.to_be_bytes()).unwrap();
            let first = observe(&batch).unwrap(); assert_ne!(first,initial);
            batch.put_meta(&key,&2u128.to_be_bytes()).unwrap();
            assert_ne!(observe(&batch).unwrap(),first);
            batch.delete_meta(&key).unwrap(); assert_eq!(observe(&batch).unwrap(),initial);
        }
        batch.put_meta("governance_revision-other",b"unrelated").unwrap();
        assert_eq!(observe(&batch).unwrap(),initial);
    }

    #[test]
    fn payment_epoch_namespace_rejects_unknown_rows_and_observes_all_changes() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let anchor = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
            burst_types::NetworkId::Dev, &burst_types::ProtocolParams::default(),
        ).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let initial = observe(&batch).unwrap();
        for key in ["payment_epoch/v2", "payment_epoch/v2/closed/test",
            "payment_epoch/v1/unknown", "payment_epoch/v1/closed", "payment_epoch/"] {
            batch.put_meta(key, b"unknown").unwrap();
            let before = batch.checkpoint_metadata(b"").unwrap();
            assert_ne!(observe(&batch).unwrap(), initial, "{key}");
            assert!(validate(&batch, || std::iter::once(Ok(anchor.clone())), &anchor).is_err(), "{key}");
            assert_eq!(batch.checkpoint_metadata(b"").unwrap(), before);
            batch.delete_meta(key).unwrap();
            validate(&batch, || std::iter::once(Ok(anchor.clone())), &anchor).unwrap();
            assert_eq!(observe(&batch).unwrap(), initial);
        }
        for key in [burst_store::meta::PAYMENT_EPOCH_KEY,
            crate::confirmed_replay::installation::CUTOFF_KEY,
            burst_store::meta::PAYMENT_VOTE_LIMIT_KEY,
            burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY] {
            // This helper classifies namespaces only; economic checks validate values.
            batch.put_meta(key, b"observed").unwrap();
            assert!(!payment_epoch_namespace(&batch).unwrap());
            assert_ne!(observe(&batch).unwrap(), initial);
            batch.delete_meta(key).unwrap();
        }
        batch.put_meta("payment_epoch/v1/closed/test", b"malformed closure").unwrap();
        assert!(payment_epoch_namespace(&batch).unwrap());
        assert!(validate(&batch, || std::iter::once(Ok(anchor.clone())), &anchor).is_err());
        batch.delete_meta("payment_epoch/v1/closed/test").unwrap();
        batch.put_meta("payment_epoch-other", b"unrelated").unwrap();
        assert_eq!(observe(&batch).unwrap(), initial);
        assert!(!payment_epoch_namespace(&batch).unwrap());
    }

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
