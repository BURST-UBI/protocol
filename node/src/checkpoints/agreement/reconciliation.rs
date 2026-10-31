use super::*;
use burst_store_lmdb::LmdbWriteTransaction;

mod index;
use index::JournalIndex;

/// Inspect local promises without replacing, unlocking or re-signing them.
/// A locally observed decision must be in the incoming authority path even if
/// its normal chain record has not yet been persisted.
/// The caller supplies an independently authenticated, contiguous authority path.
pub(crate) fn reconcile_snapshot_journals(
    batch: &LmdbWriteTransaction<'_>,
    anchors: impl IntoIterator<Item = Result<CheckpointAnchor, StoreError>>,
) -> Result<(), StoreError> {
    reconcile_journals(&batch.read_view(), anchors, false, &|_| Ok(()))
}

#[cfg(test)]
pub(crate) fn reconcile_recovery_journals(
    batch: &LmdbWriteTransaction<'_>,
    anchors: impl IntoIterator<Item = Result<CheckpointAnchor, StoreError>>,
) -> Result<(), StoreError> {
    reconcile_recovery_journals_view(&batch.read_view(), anchors)
}

#[cfg(test)]
pub(crate) fn reconcile_recovery_journals_view(
    batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    anchors: impl IntoIterator<Item = Result<CheckpointAnchor, StoreError>>,
) -> Result<(), StoreError> {
    reconcile_recovery_journals_view_with_poll(batch, anchors, &|_| Ok(()))
}

/// Allow a validated local decision immediately after the supplied authority
/// path, even if its chain record is missing. This does not install the decision
/// or reopen voting; the runtime must recover its replay coverage.
pub(crate) fn reconcile_recovery_journals_view_with_poll(
    batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    anchors: impl IntoIterator<Item = Result<CheckpointAnchor, StoreError>>,
    poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    reconcile_journals(batch, anchors, true, poll)
}

fn reconcile_journals(
    batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    anchors: impl IntoIterator<Item = Result<CheckpointAnchor, StoreError>>,
    retain_tip_decision: bool,
    poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let mut anchors = anchors.into_iter();
    let genesis = anchors
        .next()
        .ok_or_else(|| invalid("empty snapshot authority path"))??;
    if genesis.height() != 0 {
        return Err(invalid("snapshot journal authority must start at genesis"));
    }
    let prefix = format!(
        "checkpoint_round/v1/{}/{}/",
        genesis.network().as_str(),
        genesis.hash()
    );
    let mut index = None;
    for row in batch.checkpoint_metadata_entries(prefix.as_bytes())? {
        let (key, _) = row?;
        poll("journal:round-index")?;
        let suffix = std::str::from_utf8(&key[prefix.len()..]).map_err(invalid)?;
        let (representative, height) = suffix
            .split_once('/')
            .ok_or_else(|| invalid("invalid checkpoint journal key"))?;
        if burst_crypto::decode_address(representative).is_none() {
            return Err(invalid("invalid checkpoint journal signer"));
        }
        let number: u64 = height.parse().map_err(invalid)?;
        if number == 0 || height != number.to_string() {
            return Err(invalid("invalid checkpoint journal height"));
        }
        if index.is_none() {
            index = Some(JournalIndex::new(batch.storage_directory())?);
        }
        index.as_mut().unwrap().push(number, WalletAddress::new(representative))?;
    }
    let mut journals = index.as_ref().map_or(0, JournalIndex::len);
    let mut current = genesis;
    loop {
        poll("journal:round-authority")?;
        let mut decision = None;
        if let (Some(index), Some(height)) = (&index, current.height().checked_add(1)) {
            index.visit(height, |representative| {
                poll("journal:round-signer")?;
                if current.roster().weight(representative) == 0 {
                    return Err(invalid("checkpoint journal signer is outside its electorate"));
                }
                let key = format!("{prefix}{representative}/{height}");
                let bytes = batch.get_meta(&key)?
                    .ok_or_else(|| invalid("indexed checkpoint journal disappeared"))?;
                journals = journals
                    .checked_sub(1)
                    .ok_or_else(|| invalid("duplicate checkpoint journal authority"))?;
                let snapshot = decode(&bytes, &current, representative)?;
                if let Some(certificate) = snapshot.state.decision() {
                    let hash = certificate.checkpoint.hash();
                    if decision.is_some_and(|previous| previous != hash) {
                        return Err(invalid(
                            "snapshot cannot preserve conflicting local decisions",
                        ));
                    }
                    decision = Some(hash);
                }
                Ok(())
            })?;
        }
        let next = anchors.next().transpose()?;
        if decision.is_some_and(|hash| match &next {
            Some(anchor) => anchor.hash() != hash,
            None => !retain_tip_decision,
        }) {
            return Err(invalid(
                "snapshot omits a locally observed checkpoint decision",
            ));
        }
        let Some(next) = next else {
            break;
        };
        if current.height().checked_add(1) != Some(next.height())
            || next.network() != current.network()
            || next.genesis_hash() != current.genesis_hash()
            || next.cutoff() <= current.cutoff()
        {
            return Err(invalid("snapshot journal authority path is not contiguous"));
        }
        current = next;
    }
    if journals != 0 {
        return Err(invalid("snapshot predates a local checkpoint journal"));
    }
    Ok(())
}
