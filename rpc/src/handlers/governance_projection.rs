use super::*;
use burst_governance::{DelegationEngine, GovernanceEngine, GovernanceTiming, Proposal};

pub(super) struct ProjectedProposal {
    pub proposal: Proposal,
    pub phase_projected: bool,
}

pub(super) struct Page {
    pub proposals: Vec<ProjectedProposal>,
    pub next_hash: Option<TxHash>,
}

/// Project scheduled phases for display without changing governance state.
/// The committed reader selects proposals before loading any needed electorate.
/// Without a reader, use the live engines; those inputs are not one storage view.
pub(super) async fn page(
    state: &RpcState,
    after: Option<TxHash>,
    offset: u64,
    count: u32,
) -> Result<Option<Page>, RpcError> {
    if let Some(reader) = &state.governance_reader {
        let reader = reader.clone();
        let now = (state.display_now)();
        let cache = state.governance_projection_cache.clone();
        return state.read_executor.run(move || {
            let snapshot = reader.capture(crate::governance_snapshot::Selection::Page {
                after, offset, count,
            }, now)?;
            Ok(Some(Page {
                proposals: project_snapshot(&cache, &snapshot), next_hash: snapshot.next_hash,
            }))
        }).await;
    }
    let Some(engine) = &state.governance_engine else {
        return Ok(None);
    };
    let governance = engine.lock().await;
    let mut selected = governance
        .proposals_after(after)
        .skip(usize::try_from(offset).unwrap_or(usize::MAX))
        .take(count as usize + 1)
        .collect::<Vec<_>>();
    let has_more = selected.len() > count as usize;
    if has_more {
        selected.pop();
    }
    let next_hash = if has_more { selected.last().map(|proposal| proposal.hash) } else { None };
    let params = state.current_params()?;
    let proposals = project(state, &governance, &selected, &params).await?;
    Ok(Some(Page {
        proposals,
        next_hash,
    }))
}

pub(super) async fn info(
    state: &RpcState,
    hash: TxHash,
) -> Result<Option<(Proposal, bool, GovernanceTiming, burst_types::ProtocolParams, Option<burst_consti::AmendmentConflict>)>, RpcError> {
    if let Some(reader) = &state.governance_reader {
        let reader = reader.clone();
        let now = (state.display_now)();
        let cache = state.governance_projection_cache.clone();
        return state.read_executor.run(move || {
            let snapshot = reader.capture(crate::governance_snapshot::Selection::Info(hash), now)?;
            let projected = project_snapshot(&cache, &snapshot).pop()
                .ok_or_else(|| RpcError::ProposalNotFound(hash.to_string()))?;
            let timing = snapshot.governance.timing(&projected.proposal, &snapshot.params)
                .map_err(|error| RpcError::Store(error.to_string()))?;
            Ok(Some((projected.proposal, projected.phase_projected, timing,
                snapshot.params, snapshot.activation_conflict)))
        }).await;
    }
    let Some(engine) = &state.governance_engine else {
        return Ok(None);
    };
    let governance = engine.lock().await;
    let original = governance
        .get_proposal(&hash)
        .ok_or_else(|| RpcError::ProposalNotFound(hash.to_string()))?;
    let params = state.current_params()?;
    let mut selected = project(state, &governance, &[original], &params).await?;
    let projected = selected.pop().expect("one selected proposal");
    let timing = governance
        .timing(&projected.proposal, &params)
        .map_err(|error| RpcError::Store(error.to_string()))?;
    Ok(Some((
        projected.proposal,
        projected.phase_projected,
        timing,
        params,
        state.consti_engine.lock().await.activation_conflict(&hash),
    )))
}

async fn project(
    state: &RpcState,
    governance: &GovernanceEngine,
    selected: &[&Proposal],
    params: &burst_types::ProtocolParams,
) -> Result<Vec<ProjectedProposal>, RpcError> {
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    let now = (state.display_now)();
    let needs_electorate = selected
        .iter()
        .any(|proposal| governance.projection_requires_electorate(proposal, now, &params));
    let delegation = if needs_electorate {
        Some(state.delegation_engine.lock().await)
    } else {
        None
    };
    let verified = if needs_electorate {
        state
            .account_store
            .all_verified_addresses()?
    } else {
        Vec::new()
    };
    let unused_delegation = DelegationEngine::default();
    let delegation = delegation.as_deref().unwrap_or(&unused_delegation);
    Ok(selected
        .iter()
        .map(|original| {
            let proposal =
                governance.project_proposal_at(original, now, &params, delegation, &verified);
            let phase_projected = original.phase != proposal.phase;
            ProjectedProposal {
                proposal,
                phase_projected,
            }
        })
        .collect())
}

fn project_snapshot(cache: &crate::GovernanceProjectionCache, snapshot: &crate::governance_snapshot::Snapshot) -> Vec<ProjectedProposal> {
    let context = crate::governance_projection_cache::Context::from_snapshot(snapshot);
    snapshot.selected.iter().map(|hash| {
        let original = snapshot.governance.get_proposal(hash).expect("selected snapshot proposal");
        let proposal = cache.project_until(context, *hash, || {
            let proposal = snapshot.governance.project_proposal_at(original, snapshot.now,
                &snapshot.params, &snapshot.delegation, &snapshot.verified);
            let until = snapshot.governance.projection_valid_until(&proposal, snapshot.now, &snapshot.params);
            (proposal, until)
        });
        ProjectedProposal { phase_projected: original.phase != proposal.phase, proposal }
    }).collect()
}
