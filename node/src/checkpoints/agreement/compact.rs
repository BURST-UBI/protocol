use super::*;
use crate::confirmed_replay::snapshot::installed::RecoveredSnapshot;
use crate::confirmed_replay::snapshot::CompactSelectionCoverage;
use crate::connection_registry::ConnectionRegistry;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Recover saved selections using authenticated peer proofs. Capture the journal
/// before retrieval and reject changes, including creation of an absent journal,
/// under the final writer lock. Recovery leaves the ledger unchanged; returned
/// coverage still needs the atomic signing transition.
pub async fn recover_compact_selections_from_peers(
    store: Arc<LmdbStore>,
    registry: Arc<RwLock<ConnectionRegistry>>,
    recovered: RecoveredSnapshot,
    representative: WalletAddress,
) -> Result<(RecoveredSnapshot, Vec<CompactSelectionCoverage>), StoreError> {
    let (base, candidates) =
        recover_compact_candidates_inner(store, registry, recovered, representative, true).await?;
    let candidates = candidates
        .into_iter()
        .map(|candidate| match candidate {
            CompactReplayCandidate::Selection(value) => Ok(*value),
            _ => Err(invalid(
                "selection recovery encountered a different event kind",
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((base, candidates))
}

pub async fn recover_compact_candidates_from_peers(
    store: Arc<LmdbStore>,
    registry: Arc<RwLock<ConnectionRegistry>>,
    recovered: RecoveredSnapshot,
    representative: WalletAddress,
) -> Result<(RecoveredSnapshot, Vec<CompactReplayCandidate>), StoreError> {
    recover_compact_candidates_inner(store, registry, recovered, representative, false).await
}

async fn recover_compact_candidates_inner(
    store: Arc<LmdbStore>,
    registry: Arc<RwLock<ConnectionRegistry>>,
    recovered: RecoveredSnapshot,
    representative: WalletAddress,
    selection_only: bool,
) -> Result<(RecoveredSnapshot, Vec<CompactReplayCandidate>), StoreError> {
    let recovered = Arc::new(recovered);
    let candidates = recover_compact_candidates_shared_inner(
        store,
        registry,
        recovered.clone(),
        representative,
        selection_only,
    )
    .await?;
    let recovered = Arc::try_unwrap(recovered)
        .map_err(|_| invalid("compact journal recovery retained its base"))?;
    Ok((recovered, candidates))
}

pub(crate) async fn recover_compact_candidates_shared(
    store: Arc<LmdbStore>,
    registry: Arc<RwLock<ConnectionRegistry>>,
    recovered: Arc<RecoveredSnapshot>,
    representative: WalletAddress,
) -> Result<Vec<CompactReplayCandidate>, StoreError> {
    recover_compact_candidates_shared_inner(store, registry, recovered, representative, false).await
}

async fn recover_compact_candidates_shared_inner(
    store: Arc<LmdbStore>,
    registry: Arc<RwLock<ConnectionRegistry>>,
    recovered: Arc<RecoveredSnapshot>,
    representative: WalletAddress,
    selection_only: bool,
) -> Result<Vec<CompactReplayCandidate>, StoreError> {
    let source = Arc::clone(&store);
    let (mut recovered, key, bytes, manifests, decision) = tokio::task::spawn_blocking(move || {
        let anchor = recovered.continuation.anchor();
        let key = state_key(anchor, &representative)?;
        let bytes = match source.meta_store().get_meta(&key) {
            Ok(bytes) => Some(bytes),
            Err(StoreError::NotFound(_)) => None,
            Err(error) => return Err(error),
        };
        let (manifests, decision) = match &bytes {
            Some(bytes) => {
                let snapshot = decode(bytes, anchor, &representative)?;
                let decision = snapshot.state.decision().cloned();
                (
                    snapshot
                        .manifests
                        .ok_or_else(|| invalid("checkpoint signer has no replay manifests"))?,
                    decision,
                )
            }
            None => (Vec::new(), None),
        };
        if manifests.len() > 7 {
            return Err(invalid("too many retained compact candidates"));
        }
        // Do not partially recover a mixed journal and silently omit its lock.
        for manifest in &manifests {
            match manifest {
                ReplayManifest::Selection(_) => (),
                ReplayManifest::Deadline(_) if !selection_only => (),
                ReplayManifest::ChallengeOutcome(_) if !selection_only => (),
                ReplayManifest::Roster(_) if !selection_only => (),
                ReplayManifest::Activation(_) if !selection_only => (),
                ReplayManifest::Governance(_) if !selection_only => (),
                _ => {
                    return Err(invalid(
                        "compact recovery requires additional event support",
                    ))
                }
            }
        }
        Ok((recovered, key, bytes, manifests, decision))
    })
    .await
    .map_err(invalid)??;
    let mut candidates = Vec::with_capacity(manifests.len());
    for manifest in manifests {
        let (next, candidate) = recovered
            .candidate_coverage_recovery_shared(
                Arc::clone(&store),
                Arc::clone(&registry),
                manifest,
                decision.as_ref(),
            )
            .await?;
        recovered = next;
        candidates.push(candidate);
    }
    tokio::task::spawn_blocking(move || {
        let batch = store.tx_begin_write()?;
        if batch.get_meta(&key)? != bytes {
            return Err(invalid("checkpoint signer journal changed during recovery"));
        }
        recovered.verify_records_unchanged(&batch)?;
        // Earlier candidates may have become stale while a later one fetched
        // proofs. Recheck the entire set against the same final writer view.
        for candidate in &candidates {
            if decision.is_none() {
                candidate
                    .input()
                    .check_signing_parent(&batch, recovered.continuation.anchor())?;
            } else {
                candidate
                    .input()
                    .validate_parent(recovered.continuation.anchor())?;
            }
        }
        drop(batch);
        Ok(candidates)
    })
    .await
    .map_err(invalid)?
}

/// Compact selections use the existing durable round journal and locking rules.
/// Coverage, installed authority and finality are rechecked inside that writer
/// before any signature is returned. This does not persist a chain decision or
/// install economic state. Live worker integration remains release-gated.
pub fn apply_compact_selections(
    store: &LmdbStore,
    anchor: &CheckpointAnchor,
    private_key: &PrivateKey,
    command: Command<'_>,
    candidates: &[&CompactSelectionCoverage],
    clock: Option<SigningClock>,
) -> Result<Vec<Outgoing>, StoreError> {
    apply_compact_selections_observed(
        store,
        anchor,
        private_key,
        command,
        candidates,
        clock.map(ObservedSigningClock::new),
    )
}

pub(crate) fn apply_compact_selections_observed(
    store: &LmdbStore,
    anchor: &CheckpointAnchor,
    private_key: &PrivateKey,
    command: Command<'_>,
    candidates: &[&CompactSelectionCoverage],
    clock: Option<ObservedSigningClock>,
) -> Result<Vec<Outgoing>, StoreError> {
    let candidates = candidates
        .iter()
        .map(|value| CompactReplayInput::Selection(value))
        .collect::<Vec<_>>();
    apply_compact_candidates_observed(store, anchor, private_key, command, &candidates, clock)
}

pub fn apply_compact_candidates(
    store: &LmdbStore,
    anchor: &CheckpointAnchor,
    private_key: &PrivateKey,
    command: Command<'_>,
    candidates: &[CompactReplayInput<'_>],
    clock: Option<SigningClock>,
) -> Result<Vec<Outgoing>, StoreError> {
    apply_compact_candidates_observed(
        store,
        anchor,
        private_key,
        command,
        candidates,
        clock.map(ObservedSigningClock::new),
    )
}

pub(crate) fn apply_compact_candidates_observed(
    store: &LmdbStore,
    anchor: &CheckpointAnchor,
    private_key: &PrivateKey,
    command: Command<'_>,
    candidates: &[CompactReplayInput<'_>],
    clock: Option<ObservedSigningClock>,
) -> Result<Vec<Outgoing>, StoreError> {
    if !cfg!(test) && !burst_verification::checkpoint_selection_enabled(anchor.network()) {
        return Err(invalid(
            "compact checkpoint signing is behind the release gate",
        ));
    }
    if candidates.is_empty() || candidates.len() > 7 {
        return Err(invalid(
            "checkpoint transition needs one to seven replay candidates",
        ));
    }
    let mut manifests = Vec::new();
    for candidate in candidates {
        candidate.validate_parent(anchor)?;
        let manifest = candidate.manifest();
        if manifests.contains(&manifest) {
            return Err(invalid("duplicate checkpoint replay candidate"));
        }
        manifests.push(manifest);
    }
    transition(
        store,
        anchor,
        private_key,
        command,
        Some(manifests),
        |input| {
            let candidate = candidates
                .iter()
                .find(|candidate| candidate.checkpoint() == input.checkpoint)
                .ok_or_else(|| invalid("checkpoint commitments differ from compact replay"))?;
            candidate.validate(anchor, input, clock.and_then(ObservedSigningClock::current))
        },
        |batch, state| {
            let representative = derive_address(&public_from_private(private_key));
            if state.decision().is_some() {
                if batch.payment_epoch()? == anchor.hash() {
                    batch.seal_consensus_after_certificate(&representative, &anchor.hash())?;
                }
                return Ok(());
            }
            // Timeout-only rounds can have no retained value. They still must
            // belong to the installed and accepted parent before signing.
            candidates[0].check_signing_parent(batch, anchor)?;
            let retained = state.retained_values();
            for candidate in candidates {
                if retained.contains(&candidate.checkpoint().hash()) {
                    candidate.seal_local_votes(batch, anchor, &representative)?;
                }
            }
            Ok(())
        },
    )
}
