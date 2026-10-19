use super::*;

impl VerificationOrchestrator {
    /// Start review of a specific earlier judgment. The caller must derive the
    /// context from the latest certified receipt; this method checks only the
    /// panel state machine and does not authorize ledger admission or reversal.
    pub fn initiate_signed_appeal_at(
        &mut self,
        target: &WalletAddress,
        challenger: &WalletAddress,
        challenger_is_verified: bool,
        stake: u128,
        event: (crate::ChallengeReason, Timestamp, BlockHash, AppealContext),
        params: &ProtocolParams,
    ) -> Result<(), VerificationError> {
        let (reason, now, admission, appeal) = event;
        if admission.is_zero()
            || appeal.reviewed_outcome.is_zero()
            || appeal.reviewed_outcome == admission
        {
            return Err(VerificationError::Other(
                "appeal references are invalid".into(),
            ));
        }
        if !challenger_is_verified {
            return Err(VerificationError::ChallengerNotVerified(
                challenger.to_string(),
            ));
        }
        if stake < params.challenge_stake_amount {
            return Err(VerificationError::InsufficientStake {
                needed: params.challenge_stake_amount,
                provided: stake,
            });
        }
        if self.active_challenges.contains_key(target) {
            return Err(VerificationError::Other(
                "a challenge is already active for this wallet".into(),
            ));
        }
        let expected_phase = if appeal.previously_legitimate {
            VerificationPhase::Verified
        } else {
            match reason {
                crate::ChallengeReason::Fraud => VerificationPhase::Unverified,
                crate::ChallengeReason::Inactivity => VerificationPhase::Deactivated,
            }
        };
        let state = self.states.get_mut(target).ok_or_else(|| {
            VerificationError::Other("appeal target has no verification state".into())
        })?;
        if state.phase != expected_phase {
            return Err(VerificationError::Other(
                "appeal judgment differs from current verification state".into(),
            ));
        }
        let challenge =
            self.challenges
                .initiate(challenger.clone(), target.clone(), stake, reason, now);
        self.active_challenges.insert(target.clone(), challenge);
        self.active_appeals.insert(target.clone(), appeal);
        state.phase = VerificationPhase::Challenged;
        state.excluded_verifiers.clear();
        state.selected_verifiers.clear();
        state.votes.clear();
        state.revote_count = 0;
        state.started_at = now;
        self.prior_votes.remove(target);
        self.selections.remove(target);
        self.attempts.clear(target);
        self.attempts.bind_challenge(target, admission);
        Ok(())
    }

    pub fn active_appeal(&self, target: &WalletAddress) -> Option<&AppealContext> {
        self.active_appeals.get(target)
    }
}

impl OrchestratorSnapshot {
    pub(super) fn validate_appeals(&self) -> Result<(), bincode::Error> {
        validate_appeals(
            &self.active_appeals,
            &self.attempts,
            &self.active_challenges,
            &self.states,
        )
    }
}

pub(super) fn validate_appeals(
    appeals: &HashMap<WalletAddress, AppealContext>,
    attempts: &AttemptContexts,
    challenges: &HashMap<WalletAddress, Challenge>,
    states: &HashMap<WalletAddress, VerificationState>,
) -> Result<(), bincode::Error> {
    for (target, appeal) in appeals {
        let valid = !appeal.reviewed_outcome.is_zero()
            && attempts.challenge_context(target).is_some_and(|admission| {
                !admission.is_zero() && admission != appeal.reviewed_outcome
            })
            && challenges
                .get(target)
                .is_some_and(|challenge| &challenge.target == target)
            && states.get(target).is_some_and(|state| {
                &state.target == target
                    && matches!(
                        state.phase,
                        VerificationPhase::Challenged | VerificationPhase::Voting
                    )
            });
        if !valid {
            return Err(Box::new(bincode::ErrorKind::Custom(
                "invalid active appeal snapshot".into(),
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
