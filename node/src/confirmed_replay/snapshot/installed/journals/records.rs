use super::*;
use bincode::Options;
use burst_types::WalletAddress;

fn number(bytes: &[u8]) -> Result<u64, StoreError> {
    Ok(u64::from_be_bytes(
        bytes
            .try_into()
            .map_err(|_| invalid("invalid signer counter encoding"))?,
    ))
}

pub(super) fn validate(
    batch: &LmdbWriteTransaction<'_>,
    anchors: impl IntoIterator<Item = Result<CheckpointAnchor, StoreError>>,
    installed: &CheckpointAnchor,
) -> Result<(), StoreError> {
    if batch
        .checkpoint_metadata_entries(b"checkpoint_vote/")?
        .next()
        .transpose()?
        .is_some()
    {
        return Err(invalid("legacy checkpoint vote requires explicit recovery"));
    }
    super::sequences::visit(batch, |_, _| Ok(()))?;
    let mut counters = 0u64;
    for namespace in ["consensus_time", "consensus_governance"] {
        let prefix = format!("{namespace}/");
        for row in batch.checkpoint_metadata_entries(prefix.as_bytes())? {
            let (key, bytes) = row?;
            let key = std::str::from_utf8(key).map_err(invalid)?;
            let value = number(bytes)?;
            let (epoch, signer) = key
                .strip_prefix(&format!("{namespace}/v1/"))
                .and_then(|suffix| suffix.split_once('/'))
                .ok_or_else(|| invalid("invalid vote counter key"))?;
            if batch
                .get_meta(&format!("consensus_sequence/v1/{signer}"))?
                .is_none()
            {
                return Err(invalid("vote counter has no durable signer sequence"));
            }
            if namespace == "consensus_governance" {
                let timestamp = batch
                    .get_meta(&format!("consensus_time/v1/{epoch}/{signer}"))?
                    .map(|bytes| number(&bytes))
                    .transpose()?;
                if timestamp.is_none_or(|at| at > value) {
                    return Err(invalid(
                        "governance promise conflicts with its timestamp high-water mark",
                    ));
                }
            }
            counters = counters
                .checked_add(1)
                .ok_or_else(|| invalid("signer journal count overflow"))?;
        }
    }
    let mut attestations = 0u64;
    for row in batch.checkpoint_metadata_entries(b"snapshot_attestation/")? {
        row?;
        attestations = attestations
            .checked_add(1)
            .ok_or_else(|| invalid("snapshot promise count overflow"))?;
    }
    let mut previous_height = None;
    for anchor in anchors {
        let anchor = anchor?;
        if anchor.network() != installed.network()
            || anchor.genesis_hash() != installed.genesis_hash()
            || previous_height.is_some_and(|height| height >= anchor.height())
        {
            return Err(invalid("journal authority is unordered or foreign"));
        }
        previous_height = Some(anchor.height());
        if anchor.height() <= installed.height() {
            for namespace in ["consensus_time", "consensus_governance"] {
                let prefix = format!("{namespace}/v1/{}/", anchor.hash());
                for row in batch.checkpoint_metadata_entries(prefix.as_bytes())? {
                    row?;
                    counters = counters
                        .checked_sub(1)
                        .ok_or_else(|| invalid("duplicate journal authority"))?;
                }
            }
        }
        let prefix = format!(
            "snapshot_attestation/v1/{}/{}/{}/",
            anchor.network().as_str(),
            anchor.genesis_hash(),
            anchor.hash()
        );
        for row in batch.checkpoint_metadata_entries(prefix.as_bytes())? {
            let (key, bytes) = row?;
            let manifest: burst_consensus::snapshot::SnapshotManifest =
                bincode::DefaultOptions::new()
                    .with_fixint_encoding()
                    .reject_trailing_bytes()
                    .with_limit(4096)
                    .deserialize(bytes)
                    .map_err(invalid)?;
            manifest.validate(&anchor).map_err(invalid)?;
            let signer = std::str::from_utf8(key)
                .map_err(invalid)?
                .strip_prefix(&prefix)
                .filter(|signer| burst_crypto::decode_address(signer).is_some())
                .ok_or_else(|| invalid("invalid snapshot promise key"))?;
            if anchor.roster().weight(&WalletAddress::new(signer)) == 0 {
                return Err(invalid("snapshot promise signer is outside its electorate"));
            }
            attestations = attestations
                .checked_sub(1)
                .ok_or_else(|| invalid("duplicate snapshot authority"))?;
        }
    }
    if counters != 0 {
        return Err(invalid(
            "vote counter belongs to an unauthenticated or uninstalled epoch",
        ));
    }
    if attestations != 0 {
        return Err(invalid("snapshot promise has no authenticated epoch"));
    }
    for row in batch.checkpoint_metadata_entries(b"consensus_final/v2/")? {
        let (key, _) = row?;
        let key = std::str::from_utf8(key).map_err(invalid)?;
        let fields: Vec<_> = key.split('/').collect();
        if fields.len() != 6
            || batch
                .get_meta(&format!("consensus_time/v1/{}/{}", fields[2], fields[3]))?
                .is_none()
        {
            return Err(invalid(
                "final promise has no durable timestamp and sequence",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signer_recovery_streams_beyond_old_inventory_limit_without_losing_change_detection() {
        let anchor = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
            burst_types::NetworkId::Dev,
            &burst_types::ProtocolParams::default(),
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 256 << 20).unwrap();
        let signer = |number: u64| {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&number.to_be_bytes());
            burst_crypto::derive_address(&burst_types::PublicKey(bytes))
        };
        let mut batch = store.tx_begin_write().unwrap();
        for number in 0..=131_072 {
            let signer = signer(number);
            batch
                .put_meta(
                    &format!("consensus_fence/v2/{}/{signer}", anchor.hash()),
                    anchor.hash().as_bytes(),
                )
                .unwrap();
            batch
                .put_meta(
                    &format!(
                        "consensus_final/v2/{}/{signer}/{signer}/{}",
                        anchor.hash(),
                        BlockHash::new([7; 32])
                    ),
                    BlockHash::new([8; 32]).as_bytes(),
                )
                .unwrap();
            batch
                .put_meta(
                    &format!("consensus_sequence/v1/{signer}"),
                    &1u64.to_be_bytes(),
                )
                .unwrap();
            batch
                .put_meta(
                    &format!("consensus_time/v1/{}/{signer}", anchor.hash()),
                    &10u64.to_be_bytes(),
                )
                .unwrap();
        }
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert!(batch.checkpoint_metadata(b"consensus_time/").is_err());
        assert!(batch.checkpoint_metadata(b"consensus_final/").is_err());
        assert!(batch.checkpoint_metadata(b"consensus_fence/").is_err());
        assert!(
            super::super::super::super::reconciliation::retained_promises(&batch, anchor.hash())
                .is_err()
        );
        let fingerprint = super::super::observe(&batch).unwrap();
        let sequences = super::super::sequences::SequenceGuard::capture(&batch).unwrap();
        super::super::validate(&batch, || std::iter::once(Ok(anchor.clone())), &anchor).unwrap();
        batch
            .put_meta("unrelated-setting", b"unchanged authority")
            .unwrap();
        assert_eq!(super::super::observe(&batch).unwrap(), fingerprint);
        batch
            .put_meta(
                &format!("consensus_sequence/v1/{}", signer(0)),
                &2u64.to_be_bytes(),
            )
            .unwrap();
        assert_eq!(super::super::observe(&batch).unwrap(), fingerprint);
        sequences.verify(&batch).unwrap();
        let sequence_key = format!("consensus_sequence/v1/{}", signer(0));
        batch.delete_meta(&sequence_key).unwrap();
        assert!(sequences.verify(&batch).is_err());
        batch.put_meta(&sequence_key, &0u64.to_be_bytes()).unwrap();
        assert!(sequences.verify(&batch).is_err());
        batch.put_meta(&sequence_key, &2u64.to_be_bytes()).unwrap();
        let promise_key = format!("consensus_final/v2/{}/{}/{}/{}",
            anchor.hash(), signer(0), signer(0), BlockHash::new([7; 32]));
        batch.put_meta(&promise_key, BlockHash::new([9; 32]).as_bytes()).unwrap();
        assert_ne!(super::super::observe(&batch).unwrap(), fingerprint);
        drop(batch);
        drop(store);
        let store = LmdbStore::open(directory.path(), 32, 256 << 20).unwrap();
        let batch = store.tx_begin_write().unwrap();
        assert_eq!(super::super::observe(&batch).unwrap(), fingerprint);
        sequences.verify(&batch).unwrap();
        super::super::validate(&batch, || std::iter::once(Ok(anchor.clone())), &anchor).unwrap();
    }

    #[test]
    fn streamed_journal_authority_preserves_counters_and_attestations() {
        let anchor = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
            burst_types::NetworkId::Dev,
            &burst_types::ProtocolParams::default(),
        )
        .unwrap();
        let signer =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[0; 32]).public);
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_meta(
                &format!("consensus_sequence/v1/{signer}"),
                &1u64.to_be_bytes(),
            )
            .unwrap();
        for namespace in ["consensus_time", "consensus_governance"] {
            batch
                .put_meta(
                    &format!("{namespace}/v1/{}/{signer}", anchor.hash()),
                    &10u64.to_be_bytes(),
                )
                .unwrap();
        }
        let manifest =
            burst_consensus::snapshot::SnapshotManifest::new(&anchor, b"fixture").unwrap();
        batch
            .put_meta(
                &format!(
                    "snapshot_attestation/v1/dev/{}/{}/{signer}",
                    anchor.genesis_hash(),
                    anchor.hash()
                ),
                &bincode::serialize(&manifest).unwrap(),
            )
            .unwrap();
        let before = batch.checkpoint_metadata(b"").unwrap();
        validate(&batch, std::iter::once(Ok(anchor.clone())), &anchor).unwrap();
        assert!(validate(&batch, [Ok(anchor.clone()), Ok(anchor.clone())], &anchor).is_err());
        assert!(validate(&batch, std::iter::empty(), &anchor).is_err());
        assert!(validate(
            &batch,
            [Ok(anchor.clone()), Err(invalid("authority unavailable"))],
            &anchor
        )
        .unwrap_err()
        .to_string()
        .contains("authority unavailable"));
        assert_eq!(batch.checkpoint_metadata(b"").unwrap(), before);
    }
}
