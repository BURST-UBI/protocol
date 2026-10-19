use super::*;

/// Scheduled phases are a read-only view. The block transaction owns durable
/// governance state; an RPC request must never close a partially received vote.
pub(super) async fn read(
    state: &RpcState,
) -> Result<
    Option<(
        burst_governance::GovernanceEngine,
        std::collections::HashSet<TxHash>,
    )>,
    RpcError,
> {
    let Some(engine) = &state.governance_engine else {
        return Ok(None);
    };
    let governance = engine.lock().await;
    let delegation = state.delegation_engine.lock().await;
    let verified: Vec<_> = state
        .account_store
        .iter_verified_accounts()?
        .into_iter()
        .map(|account| account.address)
        .collect();
    let projected = governance.project_at(
        (state.display_now)(),
        &state.current_params()?,
        &delegation,
        &verified,
    );
    let changed = projected
        .all_proposals()
        .filter(|proposal| {
            governance
                .get_proposal(&proposal.hash)
                .is_some_and(|original| original.phase != proposal.phase)
        })
        .map(|proposal| proposal.hash)
        .collect();
    Ok(Some((projected, changed)))
}
