use super::*;
use burst_types::{BlockHash, Timestamp, TxHash};

pub(super) fn constitutional_amendment(
    proposal: &burst_governance::Proposal,
    at: Timestamp,
) -> Option<burst_consti::Amendment> {
    let (title, text, operations) = match &proposal.content {
        ProposalContent::ConstitutionalAmendment { title, text } => {
            (title.clone(), text.clone(), vec![])
        }
        ProposalContent::ConstitutionalDiff { title, operations } => {
            (title.clone(), String::new(), operations.clone())
        }
        _ => return None,
    };
    Some(burst_consti::Amendment {
        hash: proposal.hash,
        proposer: proposal.proposer.clone(),
        title,
        text,
        phase: proposal.phase.clone(),
        votes_yea: proposal.exploration_votes_yea,
        votes_nay: proposal.exploration_votes_nay,
        votes_abstain: proposal.exploration_votes_abstain,
        created_at: at,
        operations,
    })
}

impl CandidateGovernance {
    /// Apply approved effects at the recorded activation time, even if called later.
    /// The caller owns a staged ledger and engines, persists successful changes,
    /// and discards all effects on error. A conflicting amendment sets the
    /// proposal to Conflicted without turning the operation into an error.
    pub(crate) fn activate_approved(
        &mut self,
        batch: &mut impl ledger::Ledger,
        proposal_hash: TxHash,
        expected_params: &BlockHash,
        now: Timestamp,
        brn: &mut BrnEngine,
        trst: &mut TrstEngine,
    ) -> Result<(), String> {
        self.activate_with_engines(batch, proposal_hash, expected_params, now, &mut (brn, trst))
    }

    pub(super) fn activate_with_engines(
        &mut self,
        batch: &mut impl ledger::Ledger,
        proposal_hash: TxHash,
        expected_params: &BlockHash,
        now: Timestamp,
        engines: &mut impl crate::economic_candidate::Engines,
    ) -> Result<(), String> {
        let proposal = self
            .governance
            .get_proposal(&proposal_hash)
            .cloned()
            .ok_or("approved proposal missing")?;
        let activation_at = proposal.activation_at.ok_or("activation time missing")?;
        if proposal.phase != GovernancePhase::Activation || now < activation_at {
            return Err("proposal has not reached its approved activation time".into());
        }
        batch.observe_bootstrap(&self.params)?;
        self.governance
            .activate_checked(&proposal, &mut self.params, expected_params)
            .map_err(|e| e.to_string())?;
        batch.observe_bootstrap(&self.params)?;
        for (param, value) in self.governance.drain_pending_changes() {
            match param {
                GovernableParam::BrnRate => engines
                    .brn_mut()
                    .apply_rate_change(value, activation_at)
                    .map_err(|e| e.to_string())?,
                GovernableParam::TrstExpirySecs => engines.trst_mut().set_expiry_period(
                    u64::try_from(value).map_err(|_| "expiry period overflow")?,
                    activation_at,
                )?,
                _ => {}
            }
        }
        let mut phase = GovernancePhase::Activated;
        for content in self.governance.drain_activated_amendments() {
            if content != proposal.content {
                return Err("unexpected content in amendment queue".into());
            }
            let amendment = constitutional_amendment(&proposal, activation_at)
                .ok_or("nonconstitutional content in amendment queue")?;
            if self
                .constitution
                .resolve_approved_amendment(&amendment)
                .map_err(|e| e.to_string())?
                .is_some()
            {
                phase = GovernancePhase::Conflicted;
            }
        }
        for (target, evict) in self.governance.drain_pending_evictions() {
            let mut account = batch.get_account(&target).map_err(|e| e.to_string())?;
            account.orv_evicted = evict;
            batch.put_account(&account).map_err(|e| e.to_string())?;
        }
        self.governance
            .get_proposal_mut(&proposal_hash)
            .ok_or("activated proposal missing")?
            .phase = phase;
        Ok(())
    }
}
