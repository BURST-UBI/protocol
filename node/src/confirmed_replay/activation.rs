use super::*;
use crate::{
    checkpoint_activation::ActivationProof,
    checkpoint_store::CertifiedCheckpoint,
    verification_activation::{self, Activation},
};

impl ConfirmedReplay {
    pub(crate) fn can_activate(
        &self,
        selection: &CertifiedCheckpoint,
    ) -> Result<bool, ReplayError> {
        let request = selection.replayed().request();
        let context = request
            .vote_context(&selection.certificate().checkpoint)
            .map_err(|error| invalid(error.to_string()))?;
        if self
            .lifecycle_metadata
            .contains_key(&verification_activation::key(context))
        {
            return Ok(false);
        }
        Ok(
            self.verification.selection_request(request.target()) == Some(request)
                && self
                    .verification
                    .get_state(request.target())
                    .is_some_and(|state| {
                        matches!(
                            state.phase,
                            burst_verification::state::VerificationPhase::Endorsing
                                | burst_verification::state::VerificationPhase::Challenged
                                | burst_verification::state::VerificationPhase::Voting
                        ) && (state.selected_verifiers.is_empty()
                            || state.votes.len() < state.selected_verifiers.len())
                    }),
        )
    }

    pub(crate) fn apply_activation(
        &mut self,
        selection: &CertifiedCheckpoint,
        proof: &ActivationProof,
        _initial_params: &ProtocolParams,
    ) -> Result<(), ReplayError> {
        if !self.can_activate(selection)? {
            return Err(invalid("committee is not awaiting activation"));
        }
        let request = selection.replayed().request();
        let params = self.params.clone();
        let duration = match selection.replayed().lifecycle() {
            crate::checkpoint_replay::SelectionLifecycle::Challenge(_) => {
                params.challenge_duration_secs
            }
            crate::checkpoint_replay::SelectionLifecycle::Verification(_) => {
                params.verification_timeout_secs
            }
        };
        if proof.selection != selection.certificate().checkpoint.hash()
            || self.cutoff.as_secs()
                < request
                    .beacon_time()
                    .map_err(|error| invalid(error.to_string()))?
            || duration == 0
        {
            return Err(invalid(
                "activation predates its beacon or uses different parameters",
            ));
        }
        let deadline = self
            .cutoff
            .as_secs()
            .checked_add(duration)
            .ok_or_else(|| invalid("verification deadline overflow"))?;
        let context = self
            .verification
            .install_selection(
                request.target(),
                selection.parent(),
                selection.certificate(),
                &proof.beacon(),
            )
            .map_err(|error| invalid(error.to_string()))?;
        if self.verification.drain_events().iter().any(|event|
            !matches!(event, VerificationEvent::VerifiersSelected { wallet, .. } if wallet == request.target()))
        { return Err(invalid("activation has unrelated lifecycle effects")); }
        let activation = Activation {
            target: request.target().clone(),
            selection: proof.selection,
            context,
            params_hash: params.params_hash(),
            rules_at_activation: params,
            started_at: self.cutoff.as_secs(),
            deadline,
        };
        self.lifecycle_metadata.insert(
            verification_activation::key(context),
            bincode::serialize(&activation).map_err(|error| invalid(error.to_string()))?,
        );
        Ok(())
    }

    pub(super) fn require_completed_deadlines(&self, cutoff: Timestamp) -> Result<(), ReplayError> {
        for (key, bytes) in &self.lifecycle_metadata {
            if !key.starts_with("verification_activation/v1/") {
                continue;
            }
            let activation = verification_activation::decode(bytes).map_err(invalid)?;
            if cutoff.as_secs() >= activation.deadline
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
                return Err(invalid("verification deadline requires agreed completion"));
            }
        }
        Ok(())
    }
}
