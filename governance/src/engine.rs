//! BURST governance: Proposal → Voting → Cooldown → Activation.
//! `Exploration` is the stored name of the single voting phase. Promotion is
//! retained for decoding old snapshots but is not a valid transition.

use crate::error::GovernanceError;
use crate::proposal::{GovernancePhase, Proposal, ProposalContent};
use crate::vote_delegation::DelegationEngine;
use burst_transactions::governance::GovernanceVote;
use burst_types::{ProtocolParams, Timestamp, TxHash, WalletAddress};
use imbl::OrdMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

mod snapshot_view;
pub use snapshot_view::{SnapshotMap, SnapshotView};

mod records;
mod snapshot_undo;
pub use records::GovernanceRecord;

/// Delay between cooldown ending and activation applying.
/// Uses the governed propagation buffer. Scheduling alone does not establish
/// network agreement; the node must apply the certified transition.
fn activation_delay_secs(params: &ProtocolParams) -> u64 {
    params.governance_propagation_buffer_secs
}

/// The governance engine manages proposals through the four-phase lifecycle,
/// tracking endorsements, votes, and phase transitions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceEngine {
    /// All submitted proposals indexed by their transaction hash.
    proposals: OrdMap<TxHash, Proposal>,
    /// Exploration-phase votes per proposal: proposal_hash → (voter → vote).
    exploration_votes: OrdMap<TxHash, OrdMap<WalletAddress, GovernanceVote>>,
    /// Legacy second-ballot records, indexed by proposal and voter.
    promotion_votes: OrdMap<TxHash, OrdMap<WalletAddress, GovernanceVote>>,
    /// Endorsement tracking per proposal: proposal_hash → (count, total_brn_burned).
    endorsement_counts: OrdMap<TxHash, (u32, u128)>,
    /// Parameter changes that have been activated but not yet propagated to engines.
    pending_changes: Arc<Vec<(crate::params::GovernableParam, u128)>>,
    /// Constitutional amendments that have been activated but not yet applied by the node.
    pending_amendments: Arc<Vec<ProposalContent>>,
    /// Activated ORV evictions/reinstatements not yet applied to account state:
    /// `(target, evict)` where `evict = true` evicts and `false` reinstates.
    #[serde(default)]
    pending_evictions: Arc<Vec<(WalletAddress, bool)>>,
    timings: OrdMap<TxHash, crate::GovernanceTiming>,
    key_ballots: OrdMap<(TxHash, WalletAddress, burst_types::BlockHash), GovernanceVote>,
}

impl GovernanceEngine {
    /// Create a new governance engine with empty state.
    pub fn new() -> Self {
        Self {
            proposals: OrdMap::new(),
            exploration_votes: OrdMap::new(),
            promotion_votes: OrdMap::new(),
            endorsement_counts: OrdMap::new(),
            pending_changes: Arc::default(),
            pending_amendments: Arc::default(),
            pending_evictions: Arc::default(),
            timings: OrdMap::new(),
            key_ballots: OrdMap::new(),
        }
    }

    /// Submit a verified human's proposal. Emergencies enter voting immediately.
    ///
    /// Any verified wallet can submit. Endorsers pay the anti-spam cost.
    pub fn submit_proposal(
        &mut self,
        mut proposal: Proposal,
        _proposer_brn_balance: u128,
        proposer_verified: bool,
        params: &ProtocolParams,
    ) -> Result<TxHash, GovernanceError> {
        if !proposer_verified {
            return Err(GovernanceError::ProposerNotVerified);
        }
        proposal
            .content
            .validate_content()
            .map_err(|error| GovernanceError::Other(error.to_string()))?;

        if proposal.proposer.as_str().is_empty() {
            return Err(GovernanceError::Other(
                "proposal must have a valid proposer".to_string(),
            ));
        }

        if self.proposals.contains_key(&proposal.hash) {
            return Err(GovernanceError::Other("proposal already exists".into()));
        }

        if matches!(proposal.content, ProposalContent::Emergency { .. }) {
            let at = proposal.created_at;
            self.submit_emergency_proposal(&mut proposal, at, params)?;
            return Ok(proposal.hash);
        }
        proposal.phase = GovernancePhase::Proposal;

        let hash = proposal.hash;
        let timing = crate::GovernanceTiming::new(params, &proposal.content);
        timing.counting_at(timing.proposal_end(proposal.created_at)?)?;
        self.timings.insert(hash, timing);
        self.proposals.insert(hash, proposal);
        Ok(hash)
    }

    /// Submit an emergency proposal directly into the single voting window.
    ///
    /// Called after common proposer validation. No genesis or representative
    /// override is granted; governed human participation and approval still apply.
    fn submit_emergency_proposal(
        &mut self,
        proposal: &mut Proposal,
        now: Timestamp,
        params: &ProtocolParams,
    ) -> Result<(), GovernanceError> {
        if !matches!(proposal.content, ProposalContent::Emergency { .. }) {
            return Err(GovernanceError::WrongPhase);
        }
        proposal
            .content
            .validate_content()
            .map_err(|error| GovernanceError::Other(error.to_string()))?;
        let hash = proposal.hash;
        if self.proposals.contains_key(&hash) {
            return Err(GovernanceError::Other("proposal already exists".into()));
        }
        let timing = crate::GovernanceTiming::new(params, &proposal.content);
        timing.counting_at(now)?;
        proposal.phase = GovernancePhase::Exploration;
        proposal.exploration_started_at = Some(now);
        self.timings.insert(hash, timing);
        self.proposals.insert(hash, proposal.clone());
        Ok(())
    }

    pub fn timing(
        &self,
        proposal: &Proposal,
        params: &ProtocolParams,
    ) -> Result<crate::GovernanceTiming, GovernanceError> {
        match self.timings.get(&proposal.hash) {
            Some(timing) => Ok(*timing),
            None if !self.proposals.contains_key(&proposal.hash) => {
                Ok(crate::GovernanceTiming::new(params, &proposal.content))
            }
            None => Err(GovernanceError::Other(
                "stored proposal timing is missing".into(),
            )),
        }
    }

    pub fn counting_time(
        &self,
        proposal: &Proposal,
        params: &ProtocolParams,
    ) -> Result<Timestamp, GovernanceError> {
        let timing = self.timing(proposal, params)?;
        let start = match proposal.exploration_started_at {
            Some(start) => start,
            None => timing.proposal_end(proposal.created_at)?,
        };
        timing.counting_at(start)
    }

    /// Resolve proposal windows before an activation changes qualification
    /// thresholds. Later rules cannot reopen a phase that already ended.
    pub fn close_proposal_windows(
        &mut self,
        now: Timestamp,
        params: &ProtocolParams,
    ) -> Result<(), GovernanceError> {
        let proposals: Vec<_> = self
            .proposals
            .values()
            .filter(|proposal| proposal.phase == GovernancePhase::Proposal)
            .cloned()
            .collect();
        for mut proposal in proposals {
            match self.try_advance_proposal(&mut proposal, now, params) {
                Ok(_) | Err(GovernanceError::InsufficientEndorsements { .. }) => {
                    self.proposals.insert(proposal.hash, proposal);
                }
                Err(GovernanceError::PhaseNotExpired) => (),
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Withdraw a proposal. Only the proposer may withdraw, and only during the Proposal phase.
    ///
    /// Updates the in-memory proposal. The caller authenticates the operation
    /// and persists the resulting state.
    pub fn withdraw(
        &mut self,
        proposal_hash: &TxHash,
        withdrawer: &WalletAddress,
    ) -> Result<(), GovernanceError> {
        let proposal = self
            .proposals
            .get_mut(proposal_hash)
            .ok_or_else(|| GovernanceError::ProposalNotFound(proposal_hash.to_string()))?;
        if proposal.proposer != *withdrawer {
            return Err(GovernanceError::NotProposer);
        }
        if proposal.phase != GovernancePhase::Proposal {
            return Err(GovernanceError::WrongPhase);
        }
        proposal.phase = GovernancePhase::Withdrawn;
        Ok(())
    }

    /// Record an explicit veto after approval and strictly before activation.
    /// Caller authenticates creator identity and the one-way bootstrap window.
    pub fn veto_approved(
        &mut self,
        proposal_hash: &TxHash,
        now: Timestamp,
    ) -> Result<(), GovernanceError> {
        let proposal = self
            .proposals
            .get(proposal_hash)
            .ok_or_else(|| GovernanceError::ProposalNotFound(proposal_hash.to_string()))?;
        let at = proposal.activation_at.ok_or(GovernanceError::WrongPhase)?;
        if !matches!(
            proposal.phase,
            GovernancePhase::Cooldown | GovernancePhase::Activation
        ) || now >= at
            || !proposal
                .cooldown_started_at
                .is_some_and(|approved| now >= approved)
        {
            return Err(GovernanceError::WrongPhase);
        }
        self.proposals
            .get_mut(proposal_hash)
            .expect("validated proposal")
            .phase = GovernancePhase::Vetoed;
        Ok(())
    }

    /// Endorse a proposal (burn BRN to advance past spam filter).
    ///
    /// Each call increments the endorsement count and tracks total BRN burned.
    /// The node validates the signer, deadline, unique endorsement and actual
    /// BRN burn in the same transaction. Voting starts at the proposal deadline.
    pub fn endorse_proposal(
        &mut self,
        proposal_hash: &TxHash,
        brn_burned: u128,
    ) -> Result<(), GovernanceError> {
        let proposal = self
            .proposals
            .get_mut(proposal_hash)
            .ok_or_else(|| GovernanceError::ProposalNotFound(proposal_hash.to_string()))?;

        if proposal.phase != GovernancePhase::Proposal {
            return Err(GovernanceError::WrongPhase);
        }

        let entry = self
            .endorsement_counts
            .entry(*proposal_hash)
            .or_insert((0, 0));
        let count = entry
            .0
            .checked_add(1)
            .ok_or_else(|| GovernanceError::Other("endorsement count overflow".into()))?;
        let burned = entry
            .1
            .checked_add(brn_burned)
            .ok_or_else(|| GovernanceError::Other("endorsement burn overflow".into()))?;
        *entry = (count, burned);

        proposal.endorsement_count = entry.0;

        Ok(())
    }

    /// Record one direct vote in the voting window and update its raw counters.
    /// The caller authenticates the voter; counting uses the supplied electorate
    /// and resolves delegation separately.
    pub fn cast_exploration_vote(
        &mut self,
        proposal_hash: &TxHash,
        voter: &WalletAddress,
        vote: GovernanceVote,
        now: Timestamp,
        params: &ProtocolParams,
    ) -> Result<(), GovernanceError> {
        self.require_voting_window(proposal_hash, now, params)?;
        let votes = self.exploration_votes.entry(*proposal_hash).or_default();
        if votes.contains_key(voter) {
            return Err(GovernanceError::AlreadyVoted(voter.to_string()));
        }
        votes.insert(voter.clone(), vote);
        if let Some(proposal) = self.proposals.get_mut(proposal_hash) {
            match vote {
                GovernanceVote::Yea => proposal.exploration_votes_yea += 1,
                GovernanceVote::Nay => proposal.exploration_votes_nay += 1,
                GovernanceVote::Abstain => proposal.exploration_votes_abstain += 1,
            }
        }
        Ok(())
    }

    /// The caller authenticates the secondary signature and its ledger grant.
    /// Eligibility and current scope authority are resolved again at counting.
    pub fn record_key_ballot(
        &mut self,
        ballot: &crate::signing_delegation::DelegatedBallot,
        now: Timestamp,
        params: &ProtocolParams,
    ) -> Result<(), GovernanceError> {
        self.require_voting_window(&ballot.proposal, now, params)?;
        let key = (ballot.proposal, ballot.owner.clone(), ballot.grant);
        if self.key_ballots.contains_key(&key) {
            return Err(GovernanceError::AlreadyVoted(ballot.owner.to_string()));
        }
        self.key_ballots.insert(key, ballot.vote);
        Ok(())
    }

    pub fn key_ballot(
        &self,
        proposal: TxHash,
        owner: &WalletAddress,
        grant: burst_types::BlockHash,
    ) -> Option<GovernanceVote> {
        self.key_ballots
            .get(&(proposal, owner.clone(), grant))
            .copied()
    }

    fn require_voting_window(
        &self,
        proposal_hash: &TxHash,
        now: Timestamp,
        params: &ProtocolParams,
    ) -> Result<(), GovernanceError> {
        let proposal = self
            .proposals
            .get(proposal_hash)
            .ok_or_else(|| GovernanceError::ProposalNotFound(proposal_hash.to_string()))?;

        if proposal.phase != GovernancePhase::Exploration {
            return Err(GovernanceError::WrongPhase);
        }

        if let Some(started) = proposal.exploration_started_at {
            if now < started {
                return Err(GovernanceError::Other(
                    "vote predates the voting window".into(),
                ));
            }
            if now >= self.timing(proposal, params)?.voting_end(started)? {
                return Err(GovernanceError::VotingClosed);
            }
        }

        Ok(())
    }

    /// Old snapshots may contain Promotion, but BURST has no second vote.
    pub fn cast_promotion_vote(
        &mut self,
        _proposal_hash: &TxHash,
        _voter: &WalletAddress,
        _vote: GovernanceVote,
        _now: Timestamp,
        _params: &ProtocolParams,
    ) -> Result<(), GovernanceError> {
        Err(GovernanceError::WrongPhase)
    }

    /// Get a reference to a stored proposal by hash.
    pub fn get_proposal(&self, hash: &TxHash) -> Option<&Proposal> {
        self.proposals.get(hash)
    }

    /// Iterate over all proposals (any phase).
    pub fn all_proposals(&self) -> impl Iterator<Item = &Proposal> {
        self.proposals.values()
    }

    /// Seek strictly after a proposal hash in O(log P), then iterate in key order.
    pub fn proposals_after(&self, after: Option<TxHash>) -> impl Iterator<Item = &Proposal> {
        use std::ops::Bound;
        let lower = after.map_or(Bound::Unbounded, Bound::Excluded);
        self.proposals
            .range((lower, Bound::Unbounded))
            .map(|(_, proposal)| proposal)
    }

    /// Get a mutable reference to a stored proposal by hash.
    pub fn get_proposal_mut(&mut self, hash: &TxHash) -> Option<&mut Proposal> {
        self.proposals.get_mut(hash)
    }

    /// Return proposals considered by the local tick, including legacy Promotion
    /// records. Promotion remains unadvanceable under the current rules.
    pub fn active_proposal_hashes(&self) -> Vec<TxHash> {
        self.proposals
            .iter()
            .filter(|(_, p)| {
                matches!(
                    p.phase,
                    GovernancePhase::Proposal
                        | GovernancePhase::Exploration
                        | GovernancePhase::Cooldown
                        | GovernancePhase::Promotion
                )
            })
            .map(|(h, _)| *h)
            .collect()
    }

    /// Apply due effects to local parameters, then advance proposal phases.
    /// Returns hashes whose effects applied successfully. This does not certify
    /// outcomes or mark those proposals Activated; repeated calls can queue their
    /// effects again. Certified ledger replay owns final activation and persistence.
    pub fn tick(
        &mut self,
        now: Timestamp,
        params: &mut ProtocolParams,
        delegation: &DelegationEngine,
        verified_wallets: &[WalletAddress],
    ) -> Vec<TxHash> {
        let mut activated = Vec::new();

        // Apply already-due effects before advancing other proposals.
        let mut pending_activation: Vec<TxHash> = self
            .proposals
            .iter()
            .filter(|(_, p)| {
                p.phase == GovernancePhase::Activation
                    && p.activation_at
                        .map(|at| now.as_secs() >= at.as_secs())
                        .unwrap_or(false)
            })
            .map(|(h, _)| *h)
            .collect();

        pending_activation.sort_by_key(|hash| {
            (
                self.proposals[hash]
                    .activation_at
                    .map(|at| at.as_secs())
                    .unwrap_or(u64::MAX),
                *hash.as_bytes(),
            )
        });
        for hash in pending_activation {
            if let Some(proposal) = self.proposals.get(&hash) {
                let p = proposal.clone();
                if self.activate(&p, params).is_ok() {
                    activated.push(hash);
                }
            }
        }

        // Proposals advance independently; support for one does not reject another.
        let hashes = self.active_proposal_hashes();
        for hash in hashes {
            if let Some(proposal) = self.proposals.get_mut(&hash) {
                let mut p = proposal.clone();
                match self.try_advance(&mut p, now, params, Some((delegation, verified_wallets))) {
                    Ok(GovernancePhase::Activation) => {
                        tracing::debug!(
                            proposal = ?hash,
                            phase = ?GovernancePhase::Activation,
                            "governance proposal scheduled for activation"
                        );
                    }
                    Ok(new_phase) => {
                        tracing::debug!(
                            proposal = ?hash,
                            phase = ?new_phase,
                            "governance proposal advanced"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            proposal = ?hash,
                            error = %e,
                            "governance proposal phase transition failed"
                        );
                    }
                }
                if let Some(slot) = self.proposals.get_mut(&hash) {
                    *slot = p;
                }
            }
        }
        activated
    }

    /// Advance local scheduling with a temporary parameter copy. Discard newly
    /// queued effects while preserving the caller's existing queues. This mutates
    /// proposal phases but does not certify a result or commit ledger activation.
    pub fn schedule_activations(
        &mut self,
        now: Timestamp,
        params: &ProtocolParams,
        delegation: &DelegationEngine,
        verified_wallets: &[WalletAddress],
    ) -> Vec<TxHash> {
        let changes = std::mem::take(&mut self.pending_changes);
        let amendments = std::mem::take(&mut self.pending_amendments);
        let evictions = std::mem::take(&mut self.pending_evictions);
        let ready = self.tick(now, &mut params.clone(), delegation, verified_wallets);
        self.pending_changes = changes;
        self.pending_amendments = amendments;
        self.pending_evictions = evictions;
        ready
    }

    /// Get the endorsement count and total BRN burned for a proposal.
    pub fn get_endorsements(&self, hash: &TxHash) -> Option<(u32, u128)> {
        self.endorsement_counts.get(hash).copied()
    }

    /// Get all exploration votes for a proposal.
    pub fn get_exploration_votes(
        &self,
        hash: &TxHash,
    ) -> Option<&OrdMap<WalletAddress, GovernanceVote>> {
        self.exploration_votes.get(hash)
    }

    /// Get all promotion votes for a proposal.
    pub fn get_promotion_votes(
        &self,
        hash: &TxHash,
    ) -> Option<&OrdMap<WalletAddress, GovernanceVote>> {
        self.promotion_votes.get(hash)
    }

    /// Approval threshold under the supplied rules. Amendments and ordinary
    /// changes to ConstiSupermajorityBps use the constitutional threshold.
    /// Emergencies use their own threshold, raised to the constitutional one
    /// when changing ConstiSupermajorityBps. Other proposals use governance approval.
    fn get_required_supermajority(proposal: &Proposal, params: &ProtocolParams) -> u32 {
        match &proposal.content {
            ProposalContent::Emergency { param, .. } => match param {
                crate::params::GovernableParam::ConstiSupermajorityBps => params
                    .emergency_supermajority_bps
                    .max(params.consti_supermajority_bps),
                _ => params.emergency_supermajority_bps,
            },
            ProposalContent::ParameterChange { param, .. } => match param {
                crate::params::GovernableParam::ConstiSupermajorityBps => {
                    params.consti_supermajority_bps
                }
                crate::params::GovernableParam::GovernanceSupermajorityBps => {
                    params.governance_supermajority_bps
                }
                _ => params.governance_supermajority_bps,
            },
            ProposalContent::ConstitutionalAmendment { .. }
            | ProposalContent::ConstitutionalDiff { .. } => params.consti_supermajority_bps,
            // Representative eviction uses ordinary governance thresholds.
            ProposalContent::RepresentativeEviction { .. } => params.governance_supermajority_bps,
        }
    }

    /// The quorum (participation) threshold required for a proposal, in bps.
    ///
    /// Mirrors [`get_required_supermajority`](Self::get_required_supermajority):
    /// constitutional amendments (and changes to the consti thresholds
    /// themselves) require the separately-governable consti quorum
    /// (`consti_quorum_bps`). Emergencies also require the emergency quorum.
    fn get_required_quorum(proposal: &Proposal, params: &ProtocolParams) -> u32 {
        match &proposal.content {
            ProposalContent::Emergency { param, .. } => match param {
                crate::params::GovernableParam::ConstiSupermajorityBps
                | crate::params::GovernableParam::ConstiQuorumBps => {
                    params.emergency_quorum_bps.max(params.consti_quorum_bps)
                }
                _ => params.emergency_quorum_bps,
            },
            ProposalContent::ParameterChange { param, .. } => match param {
                crate::params::GovernableParam::ConstiSupermajorityBps
                | crate::params::GovernableParam::ConstiQuorumBps => params.consti_quorum_bps,
                _ => params.governance_quorum_bps,
            },
            ProposalContent::ConstitutionalAmendment { .. }
            | ProposalContent::ConstitutionalDiff { .. } => params.consti_quorum_bps,
            ProposalContent::RepresentativeEviction { .. } => params.governance_quorum_bps,
        }
    }

    /// Advance a proposal to the next phase if conditions are met.
    ///
    /// `delegation_context` supplies delegation and the electorate for the same
    /// replay cut. With `None`, tally raw counters against stored eligibility;
    /// that fallback does not establish a certified voting result.
    pub fn try_advance(
        &self,
        proposal: &mut Proposal,
        now: Timestamp,
        params: &ProtocolParams,
        delegation_context: Option<(&DelegationEngine, &[WalletAddress])>,
    ) -> Result<GovernancePhase, GovernanceError> {
        match proposal.phase {
            GovernancePhase::Proposal => self.try_advance_proposal(proposal, now, params),
            GovernancePhase::Exploration => {
                self.try_advance_exploration(proposal, now, params, delegation_context)
            }
            GovernancePhase::Cooldown => self.try_advance_cooldown(proposal, now, params),
            _ => Err(GovernanceError::WrongPhase),
        }
    }

    /// Return (yea, nay, abstain, eligible) for the supplied replay cut. With
    /// delegation, deduplicate the electorate and resolve one vote per member.
    /// Without it, use the proposal's stored counters and eligibility.
    fn tally_inputs(
        &self,
        proposal: &Proposal,
        phase: GovernancePhase,
        delegation_context: Option<(&DelegationEngine, &[WalletAddress])>,
    ) -> (u32, u32, u32, u32) {
        match delegation_context {
            Some((delegation, verified_wallets)) => {
                let eligible: std::collections::HashSet<_> = verified_wallets.iter().collect();
                let empty = OrdMap::new();
                let direct = if phase == GovernancePhase::Promotion {
                    self.promotion_votes.get(&proposal.hash)
                } else {
                    Some(self.exploration_votes.get(&proposal.hash).unwrap_or(&empty))
                };
                let (yea, nay, abstain) = match (self.get_proposal(&proposal.hash), direct) {
                    (Some(stored), Some(direct)) => {
                        Self::count_votes_in_electorate(direct, delegation, &eligible, Some(stored))
                    }
                    _ => (0, 0, 0),
                };
                (yea, nay, abstain, eligible.len() as u32)
            }
            None if phase == GovernancePhase::Promotion => (
                proposal.promotion_votes_yea,
                proposal.promotion_votes_nay,
                proposal.promotion_votes_abstain,
                proposal.total_eligible_voters,
            ),
            None => (
                proposal.exploration_votes_yea,
                proposal.exploration_votes_nay,
                proposal.exploration_votes_abstain,
                proposal.total_eligible_voters,
            ),
        }
    }

    /// Proposal → Exploration: endorsements threshold met + duration elapsed.
    fn try_advance_proposal(
        &self,
        proposal: &mut Proposal,
        now: Timestamp,
        params: &ProtocolParams,
    ) -> Result<GovernancePhase, GovernanceError> {
        let end = self
            .timing(proposal, params)?
            .proposal_end(proposal.created_at)?;
        let duration_elapsed = now >= end;
        let endorsements_met =
            proposal.endorsement_count >= params.governance_proposal_endorsements;

        if !duration_elapsed {
            return Err(GovernanceError::PhaseNotExpired);
        }
        if endorsements_met {
            proposal.phase = GovernancePhase::Exploration;
            proposal.exploration_started_at = Some(end);
            Ok(GovernancePhase::Exploration)
        } else {
            proposal.phase = GovernancePhase::Rejected;
            Err(GovernanceError::InsufficientEndorsements {
                have: proposal.endorsement_count,
                need: params.governance_proposal_endorsements,
            })
        }
    }

    /// Voting → Cooldown (normal) or Activation (emergency).
    fn try_advance_exploration(
        &self,
        proposal: &mut Proposal,
        now: Timestamp,
        params: &ProtocolParams,
        delegation_context: Option<(&DelegationEngine, &[WalletAddress])>,
    ) -> Result<GovernancePhase, GovernanceError> {
        let is_emergency = matches!(proposal.content, ProposalContent::Emergency { .. });

        let exploration_started = proposal
            .exploration_started_at
            .ok_or_else(|| GovernanceError::Other("exploration_started_at not set".to_string()))?;

        let timing = self.timing(proposal, params)?;
        let voting_ended = now >= timing.voting_end(exploration_started)?;

        if !voting_ended {
            return Err(GovernanceError::PhaseNotExpired);
        }

        let counting_time = timing.counting_at(exploration_started)?.as_secs();
        if now.as_secs() < counting_time {
            return Err(GovernanceError::PropagationBuffer);
        }

        let quorum_bps = Self::get_required_quorum(proposal, params);

        let supermajority_bps = Self::get_required_supermajority(proposal, params);

        let (yea, nay, abstain, eligible) =
            self.tally_inputs(proposal, GovernancePhase::Exploration, delegation_context);
        // Closed proposals retain the effective result, not just direct votes.
        proposal.exploration_votes_yea = yea;
        proposal.exploration_votes_nay = nay;
        proposal.exploration_votes_abstain = abstain;
        proposal.total_eligible_voters = eligible;
        self.check_vote_result(
            yea,
            nay,
            abstain,
            eligible,
            quorum_bps,
            supermajority_bps,
            proposal,
            now,
            params,
        )?;

        if is_emergency {
            proposal.phase = GovernancePhase::Activation;
            proposal.activation_at = Some(Timestamp::new(
                counting_time.saturating_add(activation_delay_secs(params)),
            ));
            Ok(GovernancePhase::Activation)
        } else {
            proposal.phase = GovernancePhase::Cooldown;
            proposal.cooldown_started_at = Some(Timestamp::new(counting_time));
            Ok(GovernancePhase::Cooldown)
        }
    }

    /// Cooldown → Activation: no second vote.
    fn try_advance_cooldown(
        &self,
        proposal: &mut Proposal,
        now: Timestamp,
        params: &ProtocolParams,
    ) -> Result<GovernancePhase, GovernanceError> {
        // Certified ballots freeze their complete activation schedule. Later
        // parameter changes cannot move this proposal's agreed timestamp.
        if let Some(activation_at) = proposal.activation_at {
            if now < activation_at {
                return Err(GovernanceError::PhaseNotExpired);
            }
            proposal.phase = GovernancePhase::Activation;
            return Ok(GovernancePhase::Activation);
        }
        let cooldown_started = proposal
            .cooldown_started_at
            .ok_or_else(|| GovernanceError::Other("cooldown_started_at not set".to_string()))?;

        let duration_elapsed =
            cooldown_started.has_expired(params.governance_cooldown_duration_secs, now);

        if duration_elapsed {
            proposal.phase = GovernancePhase::Activation;
            proposal.activation_at = Some(Timestamp::new(
                cooldown_started
                    .as_secs()
                    .saturating_add(params.governance_cooldown_duration_secs)
                    .saturating_add(activation_delay_secs(params)),
            ));
            Ok(GovernancePhase::Activation)
        } else {
            Err(GovernanceError::PhaseNotExpired)
        }
    }

    /// Check quorum and supermajority for a vote phase.
    /// Failure rejects the proposal without erasing its vote record.
    #[allow(clippy::too_many_arguments)]
    fn check_vote_result(
        &self,
        votes_yea: u32,
        votes_nay: u32,
        votes_abstain: u32,
        total_eligible_voters: u32,
        quorum_bps: u32,
        supermajority_bps: u32,
        proposal: &mut Proposal,
        now: Timestamp,
        params: &ProtocolParams,
    ) -> Result<(), GovernanceError> {
        let total_votes = u64::from(votes_yea) + u64::from(votes_nay) + u64::from(votes_abstain);
        let participation_bps = (total_votes * 10000)
            .checked_div(u64::from(total_eligible_voters))
            .unwrap_or(0)
            .min(u64::from(u32::MAX)) as u32;

        if participation_bps < quorum_bps {
            self.fail_proposal(proposal, now, params);
            return Err(GovernanceError::QuorumNotMet {
                have_bps: participation_bps,
                need_bps: quorum_bps,
            });
        }

        let total_yea_nay = u64::from(votes_yea) + u64::from(votes_nay);
        let approval_bps = (u64::from(votes_yea) * 10000)
            .checked_div(total_yea_nay)
            .unwrap_or(0) as u32;

        if approval_bps < supermajority_bps {
            self.fail_proposal(proposal, now, params);
            return Err(GovernanceError::SupermajorityNotMet {
                have_bps: approval_bps,
                need_bps: supermajority_bps,
            });
        }

        Ok(())
    }

    /// A failed vote is final. Resubmission needs a new proposal and endorsements.
    fn fail_proposal(&self, proposal: &mut Proposal, _now: Timestamp, _params: &ProtocolParams) {
        proposal.phase = GovernancePhase::Rejected;
    }

    /// Apply a parameter change from the given param and value.
    fn apply_param_change(
        param: &crate::params::GovernableParam,
        new_value: u128,
        params: &mut ProtocolParams,
    ) -> Result<(), GovernanceError> {
        param.apply(params, new_value)
    }

    /// Apply content to the supplied parameters and queue subsystem effects.
    /// This checks content, not approval, timing, signatures or prior activation.
    /// The caller validates those conditions and records the terminal phase.
    pub fn activate(
        &mut self,
        proposal: &Proposal,
        params: &mut ProtocolParams,
    ) -> Result<(), GovernanceError> {
        proposal
            .content
            .validate_content()
            .map_err(|error| GovernanceError::Other(error.to_string()))?;
        match &proposal.content {
            ProposalContent::ParameterChange { param, new_value } => {
                Self::apply_param_change(param, *new_value, params)?;
                Arc::make_mut(&mut self.pending_changes).push((param.clone(), *new_value));
                Ok(())
            }
            ProposalContent::ConstitutionalAmendment { .. }
            | ProposalContent::ConstitutionalDiff { .. } => {
                Arc::make_mut(&mut self.pending_amendments).push(proposal.content.clone());
                Ok(())
            }
            ProposalContent::Emergency {
                param, new_value, ..
            } => {
                Self::apply_param_change(param, *new_value, params)?;
                Arc::make_mut(&mut self.pending_changes).push((param.clone(), *new_value));
                Ok(())
            }
            ProposalContent::RepresentativeEviction { target, evict, .. } => {
                // Recorded for the node to apply to account state (sets/clears
                // `orv_evicted`). No ProtocolParams change.
                Arc::make_mut(&mut self.pending_evictions).push((target.clone(), *evict));
                Ok(())
            }
        }
    }

    /// Return the parameters after activation without applying or queuing effects.
    /// Authoring a candidate is not an on-chain activation.
    pub fn preview_activation(
        &self,
        proposal: &Proposal,
        params: &ProtocolParams,
    ) -> Result<ProtocolParams, GovernanceError> {
        let mut staged = self.clone();
        let mut next = params.clone();
        staged.activate(proposal, &mut next)?;
        Ok(next)
    }

    /// Apply effects only if the resulting parameter hash matches the commitment.
    /// A mismatch leaves this engine and the supplied parameters unchanged.
    /// Approval, timing and ledger authority remain caller responsibilities.
    pub fn activate_checked(
        &mut self,
        proposal: &Proposal,
        params: &mut ProtocolParams,
        expected: &burst_types::BlockHash,
    ) -> Result<(), GovernanceError> {
        let mut staged = self.clone();
        let mut next = params.clone();
        staged.activate(proposal, &mut next)?;
        if next.params_hash() != *expected {
            return Err(GovernanceError::Other(
                "activation parameter hash mismatch".into(),
            ));
        }
        *self = staged;
        *params = next;
        Ok(())
    }

    /// Take queued parameter changes. Certified replay applies their economic
    /// effects in the staged ledger at the proposal's activation timestamp.
    pub fn drain_pending_changes(&mut self) -> Vec<(crate::params::GovernableParam, u128)> {
        Arc::unwrap_or_clone(std::mem::take(&mut self.pending_changes))
    }

    /// Drain activated constitutional amendments that need to be applied by the node.
    pub fn drain_activated_amendments(&mut self) -> Vec<ProposalContent> {
        Arc::unwrap_or_clone(std::mem::take(&mut self.pending_amendments))
    }

    /// Drain activated ORV evictions/reinstatements. Each `(target, evict)` tells
    /// the node to set (`evict = true`) or clear (`false`) the target account's
    /// `orv_evicted` flag, removing/restoring it from the consensus weight set.
    pub fn drain_pending_evictions(&mut self) -> Vec<(WalletAddress, bool)> {
        Arc::unwrap_or_clone(std::mem::take(&mut self.pending_evictions))
    }

    /// Count effective votes for a proposal, including transitively delegated votes.
    ///
    /// Each eligible wallet uses its direct vote first, then its projected key
    /// ballot, then ordinary delegation. An explicit empty key projection blocks
    /// ordinary fallback. A delegation chain contributes its terminal's direct vote.
    pub fn count_effective_exploration_votes(
        &self,
        proposal_hash: &TxHash,
        delegation_engine: &DelegationEngine,
        all_verified_wallets: &[WalletAddress],
    ) -> (u32, u32, u32) {
        let empty = OrdMap::new();
        let direct_votes = self.exploration_votes.get(proposal_hash).unwrap_or(&empty);

        let Some(proposal) = self.get_proposal(proposal_hash) else {
            return (0, 0, 0);
        };
        Self::count_votes_with_delegation(
            direct_votes,
            delegation_engine,
            all_verified_wallets,
            Some(proposal),
        )
    }

    /// Tally retained Promotion records. This does not enable a second ballot.
    pub fn count_effective_promotion_votes(
        &self,
        proposal_hash: &TxHash,
        delegation_engine: &DelegationEngine,
        all_verified_wallets: &[WalletAddress],
    ) -> (u32, u32, u32) {
        let direct_votes = match self.promotion_votes.get(proposal_hash) {
            Some(votes) => votes,
            None => return (0, 0, 0),
        };

        let Some(proposal) = self.get_proposal(proposal_hash) else {
            return (0, 0, 0);
        };
        Self::count_votes_with_delegation(
            direct_votes,
            delegation_engine,
            all_verified_wallets,
            Some(proposal),
        )
    }

    /// Only the supplied verified electorate contributes votes. Stored votes or
    /// delegation pointers do not grant a person eligibility by themselves.
    fn count_votes_with_delegation(
        direct_votes: &OrdMap<WalletAddress, GovernanceVote>,
        delegation_engine: &DelegationEngine,
        all_verified_wallets: &[WalletAddress],
        proposal: Option<&Proposal>,
    ) -> (u32, u32, u32) {
        let eligible = all_verified_wallets.iter().collect();
        Self::count_votes_in_electorate(direct_votes, delegation_engine, &eligible, proposal)
    }

    fn count_votes_in_electorate(
        direct_votes: &OrdMap<WalletAddress, GovernanceVote>,
        delegation_engine: &DelegationEngine,
        eligible: &std::collections::HashSet<&WalletAddress>,
        proposal: Option<&Proposal>,
    ) -> (u32, u32, u32) {
        let parameter = proposal.and_then(|p| match &p.content {
            ProposalContent::ParameterChange { param, .. }
            | ProposalContent::Emergency { param, .. } => Some(param),
            _ => None,
        });
        let category = parameter.map(|p| p.category()).or_else(|| {
            proposal.map(|p| match &p.content {
                ProposalContent::ConstitutionalAmendment { .. }
                | ProposalContent::ConstitutionalDiff { .. } => "constitution",
                ProposalContent::RepresentativeEviction { .. } => "consensus",
                _ => unreachable!("parameter content handled above"),
            })
        });
        let mut resolver = crate::vote_delegation::BallotResolver::new(
            delegation_engine,
            proposal.map(|p| &p.hash),
            parameter,
            category,
            |person| eligible.contains(person),
        );
        let (mut yea, mut nay, mut abstain) = (0, 0, 0);
        for wallet in eligible {
            let vote = direct_votes.get(*wallet).copied().or_else(|| {
                if let Some(vote) = delegation_engine.key_ballot(wallet) {
                    return vote;
                }
                let delegate = resolver.resolve(wallet)?;
                if !eligible.contains(&delegate) {
                    return None;
                }
                direct_votes.get(&delegate).copied()
            });
            match vote {
                Some(GovernanceVote::Yea) => yea += 1,
                Some(GovernanceVote::Nay) => nay += 1,
                Some(GovernanceVote::Abstain) => abstain += 1,
                None => {}
            }
        }
        (yea, nay, abstain)
    }

    /// Compute participation in basis points from vote counts and eligible voter total.
    ///
    pub fn compute_participation_bps(
        votes_yea: u32,
        votes_nay: u32,
        votes_abstain: u32,
        total_eligible_voters: u32,
    ) -> u32 {
        if total_eligible_voters == 0 {
            return 0;
        }
        let total_votes = votes_yea as u64 + votes_nay as u64 + votes_abstain as u64;
        ((total_votes * 10000) / total_eligible_voters as u64) as u32
    }

    /// Update participation EMA: (8 * previous + 2 * current) / 10, rounded down.
    ///
    /// This historical statistic does not set the governed quorum threshold.
    pub fn update_ema(params: &mut ProtocolParams, participation_bps: u32) {
        let old = params.governance_ema_participation_bps as u64;
        let current = participation_bps as u64;
        let new_ema = (8 * old + 2 * current) / 10;
        params.governance_ema_participation_bps = new_ema as u32;
    }

    /// List support-collection and voting proposals by descending endorsements.
    /// This display ordering does not choose a winner or exclude other proposals.
    pub fn competing_proposals(&self) -> Vec<&Proposal> {
        let mut proposals: Vec<&Proposal> = self
            .proposals
            .values()
            .filter(|p| {
                p.phase == GovernancePhase::Proposal || p.phase == GovernancePhase::Exploration
            })
            .collect();
        proposals.sort_by_key(|b| std::cmp::Reverse(b.endorsement_count));
        proposals
    }

    /// Snapshot the eligible voter count for a proposal entering a vote phase.
    /// Used by direct-counter tallies when no delegation context is supplied.
    pub fn snapshot_eligible_voters(
        &mut self,
        proposal_hash: &TxHash,
        voter_count: u32,
    ) -> Result<(), GovernanceError> {
        let proposal = self
            .proposals
            .get_mut(proposal_hash)
            .ok_or_else(|| GovernanceError::ProposalNotFound(proposal_hash.to_string()))?;
        proposal.total_eligible_voters = voter_count;
        Ok(())
    }
}

impl Default for GovernanceEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    mod persistent;
    mod snapshot_undo;
    use super::*;
    use crate::proposal::ProposalContent;
    use burst_types::{ProtocolParams, WalletAddress};

    fn dummy_wallet() -> WalletAddress {
        WalletAddress::new(
            "brst_1111111111111111111111111111111111111111111111111111111111111111111111111111",
        )
    }

    fn other_wallet() -> WalletAddress {
        WalletAddress::new(
            "brst_2222222222222222222222222222222222222222222222222222222222222222222222222222",
        )
    }

    fn dummy_hash() -> TxHash {
        TxHash::new([0u8; 32])
    }

    fn default_params() -> ProtocolParams {
        ProtocolParams::default()
    }

    fn make_proposal(created_at: u64, endorsement_count: u32) -> Proposal {
        Proposal {
            hash: dummy_hash(),
            proposer: dummy_wallet(),
            phase: GovernancePhase::Proposal,
            content: ProposalContent::ParameterChange {
                param: crate::params::GovernableParam::BrnRate,
                new_value: 1000,
            },
            endorsement_count,
            total_eligible_voters: 100,
            exploration_started_at: None,
            exploration_votes_yea: 0,
            exploration_votes_nay: 0,
            exploration_votes_abstain: 0,
            cooldown_started_at: None,
            promotion_started_at: None,
            promotion_votes_yea: 0,
            promotion_votes_nay: 0,
            promotion_votes_abstain: 0,
            round: 0,
            created_at: Timestamp::new(created_at),
            activation_at: None,
        }
    }

    #[test]
    fn constitutional_diffs_use_constitution_thresholds_and_queue_exact_operations() {
        let mut engine = GovernanceEngine::new();
        let mut proposal = make_proposal(1, 10);
        proposal.content = ProposalContent::ConstitutionalDiff {
            title: "Update rights".into(),
            operations: vec![crate::proposal::AmendmentOp::ModifyArticle {
                article_number: 1,
                new_text: "Equal votes".into(),
            }],
        };
        let mut params = default_params();
        params.consti_supermajority_bps = 9100;
        params.consti_quorum_bps = 3100;
        assert_eq!(
            GovernanceEngine::get_required_supermajority(&proposal, &params),
            9100
        );
        assert_eq!(
            GovernanceEngine::get_required_quorum(&proposal, &params),
            3100
        );
        let hash = params.params_hash();
        engine
            .activate_checked(&proposal, &mut params, &hash)
            .unwrap();
        assert_eq!(params.params_hash(), hash);
        assert_eq!(engine.drain_activated_amendments(), vec![proposal.content]);
        assert!(engine.drain_pending_changes().is_empty());
    }

    #[test]
    fn proposal_content_limits_reject_before_mutating_governance() {
        let mut engine = GovernanceEngine::new();
        let mut proposal = make_proposal(1, 0);
        proposal.content = ProposalContent::ConstitutionalAmendment {
            title: "憲法".into(),
            text: "🌍".repeat(crate::content::MAX_AMENDMENT_CHARS + 1),
        };
        assert!(engine
            .submit_proposal(proposal.clone(), 0, true, &default_params())
            .is_err());
        assert!(engine.get_proposal(&proposal.hash).is_none());
        if let ProposalContent::ConstitutionalAmendment { text, .. } = &mut proposal.content {
            text.pop();
        }
        engine
            .submit_proposal(proposal.clone(), 0, true, &default_params())
            .unwrap();
        assert_eq!(
            engine.get_proposal(&proposal.hash).unwrap().content,
            proposal.content
        );

        let mut emergency = make_emergency_proposal(2);
        emergency.hash = TxHash::new([7; 32]);
        if let ProposalContent::Emergency { description, .. } = &mut emergency.content {
            *description = "x".repeat(crate::content::MAX_CONTENT_BYTES);
        }
        assert!(engine
            .submit_emergency_proposal(&mut emergency, Timestamp::new(2), &default_params())
            .is_err());
        assert_eq!(emergency.phase, GovernancePhase::Proposal);
        assert!(engine.get_proposal(&emergency.hash).is_none());
    }

    #[test]
    fn preview_and_invalid_activation_hash_never_queue_effects() {
        let mut engine = GovernanceEngine::new();
        let mut params = default_params();
        let original_hash = params.params_hash();
        let proposal = make_proposal(1, 10);
        let preview = engine.preview_activation(&proposal, &params).unwrap();
        assert_eq!(preview.brn_rate, 1000);
        assert_eq!(params.params_hash(), original_hash);
        assert!(engine.drain_pending_changes().is_empty());
        assert!(engine
            .activate_checked(&proposal, &mut params, &burst_types::BlockHash::ZERO)
            .is_err());
        assert_eq!(params.params_hash(), original_hash);
        assert!(engine.drain_pending_changes().is_empty());
        engine
            .activate_checked(&proposal, &mut params, &preview.params_hash())
            .unwrap();
        assert_eq!(params.brn_rate, 1000);
        assert_eq!(engine.drain_pending_changes().len(), 1);
    }

    #[test]
    fn preview_does_not_queue_constitution_or_eviction_effects() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let mut proposal = make_proposal(1, 10);
        proposal.content = ProposalContent::ConstitutionalAmendment {
            title: "test".into(),
            text: "test".into(),
        };
        engine.preview_activation(&proposal, &params).unwrap();
        assert!(engine.drain_activated_amendments().is_empty());
        assert!(engine.drain_pending_evictions().is_empty());
    }

    fn make_emergency_proposal(created_at: u64) -> Proposal {
        Proposal {
            hash: dummy_hash(),
            proposer: dummy_wallet(),
            phase: GovernancePhase::Proposal,
            content: ProposalContent::Emergency {
                description: "Critical rate fix".to_string(),
                param: crate::params::GovernableParam::BrnRate,
                new_value: 500,
            },
            endorsement_count: 0,
            total_eligible_voters: 100,
            exploration_started_at: None,
            exploration_votes_yea: 0,
            exploration_votes_nay: 0,
            exploration_votes_abstain: 0,
            cooldown_started_at: None,
            promotion_started_at: None,
            promotion_votes_yea: 0,
            promotion_votes_nay: 0,
            promotion_votes_abstain: 0,
            round: 0,
            created_at: Timestamp::new(created_at),
            activation_at: None,
        }
    }

    fn unique_hash(seed: u8) -> TxHash {
        TxHash::new([seed; 32])
    }

    fn voter_wallet(id: u32) -> WalletAddress {
        WalletAddress::new(format!("brst_{:0>75}", id))
    }

    /// Submit as a verified proposer with default parameters.
    fn submit(engine: &mut GovernanceEngine, proposal: Proposal) -> TxHash {
        let params = default_params();
        engine
            .submit_proposal(proposal, u128::MAX, true, &params)
            .unwrap()
    }

    #[test]
    fn proposal_keyset_pages_survive_mutations_before_the_boundary() {
        let mut engine = GovernanceEngine::new();
        for n in [2u8, 4, 6, 8] {
            let mut proposal = make_proposal(1, 10);
            proposal.hash = TxHash::new([n; 32]);
            engine.proposals.insert(proposal.hash, proposal);
        }
        let first: Vec<_> = engine
            .proposals_after(None)
            .take(2)
            .map(|p| p.hash)
            .collect();
        assert_eq!(first, vec![TxHash::new([2; 32]), TxHash::new([4; 32])]);
        engine.proposals.remove(&first[0]);
        engine.proposals.remove(&first[1]);
        for n in [1u8, 5] {
            let mut proposal = make_proposal(1, 10);
            proposal.hash = TxHash::new([n; 32]);
            engine.proposals.insert(proposal.hash, proposal);
        }
        let next: Vec<_> = engine
            .proposals_after(Some(first[1]))
            .map(|p| p.hash)
            .collect();
        assert_eq!(
            next,
            vec![
                TxHash::new([5; 32]),
                TxHash::new([6; 32]),
                TxHash::new([8; 32])
            ]
        );
        assert_eq!(
            engine.proposals_after(Some(TxHash::new([255; 32]))).count(),
            0
        );
    }

    #[test]
    fn test_submit_proposal() {
        let mut engine = GovernanceEngine::new();
        let proposal = make_proposal(1000, 0);
        let params = default_params();
        let result = engine.submit_proposal(proposal, u128::MAX, true, &params);
        assert!(result.is_ok());
        let hash = result.unwrap();
        assert!(engine.get_proposal(&hash).is_some());
    }

    #[test]
    fn test_submit_proposal_returns_hash() {
        let mut engine = GovernanceEngine::new();
        let proposal = make_proposal(1000, 0);
        let expected_hash = proposal.hash;
        let hash = submit(&mut engine, proposal);
        assert_eq!(hash, expected_hash);
    }

    #[test]
    fn test_submit_proposal_sets_phase() {
        let mut engine = GovernanceEngine::new();
        let mut proposal = make_proposal(1000, 0);
        proposal.phase = GovernancePhase::Exploration; // intentionally wrong
        let hash = submit(&mut engine, proposal);
        let stored = engine.get_proposal(&hash).unwrap();
        assert_eq!(stored.phase, GovernancePhase::Proposal);
    }

    #[test]
    fn test_endorse_proposal() {
        let mut engine = GovernanceEngine::new();
        let proposal = make_proposal(1000, 0);
        let hash = submit(&mut engine, proposal);
        assert!(engine.endorse_proposal(&hash, 1000).is_ok());

        let (count, burned) = engine.get_endorsements(&hash).unwrap();
        assert_eq!(count, 1);
        assert_eq!(burned, 1000);
    }

    #[test]
    fn test_endorse_increments_count() {
        let mut engine = GovernanceEngine::new();
        let proposal = make_proposal(1000, 0);
        let hash = submit(&mut engine, proposal);

        engine.endorse_proposal(&hash, 500).unwrap();
        engine.endorse_proposal(&hash, 300).unwrap();
        engine.endorse_proposal(&hash, 200).unwrap();

        let (count, burned) = engine.get_endorsements(&hash).unwrap();
        assert_eq!(count, 3);
        assert_eq!(burned, 1000);

        let stored = engine.get_proposal(&hash).unwrap();
        assert_eq!(stored.endorsement_count, 3);
    }

    #[test]
    fn test_endorse_wrong_phase() {
        let mut engine = GovernanceEngine::new();
        let mut proposal = make_proposal(1000, 0);
        proposal.phase = GovernancePhase::Proposal;
        let hash = submit(&mut engine, proposal);

        // Force phase change to Exploration to test rejection
        engine.get_proposal_mut(&hash).unwrap().phase = GovernancePhase::Exploration;

        let result = engine.endorse_proposal(&hash, 1000);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), GovernanceError::WrongPhase));
    }

    #[test]
    fn test_endorse_proposal_not_found() {
        let mut engine = GovernanceEngine::new();
        let fake_hash = unique_hash(99);
        let result = engine.endorse_proposal(&fake_hash, 1000);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            GovernanceError::ProposalNotFound(_)
        ));
    }

    #[test]
    fn test_cast_exploration_vote() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let mut proposal = make_proposal(1000, 0);
        proposal.phase = GovernancePhase::Exploration;
        proposal.exploration_started_at = Some(Timestamp::new(1000));
        let hash = submit(&mut engine, proposal);

        // Force the phase (submit resets to Proposal)
        engine.get_proposal_mut(&hash).unwrap().phase = GovernancePhase::Exploration;

        let voter = voter_wallet(1);
        assert!(engine
            .cast_exploration_vote(
                &hash,
                &voter,
                GovernanceVote::Yea,
                Timestamp::new(1000),
                &params
            )
            .is_ok());

        let stored = engine.get_proposal(&hash).unwrap();
        assert_eq!(stored.exploration_votes_yea, 1);
    }

    #[test]
    fn delegation_resolves_into_exploration_tally() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let proposal = make_proposal(1000, 0);
        let hash = submit(&mut engine, proposal);
        engine.get_proposal_mut(&hash).unwrap().phase = GovernancePhase::Exploration;

        let a = voter_wallet(1);
        let b = voter_wallet(2);
        let c = voter_wallet(3);

        // A votes directly; B and C delegate without casting their own votes.
        engine
            .cast_exploration_vote(
                &hash,
                &a,
                GovernanceVote::Yea,
                Timestamp::new(1000),
                &params,
            )
            .unwrap();
        let mut delegation = crate::vote_delegation::DelegationEngine::new(10);
        delegation.delegate(&b, &a).unwrap();
        delegation.delegate(&c, &a).unwrap();

        let proposal = engine.get_proposal(&hash).unwrap();

        // No delegation context → only A's direct vote is counted.
        let (yea, _, _, _) = engine.tally_inputs(proposal, GovernancePhase::Exploration, None);
        assert_eq!(yea, 1);

        // The supplied electorate includes ten accounts; A, B and C contribute yea.
        let (yea, nay, abstain, eligible) = engine.tally_inputs(
            proposal,
            GovernancePhase::Exploration,
            Some((&delegation, &(1..=10).map(voter_wallet).collect::<Vec<_>>())),
        );
        assert_eq!((yea, nay, abstain), (3, 0, 0));
        assert_eq!(eligible, 10);
    }

    #[test]
    fn test_cast_exploration_vote_duplicate_rejected() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let proposal = make_proposal(1000, 0);
        let hash = submit(&mut engine, proposal);
        engine.get_proposal_mut(&hash).unwrap().phase = GovernancePhase::Exploration;

        let voter = voter_wallet(1);
        engine
            .cast_exploration_vote(
                &hash,
                &voter,
                GovernanceVote::Yea,
                Timestamp::new(1000),
                &params,
            )
            .unwrap();

        let result = engine.cast_exploration_vote(
            &hash,
            &voter,
            GovernanceVote::Nay,
            Timestamp::new(1001),
            &params,
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            GovernanceError::AlreadyVoted(_)
        ));
    }

    #[test]
    fn test_cast_exploration_vote_wrong_phase() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let proposal = make_proposal(1000, 0);
        let hash = submit(&mut engine, proposal);
        // Phase is Proposal, not Exploration

        let voter = voter_wallet(1);
        let result = engine.cast_exploration_vote(
            &hash,
            &voter,
            GovernanceVote::Yea,
            Timestamp::new(1000),
            &params,
        );
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), GovernanceError::WrongPhase));
    }

    #[test]
    fn test_cast_exploration_vote_counts() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let proposal = make_proposal(1000, 0);
        let hash = submit(&mut engine, proposal);
        engine.get_proposal_mut(&hash).unwrap().phase = GovernancePhase::Exploration;

        engine
            .cast_exploration_vote(
                &hash,
                &voter_wallet(1),
                GovernanceVote::Yea,
                Timestamp::new(1000),
                &params,
            )
            .unwrap();
        engine
            .cast_exploration_vote(
                &hash,
                &voter_wallet(2),
                GovernanceVote::Nay,
                Timestamp::new(1000),
                &params,
            )
            .unwrap();
        engine
            .cast_exploration_vote(
                &hash,
                &voter_wallet(3),
                GovernanceVote::Abstain,
                Timestamp::new(1000),
                &params,
            )
            .unwrap();
        engine
            .cast_exploration_vote(
                &hash,
                &voter_wallet(4),
                GovernanceVote::Yea,
                Timestamp::new(1000),
                &params,
            )
            .unwrap();

        let stored = engine.get_proposal(&hash).unwrap();
        assert_eq!(stored.exploration_votes_yea, 2);
        assert_eq!(stored.exploration_votes_nay, 1);
        assert_eq!(stored.exploration_votes_abstain, 1);
    }

    #[test]
    fn test_legacy_promotion_cannot_accept_votes() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let hash = submit(&mut engine, make_proposal(1000, 0));
        engine.get_proposal_mut(&hash).unwrap().phase = GovernancePhase::Promotion;
        assert!(matches!(
            engine.cast_promotion_vote(
                &hash,
                &voter_wallet(1),
                GovernanceVote::Yea,
                Timestamp::new(1000),
                &params
            ),
            Err(GovernanceError::WrongPhase)
        ));
        assert!(engine.get_promotion_votes(&hash).is_none());
    }

    #[test]
    fn test_voting_refuses_cooldown_and_activation() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let hash = submit(&mut engine, make_proposal(1000, 0));
        for phase in [
            GovernancePhase::Cooldown,
            GovernancePhase::Activation,
            GovernancePhase::Rejected,
        ] {
            engine.get_proposal_mut(&hash).unwrap().phase = phase;
            assert!(matches!(
                engine.cast_exploration_vote(
                    &hash,
                    &voter_wallet(1),
                    GovernanceVote::Yea,
                    Timestamp::new(1000),
                    &params
                ),
                Err(GovernanceError::WrongPhase)
            ));
        }
        assert!(engine.get_exploration_votes(&hash).is_none());
    }

    #[test]
    fn test_try_advance_proposal_to_exploration() {
        let engine = GovernanceEngine::new();
        let params = default_params();
        let mut proposal = make_proposal(1000, params.governance_proposal_endorsements);

        // Before duration elapsed — should fail
        let now = Timestamp::new(
            proposal.created_at.as_secs() + params.governance_proposal_duration_secs - 1,
        );
        assert!(engine
            .try_advance(&mut proposal, now, &params, None)
            .is_err());

        // After duration elapsed — should advance to Exploration
        let now = Timestamp::new(
            proposal.created_at.as_secs() + params.governance_proposal_duration_secs,
        );
        let result = engine.try_advance(&mut proposal, now, &params, None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), GovernancePhase::Exploration);
        assert_eq!(proposal.phase, GovernancePhase::Exploration);
        assert!(proposal.exploration_started_at.is_some());
    }

    #[test]
    fn test_try_advance_exploration_to_cooldown() {
        let engine = GovernanceEngine::new();
        let params = default_params();
        let exploration_started = Timestamp::new(1000);
        let mut proposal = make_proposal(0, 10);
        proposal.phase = GovernancePhase::Exploration;
        proposal.exploration_started_at = Some(exploration_started);
        proposal.exploration_votes_yea = 85;
        proposal.exploration_votes_nay = 15;
        proposal.exploration_votes_abstain = 0;

        // Before duration elapsed — should fail
        let now = Timestamp::new(
            exploration_started.as_secs() + params.governance_exploration_duration_secs - 1,
        );
        assert!(engine
            .try_advance(&mut proposal, now, &params, None)
            .is_err());

        // After duration elapsed but in propagation buffer — should fail
        let now = Timestamp::new(
            exploration_started.as_secs() + params.governance_exploration_duration_secs,
        );
        assert!(matches!(
            engine
                .try_advance(&mut proposal, now, &params, None)
                .unwrap_err(),
            GovernanceError::PropagationBuffer
        ));

        // After duration + propagation buffer — should advance to Cooldown
        let now = Timestamp::new(
            exploration_started.as_secs()
                + params.governance_exploration_duration_secs
                + params.governance_propagation_buffer_secs,
        );
        let result = engine.try_advance(&mut proposal, now, &params, None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), GovernancePhase::Cooldown);
        assert_eq!(proposal.phase, GovernancePhase::Cooldown);
        assert!(proposal.cooldown_started_at.is_some());
    }

    #[test]
    fn test_try_advance_exploration_rejects_on_quorum_failure() {
        let engine = GovernanceEngine::new();
        let params = default_params();
        let exploration_started = Timestamp::new(1000);
        let mut proposal = make_proposal(0, 10);
        proposal.phase = GovernancePhase::Exploration;
        proposal.exploration_started_at = Some(exploration_started);
        proposal.exploration_votes_yea = 20; // Only 20% participation (below quorum)
        proposal.exploration_votes_nay = 0;
        proposal.exploration_votes_abstain = 0;

        let now = Timestamp::new(
            exploration_started.as_secs()
                + params.governance_exploration_duration_secs
                + params.governance_propagation_buffer_secs,
        );
        let result = engine.try_advance(&mut proposal, now, &params, None);
        assert!(result.is_err());
        // Rejection is final; the vote record is retained.
        assert_eq!(proposal.phase, GovernancePhase::Rejected);
        assert_eq!(proposal.round, 0);
        assert_eq!(proposal.exploration_votes_yea, 20);
    }

    #[test]
    fn test_try_advance_exploration_rejects_on_supermajority_failure() {
        let engine = GovernanceEngine::new();
        let params = default_params();
        let exploration_started = Timestamp::new(1000);
        let mut proposal = make_proposal(0, 10);
        proposal.phase = GovernancePhase::Exploration;
        proposal.exploration_started_at = Some(exploration_started);
        proposal.exploration_votes_yea = 50; // 50% — below 80% supermajority
        proposal.exploration_votes_nay = 50;
        proposal.exploration_votes_abstain = 0;

        let now = Timestamp::new(
            exploration_started.as_secs()
                + params.governance_exploration_duration_secs
                + params.governance_propagation_buffer_secs,
        );
        let result = engine.try_advance(&mut proposal, now, &params, None);
        assert!(result.is_err());
        // Rejection is final; the vote record is retained.
        assert_eq!(proposal.phase, GovernancePhase::Rejected);
        assert_eq!(proposal.round, 0);
    }

    #[test]
    fn test_try_advance_cooldown_to_activation() {
        let engine = GovernanceEngine::new();
        let params = default_params();
        let cooldown_started = Timestamp::new(1000);
        let mut proposal = make_proposal(0, 10);
        proposal.phase = GovernancePhase::Cooldown;
        proposal.exploration_started_at = Some(Timestamp::new(0));
        proposal.exploration_votes_yea = 85;
        proposal.exploration_votes_nay = 15;
        proposal.cooldown_started_at = Some(cooldown_started);

        // Before duration elapsed — should fail
        let now = Timestamp::new(
            cooldown_started.as_secs() + params.governance_cooldown_duration_secs - 1,
        );
        assert!(engine
            .try_advance(&mut proposal, now, &params, None)
            .is_err());

        // After duration elapsed — should advance to Activation
        let now =
            Timestamp::new(cooldown_started.as_secs() + params.governance_cooldown_duration_secs);
        let result = engine.try_advance(&mut proposal, now, &params, None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), GovernancePhase::Activation);
        assert_eq!(proposal.phase, GovernancePhase::Activation);
        assert!(proposal.promotion_started_at.is_none());
        assert_eq!(
            proposal.activation_at,
            Some(Timestamp::new(
                cooldown_started.as_secs()
                    + params.governance_cooldown_duration_secs
                    + params.governance_propagation_buffer_secs,
            ))
        );
    }

    #[test]
    fn test_legacy_promotion_cannot_activate() {
        let engine = GovernanceEngine::new();
        let params = default_params();
        let mut proposal = make_proposal(1000, 10);
        proposal.phase = GovernancePhase::Promotion;
        proposal.promotion_started_at = Some(Timestamp::new(1000));
        proposal.promotion_votes_yea = 100;
        assert!(matches!(
            engine.try_advance(&mut proposal, Timestamp::new(u64::MAX), &params, None,),
            Err(GovernanceError::WrongPhase)
        ));
        assert!(proposal.activation_at.is_none());
    }

    #[test]
    fn test_governance_threshold_has_no_hardcoded_floor() {
        let engine = GovernanceEngine::new();
        let mut params = default_params();
        params.governance_supermajority_bps = 8000;
        params.governance_ema_participation_bps = 10000;
        let mut proposal = make_proposal(0, 10);
        proposal.content = ProposalContent::ParameterChange {
            param: crate::params::GovernableParam::GovernanceSupermajorityBps,
            new_value: 7500,
        };
        proposal.phase = GovernancePhase::Exploration;
        proposal.exploration_started_at = Some(Timestamp::new(1000));
        proposal.exploration_votes_yea = 24;
        proposal.exploration_votes_nay = 6;
        proposal.total_eligible_voters = 100;
        let now = Timestamp::new(
            1000 + params.governance_exploration_duration_secs
                + params.governance_propagation_buffer_secs,
        );
        // Exactly 30% quorum and 80% yes passes, regardless of historical EMA.
        assert_eq!(
            engine
                .try_advance(&mut proposal, now, &params, None)
                .unwrap(),
            GovernancePhase::Cooldown
        );
    }

    #[test]
    fn ordinary_quorum_uses_the_governed_threshold() {
        let mut params = default_params();
        let proposal = make_proposal(1000, params.governance_proposal_endorsements);
        for quorum in [0, 3000, 8000, 10000] {
            params.governance_quorum_bps = quorum;
            assert_eq!(
                GovernanceEngine::get_required_quorum(&proposal, &params),
                quorum
            );
        }
    }

    #[test]
    // Engine transitions only; no signed operations or certified activation.
    fn test_full_four_phase_lifecycle() {
        let engine = GovernanceEngine::new();
        let params = default_params();
        let mut proposal = make_proposal(1000, params.governance_proposal_endorsements);

        // Phase 1 → 2: Proposal → Exploration
        let now = Timestamp::new(
            proposal.created_at.as_secs() + params.governance_proposal_duration_secs,
        );
        let result = engine.try_advance(&mut proposal, now, &params, None);
        assert_eq!(result.unwrap(), GovernancePhase::Exploration);

        // Simulate exploration votes (85 yea, 5 nay, 10 abstain = 100% participation, 94.4% supermajority)
        proposal.exploration_votes_yea = 85;
        proposal.exploration_votes_nay = 5;
        proposal.exploration_votes_abstain = 10;

        // Phase 2 → 3: Exploration → Cooldown (with propagation buffer)
        let exploration_started = proposal.exploration_started_at.unwrap();
        let now = Timestamp::new(
            exploration_started.as_secs()
                + params.governance_exploration_duration_secs
                + params.governance_propagation_buffer_secs,
        );
        let result = engine.try_advance(&mut proposal, now, &params, None);
        assert_eq!(result.unwrap(), GovernancePhase::Cooldown);

        // Cooldown is followed by activation, not another ballot.
        let cooldown_started = proposal.cooldown_started_at.unwrap();
        let now =
            Timestamp::new(cooldown_started.as_secs() + params.governance_cooldown_duration_secs);
        assert_eq!(
            engine
                .try_advance(&mut proposal, now, &params, None)
                .unwrap(),
            GovernancePhase::Activation
        );
        assert!(proposal.promotion_started_at.is_none());
        assert_eq!(proposal.phase, GovernancePhase::Activation);
        assert!(proposal.activation_at.is_some());
    }

    #[test]
    fn phase_deadlines_do_not_depend_on_scheduler_delay() {
        let engine = GovernanceEngine::new();
        let params = default_params();
        let original = make_proposal(1000, params.governance_proposal_endorsements);
        let voting = 1000 + params.governance_proposal_duration_secs;
        let cooldown = voting
            + params.governance_exploration_duration_secs
            + params.governance_propagation_buffer_secs;
        let activation = cooldown + params.governance_cooldown_duration_secs;
        for delay in [0, 3, 300, 86400] {
            let mut proposal = original.clone();
            engine
                .try_advance(&mut proposal, Timestamp::new(voting + delay), &params, None)
                .unwrap();
            assert_eq!(
                proposal.exploration_started_at,
                Some(Timestamp::new(voting))
            );
            proposal.exploration_votes_yea = 80;
            proposal.exploration_votes_nay = 20;
            engine
                .try_advance(
                    &mut proposal,
                    Timestamp::new(cooldown + delay),
                    &params,
                    None,
                )
                .unwrap();
            assert_eq!(proposal.cooldown_started_at, Some(Timestamp::new(cooldown)));
            engine
                .try_advance(
                    &mut proposal,
                    Timestamp::new(activation + delay),
                    &params,
                    None,
                )
                .unwrap();
            assert_eq!(
                proposal.activation_at,
                Some(Timestamp::new(
                    activation + params.governance_propagation_buffer_secs
                ))
            );
        }
    }

    #[test]
    // Aggregate arithmetic only, not a population-scale ballot traversal.
    fn tally_handles_billions_of_people_without_overflow() {
        let engine = GovernanceEngine::new();
        let params = default_params();
        let mut proposal = make_proposal(0, 10);
        proposal.phase = GovernancePhase::Exploration;
        proposal.exploration_started_at = Some(Timestamp::new(1000));
        proposal.total_eligible_voters = u32::MAX;
        proposal.exploration_votes_yea = 3_600_000_000;
        proposal.exploration_votes_nay = 400_000_000;
        proposal.exploration_votes_abstain = 294_967_295;
        let now = Timestamp::new(
            1000 + params.governance_exploration_duration_secs
                + params.governance_propagation_buffer_secs,
        );
        assert_eq!(
            engine
                .try_advance(&mut proposal, now, &params, None)
                .unwrap(),
            GovernancePhase::Cooldown
        );
    }

    #[test]
    fn insufficient_endorsements_fail_at_the_proposal_deadline() {
        let engine = GovernanceEngine::new();
        let params = default_params();
        let mut proposal = make_proposal(1000, params.governance_proposal_endorsements - 1);
        let now = Timestamp::new(1000 + params.governance_proposal_duration_secs);
        assert!(matches!(
            engine.try_advance(&mut proposal, now, &params, None),
            Err(GovernanceError::InsufficientEndorsements { .. })
        ));
        assert_eq!(proposal.phase, GovernancePhase::Rejected);
    }

    #[test]
    fn proposal_endorsements_and_votes_are_recorded() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();

        let mut proposal = make_proposal(1000, 0);
        proposal.hash = unique_hash(42);
        let hash = submit(&mut engine, proposal);

        // Endorse until threshold met
        for _ in 0..params.governance_proposal_endorsements {
            engine.endorse_proposal(&hash, 100).unwrap();
        }

        let stored = engine.get_proposal(&hash).unwrap();
        assert_eq!(
            stored.endorsement_count,
            params.governance_proposal_endorsements
        );

        // Advance Proposal → Exploration
        let now = Timestamp::new(1000 + params.governance_proposal_duration_secs);
        let mut proposal = engine.get_proposal(&hash).unwrap().clone();
        let result = engine.try_advance(&mut proposal, now, &params, None);
        assert_eq!(result.unwrap(), GovernancePhase::Exploration);

        *engine.get_proposal_mut(&hash).unwrap() = proposal;
        engine.get_proposal_mut(&hash).unwrap().phase = GovernancePhase::Exploration;
        engine
            .get_proposal_mut(&hash)
            .unwrap()
            .exploration_started_at = Some(now);

        // Cast exploration votes via engine (within voting window)
        for i in 0..70u32 {
            engine
                .cast_exploration_vote(
                    &hash,
                    &voter_wallet(i),
                    GovernanceVote::Yea,
                    Timestamp::new(now.as_secs() + 1),
                    &params,
                )
                .unwrap();
        }
        for i in 70..90u32 {
            engine
                .cast_exploration_vote(
                    &hash,
                    &voter_wallet(i),
                    GovernanceVote::Nay,
                    Timestamp::new(now.as_secs() + 1),
                    &params,
                )
                .unwrap();
        }
        for i in 90..100u32 {
            engine
                .cast_exploration_vote(
                    &hash,
                    &voter_wallet(i),
                    GovernanceVote::Abstain,
                    Timestamp::new(now.as_secs() + 1),
                    &params,
                )
                .unwrap();
        }

        let stored = engine.get_proposal(&hash).unwrap();
        assert_eq!(stored.exploration_votes_yea, 70);
        assert_eq!(stored.exploration_votes_nay, 20);
        assert_eq!(stored.exploration_votes_abstain, 10);
    }

    #[test]
    fn test_activate_brn_rate() {
        let mut engine = GovernanceEngine::new();
        let mut params = default_params();
        let original_rate = params.brn_rate;
        let new_rate = original_rate + 1000;

        let mut proposal = make_proposal(0, 10);
        proposal.phase = GovernancePhase::Activation;
        proposal.content = ProposalContent::ParameterChange {
            param: crate::params::GovernableParam::BrnRate,
            new_value: new_rate,
        };
        proposal.exploration_started_at = Some(Timestamp::new(0));
        proposal.cooldown_started_at = Some(Timestamp::new(0));
        proposal.promotion_started_at = Some(Timestamp::new(0));
        proposal.activation_at = Some(Timestamp::new(0));

        assert!(engine.activate(&proposal, &mut params).is_ok());
        assert_eq!(params.brn_rate, new_rate);
    }

    #[test]
    fn test_activate_representative_eviction() {
        let mut engine = GovernanceEngine::new();
        let mut params = default_params();
        let target = WalletAddress::new("brst_bad_rep");

        let mut proposal = make_proposal(0, 10);
        proposal.phase = GovernancePhase::Activation;
        proposal.content = ProposalContent::RepresentativeEviction {
            target: target.clone(),
            evict: true,
            reason: "double-signing".to_string(),
        };

        // Activation applies no ProtocolParams change but queues the eviction.
        let params_before = params.params_hash();
        assert!(engine.activate(&proposal, &mut params).is_ok());
        assert_eq!(
            params.params_hash(),
            params_before,
            "eviction must not change params"
        );

        let evictions = engine.drain_pending_evictions();
        assert_eq!(evictions, vec![(target.clone(), true)]);
        // Draining is one-shot.
        assert!(engine.drain_pending_evictions().is_empty());

        // A reinstatement queues the inverse.
        let mut reinstate = make_proposal(1, 10);
        reinstate.phase = GovernancePhase::Activation;
        reinstate.content = ProposalContent::RepresentativeEviction {
            target: target.clone(),
            evict: false,
            reason: String::new(),
        };
        assert!(engine.activate(&reinstate, &mut params).is_ok());
        assert_eq!(engine.drain_pending_evictions(), vec![(target, false)]);
    }

    #[test]
    fn test_eviction_uses_governance_threshold_not_consti() {
        // Eviction uses the ordinary quorum and approval thresholds.
        let params = default_params();
        let mut proposal = make_proposal(0, 10);
        proposal.content = ProposalContent::RepresentativeEviction {
            target: WalletAddress::new("brst_x"),
            evict: true,
            reason: String::new(),
        };
        assert_eq!(
            GovernanceEngine::get_required_supermajority(&proposal, &params),
            params.governance_supermajority_bps
        );
        assert_eq!(
            GovernanceEngine::get_required_quorum(&proposal, &params),
            params.governance_quorum_bps
        );
    }

    #[test]
    fn test_activate_trst_expiry() {
        let mut engine = GovernanceEngine::new();
        let mut params = default_params();
        let new_expiry = 86400u128; // 1 day

        let mut proposal = make_proposal(0, 10);
        proposal.phase = GovernancePhase::Activation;
        proposal.content = ProposalContent::ParameterChange {
            param: crate::params::GovernableParam::TrstExpirySecs,
            new_value: new_expiry,
        };
        proposal.exploration_started_at = Some(Timestamp::new(0));
        proposal.cooldown_started_at = Some(Timestamp::new(0));
        proposal.promotion_started_at = Some(Timestamp::new(0));
        proposal.activation_at = Some(Timestamp::new(0));

        assert!(engine.activate(&proposal, &mut params).is_ok());
        assert_eq!(params.trst_expiry_secs, new_expiry as u64);
    }

    #[test]
    fn explicit_veto_requires_approval_and_precedes_activation() {
        for phase in [GovernancePhase::Cooldown, GovernancePhase::Activation] {
            let mut engine = GovernanceEngine::new();
            let mut proposal = make_proposal(1000, 0);
            proposal.phase = phase.clone();
            proposal.cooldown_started_at = Some(Timestamp::new(100));
            proposal.activation_at = Some(Timestamp::new(200));
            let hash = proposal.hash;
            engine.proposals.insert(hash, proposal);
            for now in [99, 200, 201] {
                assert!(engine.veto_approved(&hash, Timestamp::new(now)).is_err());
                assert_eq!(engine.get_proposal(&hash).unwrap().phase, phase);
            }
            engine.veto_approved(&hash, Timestamp::new(100)).unwrap();
            assert_eq!(
                engine.get_proposal(&hash).unwrap().phase,
                GovernancePhase::Vetoed
            );
            assert!(engine.veto_approved(&hash, Timestamp::new(101)).is_err());
            let mut vetoed = engine.get_proposal(&hash).unwrap().clone();
            assert!(engine
                .try_advance(&mut vetoed, Timestamp::new(300), &default_params(), None)
                .is_err());
            assert_eq!(vetoed.phase, GovernancePhase::Vetoed);
        }
        let mut engine = GovernanceEngine::new();
        let proposal = make_proposal(1000, 0);
        let hash = proposal.hash;
        engine.proposals.insert(hash, proposal);
        assert!(engine.veto_approved(&hash, Timestamp::new(100)).is_err());
    }

    #[test]
    fn test_withdraw_proposal_success() {
        let mut engine = GovernanceEngine::new();
        let proposer = dummy_wallet();
        let proposal = make_proposal(1000, 0);
        let hash = proposal.hash;
        engine.proposals.insert(hash, proposal);

        let result = engine.withdraw(&hash, &proposer);
        assert!(result.is_ok());
        assert_eq!(
            engine.proposals.get(&hash).unwrap().phase,
            GovernancePhase::Withdrawn
        );
    }

    #[test]
    fn test_withdraw_wrong_phase() {
        let mut engine = GovernanceEngine::new();
        let proposer = dummy_wallet();
        let mut proposal = make_proposal(1000, 0);
        proposal.phase = GovernancePhase::Exploration;
        let hash = proposal.hash;
        engine.proposals.insert(hash, proposal);

        let result = engine.withdraw(&hash, &proposer);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), GovernanceError::WrongPhase));
    }

    #[test]
    fn test_withdraw_not_proposer() {
        let mut engine = GovernanceEngine::new();
        let impostor = other_wallet();
        let proposal = make_proposal(1000, 0);
        let hash = proposal.hash;
        engine.proposals.insert(hash, proposal);

        let result = engine.withdraw(&hash, &impostor);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), GovernanceError::NotProposer));
    }

    #[test]
    fn test_cannot_advance_withdrawn_proposal() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let proposer = dummy_wallet();
        let proposal = make_proposal(1000, 10);
        let hash = proposal.hash;
        let created_at_secs = proposal.created_at.as_secs();
        engine.proposals.insert(hash, proposal);

        engine.withdraw(&hash, &proposer).unwrap();
        assert_eq!(
            engine.proposals.get(&hash).unwrap().phase,
            GovernancePhase::Withdrawn
        );

        let now = Timestamp::new(created_at_secs + params.governance_proposal_duration_secs);
        let mut proposal_copy = engine.proposals.get(&hash).unwrap().clone();
        let result = engine.try_advance(&mut proposal_copy, now, &params, None);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), GovernanceError::WrongPhase));
    }

    #[test]
    fn test_submit_emergency_proposal() {
        let mut engine = GovernanceEngine::new();
        let now = Timestamp::new(1000);
        let mut proposal = make_emergency_proposal(1000);

        let result = engine.submit_emergency_proposal(&mut proposal, now, &default_params());
        assert!(result.is_ok());
        assert_eq!(proposal.phase, GovernancePhase::Exploration);
        assert_eq!(proposal.exploration_started_at, Some(now));
    }

    #[test]
    fn test_submit_emergency_rejects_non_emergency() {
        let mut engine = GovernanceEngine::new();
        let now = Timestamp::new(1000);
        let mut proposal = make_proposal(1000, 0);

        let result = engine.submit_emergency_proposal(&mut proposal, now, &default_params());
        assert!(result.is_err());
    }

    #[test]
    fn test_emergency_skips_proposal_and_cooldown() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let now = Timestamp::new(1000);
        let mut proposal = make_emergency_proposal(1000);

        // Submit as emergency → starts in Exploration
        engine
            .submit_emergency_proposal(&mut proposal, now, &params)
            .unwrap();
        assert_eq!(proposal.phase, GovernancePhase::Exploration);

        // 96% approval exceeds the default 95% threshold.
        proposal.exploration_votes_yea = 96;
        proposal.exploration_votes_nay = 4;
        proposal.exploration_votes_abstain = 0;

        let deadline = 1000
            + params.emergency_voting_duration_secs
            + params.governance_propagation_buffer_secs;
        assert_eq!(
            engine
                .try_advance(&mut proposal, Timestamp::new(deadline), &params, None)
                .unwrap(),
            GovernancePhase::Activation
        );
        assert!(proposal.cooldown_started_at.is_none());
        assert!(proposal.promotion_started_at.is_none());
        assert_eq!(
            proposal.activation_at,
            Some(Timestamp::new(
                deadline + params.governance_propagation_buffer_secs
            ))
        );
    }

    #[test]
    fn test_emergency_24h_voting_period() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let now = Timestamp::new(1000);
        let mut proposal = make_emergency_proposal(1000);
        engine
            .submit_emergency_proposal(&mut proposal, now, &params)
            .unwrap();

        proposal.exploration_votes_yea = 96;
        proposal.exploration_votes_nay = 4;
        proposal.exploration_votes_abstain = 0;

        // Before 24h → should fail (PhaseNotExpired)
        let now = Timestamp::new(1000 + params.emergency_voting_duration_secs - 1);
        assert!(engine
            .try_advance(&mut proposal, now, &params, None)
            .is_err());

        // At exactly 24h but before propagation buffer → should fail (PropagationBuffer)
        let now = Timestamp::new(1000 + params.emergency_voting_duration_secs);
        assert!(matches!(
            engine
                .try_advance(&mut proposal, now, &params, None)
                .unwrap_err(),
            GovernanceError::PropagationBuffer
        ));

        // At 24h + propagation buffer → should pass
        let now = Timestamp::new(
            1000 + params.emergency_voting_duration_secs
                + params.governance_propagation_buffer_secs,
        );
        assert!(engine
            .try_advance(&mut proposal, now, &params, None)
            .is_ok());
    }

    #[test]
    fn test_emergency_requires_95_percent_supermajority() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let now = Timestamp::new(1000);
        let mut proposal = make_emergency_proposal(1000);
        engine
            .submit_emergency_proposal(&mut proposal, now, &params)
            .unwrap();

        // 94% yea → should fail (need 95%)
        proposal.exploration_votes_yea = 94;
        proposal.exploration_votes_nay = 6;
        proposal.exploration_votes_abstain = 0;

        let now = Timestamp::new(
            1000 + params.emergency_voting_duration_secs
                + params.governance_propagation_buffer_secs,
        );
        let result = engine.try_advance(&mut proposal, now, &params, None);
        assert!(result.is_err());
        // Rejection is final; the vote record is retained.
        assert_eq!(proposal.phase, GovernancePhase::Rejected);
        assert_eq!(proposal.round, 0);
    }

    #[test]
    fn test_emergency_95_percent_passes() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let now = Timestamp::new(1000);
        let mut proposal = make_emergency_proposal(1000);
        engine
            .submit_emergency_proposal(&mut proposal, now, &params)
            .unwrap();

        // Exactly 95% passes.
        proposal.exploration_votes_yea = 95;
        proposal.exploration_votes_nay = 5;
        proposal.exploration_votes_abstain = 0;

        let now = Timestamp::new(
            1000 + params.emergency_voting_duration_secs
                + params.governance_propagation_buffer_secs,
        );
        let result = engine.try_advance(&mut proposal, now, &params, None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), GovernancePhase::Activation);
    }

    #[test]
    fn emergency_submission_uses_verified_entrypoint_and_cannot_replace_an_existing_ballot() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let proposal = make_emergency_proposal(1000);
        assert!(matches!(
            engine.submit_proposal(proposal.clone(), 0, false, &params),
            Err(GovernanceError::ProposerNotVerified)
        ));
        assert!(engine.get_proposal(&proposal.hash).is_none());
        engine
            .submit_proposal(proposal.clone(), 0, true, &params)
            .unwrap();
        let stored = engine.get_proposal(&proposal.hash).unwrap().clone();
        assert_eq!(stored.phase, GovernancePhase::Exploration);
        assert_eq!(stored.exploration_started_at, Some(proposal.created_at));
        assert_eq!(
            engine.counting_time(&stored, &params).unwrap().as_secs(),
            1000 + params.emergency_voting_duration_secs
                + params.governance_propagation_buffer_secs
        );
        assert!(engine
            .submit_proposal(proposal.clone(), 0, true, &params)
            .is_err());
        assert_eq!(
            bincode::serialize(engine.get_proposal(&proposal.hash).unwrap()).unwrap(),
            bincode::serialize(&stored).unwrap()
        );
    }

    #[test]
    fn emergency_quorum_is_participation_not_approval() {
        let params = default_params();
        for (yea, nay, abstain, passes) in [
            (30, 0, 0, false),
            (94, 0, 0, false),
            (90, 5, 0, false),
            (95, 0, 0, true),
            (90, 0, 5, true),
        ] {
            let mut engine = GovernanceEngine::new();
            let mut proposal = make_emergency_proposal(1000);
            engine
                .submit_emergency_proposal(&mut proposal, Timestamp::new(1000), &params)
                .unwrap();
            proposal.exploration_votes_yea = yea;
            proposal.exploration_votes_nay = nay;
            proposal.exploration_votes_abstain = abstain;
            let at = engine.counting_time(&proposal, &params).unwrap();
            let result = engine.try_advance(&mut proposal, at, &params, None);
            assert_eq!(result.is_ok(), passes, "votes: {yea}/{nay}/{abstain}");
            if yea + nay + abstain < 95 {
                assert!(matches!(result, Err(GovernanceError::QuorumNotMet { .. })));
            }
        }
    }

    #[test]
    fn emergency_timing_is_frozen_but_counting_uses_governed_thresholds_after_restore() {
        let mut engine = GovernanceEngine::new();
        let mut params = default_params();
        params.emergency_voting_duration_secs = 17;
        params.governance_proposal_duration_secs = u64::MAX;
        params.governance_propagation_buffer_secs = 3;
        let mut proposal = make_emergency_proposal(1000);
        engine
            .submit_emergency_proposal(&mut proposal, Timestamp::new(1000), &params)
            .unwrap();
        let bytes = bincode::serialize(&engine).unwrap();
        let restored: GovernanceEngine = bincode::deserialize(&bytes).unwrap();
        params.emergency_voting_duration_secs = 1;
        params.emergency_quorum_bps = 8000;
        params.emergency_supermajority_bps = 7500;
        assert_eq!(
            restored.counting_time(&proposal, &params).unwrap(),
            Timestamp::new(1020)
        );
        proposal.exploration_votes_yea = 60;
        proposal.exploration_votes_nay = 20;
        assert!(matches!(
            restored.try_advance(&mut proposal, Timestamp::new(1019), &params, None),
            Err(GovernanceError::PropagationBuffer)
        ));
        assert_eq!(
            restored
                .try_advance(&mut proposal, Timestamp::new(1020), &params, None)
                .unwrap(),
            GovernancePhase::Activation
        );
    }

    #[test]
    fn emergency_does_not_bypass_constitution_thresholds() {
        let params = ProtocolParams {
            consti_supermajority_bps: 9900,
            consti_quorum_bps: 9800,
            ..default_params()
        };
        let mut proposal = make_emergency_proposal(1000);
        proposal.content = ProposalContent::Emergency {
            description: "Threshold change".into(),
            param: crate::params::GovernableParam::ConstiSupermajorityBps,
            new_value: 1,
        };
        assert_eq!(
            GovernanceEngine::get_required_supermajority(&proposal, &params),
            9900
        );
        assert_eq!(
            GovernanceEngine::get_required_quorum(&proposal, &params),
            9800
        );
    }

    #[test]
    fn test_emergency_full_lifecycle() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let now = Timestamp::new(1000);
        let mut proposal = make_emergency_proposal(1000);

        engine
            .submit_emergency_proposal(&mut proposal, now, &params)
            .unwrap();

        // Voting → Activation
        proposal.exploration_votes_yea = 96;
        proposal.exploration_votes_nay = 4;
        proposal.exploration_votes_abstain = 0;
        let now = Timestamp::new(
            1000 + params.emergency_voting_duration_secs
                + params.governance_propagation_buffer_secs,
        );
        engine
            .try_advance(&mut proposal, now, &params, None)
            .unwrap();

        assert_eq!(proposal.phase, GovernancePhase::Activation);

        // Apply content directly; this test does not exercise certified activation.
        let mut protocol_params = default_params();
        engine.activate(&proposal, &mut protocol_params).unwrap();
        assert_eq!(protocol_params.brn_rate, 500);
    }

    #[test]
    fn test_consti_supermajority_change_requires_90_percent() {
        let engine = GovernanceEngine::new();
        let params = default_params();
        let exploration_started = Timestamp::new(1000);

        // Proposal to change ConstiSupermajorityBps
        let mut proposal = Proposal {
            hash: dummy_hash(),
            proposer: dummy_wallet(),
            phase: GovernancePhase::Exploration,
            content: ProposalContent::ParameterChange {
                param: crate::params::GovernableParam::ConstiSupermajorityBps,
                new_value: 8500,
            },
            endorsement_count: 10,
            total_eligible_voters: 100,
            exploration_started_at: Some(exploration_started),
            exploration_votes_yea: 85, // 85% — passes 80% governance but fails 90% consti
            exploration_votes_nay: 15,
            exploration_votes_abstain: 0,
            cooldown_started_at: None,
            promotion_started_at: None,
            promotion_votes_yea: 0,
            promotion_votes_nay: 0,
            promotion_votes_abstain: 0,
            round: 0,
            created_at: Timestamp::new(0),
            activation_at: None,
        };

        let now = Timestamp::new(
            exploration_started.as_secs()
                + params.governance_exploration_duration_secs
                + params.governance_propagation_buffer_secs,
        );
        // 85% yea (8500 bps) should FAIL because consti requires 90% (9000 bps)
        let result = engine.try_advance(&mut proposal, now, &params, None);
        assert!(result.is_err());
        // Rejection is final; the vote record is retained.
        assert_eq!(proposal.phase, GovernancePhase::Rejected);
        assert_eq!(proposal.round, 0);
    }

    #[test]
    fn consti_supermajority_change_passes_above_threshold() {
        let engine = GovernanceEngine::new();
        let params = default_params();
        let exploration_started = Timestamp::new(1000);

        let mut proposal = Proposal {
            hash: dummy_hash(),
            proposer: dummy_wallet(),
            phase: GovernancePhase::Exploration,
            content: ProposalContent::ParameterChange {
                param: crate::params::GovernableParam::ConstiSupermajorityBps,
                new_value: 8500,
            },
            endorsement_count: 10,
            total_eligible_voters: 100,
            exploration_started_at: Some(exploration_started),
            exploration_votes_yea: 91, // 91% — passes 90% consti threshold
            exploration_votes_nay: 9,
            exploration_votes_abstain: 0,
            cooldown_started_at: None,
            promotion_started_at: None,
            promotion_votes_yea: 0,
            promotion_votes_nay: 0,
            promotion_votes_abstain: 0,
            round: 0,
            created_at: Timestamp::new(0),
            activation_at: None,
        };

        let now = Timestamp::new(
            exploration_started.as_secs()
                + params.governance_exploration_duration_secs
                + params.governance_propagation_buffer_secs,
        );
        let result = engine.try_advance(&mut proposal, now, &params, None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), GovernancePhase::Cooldown);
    }

    #[test]
    fn test_normal_param_change_still_uses_80_percent() {
        let engine = GovernanceEngine::new();
        let params = default_params();
        let exploration_started = Timestamp::new(1000);

        // BrnRate change — should use governance_supermajority_bps (80%)
        let mut proposal = Proposal {
            hash: dummy_hash(),
            proposer: dummy_wallet(),
            phase: GovernancePhase::Exploration,
            content: ProposalContent::ParameterChange {
                param: crate::params::GovernableParam::BrnRate,
                new_value: 1000,
            },
            endorsement_count: 10,
            total_eligible_voters: 100,
            exploration_started_at: Some(exploration_started),
            exploration_votes_yea: 85, // 85% — passes 80% governance threshold
            exploration_votes_nay: 15,
            exploration_votes_abstain: 0,
            cooldown_started_at: None,
            promotion_started_at: None,
            promotion_votes_yea: 0,
            promotion_votes_nay: 0,
            promotion_votes_abstain: 0,
            round: 0,
            created_at: Timestamp::new(0),
            activation_at: None,
        };

        let now = Timestamp::new(
            exploration_started.as_secs()
                + params.governance_exploration_duration_secs
                + params.governance_propagation_buffer_secs,
        );
        let result = engine.try_advance(&mut proposal, now, &params, None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), GovernancePhase::Cooldown);
    }

    #[test]
    fn test_submit_proposal_does_not_charge_submission() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let proposal = make_proposal(1000, 0);

        // Proposal submission is open to every verified wallet.
        let result = engine.submit_proposal(proposal, 0, true, &params);
        assert!(result.is_ok());
    }

    #[test]
    fn submission_accepts_balance_equal_to_endorsement_cost() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let proposal = make_proposal(1000, 0);

        // Submission does not spend this balance.
        let result =
            engine.submit_proposal(proposal, params.governance_proposal_cost, true, &params);
        assert!(result.is_ok());
    }

    #[test]
    fn test_submit_proposal_does_not_require_endorsement_balance() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let proposal = make_proposal(1000, 0);

        let result =
            engine.submit_proposal(proposal, params.governance_proposal_cost - 1, true, &params);
        assert!(result.is_ok());
    }

    #[test]
    fn test_activate_governance_proposal_cost() {
        let mut engine = GovernanceEngine::new();
        let mut params = default_params();

        let mut proposal = make_proposal(0, 10);
        proposal.phase = GovernancePhase::Activation;
        proposal.content = ProposalContent::ParameterChange {
            param: crate::params::GovernableParam::GovernanceProposalCost,
            new_value: 999,
        };

        engine.activate(&proposal, &mut params).unwrap();
        assert_eq!(params.governance_proposal_cost, 999);
    }

    #[test]
    // Raw storage accepts 200; the bootstrap policy separately caps its effect.
    fn test_activate_bootstrap_exit_threshold() {
        let mut engine = GovernanceEngine::new();
        let mut params = default_params();

        let mut proposal = make_proposal(0, 10);
        proposal.phase = GovernancePhase::Activation;
        proposal.content = ProposalContent::ParameterChange {
            param: crate::params::GovernableParam::BootstrapExitThreshold,
            new_value: 200,
        };

        engine.activate(&proposal, &mut params).unwrap();
        assert_eq!(params.bootstrap_exit_threshold, 200);
    }

    #[test]
    fn test_activate_new_wallet_rate_limit_duration() {
        let mut engine = GovernanceEngine::new();
        let mut params = default_params();

        let mut proposal = make_proposal(0, 10);
        proposal.phase = GovernancePhase::Activation;
        proposal.content = ProposalContent::ParameterChange {
            param: crate::params::GovernableParam::NewWalletRateLimitDurationSecs,
            new_value: 86400,
        };

        engine.activate(&proposal, &mut params).unwrap();
        assert_eq!(params.new_wallet_rate_limit_duration_secs, 86400);
    }

    #[test]
    fn activate_verifier_eligibility_and_cooldown_parameters() {
        use crate::params::GovernableParam;
        let mut engine = GovernanceEngine::new();
        let mut params = default_params();
        for param in [
            GovernableParam::MinVerificationAgeSecs,
            GovernableParam::NeitherPenaltyCooldownSecs,
        ] {
            for value in [0, 7, u64::MAX as u128, u128::MAX] {
                let mut proposal = make_proposal(0, 10);
                proposal.phase = GovernancePhase::Activation;
                proposal.content = ProposalContent::ParameterChange {
                    param: param.clone(),
                    new_value: value,
                };
                if value > param.maximum_value() {
                    let before = params.clone();
                    let pending = engine.pending_changes.clone();
                    assert!(engine.activate(&proposal, &mut params).is_err());
                    assert_eq!(params, before);
                    assert_eq!(engine.pending_changes, pending);
                    assert!(proposal.content.encode_compact().is_err());
                    continue;
                }
                engine.activate(&proposal, &mut params).unwrap();
                assert_eq!(
                    param.value_view(&params).current,
                    value.min(u64::MAX as u128).to_string()
                );
                let encoded = proposal.content.encode_compact().unwrap();
                assert_eq!(
                    ProposalContent::decode_compact(&encoded).unwrap(),
                    proposal.content
                );
            }
        }
    }

    #[test]
    fn activate_verifier_balance_and_emergency_rules_losslessly() {
        use crate::params::GovernableParam::*;
        let mut engine = GovernanceEngine::new();
        let mut params = default_params();
        for (param, maximum) in [
            (MinVerifierBrnBalance, u128::MAX),
            (EmergencyVotingDurationSecs, u64::MAX as u128),
            (EmergencyQuorumBps, u32::MAX as u128),
            (EmergencySupermajorityBps, u32::MAX as u128),
        ] {
            for value in [0, 1, 9501, maximum, u128::MAX] {
                let mut proposal = make_proposal(0, 10);
                proposal.phase = GovernancePhase::Activation;
                proposal.content = ProposalContent::ParameterChange {
                    param: param.clone(),
                    new_value: value,
                };
                if value > param.maximum_value() {
                    let before = params.clone();
                    let pending = engine.pending_changes.clone();
                    assert!(engine.activate(&proposal, &mut params).is_err());
                    assert_eq!(params, before);
                    assert_eq!(engine.pending_changes, pending);
                    assert!(proposal.content.encode_compact().is_err());
                    continue;
                }
                engine.activate(&proposal, &mut params).unwrap();
                assert_eq!(
                    param.value_view(&params).current,
                    value.min(maximum).to_string()
                );
                let encoded = proposal.content.encode_compact().unwrap();
                assert_eq!(
                    ProposalContent::decode_compact(&encoded).unwrap(),
                    proposal.content
                );
            }
        }
    }

    #[test]
    fn activate_rolling_neither_rules() {
        use crate::params::GovernableParam;
        let mut engine = GovernanceEngine::new();
        let mut params = default_params();
        for param in [
            GovernableParam::NeitherPenaltyThresholdBps,
            GovernableParam::NeitherWindowAssignments,
        ] {
            for value in [0, 4, 7500, u32::MAX as u128, u128::MAX] {
                let mut proposal = make_proposal(0, 10);
                proposal.phase = GovernancePhase::Activation;
                proposal.content = ProposalContent::ParameterChange {
                    param: param.clone(),
                    new_value: value,
                };
                if value > param.maximum_value() {
                    let before = params.clone();
                    let pending = engine.pending_changes.clone();
                    assert!(engine.activate(&proposal, &mut params).is_err());
                    assert_eq!(params, before);
                    assert_eq!(engine.pending_changes, pending);
                    assert!(proposal.content.encode_compact().is_err());
                    continue;
                }
                engine.activate(&proposal, &mut params).unwrap();
                assert_eq!(
                    param.value_view(&params).current,
                    value.min(u32::MAX as u128).to_string()
                );
                let encoded = proposal.content.encode_compact().unwrap();
                assert_eq!(
                    ProposalContent::decode_compact(&encoded).unwrap(),
                    proposal.content
                );
            }
        }
    }

    #[test]
    fn oversized_parameter_is_rejected_before_submission_or_activation_effects() {
        for param in crate::params::GovernableParam::ALL {
            let Some(value) = param.maximum_value().checked_add(1) else {
                continue;
            };
            for emergency in [false, true] {
                let mut engine = GovernanceEngine::new();
                let mut params = default_params();
                let before = params.clone();
                let mut proposal = make_proposal(0, 10);
                proposal.content = if emergency {
                    ProposalContent::Emergency {
                        description: "overflow".into(),
                        param: param.clone(),
                        new_value: value,
                    }
                } else {
                    ProposalContent::ParameterChange {
                        param: param.clone(),
                        new_value: value,
                    }
                };
                assert!(engine
                    .submit_proposal(proposal.clone(), u128::MAX, true, &params)
                    .is_err());
                assert!(engine.proposals.is_empty());
                assert!(engine.preview_activation(&proposal, &params).is_err());
                assert!(engine.activate(&proposal, &mut params).is_err());
                assert_eq!(params, before);
                assert!(engine.pending_changes.is_empty());
            }
        }
    }

    #[test]
    fn test_compute_participation_bps_full() {
        let bps = GovernanceEngine::compute_participation_bps(70, 20, 10, 100);
        assert_eq!(bps, 10000); // 100% participation
    }

    #[test]
    fn test_compute_participation_bps_partial() {
        let bps = GovernanceEngine::compute_participation_bps(30, 10, 10, 100);
        assert_eq!(bps, 5000); // 50% participation
    }

    #[test]
    fn test_compute_participation_bps_zero_eligible() {
        let bps = GovernanceEngine::compute_participation_bps(10, 5, 0, 0);
        assert_eq!(bps, 0);
    }

    #[test]
    fn test_compute_participation_bps_no_votes() {
        let bps = GovernanceEngine::compute_participation_bps(0, 0, 0, 100);
        assert_eq!(bps, 0);
    }

    #[test]
    fn test_update_ema_initial() {
        let mut params = default_params();
        params.governance_ema_participation_bps = 5000; // 50%
        GovernanceEngine::update_ema(&mut params, 10000); // 100% participation
        assert_eq!(params.governance_ema_participation_bps, 6000);
    }

    #[test]
    fn test_update_ema_low_participation_drags_down() {
        let mut params = default_params();
        params.governance_ema_participation_bps = 8000;
        GovernanceEngine::update_ema(&mut params, 2000);
        assert_eq!(params.governance_ema_participation_bps, 6800);
    }

    #[test]
    fn test_update_ema_same_participation_stays_stable() {
        let mut params = default_params();
        params.governance_ema_participation_bps = 5000;
        GovernanceEngine::update_ema(&mut params, 5000);
        assert_eq!(params.governance_ema_participation_bps, 5000);
    }

    #[test]
    fn test_update_ema_from_zero() {
        let mut params = default_params();
        params.governance_ema_participation_bps = 0;
        GovernanceEngine::update_ema(&mut params, 7000);
        assert_eq!(params.governance_ema_participation_bps, 1400);
    }

    #[test]
    fn test_competing_proposals_empty() {
        let engine = GovernanceEngine::new();
        assert!(engine.competing_proposals().is_empty());
    }

    #[test]
    fn test_competing_proposals_filters_phases() {
        let mut engine = GovernanceEngine::new();

        let mut p1 = make_proposal(1000, 5);
        p1.hash = unique_hash(1);
        submit(&mut engine, p1);

        let mut p2 = make_proposal(1000, 10);
        p2.hash = unique_hash(2);
        submit(&mut engine, p2);
        engine.get_proposal_mut(&unique_hash(2)).unwrap().phase = GovernancePhase::Exploration;

        let mut p3 = make_proposal(1000, 3);
        p3.hash = unique_hash(3);
        submit(&mut engine, p3);
        engine.get_proposal_mut(&unique_hash(3)).unwrap().phase = GovernancePhase::Cooldown;

        let competing = engine.competing_proposals();
        // p3 is in Cooldown, should be excluded
        assert_eq!(competing.len(), 2);
        // Sorted by endorsement_count descending: p2 (10) then p1 (5)
        assert_eq!(competing[0].hash, unique_hash(2));
        assert_eq!(competing[1].hash, unique_hash(1));
    }

    #[test]
    fn test_competing_proposals_sorted_by_endorsement() {
        let mut engine = GovernanceEngine::new();

        for i in 0u8..5 {
            let mut p = make_proposal(1000, i as u32 * 3);
            p.hash = unique_hash(i);
            submit(&mut engine, p);
        }

        let competing = engine.competing_proposals();
        assert_eq!(competing.len(), 5);
        for window in competing.windows(2) {
            assert!(window[0].endorsement_count >= window[1].endorsement_count);
        }
    }

    #[test]
    fn test_snapshot_eligible_voters() {
        let mut engine = GovernanceEngine::new();
        let proposal = make_proposal(1000, 0);
        let hash = submit(&mut engine, proposal);

        assert!(engine.snapshot_eligible_voters(&hash, 250).is_ok());
        let stored = engine.get_proposal(&hash).unwrap();
        assert_eq!(stored.total_eligible_voters, 250);
    }

    #[test]
    fn test_snapshot_eligible_voters_not_found() {
        let mut engine = GovernanceEngine::new();
        let fake_hash = unique_hash(99);
        let result = engine.snapshot_eligible_voters(&fake_hash, 100);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            GovernanceError::ProposalNotFound(_)
        ));
    }

    #[test]
    fn test_snapshot_eligible_voters_overwrites() {
        let mut engine = GovernanceEngine::new();
        let proposal = make_proposal(1000, 0);
        let hash = submit(&mut engine, proposal);

        engine.snapshot_eligible_voters(&hash, 100).unwrap();
        assert_eq!(
            engine.get_proposal(&hash).unwrap().total_eligible_voters,
            100
        );

        engine.snapshot_eligible_voters(&hash, 500).unwrap();
        assert_eq!(
            engine.get_proposal(&hash).unwrap().total_eligible_voters,
            500
        );
    }

    #[test]
    fn test_qualified_proposals_advance_independently() {
        let mut engine = GovernanceEngine::new();
        let mut params = default_params();

        let created_at = 1000u64;
        let mut p1 = make_proposal(created_at, 0);
        p1.hash = unique_hash(1);
        submit(&mut engine, p1);

        let mut p2 = make_proposal(created_at, 0);
        p2.hash = unique_hash(2);
        submit(&mut engine, p2);

        // Both proposals meet the endorsement threshold.
        for _ in 0..params.governance_proposal_endorsements + 3 {
            engine.endorse_proposal(&unique_hash(1), 100).unwrap();
        }
        for _ in 0..params.governance_proposal_endorsements {
            engine.endorse_proposal(&unique_hash(2), 100).unwrap();
        }

        let now = Timestamp::new(created_at + params.governance_proposal_window_secs);
        let delegation = crate::vote_delegation::DelegationEngine::new(10);
        engine.tick(now, &mut params, &delegation, &[]);

        let p1_stored = engine.get_proposal(&unique_hash(1)).unwrap();
        assert_eq!(p1_stored.phase, GovernancePhase::Exploration);

        let p2_stored = engine.get_proposal(&unique_hash(2)).unwrap();
        assert_eq!(p2_stored.phase, GovernancePhase::Exploration);
        assert_eq!(p2_stored.round, 0);
        assert_eq!(
            p2_stored.endorsement_count,
            params.governance_proposal_endorsements
        );
    }

    #[test]
    fn test_terminal_rejection_after_max_rounds() {
        let engine = GovernanceEngine::new();
        let params = default_params();
        let exploration_started = Timestamp::new(1000);
        let mut proposal = make_proposal(0, 10);
        proposal.phase = GovernancePhase::Exploration;
        proposal.exploration_started_at = Some(exploration_started);
        proposal.exploration_votes_yea = 20; // quorum failure
        proposal.round = params.governance_max_rounds; // already at max

        let now = Timestamp::new(
            exploration_started.as_secs()
                + params.governance_exploration_duration_secs
                + params.governance_propagation_buffer_secs,
        );
        let result = engine.try_advance(&mut proposal, now, &params, None);
        assert!(result.is_err());
        assert_eq!(proposal.phase, GovernancePhase::Rejected);
    }

    #[test]
    fn test_rejection_preserves_vote_record() {
        let engine = GovernanceEngine::new();
        let params = default_params();
        let exploration_started = Timestamp::new(1000);
        let mut proposal = make_proposal(0, 10);
        proposal.phase = GovernancePhase::Exploration;
        proposal.exploration_started_at = Some(exploration_started);
        proposal.exploration_votes_yea = 20;
        proposal.exploration_votes_nay = 30;
        proposal.exploration_votes_abstain = 5;

        let now = Timestamp::new(
            exploration_started.as_secs()
                + params.governance_exploration_duration_secs
                + params.governance_propagation_buffer_secs,
        );
        let _ = engine.try_advance(&mut proposal, now, &params, None);

        assert_eq!(proposal.phase, GovernancePhase::Rejected);
        assert_eq!(proposal.round, 0);
        assert_eq!(proposal.exploration_votes_yea, 20);
        assert_eq!(proposal.exploration_votes_nay, 30);
        assert_eq!(proposal.exploration_votes_abstain, 5);
        assert_eq!(proposal.endorsement_count, 10);
        assert!(proposal.exploration_started_at.is_some());
    }

    #[test]
    fn test_rejection_cannot_retry_without_new_proposal() {
        let engine = GovernanceEngine::new();
        let params = default_params();
        let mut proposal = make_proposal(0, 10);
        proposal.phase = GovernancePhase::Exploration;
        proposal.exploration_started_at = Some(Timestamp::new(1000));
        proposal.exploration_votes_yea = 20;
        let now = Timestamp::new(
            1000 + params.governance_exploration_duration_secs
                + params.governance_propagation_buffer_secs,
        );
        assert!(matches!(
            engine.try_advance(&mut proposal, now, &params, None),
            Err(GovernanceError::QuorumNotMet { .. })
        ));
        for _ in 0..10 {
            assert!(matches!(
                engine.try_advance(&mut proposal, now, &params, None),
                Err(GovernanceError::WrongPhase)
            ));
        }
        assert_eq!(proposal.phase, GovernancePhase::Rejected);
        assert_eq!(proposal.round, 0);
    }

    #[test]
    fn phase_tally_shares_eligibility_without_changing_missing_vote_rules() {
        let mut engine = GovernanceEngine::new();
        let hash = submit(&mut engine, make_proposal(1000, 0));
        let proposal = engine.get_proposal(&hash).unwrap().clone();
        let people: Vec<_> = (1..=3).map(voter_wallet).collect();
        let mut electorate = people.clone();
        electorate.extend(people.clone());
        let mut delegation = DelegationEngine::new(10);
        delegation.project_key_ballot(people[0].clone(), Some(GovernanceVote::Yea));
        delegation.delegate(&people[1], &people[2]).unwrap();
        let direct = OrdMap::from_iter([(people[2].clone(), GovernanceVote::Nay)]);
        for populated in [false, true] {
            if populated {
                engine.exploration_votes.insert(hash, direct.clone());
                engine.promotion_votes.insert(hash, direct.clone());
            }
            for phase in [GovernancePhase::Exploration, GovernancePhase::Promotion] {
                let expected = if phase == GovernancePhase::Promotion {
                    engine.count_effective_promotion_votes(&hash, &delegation, &electorate)
                } else {
                    engine.count_effective_exploration_votes(&hash, &delegation, &electorate)
                };
                let actual =
                    engine.tally_inputs(&proposal, phase.clone(), Some((&delegation, &electorate)));
                assert_eq!(actual, (expected.0, expected.1, expected.2, 3));
                assert_eq!(
                    actual,
                    if populated {
                        (1, 2, 0, 3)
                    } else if phase == GovernancePhase::Promotion {
                        (0, 0, 0, 3)
                    } else {
                        (1, 0, 0, 3)
                    }
                );
                assert_eq!(
                    engine.tally_inputs(&proposal, phase, Some((&delegation, &[]))),
                    (0, 0, 0, 0)
                );
            }
        }
        let mut absent = proposal;
        absent.hash = TxHash::new([98; 32]);
        for phase in [GovernancePhase::Exploration, GovernancePhase::Promotion] {
            assert_eq!(
                engine.tally_inputs(&absent, phase, Some((&delegation, &electorate))),
                (0, 0, 0, 3)
            );
        }
    }

    #[test]
    fn key_only_ballots_count_and_explicit_absence_blocks_legacy_fallback() {
        let mut engine = GovernanceEngine::new();
        let hash = submit(&mut engine, make_proposal(1000, 0));
        let people: Vec<_> = (1..=3).map(voter_wallet).collect();
        let mut delegation = DelegationEngine::new(10);
        delegation.project_key_ballot(people[0].clone(), Some(GovernanceVote::Yea));
        delegation.project_key_ballot(people[1].clone(), Some(GovernanceVote::Nay));
        assert_eq!(
            engine.count_effective_exploration_votes(&hash, &delegation, &people),
            (1, 1, 0)
        );
        delegation.delegate(&people[0], &people[2]).unwrap();
        delegation.project_key_ballot(people[0].clone(), None);
        engine.exploration_votes.insert(
            hash,
            OrdMap::from_iter([(people[2].clone(), GovernanceVote::Yea)]),
        );
        assert_eq!(
            engine.count_effective_exploration_votes(&hash, &delegation, &people),
            (1, 1, 0)
        );
        engine
            .exploration_votes
            .get_mut(&hash)
            .unwrap()
            .insert(people[1].clone(), GovernanceVote::Abstain);
        assert_eq!(
            engine.count_effective_exploration_votes(&hash, &delegation, &people),
            (1, 0, 1)
        );
        // A cut-local projection must not become durable authority for a later proposal.
        assert!(DelegationEngine::load_state(&delegation.save_state())
            .unwrap()
            .key_ballot(&people[1])
            .is_none());
    }

    #[test]
    fn tally_uses_proposal_parameter_category_and_direct_vote_precedence() {
        use crate::{DelegationScope, GovernableParam};
        let proposal = make_proposal(1000, 0);
        let people: Vec<_> = (1..=5).map(voter_wallet).collect();
        let owner = &people[0];
        let mut delegation = DelegationEngine::new(10);
        let mut votes = OrdMap::from_iter([
            (people[1].clone(), GovernanceVote::Yea),
            (people[2].clone(), GovernanceVote::Nay),
            (people[3].clone(), GovernanceVote::Abstain),
            (people[4].clone(), GovernanceVote::Yea),
        ]);
        let count = |delegation: &DelegationEngine, direct: &OrdMap<_, _>| {
            GovernanceEngine::count_votes_with_delegation(
                direct,
                delegation,
                &people,
                Some(&proposal),
            )
        };
        delegation.delegate(owner, &people[1]).unwrap();
        assert_eq!(count(&delegation, &votes), (3, 1, 1));
        delegation
            .delegate_scoped(
                owner,
                &people[2],
                DelegationScope::Category("economic".into()),
            )
            .unwrap();
        assert_eq!(count(&delegation, &votes), (2, 2, 1));
        delegation
            .delegate_scoped(
                owner,
                &people[3],
                DelegationScope::Parameter(GovernableParam::BrnRate),
            )
            .unwrap();
        assert_eq!(count(&delegation, &votes), (2, 1, 2));
        delegation
            .delegate_scoped(owner, &people[4], DelegationScope::Proposal(proposal.hash))
            .unwrap();
        assert_eq!(count(&delegation, &votes), (3, 1, 1));
        votes.insert(owner.clone(), GovernanceVote::Nay);
        assert_eq!(count(&delegation, &votes), (2, 2, 1));
        let restored = DelegationEngine::load_state(&delegation.save_state()).unwrap();
        assert_eq!(count(&restored, &votes), (2, 2, 1));
    }

    #[test]
    fn revoked_intermediate_cannot_redirect_another_humans_vote() {
        let a = voter_wallet(1);
        let b = voter_wallet(2);
        let c = voter_wallet(3);
        let mut delegation = DelegationEngine::new(10);
        delegation.delegate(&a, &b).unwrap();
        delegation.delegate(&b, &c).unwrap();
        let votes = OrdMap::from_iter([(c.clone(), GovernanceVote::Yea)]);
        assert_eq!(
            GovernanceEngine::count_votes_with_delegation(
                &votes,
                &delegation,
                &[a.clone(), c.clone()],
                None
            ),
            (1, 0, 0)
        );
        assert_eq!(
            GovernanceEngine::count_votes_with_delegation(&votes, &delegation, &[a, b, c], None),
            (3, 0, 0)
        );
    }

    #[test]
    fn test_effective_votes_with_delegation() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let mut delegation_engine = crate::vote_delegation::DelegationEngine::new(10);

        let proposal = make_proposal(1000, 0);
        let hash = submit(&mut engine, proposal);
        engine.get_proposal_mut(&hash).unwrap().phase = GovernancePhase::Exploration;

        let delegate = voter_wallet(1);
        let delegator_a = voter_wallet(2);
        let delegator_b = voter_wallet(3);
        let direct_voter = voter_wallet(4);

        delegation_engine.delegate(&delegator_a, &delegate).unwrap();
        delegation_engine.delegate(&delegator_b, &delegate).unwrap();

        engine
            .cast_exploration_vote(
                &hash,
                &delegate,
                GovernanceVote::Yea,
                Timestamp::new(1000),
                &params,
            )
            .unwrap();
        engine
            .cast_exploration_vote(
                &hash,
                &direct_voter,
                GovernanceVote::Nay,
                Timestamp::new(1000),
                &params,
            )
            .unwrap();

        let all_wallets = vec![
            delegate.clone(),
            delegator_a.clone(),
            delegator_b.clone(),
            direct_voter.clone(),
        ];
        let (yea, nay, abstain) =
            engine.count_effective_exploration_votes(&hash, &delegation_engine, &all_wallets);
        // delegate voted Yea + 2 delegators = 3 yea, direct_voter voted Nay = 1 nay
        assert_eq!(yea, 3);
        assert_eq!(nay, 1);
        assert_eq!(abstain, 0);
    }

    #[test]
    fn test_effective_votes_direct_vote_overrides_delegation() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let mut delegation_engine = crate::vote_delegation::DelegationEngine::new(10);

        let proposal = make_proposal(1000, 0);
        let hash = submit(&mut engine, proposal);
        engine.get_proposal_mut(&hash).unwrap().phase = GovernancePhase::Exploration;

        let delegate = voter_wallet(1);
        let delegator = voter_wallet(2);

        delegation_engine.delegate(&delegator, &delegate).unwrap();

        engine
            .cast_exploration_vote(
                &hash,
                &delegate,
                GovernanceVote::Yea,
                Timestamp::new(1000),
                &params,
            )
            .unwrap();
        // Delegator votes directly Nay — overrides delegation
        engine
            .cast_exploration_vote(
                &hash,
                &delegator,
                GovernanceVote::Nay,
                Timestamp::new(1000),
                &params,
            )
            .unwrap();

        let all_wallets = vec![delegate.clone(), delegator.clone()];
        let (yea, nay, abstain) =
            engine.count_effective_exploration_votes(&hash, &delegation_engine, &all_wallets);
        assert_eq!(yea, 1); // only delegate
        assert_eq!(nay, 1); // only delegator (voted directly)
        assert_eq!(abstain, 0);
    }

    #[test]
    fn unverified_delegators_and_ineligible_old_votes_cannot_add_weight() {
        let representative = voter_wallet(1);
        let person = voter_wallet(2);
        let unverified = voter_wallet(3);
        let mut delegation = DelegationEngine::new(10);
        delegation.delegate(&person, &representative).unwrap();
        for id in 3..100 {
            delegation
                .delegate(&voter_wallet(id), &representative)
                .unwrap();
        }
        let direct = OrdMap::from_iter([
            (representative.clone(), GovernanceVote::Yea),
            (unverified, GovernanceVote::Nay),
        ]);
        let eligible = vec![representative.clone(), person.clone(), person.clone()];
        assert_eq!(
            GovernanceEngine::count_votes_with_delegation(&direct, &delegation, &eligible, None),
            (2, 0, 0)
        );
        // A stored ballot from a now-ineligible representative is not authority.
        assert_eq!(
            GovernanceEngine::count_votes_with_delegation(&direct, &delegation, &[person], None),
            (0, 0, 0)
        );
        assert_eq!(
            GovernanceEngine::count_votes_with_delegation(&direct, &delegation, &[], None),
            (0, 0, 0)
        );
    }

    #[test]
    fn test_effective_votes_no_delegation() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let delegation_engine = crate::vote_delegation::DelegationEngine::new(10);

        let proposal = make_proposal(1000, 0);
        let hash = submit(&mut engine, proposal);
        engine.get_proposal_mut(&hash).unwrap().phase = GovernancePhase::Exploration;

        engine
            .cast_exploration_vote(
                &hash,
                &voter_wallet(1),
                GovernanceVote::Yea,
                Timestamp::new(1000),
                &params,
            )
            .unwrap();
        engine
            .cast_exploration_vote(
                &hash,
                &voter_wallet(2),
                GovernanceVote::Nay,
                Timestamp::new(1000),
                &params,
            )
            .unwrap();

        let all_wallets = vec![voter_wallet(1), voter_wallet(2)];
        let (yea, nay, abstain) =
            engine.count_effective_exploration_votes(&hash, &delegation_engine, &all_wallets);
        assert_eq!(yea, 1);
        assert_eq!(nay, 1);
        assert_eq!(abstain, 0);
    }
}
