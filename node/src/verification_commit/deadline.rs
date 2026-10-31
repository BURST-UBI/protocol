use super::*;
use burst_verification::CommitteeActivation;

/// Called only while building a deadline checkpoint's disposable projection.
/// The caller supplies prior defaults derived from earlier certified boundaries.
#[allow(clippy::too_many_arguments)]
pub(crate) fn settle_panel_deadline_candidate(
    batch: &mut impl Ledger,
    activation: &CommitteeActivation,
    cutoff: Timestamp,
    params: &ProtocolParams,
    orchestrator: &mut VerificationOrchestrator,
    engines: (&mut BrnEngine, &mut TrstEngine),
    prior_defaults: &HashSet<WalletAddress>,
    network: burst_types::NetworkId,
) -> Result<Vec<WalletAddress>, String> {
    #[cfg(test)]
    let candidate_started = std::time::Instant::now();
    let before = orchestrator.storage_clone();
    let mut scoped = orchestrator.clone();
    scoped.begin_record_changes();
    let (mut candidate, absent) = if scoped.has_active_challenge(&activation.target) {
        scoped.challenge_deadline_candidate(activation, params, cutoff)
    } else {
        scoped.normal_deadline_candidate(activation, params, cutoff)
    }
    .map_err(|error| error.to_string())?;
    #[cfg(test)]
    stage_time("candidate", candidate_started);
    #[cfg(test)]
    let authority_started = std::time::Instant::now();
    let mut defaults = prior_defaults.clone();
    for address in &absent {
        if !defaults.insert(address.clone()) {
            return Err("deadline reused a verifier from an earlier panel".into());
        }
    }
    let events = candidate.drain_events();
    let terminal = events.iter().find_map(|event| match event {
        VerificationEvent::VerificationComplete {
            wallet, outcomes, ..
        } if wallet == &activation.target => Some((outcomes.verifiers.as_slice(), None)),
        VerificationEvent::ChallengeResolved { wallet, outcome }
            if wallet == &activation.target =>
        {
            Some((outcome.verifier_outcomes.as_slice(), Some(outcome)))
        }
        _ => None,
    });
    if let Some((voters, challenger)) = terminal {
        let mut sources = Vec::new();
        if let Some(outcome) = challenger {
            if outcome.outcome == ChallengeResult::Expired {
                return Err("certified deadline cannot use legacy challenge expiry".into());
            }
            sources.push(outcome_context::stake_source(
                batch,
                &activation.target,
                &outcome.challenger,
                outcome.challenger_stake,
                true,
                outcome.challenge_context,
            )?);
        }
        let mut observed_defaults = HashSet::new();
        for vote in voters {
            if defaults.contains(&vote.address) {
                if vote.selection_context.is_some()
                    || vote.staked != 0
                    || vote.penalty != 0
                    || vote.trst_reward != 0
                {
                    return Err("deadline default differs from its recorded absent voter".into());
                }
                observed_defaults.insert(vote.address.clone());
            } else {
                sources.push(outcome_context::stake_source(
                    batch,
                    &activation.target,
                    &vote.address,
                    vote.staked,
                    false,
                    vote.selection_context,
                )?);
            }
        }
        if observed_defaults != defaults {
            return Err("deadline outcome omits a recorded default".into());
        }
        let hash = sources
            .iter()
            .max_by_key(|block| (block.timestamp.as_secs(), *block.hash.as_bytes()))
            .map(|block| block.hash)
            .unwrap_or_else(|| {
                BlockHash::new(burst_crypto::blake2b_256_multi(&[
                    b"BURST/empty-deadline-outcome/v1\0",
                    activation.context.as_bytes(),
                    &cutoff.as_secs().to_be_bytes(),
                ]))
            });
        let context = Settlement {
            network,
            hash,
            target: activation.target.clone(),
            timestamp: cutoff,
            sources: Vec::new(),
            authority: SettlementAuthority::CertifiedDeadline { absent: defaults },
        };
        #[cfg(test)]
        stage_time("authority", authority_started);
        #[cfg(test)]
        let settlement_started = std::time::Instant::now();
        settle(batch, &context, events, params, engines)?;
        if !sources.is_empty() {
            let hashes: Vec<_> = sources.iter().map(|source| source.hash).collect();
            dependencies::persist(batch, context.hash, context.hash, &hashes)?;
        }
        #[cfg(test)]
        stage_time("settlement", settlement_started);
    } else if events
        .iter()
        .any(|event| !matches!(event, VerificationEvent::VerifierPenalized { .. }))
    {
        return Err("deadline has unrelated lifecycle effects".into());
    }
    #[cfg(test)]
    let persistence_started = std::time::Instant::now();
    persist(batch, &before, &candidate)?;
    #[cfg(test)]
    stage_time("persistence", persistence_started);
    *orchestrator = candidate;
    Ok(absent)
}


#[cfg(test)]
fn stage_time(stage: &str, started: std::time::Instant) {
    if std::env::var_os("BURST_DEADLINE_STAGE_DIAGNOSTIC").is_some() {
        println!("signed_deadline_stage stage={stage} elapsed_ns={}", started.elapsed().as_nanos());
    }
}
