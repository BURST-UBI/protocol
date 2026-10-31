//! Publish certified committees only after their orchestrator snapshot commits.

use bincode::Options;
use burst_store::StoreError;
use burst_store_lmdb::{block_undo::UndoTable, LmdbStore, LmdbWriteTransaction};
use burst_types::ProtocolParams;
use burst_verification::{bootstrap::BootstrapPhase, VerificationEvent, VerificationOrchestrator};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::checkpoint_store::CertifiedCheckpoint;

fn invalid(error: impl ToString) -> StoreError {
    StoreError::Corruption(error.to_string())
}

/// `None` defers a superseded panel while a new lifecycle awaits certification.
/// Check under the installation lock so admission cannot race the decision.
pub(crate) async fn install(
    store: Arc<LmdbStore>,
    live: Arc<Mutex<VerificationOrchestrator>>,
    checkpoint: CertifiedCheckpoint,
    params: ProtocolParams,
) -> Result<Option<CertifiedCheckpoint>, StoreError> {
    let mut live = live.lock_owned().await;
    let active = live
        .active_challenge_context(checkpoint.replayed().request().target())
        .map_err(invalid)?;
    if active.is_some_and(|source| {
        checkpoint.replayed().lifecycle()
            != crate::checkpoint_replay::SelectionLifecycle::Challenge(source)
    }) {
        return Ok(None);
    }
    // This is the signed lifecycle start, not the local clock. A fresh
    // endorsement after settlement can precede its next selection certificate.
    if live
        .get_state(checkpoint.replayed().request().target())
        .is_some_and(|state| {
            state.started_at.as_secs() > checkpoint.certificate().checkpoint.cutoff
        })
    {
        return Ok(None);
    }
    tokio::task::spawn_blocking(move || {
        let mut batch = store.tx_begin_write()?;
        if !checkpoint.publication_epoch_ready(&batch, &params)? {
            return Ok(None);
        }
        if let Some(candidate) = stage(&mut batch, &checkpoint, &params, &live)? {
            batch.commit()?;
            *live = candidate;
        }
        Ok(Some(checkpoint))
    })
    .await
    .map_err(invalid)?
}

/// The caller holds the live orchestrator lock and publishes the returned copy
/// only after committing this transaction. No economic effects run here.
pub(crate) fn stage(
    batch: &mut LmdbWriteTransaction<'_>,
    checkpoint: &CertifiedCheckpoint,
    initial_params: &ProtocolParams,
    live: &VerificationOrchestrator,
) -> Result<Option<VerificationOrchestrator>, StoreError> {
    let archive = checkpoint.archive_selection(batch, initial_params)?;
    let parameter_hash = if let Some(archive) = &archive {
        archive.parameter_hash
    } else {
        checkpoint.require_persisted(batch)?;
        checkpoint.live_parameter_hash(batch)?
    };
    if parameter_hash.is_none() {
        return Ok(None);
    }
    let replay = checkpoint.replayed();
    let request = replay.request();
    let context = request
        .vote_context(&checkpoint.certificate().checkpoint)
        .map_err(invalid)?;
    let state = live
        .get_state(request.target())
        .ok_or_else(|| invalid("certified target is missing from the live orchestrator"))?;
    let installed = live.selection_context(request.target()) == Some(context);
    if installed || state.revote_count > request.attempt() {
        let committee = request
            .select(
                checkpoint.parent(),
                checkpoint.certificate(),
                checkpoint
                    .beacon()
                    .ok_or_else(|| invalid("installed committee has no persisted beacon"))?,
            )
            .map_err(invalid)?;
        if installed && state.selected_verifiers != committee {
            return Err(invalid(
                "installed committee differs from its certified selection",
            ));
        }
        let completed_deadline = if !installed {
            if let Some(bytes) = batch.get_meta(&crate::verification_deadline::key(context))? {
                let completion = crate::verification_deadline::decode(&bytes).map_err(invalid)?;
                if completion.activation.target != *request.target()
                    || completion.activation.context != context
                    || completion.activation.selection != checkpoint.certificate().checkpoint.hash()
                    || completion
                        .absent
                        .iter()
                        .any(|voter| !committee.contains(voter))
                {
                    return Err(invalid(
                        "installed deadline differs from its certified panel",
                    ));
                }
                let signed: Vec<_> = committee
                    .iter()
                    .filter(|voter| !completion.absent.contains(voter))
                    .cloned()
                    .collect();
                signed.is_empty() || live.completed_selection(request.target(), context, &signed)
            } else {
                false
            }
        } else {
            false
        };
        if !installed
            && !completed_deadline
            && !live.completed_selection(request.target(), context, &committee)
        {
            return Err(invalid(
                "earlier certified panel has no complete signed voting history",
            ));
        }
        return Ok(None);
    }
    if !replay.bootstrap_complete()
        || crate::bootstrap_phase::read(batch).map_err(invalid)? != BootstrapPhase::Complete
    {
        return Err(invalid("committee installation requires bootstrap exit"));
    }
    let params = match batch.undo_value(UndoTable::BrnMeta, b"protocol_params")? {
        Some(bytes) => bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(64 * 1024)
            .reject_trailing_bytes()
            .deserialize::<ProtocolParams>(&bytes)
            .map_err(invalid)?,
        None => initial_params.clone(),
    };
    if parameter_hash != Some(params.params_hash()) {
        return Err(invalid(
            "committee rules differ from the installed certified epoch",
        ));
    }
    let heads = archive
        .as_ref()
        .map_or(checkpoint.heads(), |archive| archive.heads.as_slice());
    for head in heads {
        if batch.get_account(&head.account)?.confirmation_height < head.height
            || batch.block_at_height(&head.account, head.height)? != Some(head.hash)
        {
            return Err(invalid(
                "certified committee history is not locally confirmed",
            ));
        }
    }
    let phase = if request.attempt() == 0 {
        match replay.lifecycle() {
            crate::checkpoint_replay::SelectionLifecycle::Verification(_) => {
                burst_verification::state::VerificationPhase::Endorsing
            }
            crate::checkpoint_replay::SelectionLifecycle::Challenge(_) => {
                burst_verification::state::VerificationPhase::Challenged
            }
        }
    } else {
        burst_verification::state::VerificationPhase::Voting
    };
    if state.phase != phase
        || state.revote_count != request.attempt()
        || !state.selected_verifiers.is_empty()
        || !state.votes.is_empty()
    {
        return Err(invalid(
            "live target is not awaiting this certified committee",
        ));
    }
    if crate::verification_deadline::live_history_commitment(
        batch,
        live,
        request.target(),
        replay.lifecycle(),
    )
    .map_err(invalid)?
        != replay.selection_history_commitment()
    {
        return Err(invalid(
            "live selection history differs from the certified cut",
        ));
    }
    let mut candidate = live.clone();
    if !candidate.drain_events().is_empty() {
        return Err(invalid(
            "committee installation found uncommitted lifecycle events",
        ));
    }
    let frozen = live.selection_request(request.target()) == Some(request);
    if frozen && checkpoint.beacon().is_none() {
        return Ok(None);
    }
    candidate
        .freeze_replayed_selection(request, &params)
        .map_err(invalid)?;
    if let Some(beacon) = checkpoint.beacon() {
        candidate
            .install_selection(
                request.target(),
                checkpoint.parent(),
                checkpoint.certificate(),
                beacon,
            )
            .map_err(invalid)?;
        let events = candidate.drain_events();
        if events.len() != 1
            || !matches!(&events[0],
            VerificationEvent::VerifiersSelected { wallet, .. } if wallet == request.target())
        {
            return Err(invalid(
                "unexpected certified committee installation effects",
            ));
        }
    }
    batch.put_meta(
        crate::endorsement_commit::SNAPSHOT_KEY,
        &candidate.snapshot().encode().map_err(invalid)?,
    )?;
    Ok(Some(candidate))
}
