use super::*;
use burst_store_lmdb::meta::{ConsensusVoteReservation, LmdbMetaStore};

// A negative scheduling hint only. Ready candidates still pass the complete
// body, causal, governance and reservation checks below in fresh transactions.
fn indexed_pending_predecessor(
    store: &LmdbStore,
    snapshot: &burst_store_lmdb::LmdbReadTransaction<'_>,
    hash: &BlockHash,
) -> Result<Option<BlockHash>, StoreError> {
    let blocks = store.block_store();
    let Some(position) = blocks.indexed_position_txn(snapshot, hash)? else {
        return Ok(None);
    };
    let account = store
        .account_store()
        .get_account_txn(snapshot, &position.account)?;
    if account.address != position.account
        || position.height > account.block_count
        || account.confirmation_height > account.block_count
    {
        return Err(StoreError::Corruption(
            "final vote position/account mismatch".into(),
        ));
    }
    if position.height - 1 <= account.confirmation_height {
        return Ok(None);
    }
    blocks
        .block_at_height_txn(snapshot, &position.account, position.height - 1)?
        .map(Some)
        .ok_or_else(|| StoreError::Corruption("final vote predecessor index missing".into()))
}

/// Reserve ready votes together before signing; preserve one result per input.
/// Dependencies can defer individual votes. Cached votes, duplicates or a batch
/// error fall back to individual attempts in input order. A sync error may leave
/// reservations committed, but the failed batch returns no signatures.
pub(crate) fn generate_final_votes(
    store: &LmdbStore,
    generator: &mut VoteGenerator,
    hashes: &[BlockHash],
) -> Vec<Result<GeneratedVote, VoteGenerationError>> {
    if hashes.len() > LmdbMetaStore::MAX_CONSENSUS_RESERVATIONS {
        return hashes
            .iter()
            .map(|_| {
                Err(StoreError::Backend("final vote group exceeds reservation limit".into()).into())
            })
            .collect();
    }
    let started = std::time::Instant::now();
    match fresh_group(store, generator, hashes) {
        Ok(Some(votes)) => {
            let count = votes.iter().filter(|vote| vote.is_ok()).count();
            if count > 0 {
                tracing::debug!(
                    count,
                    deferred = votes.len() - count,
                    elapsed_us = started.elapsed().as_micros(),
                    "final vote reservation group committed"
                );
            } else {
                tracing::debug!(
                    deferred = votes.len(),
                    elapsed_us = started.elapsed().as_micros(),
                    "final vote group waiting for dependencies"
                );
            }
            votes
        }
        outcome => {
            let cached_or_duplicate = outcome.is_ok();
            let votes = hashes
                .iter()
                .map(|&hash| generate_vote(store, generator, hash, true))
                .collect();
            tracing::debug!(
                count = hashes.len(),
                cached_or_duplicate,
                elapsed_us = started.elapsed().as_micros(),
                "final vote group used singleton fallback"
            );
            votes
        }
    }
}

fn fresh_group(
    store: &LmdbStore,
    generator: &mut VoteGenerator,
    hashes: &[BlockHash],
) -> Result<Option<Vec<Result<GeneratedVote, VoteGenerationError>>>, VoteGenerationError> {
    let mut seen = std::collections::HashSet::new();
    // Keep cached responses in singleton order: a later fresh governance
    // promise must not precede validation of an earlier cached response.
    if hashes
        .iter()
        .any(|hash| !seen.insert(*hash) || generator.cached_final_response(hash).is_some())
    {
        return Ok(None);
    }
    if hashes.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let stage_started =
        std::env::var_os("BURST_FINALITY_STAGE_LOG").map(|_| std::time::Instant::now());
    let stage = |phase: &str| {
        if let Some(started) = stage_started {
            tracing::debug!(voter = %generator.representative, hash = %hashes[0],
                count = hashes.len(), phase, elapsed_us = started.elapsed().as_micros(),
                "final vote batch stage");
        }
    };
    stage("loading_candidates");
    let mut indexed_pending = std::collections::HashMap::new();
    let mut blocks = Vec::with_capacity(hashes.len());
    {
        let snapshot = store.tx_begin_read()?;
        for hash in hashes {
            if let Some(previous) = indexed_pending_predecessor(store, &snapshot, hash)? {
                indexed_pending.insert(*hash, previous);
            }
        }
    }
    // Candidate loading owns its read transactions; do not nest them inside the
    // index probe's thread-local LMDB reader slot.
    for hash in hashes {
        if !indexed_pending.contains_key(hash) {
            blocks.push(vote_candidate(store, *hash)?);
        }
    }
    let mut candidates = Vec::with_capacity(blocks.len());
    let mut collect = |block: StateBlock, result: Result<Option<u64>, VoteGenerationError>| {
        match result {
            Ok(limit) => candidates.push(Ok((block, limit))),
            Err(error @ VoteGenerationError::DependenciesPending(_)) => candidates.push(Err(error)),
            Err(error) => return Err(error),
        }
        Ok(())
    };
    if blocks.iter().any(needs_governance_readiness) {
        // Governance readiness requires the writer-backed schedule view.
        stage("waiting_for_readiness_writer");
        let batch = store.tx_begin_write()?;
        stage("readiness_writer_acquired");
        for block in blocks {
            let result = final_vote_limit_in(&batch, &block);
            collect(block, result)?;
        }
    } else {
        // Payments use a single committed snapshot without queuing for a writer.
        // Drop it before the durable reservation, which rechecks authority fences.
        stage("opening_readiness_snapshot");
        let snapshot = crate::verification_commit::dependencies::view::Snapshot::new(store)?;
        stage("readiness_snapshot_acquired");
        for block in blocks {
            let result = check_final_dependencies(&snapshot, &block).map(|()| None);
            collect(block, result)?;
        }
    }
    stage("readiness_finished");
    let requests: Vec<_> = candidates
        .iter()
        .filter_map(|candidate| candidate.as_ref().ok())
        .map(|(block, limit)| ConsensusVoteReservation {
            payment_epoch: &block.payment_epoch,
            representative: &generator.representative,
            account: &block.account,
            previous: &block.previous,
            hash: &block.hash,
            is_final: true,
            sequence_floor: generator.sequence(),
            timestamp: Some(block.timestamp.as_secs()),
            governance_limit: *limit,
        })
        .collect();
    // Dependency checks reserve nothing. Keep their errors in input order; only
    // ready candidates consume sequence numbers in the shared durable cut.
    let mut sequences = if requests.is_empty() {
        Vec::new()
    } else {
        store.meta_store().reserve_consensus_votes(&requests)?
    }
    .into_iter();
    stage("reservation_finished");
    let mut votes = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let (block, limit) = match candidate {
            Ok(candidate) => candidate,
            Err(error) => {
                votes.push(Err(error));
                continue;
            }
        };
        let sequence = sequences
            .next()
            .expect("one reserved sequence per ready candidate");
        generator.advance_sequence_to(sequence - 1);
        let vote = generator.generate_final_vote(block.hash);
        if limit.is_none() {
            generator.remember_final_response(vote.clone());
        }
        generator.remember_response_choice(block.account, block.previous, &vote);
        votes.push(Ok(vote));
    }
    let mut votes = votes.into_iter();
    Ok(Some(
        hashes
            .iter()
            .map(|hash| match indexed_pending.get(hash) {
                Some(previous) => Err(VoteGenerationError::DependenciesPending(vec![*previous])),
                None => votes
                    .next()
                    .expect("one result per fully checked candidate"),
            })
            .collect(),
    ))
}

/// Reserve fresh canonical soft responses together. Otherwise, respond to each
/// input individually in order; failures remain specific to each result.
pub(crate) fn respond_votes(
    store: &LmdbStore,
    generator: &mut VoteGenerator,
    hashes: &[BlockHash],
) -> Vec<Result<Option<GeneratedVote>, StoreError>> {
    if hashes.len() > LmdbMetaStore::MAX_CONSENSUS_RESERVATIONS {
        return hashes
            .iter()
            .map(|_| {
                Err(StoreError::Backend(
                    "vote response group exceeds reservation limit".into(),
                ))
            })
            .collect();
    }
    if hashes.len() > 1 {
        if let Ok(Some(votes)) = fresh_soft_group(store, generator, hashes) {
            return votes.into_iter().map(|vote| Ok(Some(vote))).collect();
        }
    }
    hashes
        .iter()
        .map(|&hash| respond_vote(store, generator, hash))
        .collect()
}

fn fresh_soft_group(
    store: &LmdbStore,
    generator: &mut VoteGenerator,
    hashes: &[BlockHash],
) -> Result<Option<Vec<GeneratedVote>>, StoreError> {
    if !generator.is_representative || hashes.is_empty() {
        return Ok(None);
    }
    let started = std::time::Instant::now();
    let mut seen = std::collections::HashSet::new();
    let mut blocks = Vec::with_capacity(hashes.len());
    {
        let snapshot = store.tx_begin_read()?;
        for &hash in hashes {
            if !seen.insert(hash) || generator.cached_final_response(&hash).is_some() {
                return Ok(None);
            }
            let bytes = store.block_store().get_block_txn(&snapshot, &hash)?;
            let block: StateBlock = bincode::deserialize(&bytes)
                .map_err(|error| StoreError::Serialization(error.to_string()))?;
            if block.hash != hash || block.compute_hash() != hash {
                return Err(StoreError::Corruption(
                    "vote candidate hash mismatch".into(),
                ));
            }
            if needs_governance_readiness(&block)
                || generator
                    .cached_soft_response(&block.account, block.previous, hash)
                    .is_some()
            {
                return Ok(None);
            }
            let account = store
                .account_store()
                .get_account_txn(&snapshot, &block.account)?;
            let Some(height) = store.block_store().height_of_block_txn(&snapshot, &hash)? else {
                return Ok(None);
            };
            if height <= account.confirmation_height
                || store
                    .block_store()
                    .block_at_height_txn(&snapshot, &block.account, height)?
                    != Some(hash)
            {
                return Ok(None);
            }
            blocks.push(block);
        }
    }
    let requests: Vec<_> = blocks
        .iter()
        .map(|block| ConsensusVoteReservation {
            payment_epoch: &block.payment_epoch,
            representative: &generator.representative,
            account: &block.account,
            previous: &block.previous,
            hash: &block.hash,
            is_final: false,
            sequence_floor: generator.sequence(),
            timestamp: Some(block.timestamp.as_secs()),
            governance_limit: None,
        })
        .collect();
    // Every current epoch/fence/deadline/choice guard runs in the shared writer.
    // No signer state is touched and no signature is released before commit+sync.
    let sequences = store.meta_store().reserve_consensus_votes(&requests)?;
    let mut votes = Vec::with_capacity(blocks.len());
    for (block, sequence) in blocks.into_iter().zip(sequences) {
        generator.advance_sequence_to(sequence - 1);
        let vote = generator.generate_vote(block.hash);
        generator.remember_response_choice(block.account, block.previous, &vote);
        votes.push(vote);
    }
    tracing::debug!(
        count = votes.len(),
        elapsed_us = started.elapsed().as_micros(),
        "soft vote reservation group committed"
    );
    Ok(Some(votes))
}

#[cfg(test)]
mod tests;
