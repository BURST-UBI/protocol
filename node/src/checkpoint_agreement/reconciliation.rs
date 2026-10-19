use super::*;
use burst_store_lmdb::LmdbWriteTransaction;

/// Inspect local promises without replacing, unlocking or re-signing them.
/// A locally observed decision must be in the incoming authority path even if
/// its normal chain record has not yet been persisted.
/// The caller supplies an independently authenticated, contiguous authority path.
pub(crate) fn reconcile_snapshot_journals(
    batch: &LmdbWriteTransaction<'_>,
    anchors: impl IntoIterator<Item = Result<CheckpointAnchor, StoreError>>,
) -> Result<(), StoreError> {
    reconcile_journals(batch, anchors, false)
}

/// Startup preserves a validated decision at the accepted tip even when its
/// chain record was not persisted before shutdown. This does not install that
/// decision or reopen voting; the runtime must recover its replay coverage.
pub(crate) fn reconcile_recovery_journals(
    batch: &LmdbWriteTransaction<'_>,
    anchors: impl IntoIterator<Item = Result<CheckpointAnchor, StoreError>>,
) -> Result<(), StoreError> {
    reconcile_journals(batch, anchors, true)
}

fn reconcile_journals(
    batch: &LmdbWriteTransaction<'_>,
    anchors: impl IntoIterator<Item = Result<CheckpointAnchor, StoreError>>,
    retain_tip_decision: bool,
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
    let mut journals = 0u64;
    for row in batch.checkpoint_metadata_entries(prefix.as_bytes())? {
        let (key, _) = row?;
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
        journals = journals
            .checked_add(1)
            .ok_or_else(|| invalid("checkpoint journal count overflow"))?;
    }
    let mut current = genesis;
    loop {
        let mut decision = None;
        if let Some(height) = current.height().checked_add(1) {
            for representative in current.roster().representatives() {
                let key = format!("{prefix}{representative}/{height}");
                let Some(bytes) = batch.get_meta(&key)? else {
                    continue;
                };
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
            }
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
