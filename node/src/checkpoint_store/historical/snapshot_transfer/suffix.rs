use super::*;
use crate::confirmed_replay::snapshot::{
    history::HistoryCursor,
    tail::{TransferredTail, MAX_TAIL_BLOCKS, MAX_TAIL_BYTES},
};

pub(super) async fn fetch(
    store: &Arc<LmdbStore>,
    registry: &Arc<RwLock<ConnectionRegistry>>,
    recovered: &installed::RecoveredSnapshot,
    certificate: &CheckpointCertificate,
    manifest: &crate::checkpoint_agreement::ReplayManifest,
) -> Result<TransferredTail, StoreError> {
    if certificate.checkpoint != *manifest.checkpoint() {
        return Err(invalid("suffix manifest differs from certificate"));
    }
    let cursors = HistoryCursor::certified_cut(
        recovered.continuation.anchor(),
        certificate,
        manifest.heads(),
    )
    .map_err(invalid)?;
    let mut requests = Vec::new();
    let mut single_heads = Vec::new();
    let mut count = 0u64;
    for cursor in cursors {
        let head = cursor
            .next()
            .ok_or_else(|| invalid("empty certified suffix head"))?;
        let account = recovered.continuation.state().account(&head.account);
        let floor = account.map_or(0, |account| account.block_count);
        let length = head
            .height
            .checked_sub(floor)
            .ok_or_else(|| invalid("certified suffix rolls back parent account"))?;
        count = count
            .checked_add(length)
            .filter(|count| *count <= MAX_TAIL_BLOCKS as u64)
            .ok_or_else(|| invalid("certified suffix exceeds block budget"))?;
        if length == 0 {
            if account.is_none_or(|account| account.head != head.hash) {
                return Err(invalid("certified suffix changes an unchanged account"));
            }
        } else if length == 1 {
            single_heads.push(cursor);
        } else {
            requests.push((cursor, floor));
        }
    }
    let mut tail = TransferredTail::new(store.env().path())?;
    let mut used = 0usize;
    tracing::debug!(single_heads = single_heads.len(), long_suffixes = requests.len(),
        "checkpoint suffix transfer planned");
    for batch in single_heads.chunks(crate::checkpoint_sync::witness::MAX_EXACT_HEADS) {
        let proofs = crate::checkpoint_sync::witness::fetch_exact_heads(registry, batch.to_vec())
            .await?;
        let incoming = proofs.into_iter().map(|proof| proof.block().clone()).collect();
        (tail, used) = append(tail, used, incoming).await?;
    }
    for (mut cursor, floor) in requests {
        while cursor.next().is_some_and(|head| head.height > floor) {
            let incoming =
                crate::checkpoint_sync::witness::fetch_suffix(registry, &mut cursor, floor).await?;
            (tail, used) = append(tail, used, incoming).await?;
        }
    }
    let store = store.clone();
    #[cfg(test)]
    if count > 0 {
        let probe = registry.read().await.recovery_probe;
        if let Some(probe) = probe {
            probe("checkpoint_suffix_received");
        }
    }
    let (mut tail, missing) = tokio::task::spawn_blocking(move || {
        let read = store.tx_begin_read()?;
        let mut contents = Vec::new();
        let mut missing = Vec::new();
        for block in tail.blocks() {
            let block = block?;
            if !crate::governance_content::is_full(&block) {
                continue;
            }
            let manifest = crate::governance_content::authenticate(&block).map_err(invalid)?;
            used = used
                .checked_add(manifest.length as usize)
                .filter(|used| *used <= MAX_TAIL_BYTES)
                .ok_or_else(|| invalid("certified suffix content exceeds byte budget"))?;
            let bytes = match store
                .meta_store()
                .get_meta_txn(&read, &crate::governance_content::key(block.hash))?
            {
                Some(bytes) => Some(bytes),
                None => store.meta_store().get_meta_txn(
                    &read,
                    &crate::confirmed_history::proposal_archive_key(block.hash),
                )?,
            };
            if let Some(bytes) = bytes {
                manifest.decode_verified(&bytes).map_err(invalid)?;
                tracing::debug!(block = %block.hash, "recovery proposal content already stored");
                contents.push(crate::confirmed_history::ProposalWitness {
                    block: block.hash,
                    bytes,
                });
            } else {
                missing.push(block);
            }
        }
        tail.contents = contents;
        Ok::<_, StoreError>((tail, missing))
    })
    .await
    .map_err(invalid)??;
    if !missing.is_empty() {
        let transfers = registry
            .read()
            .await
            .recovery_content()
            .ok_or_else(|| invalid("recovery proposal content service is unavailable"))?;
        for block in missing {
            let bytes = transfers.fetch(&block).await.map_err(invalid)?;
            tail.contents
                .push(crate::confirmed_history::ProposalWitness {
                    block: block.hash,
                    bytes: bytes.as_ref().clone(),
                });
        }
    }
    #[cfg(test)]
    if !tail.contents.is_empty() {
        let probe = registry.read().await.recovery_probe;
        if let Some(probe) = probe {
            probe("checkpoint_content_received");
        }
    }
    Ok(tail)
}

async fn append(
    mut tail: TransferredTail,
    mut used: usize,
    incoming: Vec<burst_ledger::StateBlock>,
) -> Result<(TransferredTail, usize), StoreError> {
    tokio::task::spawn_blocking(move || {
        for block in incoming {
            used = used
                .checked_add(bincode::serialized_size(&block).map_err(invalid)? as usize)
                .filter(|used| *used <= MAX_TAIL_BYTES)
                .ok_or_else(|| invalid("certified suffix exceeds byte budget"))?;
            tail.insert(&block)?;
        }
        Ok((tail, used))
    })
    .await
    .map_err(invalid)?
}
