use super::*;
use crate::verification_commit::ledger::ReplayedLedger;
use crate::{verification_activation, verification_deadline};

impl ConfirmedReplay {
    /// Apply due panel completions and pending-payment returns to replayed state.
    /// The caller must replay the signed history first and certify the resulting
    /// checkpoint before installation. Discard this candidate on error.
    pub(crate) fn apply_agreed_deadlines(
        &mut self,
        history: &impl LifecycleHistory,
        params: &ProtocolParams,
    ) -> Result<Option<BlockHash>, ReplayError> {
        let panels = self.apply_panel_deadlines(history, params)?;
        let mut returns: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, pending)| pending_expired(pending, params, self.cutoff))
            .map(|(key, pending)| (key.clone(), pending.clone()))
            .collect();
        if returns.is_empty() {
            return Ok(panels);
        }
        returns.sort_by(|a, b| {
            a.0 .0
                .as_str()
                .cmp(b.0 .0.as_str())
                .then_with(|| a.0 .1.as_bytes().cmp(b.0 .1.as_bytes()))
        });
        let mut root = BlockHash::new(burst_crypto::blake2b_256_multi(&[
            b"BURST/pending-expiry-checkpoint/v1\0",
            &self.cutoff.as_secs().to_be_bytes(),
            panels.unwrap_or(BlockHash::ZERO).as_bytes(),
            &(returns.len() as u64).to_be_bytes(),
        ]));
        for ((destination, hash), mut pending) in returns {
            let bytes = bincode::serialize(&(&destination, hash, &pending))
                .map_err(|error| invalid(error.to_string()))?;
            root = BlockHash::new(burst_crypto::blake2b_256_multi(&[
                b"BURST/pending-expiry-entry/v1\0",
                root.as_bytes(),
                &bytes,
            ]));
            let returned =
                crate::block_economics::create_returned_token(&destination, &hash, &pending);
            crate::block_economics::deliver_pending(&mut self.trst, &pending, returned, self.cutoff)
                .map_err(invalid)?;
            self.pending.remove(&(destination, hash));
            pending.return_credit = true;
            if self
                .pending
                .insert((pending.source.clone(), hash), pending)
                .is_some()
            {
                return Err(invalid("expiry return credit already exists"));
            }
        }
        self.advance_cutoff(self.cutoff, params)?;
        Ok(Some(root))
    }

    /// All due panels share one boundary; leaving one overdue would prevent the
    /// next strictly later checkpoint from replaying its parent.
    pub(crate) fn apply_panel_deadlines(
        &mut self,
        history: &impl LifecycleHistory,
        params: &ProtocolParams,
    ) -> Result<Option<BlockHash>, ReplayError> {
        let mut due = Vec::new();
        let mut previous = Vec::new();
        for (key, bytes) in &self.lifecycle_metadata {
            if key.starts_with("verification_deadline/v1/") {
                previous.push(verification_deadline::decode(bytes).map_err(invalid)?);
            }
            if !key.starts_with("verification_activation/v1/") {
                continue;
            }
            let activation = verification_activation::decode(bytes).map_err(invalid)?;
            if activation.deadline <= self.cutoff.as_secs()
                && self.verification.selection_context(&activation.target)
                    == Some(activation.context)
                && self
                    .verification
                    .get_state(&activation.target)
                    .is_some_and(|state| {
                        state.phase == burst_verification::state::VerificationPhase::Voting
                            && state.votes.len() < state.selected_verifiers.len()
                    })
            {
                due.push(activation);
            }
        }
        if due.is_empty() {
            return Ok(None);
        }
        due.sort_by_key(|value| (value.deadline, value.context));
        let applied = history.applied_blocks();
        let current = history.first_block()?;
        let mut completions = Vec::new();
        for activation in due {
            let lifecycle = match self.selection_lifecycle(&activation.target)? {
                crate::checkpoint_replay::SelectionLifecycle::Verification(origin)
                | crate::checkpoint_replay::SelectionLifecycle::Challenge(origin) => origin,
            };
            let prior_defaults = previous
                .iter()
                .filter(|value| {
                    value.activation.target == activation.target && value.lifecycle == lifecycle
                })
                .flat_map(|value| value.absent.iter().cloned())
                .collect();
            let mut ledger = ReplayedLedger {
                bootstrap: &mut self.bootstrap_phase,
                bootstrap_threshold: self.params.bootstrap_exit_threshold,
                mutations: self.mutations.as_mut(),
                accounts: &mut self.accounts,
                pending: &mut self.pending,
                metadata: &mut self.lifecycle_metadata,
                applied: &applied,
                current: &current,
            };
            let absent = crate::verification_commit::settle_panel_deadline_candidate(
                &mut ledger,
                &activation,
                self.cutoff,
                params,
                &mut self.verification,
                (&mut self.brn, &mut self.trst),
                &prior_defaults,
                self.network,
            )
            .map_err(invalid)?;
            let completion = verification_deadline::Completion {
                activation,
                lifecycle,
                completed_at: self.cutoff.as_secs(),
                absent,
            };
            let bytes =
                bincode::serialize(&completion).map_err(|error| invalid(error.to_string()))?;
            verification_deadline::decode(&bytes).map_err(invalid)?;
            if self
                .lifecycle_metadata
                .insert(
                    verification_deadline::key(completion.activation.context),
                    bytes.clone(),
                )
                .is_some()
            {
                return Err(invalid("deadline completion was already applied"));
            }
            for voter in &completion.absent {
                if previous.iter().any(|value| {
                    value.activation.target == completion.activation.target
                        && value.lifecycle == lifecycle
                        && value.absent.contains(voter)
                }) {
                    return Err(invalid("deadline reused an absent verifier"));
                }
                self.lifecycle_metadata.insert(
                    verification_deadline::voter_key(&completion.activation.target, voter),
                    completion.activation.context.as_bytes().to_vec(),
                );
            }
            completions.push(completion);
        }
        self.require_completed_deadlines(self.cutoff)?;
        verification_deadline::commitment(&completions)
            .map(Some)
            .map_err(invalid)
    }
}
