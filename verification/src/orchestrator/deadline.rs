use super::*;

/// Untrusted until reconstructed from a certified activation checkpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitteeActivation {
    pub target: WalletAddress,
    pub selection: BlockHash,
    pub context: BlockHash,
    pub params_hash: BlockHash,
    /// Authenticated activation rules validate the fixed window, not later votes.
    pub rules_at_activation: ProtocolParams,
    pub started_at: u64,
    pub deadline: u64,
}

impl VerificationOrchestrator {
    /// Compute one normal-panel deadline on an isolated copy. The node must
    /// authenticate the activation, certify this completion and atomically install
    /// its economic effects. A clock reading alone cannot publish this candidate.
    /// Missing voters remain distinct from signed votes in AttemptContexts.
    /// Activation rules fix the deadline; `params` governs the resulting tally
    /// and Neither penalties, so the caller must supply the agreed settlement rules.
    pub fn normal_deadline_candidate(
        &self,
        activation: &CommitteeActivation,
        params: &ProtocolParams,
        cutoff: Timestamp,
    ) -> Result<(Self, Vec<WalletAddress>), VerificationError> {
        let invalid = |message: &str| VerificationError::Other(message.into());
        let wallet = &activation.target;
        if self.active_challenges.contains_key(wallet) {
            return Err(invalid("normal deadline cannot resolve a challenge"));
        }
        self.deadline_candidate(activation, params, cutoff)
    }

    /// Complete an activated challenge panel on a disposable copy. Admission is
    /// not a voting deadline: the authenticated activation supplies the window.
    /// The caller must certify the result before publishing any verdict, stake
    /// release, identity change or provenance revocation. As for normal panels,
    /// activation rules fix the window while `params` supplies settlement rules.
    pub fn challenge_deadline_candidate(
        &self,
        activation: &CommitteeActivation,
        params: &ProtocolParams,
        cutoff: Timestamp,
    ) -> Result<(Self, Vec<WalletAddress>), VerificationError> {
        let (challenge, _) = self
            .selection_challenge(&activation.target)?
            .ok_or_else(|| VerificationError::Other("deadline has no signed challenge".into()))?;
        if activation.started_at < challenge.initiated_at.as_secs() {
            return Err(VerificationError::Other(
                "activation predates its challenge".into(),
            ));
        }
        let (mut candidate, absent) = self.deadline_candidate(activation, params, cutoff)?;
        candidate.try_resolve_challenge(&activation.target, params)?;
        Ok((candidate, absent))
    }

    fn deadline_candidate(
        &self,
        activation: &CommitteeActivation,
        params: &ProtocolParams,
        cutoff: Timestamp,
    ) -> Result<(Self, Vec<WalletAddress>), VerificationError> {
        let invalid = |message: &str| VerificationError::Other(message.into());
        let wallet = &activation.target;
        let duration = if self.active_challenges.contains_key(wallet) {
            activation.rules_at_activation.challenge_duration_secs
        } else {
            activation.rules_at_activation.verification_timeout_secs
        };
        if !self.pending_events.is_empty() {
            return Err(invalid(
                "deadline candidate has uncommitted lifecycle events",
            ));
        }
        let selection = self
            .selections
            .get(wallet)
            .ok_or_else(|| invalid("deadline has no certified selection"))?;
        let (checkpoint, _) = selection
            .certified
            .as_ref()
            .ok_or_else(|| invalid("deadline committee is not certified"))?;
        if activation.context.is_zero()
            || self.selection_context(wallet) != Some(activation.context)
            || checkpoint.hash() != activation.selection
            || activation.params_hash != activation.rules_at_activation.params_hash()
            || duration == 0
            || activation.started_at.checked_add(duration) != Some(activation.deadline)
            || activation.started_at
                < selection
                    .request
                    .beacon_time()
                    .map_err(|error| invalid(&error.to_string()))?
            || cutoff.as_secs() < activation.deadline
        {
            return Err(invalid(
                "deadline differs from its activation or parameters",
            ));
        }
        let state = self
            .states
            .get(wallet)
            .ok_or_else(|| invalid("deadline target is missing"))?;
        if state.phase != VerificationPhase::Voting
            || state.selected_verifiers.is_empty()
            || state.votes.len() >= state.selected_verifiers.len()
            || state.revote_count != selection.request.attempt()
        {
            return Err(invalid("deadline panel is not awaiting missing votes"));
        }
        for vote in &state.votes {
            if self.attempts.vote_context(wallet, &vote.verifier) != Some(activation.context)
                || vote.timestamp.as_secs() >= activation.deadline
                || (vote.vote == Vote::Neither && vote.stake_amount != 0)
            {
                return Err(invalid("deadline panel contains an unbound or late vote"));
            }
        }
        let absent = missing_voters(state)?;
        let mut candidate = self.clone();
        for verifier in &absent {
            // The existing tally and Neither penalties apply, but no signed vote
            // binding or stake receipt is invented for an absent participant.
            candidate.cast_eligible_vote_at(
                wallet,
                verifier,
                Vote::Neither,
                params,
                Timestamp::new(activation.deadline),
            )?;
        }
        Ok((candidate, absent))
    }
}

/// Validate the round once before bypassing per-vote membership scans. Sorting
/// preserves the penalty/event/default order of sequential deadline settlement.
fn missing_voters(state: &VerificationState) -> Result<Vec<WalletAddress>, VerificationError> {
    use std::collections::HashSet;
    let selected: HashSet<_> = state.selected_verifiers.iter().collect();
    if selected.len() != state.selected_verifiers.len() {
        return Err(VerificationError::Other(
            "deadline panel repeats a selected verifier".into(),
        ));
    }
    let mut voted = HashSet::with_capacity(state.votes.len());
    for vote in &state.votes {
        if !selected.contains(&vote.verifier) || !voted.insert(&vote.verifier) {
            return Err(VerificationError::Other(
                "deadline panel has an invalid or repeated voter".into(),
            ));
        }
    }
    let mut absent: Vec<_> = state
        .selected_verifiers
        .iter()
        .filter(|verifier| !voted.contains(verifier))
        .cloned()
        .collect();
    absent.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    Ok(absent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel(count: usize) -> (VerificationOrchestrator, WalletAddress, ProtocolParams) {
        let wallet = WalletAddress::new("brst_deadline_target");
        let params = ProtocolParams {
            endorsement_threshold: 1,
            ..ProtocolParams::default()
        };
        let mut engine = VerificationOrchestrator::new();
        engine
            .process_endorsement_at(
                &wallet,
                &WalletAddress::new("brst_endorser"),
                params.endorsement_burn_amount,
                &params,
                Timestamp::new(1),
            )
            .unwrap();
        let state = engine.states.get_mut(&wallet).unwrap();
        state.phase = VerificationPhase::Voting;
        state.selected_verifiers = (0..count)
            .rev()
            .map(|n| WalletAddress::new(format!("brst_deadline_voter_{n:04}")))
            .collect();
        engine.drain_events();
        (engine, wallet, params)
    }

    #[test]
    fn batched_defaults_match_sequential_votes_penalties_tallies_and_revotes() {
        for count in [2, 32, 256] {
            for voted in [0, 1, count / 2] {
                for (threshold, max_revotes) in [(5000, 0), (9000, 0), (9000, 2)] {
                    let (mut engine, wallet, mut params) = panel(count);
                    params.verification_threshold_bps = threshold;
                    params.max_revotes = max_revotes;
                    let selected = engine.states[&wallet].selected_verifiers.clone();
                    for verifier in selected.iter().take(voted) {
                        engine
                            .cast_vote_at(
                                &wallet,
                                verifier,
                                Vote::Legitimate,
                                &params,
                                Timestamp::new(2),
                            )
                            .unwrap();
                    }
                    engine.drain_events();
                    let before = engine.snapshot().encode().unwrap();
                    let state = &engine.states[&wallet];
                    let mut old_absent: Vec<_> = state
                        .selected_verifiers
                        .iter()
                        .filter(|verifier| {
                            !state.votes.iter().any(|vote| &vote.verifier == *verifier)
                        })
                        .cloned()
                        .collect();
                    old_absent.sort_by(|a, b| a.as_str().cmp(b.as_str()));
                    let absent = missing_voters(state).unwrap();
                    assert_eq!(absent, old_absent);
                    let baseline = engine.storage_clone();
                    engine.begin_record_changes();
                    let mut expected = engine.clone();
                    let mut actual = engine.clone();
                    for verifier in old_absent {
                        expected
                            .cast_vote_at(
                                &wallet,
                                &verifier,
                                Vote::Neither,
                                &params,
                                Timestamp::new(60),
                            )
                            .unwrap();
                    }
                    for verifier in absent {
                        actual
                            .cast_eligible_vote_at(
                                &wallet,
                                &verifier,
                                Vote::Neither,
                                &params,
                                Timestamp::new(60),
                            )
                            .unwrap();
                    }
                    crate::orchestrator::records::assert_normalized_roundtrip(&actual);
                    let logical = actual.target_record(&wallet).unwrap();
                    let rows =
                        crate::orchestrator::records::normalized::target_rows(&logical).unwrap();
                    let decoded =
                        crate::orchestrator::records::normalized::target_from_rows(&wallet, &rows)
                            .unwrap();
                    assert_eq!(
                        bincode::serialize(&decoded).unwrap(),
                        bincode::serialize(&logical).unwrap()
                    );
                    let mut patched = crate::orchestrator::records::normalized::target_rows(
                        &baseline.target_record(&wallet).unwrap(),
                    )
                    .unwrap();
                    crate::orchestrator::records::normalized::visit_target_delta(
                        &baseline,
                        &actual,
                        |key, old, new| {
                            assert_eq!(patched.get(&key), old.as_ref());
                            match new {
                                Some(bytes) => {
                                    patched.insert(key, bytes);
                                }
                                None => {
                                    patched.remove(&key);
                                }
                            }
                            Ok(())
                        },
                    )
                    .unwrap();
                    assert_eq!(patched, rows);

                    assert_eq!(
                        actual.snapshot().encode().unwrap(),
                        expected.snapshot().encode().unwrap()
                    );
                    assert_eq!(
                        format!("{:?}", actual.drain_events()),
                        format!("{:?}", expected.drain_events())
                    );
                    assert_eq!(engine.snapshot().encode().unwrap(), before);
                }
            }
        }
    }

    #[test]
    fn deadline_batch_rejects_duplicate_members_and_invalid_prior_votes() {
        for change in 0..3 {
            let (mut engine, wallet, params) = panel(3);
            let voter = engine.states[&wallet].selected_verifiers[0].clone();
            engine
                .cast_vote_at(
                    &wallet,
                    &voter,
                    Vote::Legitimate,
                    &params,
                    Timestamp::new(2),
                )
                .unwrap();
            let state = engine.states.get_mut(&wallet).unwrap();
            match change {
                0 => state.selected_verifiers.push(voter),
                1 => state.votes.push(state.votes[0].clone()),
                _ => state.votes[0].verifier = WalletAddress::new("brst_not_selected"),
            }
            assert!(missing_voters(state).is_err());
        }
    }
    #[test]
    #[ignore = "explicit capped component deadline stage diagnostic; not node latency"]
    fn deadline_batch_stage_scaling() {
        use std::time::Instant;
        for count in [1_000, 10_000, 100_000] {
            for max_revotes in [0, 2] {
                for sample in 0..3 {
                    let start = Instant::now();
                    let (mut engine, wallet, mut params) = panel(count);
                    params.max_revotes = max_revotes;
                    let setup_ns = start.elapsed().as_nanos();
                    let start = Instant::now();
                    let absent = missing_voters(&engine.states[&wallet]).unwrap();
                    let membership_ns = start.elapsed().as_nanos();
                    assert_eq!(absent.len(), count);
                    let start = Instant::now();
                    for verifier in &absent[..count - 1] {
                        assert!(engine
                            .cast_eligible_vote_at(
                                &wallet,
                                verifier,
                                Vote::Neither,
                                &params,
                                Timestamp::new(60)
                            )
                            .unwrap()
                            .is_none());
                    }
                    let defaults_ns = start.elapsed().as_nanos();
                    let start = Instant::now();
                    let completion = engine
                        .cast_eligible_vote_at(
                            &wallet,
                            &absent[count - 1],
                            Vote::Neither,
                            &params,
                            Timestamp::new(60),
                        )
                        .unwrap();
                    let completion_ns = start.elapsed().as_nanos();
                    let events = engine.drain_events();
                    assert_eq!(
                        events
                            .iter()
                            .filter(|e| matches!(e, VerificationEvent::VerifierPenalized { .. }))
                            .count(),
                        count
                    );
                    if max_revotes == 0 {
                        assert!(
                            matches!(completion, Some(VerificationEvent::VerificationComplete {
                            result: VerificationResult::Failed, ref outcomes, ..
                        }) if outcomes.verifiers.len() == count && outcomes.verifiers.iter()
                            .all(|v| v.staked == 0 && v.penalty == 0 && v.trst_reward == 0))
                        );
                        assert_eq!(engine.states[&wallet].phase, VerificationPhase::Failed);
                    } else {
                        assert!(completion.is_none());
                        assert_eq!(engine.states[&wallet].revote_count, 1);
                        assert_eq!(engine.states[&wallet].excluded_verifiers.len(), count);
                        assert_eq!(engine.prior_votes[&wallet].len(), count);
                    }
                    let start = Instant::now();
                    let snapshot = engine.snapshot().encode().unwrap();
                    let snapshot_ns = start.elapsed().as_nanos();
                    println!("deadline_scaling count={count} max_revotes={max_revotes} sample={sample} setup_ns={setup_ns} membership_ns={membership_ns} defaults_ns={defaults_ns} completion_ns={completion_ns} snapshot_ns={snapshot_ns} snapshot_bytes={}", snapshot.len());
                }
            }
        }
    }

    #[test]
    fn deadline_target_row_limit_diagnostic() {
        for count in [128, 1_024] {
            let (mut engine, wallet, mut params) = panel(count);
            params.max_revotes = 0;
            assert!(engine
                .target_record(&wallet)
                .unwrap()
                .storage_entry()
                .is_ok());
            let absent = missing_voters(&engine.states[&wallet]).unwrap();
            for voter in absent {
                engine
                    .cast_eligible_vote_at(
                        &wallet,
                        &voter,
                        Vote::Neither,
                        &params,
                        Timestamp::new(60),
                    )
                    .unwrap();
            }
            engine.drain_events();
            let snapshot = engine.snapshot().encode().unwrap();
            let restored =
                VerificationOrchestrator::restore(OrchestratorSnapshot::decode(&snapshot).unwrap());
            assert_eq!(restored.states[&wallet].votes.len(), count);
            let row = engine.target_record(&wallet).unwrap().storage_entry();
            if count == 128 {
                assert!(row.is_ok());
            } else {
                assert_eq!(row.unwrap_err(), "verification record exceeds row limit");
                assert!(snapshot.len() < 64 << 20);
            }
            let logical = engine.target_record(&wallet).unwrap();
            let rows = crate::orchestrator::records::normalized::target_rows(&logical).unwrap();
            let decoded =
                crate::orchestrator::records::normalized::target_from_rows(&wallet, &rows).unwrap();
            assert_eq!(
                bincode::serialize(&decoded).unwrap(),
                bincode::serialize(&logical).unwrap()
            );
            assert!(rows
                .values()
                .all(|bytes| bytes.len() <= burst_types::MAX_P2P_MESSAGE_BYTES));
            for mutation in 0..4 {
                let mut bad = rows.clone();
                let member = bad
                    .keys()
                    .find(|key| key.contains("/1/3/"))
                    .unwrap()
                    .clone();
                match mutation {
                    0 => {
                        bad.remove(&member);
                    }
                    1 => {
                        bad.get_mut(&member).unwrap().push(0);
                    }
                    2 => {
                        let value = bad.remove(&member).unwrap();
                        bad.insert(format!("{member}00"), value);
                    }
                    _ => {
                        let header = bad.keys().find(|key| key.ends_with("/0")).unwrap().clone();
                        bad.remove(&header);
                    }
                }
                assert!(
                    crate::orchestrator::records::normalized::target_from_rows(&wallet, &bad)
                        .is_err()
                );
            }
            println!(
                "deadline_row_boundary count={count} snapshot_bytes={} row_supported={}",
                snapshot.len(),
                count == 128
            );
        }
    }
}
