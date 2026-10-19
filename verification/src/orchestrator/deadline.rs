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
    /// release, identity change or provenance revocation.
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
        let mut absent: Vec<_> = state
            .selected_verifiers
            .iter()
            .filter(|verifier| !state.votes.iter().any(|vote| &vote.verifier == *verifier))
            .cloned()
            .collect();
        absent.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        let mut candidate = self.clone();
        for verifier in &absent {
            // The existing tally and Neither penalties apply, but no signed vote
            // binding or stake receipt is invented for an absent participant.
            candidate.cast_vote_at(
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
