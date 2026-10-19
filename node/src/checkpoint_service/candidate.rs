use crate::{
    checkpoint_activation::PreparedActivation,
    checkpoint_agreement::ReplayedCandidate,
    checkpoint_deadline::PreparedDeadline,
    checkpoint_governance::PreparedGovernance,
    checkpoint_outcome::PreparedChallengeOutcome,
    checkpoint_replay::SigningClock,
    checkpoint_roster::PreparedRoster,
    checkpoint_store::{chain, CertifiedCheckpoint},
};
use burst_store::StoreError;
use burst_store_lmdb::LmdbStore;
use burst_types::{NetworkId, ProtocolParams};

/// Initial discovery and unsigned refresh share boundary ordering. Neither may
/// move lifecycle effects across a due governance boundary using old rules.
pub(crate) fn discover_candidate(
    store: &LmdbStore,
    network: NetworkId,
    params: &ProtocolParams,
    time: SigningClock,
) -> Result<Option<ReplayedCandidate>, StoreError> {
    let events = chain::load(store, network, params)?;
    let safe_cut = time
        .now
        .checked_sub(time.error_bound)
        .and_then(|time| time.checked_sub(1));
    let governance_cut = match safe_cut {
        Some(cutoff) => {
            PreparedGovernance::due_at_in_chain(store, network, params, cutoff, &events)?
        }
        None => None,
    };
    let lifecycle_cut = governance_cut
        .and_then(|cut| cut.checked_sub(1))
        .or(safe_cut);
    if let Some(cutoff) = lifecycle_cut {
        if let Some(value) =
            PreparedDeadline::prepare_next_in_chain(store, network, params, cutoff, &events)?
        {
            return Ok(Some(ReplayedCandidate::Deadline(Box::new(value))));
        }
        if let Some(value) = PreparedChallengeOutcome::prepare_next_in_chain(
            store, network, params, cutoff, &events,
        )? {
            return Ok(Some(ReplayedCandidate::ChallengeOutcome(Box::new(value))));
        }
        if let Some(value) =
            PreparedActivation::prepare_next_in_chain(store, network, params, cutoff, &events)?
        {
            return Ok(Some(ReplayedCandidate::Activation(Box::new(value))));
        }
    }
    if let Some(cutoff) = governance_cut {
        return Ok(PreparedGovernance::prepare_next_in_chain(
            store, network, params, cutoff, &events,
        )?
        .map(|value| ReplayedCandidate::Governance(Box::new(value))));
    }
    if let Some(value) =
        CertifiedCheckpoint::prepare_next_in_chain(store, network, params, time.now, &events)?
    {
        value
            .request()
            .validate_before_signing(value.checkpoint(), time.now, time.error_bound)
            .map_err(|error| StoreError::Corruption(error.to_string()))?;
        return Ok(Some(ReplayedCandidate::Selection(Box::new(value))));
    }
    match safe_cut {
        Some(cutoff) => Ok(PreparedRoster::prepare_next_in_chain(
            store, network, params, cutoff, &events,
        )?
        .map(|value| ReplayedCandidate::Roster(Box::new(value)))),
        None => Ok(None),
    }
}
