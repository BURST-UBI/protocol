//! BURST governance: Proposal → Voting → Cooldown → Activation.
//! `Exploration` is the stored name of the single voting phase. Promotion is
//! retained for decoding old snapshots but is not a valid transition.

use crate::delegation::DelegationEngine;
use crate::error::GovernanceError;
use crate::proposal::{GovernancePhase, Proposal, ProposalContent};
use burst_transactions::governance::GovernanceVote;
use burst_types::{ProtocolParams, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

mod snapshot_undo;

/// Delay between cooldown ending and activation applying.
/// Ensures all nodes activate the change at the same deterministic timestamp.
/// Uses the governance_propagation_buffer_secs parameter (default 3600s / 1 hour).
fn activation_delay_secs(params: &ProtocolParams) -> u64 {
    params.governance_propagation_buffer_secs
}

/// The governance engine manages proposals through the four-phase lifecycle,
/// tracking endorsements, votes, and phase transitions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceEngine {
    /// All submitted proposals indexed by their transaction hash.
    proposals: HashMap<TxHash, Proposal>,
    /// Exploration-phase votes per proposal: proposal_hash → (voter → vote).
    exploration_votes: HashMap<TxHash, HashMap<WalletAddress, GovernanceVote>>,
    /// Promotion-phase votes per proposal: proposal_hash → (voter → vote).
    promotion_votes: HashMap<TxHash, HashMap<WalletAddress, GovernanceVote>>,
    /// Endorsement tracking per proposal: proposal_hash → (count, total_brn_burned).
    endorsement_counts: HashMap<TxHash, (u32, u128)>,
    /// Parameter changes that have been activated but not yet propagated to engines.
    pending_changes: Vec<(crate::params::GovernableParam, u128)>,
    /// Constitutional amendments that have been activated but not yet applied by the node.
    pending_amendments: Vec<ProposalContent>,
    /// Activated ORV evictions/reinstatements not yet applied to account state:
    /// `(target, evict)` where `evict = true` evicts and `false` reinstates.
    #[serde(default)]
    pending_evictions: Vec<(WalletAddress, bool)>,
    timings: HashMap<TxHash, crate::GovernanceTiming>,
    key_ballots: HashMap<(TxHash, WalletAddress, burst_types::BlockHash), GovernanceVote>,
}

impl GovernanceEngine {
    /// Create a new governance engine with empty state.
    pub fn new() -> Self {
        Self {
            proposals: HashMap::new(),
            exploration_votes: HashMap::new(),
            promotion_votes: HashMap::new(),
            endorsement_counts: HashMap::new(),
            pending_changes: Vec::new(),
            pending_amendments: Vec::new(),
            pending_evictions: Vec::new(),
            timings: HashMap::new(),
            key_ballots: HashMap::new(),
        }
    }

    /// Submit a verified human's proposal. Emergencies enter voting immediately.
    ///
    /// Any verified wallet can submit. The anti-spam cost is paid by endorsers
    /// (whitepaper Governance Process and implementation decision 20.1).
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

    /// Submit an emergency proposal — enters Exploration phase immediately.
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
    /// Updates the internal `proposals` map so the withdrawal is persisted.
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

    /// Cast a vote during the Exploration phase.
    ///
    /// Validates the proposal is in the Exploration phase, that voting hasn't
    /// closed, and that the voter hasn't already voted. Records the vote and
    /// updates the proposal's aggregate vote counts.
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
        ballot: &crate::key_delegation::DelegatedBallot,
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

    /// Get a mutable reference to a stored proposal by hash.
    pub fn get_proposal_mut(&mut self, hash: &TxHash) -> Option<&mut Proposal> {
        self.proposals.get_mut(hash)
    }

    /// Return hashes of all proposals in an active (advanceable) phase.
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

    /// Advance all eligible proposals and activate any whose deferred timestamp has passed.
    /// Returns a list of proposals that were activated.
    ///
    /// Two-pass approach:
    /// 1. Activate proposals already in the Activation phase whose `activation_at` <= `now`.
    /// 2. Try advancing other proposals; newly promoted ones get deferred activation.
    pub fn tick(
        &mut self,
        now: Timestamp,
        params: &mut ProtocolParams,
        delegation: &DelegationEngine,
        verified_wallets: &[WalletAddress],
    ) -> Vec<TxHash> {
        let mut activated = Vec::new();

        // Pass 1: activate proposals whose deferred activation timestamp has arrived
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

    /// Advance scheduling without applying a parameter change off-ledger.
    /// The node must submit and validate an activation block before those effects
    /// become live. Existing queued effects are preserved for their owner.
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
    ) -> Option<&HashMap<WalletAddress, GovernanceVote>> {
        self.exploration_votes.get(hash)
    }

    /// Get all promotion votes for a proposal.
    pub fn get_promotion_votes(
        &self,
        hash: &TxHash,
    ) -> Option<&HashMap<WalletAddress, GovernanceVote>> {
        self.promotion_votes.get(hash)
    }

    /// Determine the required supermajority threshold for a proposal.
    ///
    /// Returns the supermajority required to pass a given proposal.
    ///
    /// Meta-threshold rules (whitepaper §6):
    /// - Changing `ConstiSupermajorityBps` requires the CURRENT consti threshold.
    ///   This is the self-referential property: "The Consti threshold is changed
    ///   by hitting that same Consti threshold (not the parameter threshold)."
    /// - Changing `GovernanceSupermajorityBps` uses the current threshold,
    ///   with no hardcoded floor (implementation decision 20.10).
    /// - Constitutional amendments require the consti threshold.
    /// - All other parameter changes use the normal governance threshold.
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
            // Evicting/reinstating a rep is a serious but ordinary governance
            // action → the normal governance supermajority (a values choice, not
            // a consti-level change).
            ProposalContent::RepresentativeEviction { .. } => params.governance_supermajority_bps,
        }
    }

    /// The quorum (participation) threshold required for a proposal, in bps.
    ///
    /// Mirrors [`get_required_supermajority`](Self::get_required_supermajority):
    /// constitutional amendments (and changes to the consti thresholds
    /// themselves) require the separately-governable consti quorum
    /// (`consti_quorum_bps`). Emergencies also require the emergency quorum.
    /// The whitepaper (§Governance Process) requires BOTH a supermajority AND a
    /// quorum — previously the consti quorum was never enforced.
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

    /// Calculate adaptive quorum based on historical participation (Tezos-style EMA).
    ///
    /// Formula: `adjusted_quorum = max(base_quorum, ema_participation * 0.8)`
    ///
    /// This prevents gaming by low-turnout votes while allowing participation to set the bar.
    /// All values are in basis points (10000 = 100%).
    pub fn adaptive_quorum(&self, base_quorum_bps: u32, ema_participation_bps: u32) -> u32 {
        let adjusted = (ema_participation_bps as u64 * 8000 / 10000) as u32;
        base_quorum_bps.max(adjusted)
    }

    /// Advance a proposal to the next phase if conditions are met.
    ///
    /// `delegation_ctx` supplies the delegation graph + current verified-wallet
    /// count so the vote tally counts EFFECTIVE (delegation-resolved) votes and
    /// uses a fresh eligibility denominator. Pass `None` to tally the raw direct
    /// counters against the proposal's snapshotted eligibility (used in tests and
    /// when delegation isn't relevant).
    pub fn try_advance(
        &self,
        proposal: &mut Proposal,
        now: Timestamp,
        params: &ProtocolParams,
        delegation_ctx: Option<(&DelegationEngine, &[WalletAddress])>,
    ) -> Result<GovernancePhase, GovernanceError> {
        match proposal.phase {
            GovernancePhase::Proposal => self.try_advance_proposal(proposal, now, params),
            GovernancePhase::Exploration => {
                self.try_advance_exploration(proposal, now, params, delegation_ctx)
            }
            GovernancePhase::Cooldown => self.try_advance_cooldown(proposal, now, params),
            _ => Err(GovernanceError::WrongPhase),
        }
    }

    /// Resolve the (yea, nay, abstain, eligible_denominator) used for a phase
    /// tally. With a delegation context, votes are resolved through the
    /// delegation graph (a delegator who did not vote directly is counted for
    /// its delegate — no double counting) and the denominator is the live
    /// verified-wallet count; without it, the raw direct counters and the
    /// proposal's snapshotted eligibility are used.
    fn tally_inputs(
        &self,
        proposal: &Proposal,
        phase: GovernancePhase,
        delegation_ctx: Option<(&DelegationEngine, &[WalletAddress])>,
    ) -> (u32, u32, u32, u32) {
        match delegation_ctx {
            Some((delegation, verified_wallets)) => {
                let (yea, nay, abstain) = if phase == GovernancePhase::Promotion {
                    self.count_effective_promotion_votes(
                        &proposal.hash,
                        delegation,
                        verified_wallets,
                    )
                } else {
                    self.count_effective_exploration_votes(
                        &proposal.hash,
                        delegation,
                        verified_wallets,
                    )
                };
                (
                    yea,
                    nay,
                    abstain,
                    verified_wallets
                        .iter()
                        .collect::<std::collections::HashSet<_>>()
                        .len() as u32,
                )
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
        delegation_ctx: Option<(&DelegationEngine, &[WalletAddress])>,
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

        let effective_quorum = Self::get_required_quorum(proposal, params);

        let supermajority_bps = Self::get_required_supermajority(proposal, params);

        let (yea, nay, abstain, eligible) =
            self.tally_inputs(proposal, GovernancePhase::Exploration, delegation_ctx);
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
            effective_quorum,
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
        let supermajority_actual_bps = (u64::from(votes_yea) * 10000)
            .checked_div(total_yea_nay)
            .unwrap_or(0) as u32;

        if supermajority_actual_bps < supermajority_bps {
            self.fail_proposal(proposal, now, params);
            return Err(GovernanceError::SupermajorityNotMet {
                have_bps: supermajority_actual_bps,
                need_bps: supermajority_bps,
            });
        }

        Ok(())
    }

    /// A failed vote is final. Resubmission needs a new proposal and endorsements.
    fn fail_proposal(&self, proposal: &mut Proposal, _now: Timestamp, _params: &ProtocolParams) {
        proposal.phase = GovernancePhase::Rejected;
    }

    /// Safely convert u128 to u32, saturating at u32::MAX to prevent truncation.
    #[inline]
    fn saturating_u32(value: u128) -> u32 {
        u32::try_from(value).unwrap_or(u32::MAX)
    }

    /// Safely convert u128 to u64, saturating at u64::MAX to prevent truncation.
    #[inline]
    fn saturating_u64(value: u128) -> u64 {
        u64::try_from(value).unwrap_or(u64::MAX)
    }

    /// Apply a parameter change from the given param and value.
    fn apply_param_change(
        param: &crate::params::GovernableParam,
        new_value: u128,
        params: &mut ProtocolParams,
    ) {
        match param {
            crate::params::GovernableParam::BrnRate => {
                params.brn_rate = new_value;
            }
            crate::params::GovernableParam::TrstExpirySecs => {
                params.trst_expiry_secs = Self::saturating_u64(new_value);
            }
            crate::params::GovernableParam::EndorsementThreshold => {
                params.endorsement_threshold = Self::saturating_u32(new_value);
            }
            crate::params::GovernableParam::EndorsementBurnAmount => {
                params.endorsement_burn_amount = new_value;
            }
            crate::params::GovernableParam::NumVerifiers => {
                params.num_verifiers = Self::saturating_u32(new_value);
            }
            crate::params::GovernableParam::VerificationThresholdBps => {
                params.verification_threshold_bps = Self::saturating_u32(new_value);
            }
            crate::params::GovernableParam::VerifierStakeAmount => {
                params.verifier_stake_amount = new_value;
            }
            crate::params::GovernableParam::MaxRevotes => {
                params.max_revotes = Self::saturating_u32(new_value);
            }
            crate::params::GovernableParam::ChallengeStakeAmount => {
                params.challenge_stake_amount = new_value;
            }
            crate::params::GovernableParam::GovernanceProposalDurationSecs => {
                params.governance_proposal_duration_secs = Self::saturating_u64(new_value);
            }
            crate::params::GovernableParam::GovernanceExplorationDurationSecs => {
                params.governance_exploration_duration_secs = Self::saturating_u64(new_value);
            }
            crate::params::GovernableParam::GovernanceCooldownDurationSecs => {
                params.governance_cooldown_duration_secs = Self::saturating_u64(new_value);
            }
            crate::params::GovernableParam::GovernancePromotionDurationSecs => {
                params.governance_promotion_duration_secs = Self::saturating_u64(new_value);
            }
            crate::params::GovernableParam::GovernanceSupermajorityBps => {
                params.governance_supermajority_bps = Self::saturating_u32(new_value);
            }
            crate::params::GovernableParam::GovernanceQuorumBps => {
                params.governance_quorum_bps = Self::saturating_u32(new_value);
            }
            crate::params::GovernableParam::GovernanceProposalEndorsements => {
                params.governance_proposal_endorsements = Self::saturating_u32(new_value);
            }
            crate::params::GovernableParam::GovernanceEmaParticipationBps => {
                params.governance_ema_participation_bps = Self::saturating_u32(new_value);
            }
            crate::params::GovernableParam::ConstiSupermajorityBps => {
                params.consti_supermajority_bps = Self::saturating_u32(new_value);
            }
            crate::params::GovernableParam::ConstiQuorumBps => {
                params.consti_quorum_bps = Self::saturating_u32(new_value);
            }
            crate::params::GovernableParam::VerificationTimeoutSecs => {
                params.verification_timeout_secs = Self::saturating_u64(new_value);
            }
            crate::params::GovernableParam::ChallengeDurationSecs => {
                params.challenge_duration_secs = Self::saturating_u64(new_value);
            }
            crate::params::GovernableParam::ChallengeRewardBps => {
                params.challenge_reward_bps = Self::saturating_u32(new_value);
            }
            crate::params::GovernableParam::ChallengeRewardCap => {
                params.challenge_reward_cap = new_value;
            }
            crate::params::GovernableParam::NewWalletSpendingLimit => {
                params.new_wallet_spending_limit = new_value;
            }
            crate::params::GovernableParam::NewWalletLimitDurationSecs => {
                params.new_wallet_limit_duration_secs = Self::saturating_u64(new_value);
            }
            crate::params::GovernableParam::BootstrapExitThreshold => {
                params.bootstrap_exit_threshold = Self::saturating_u32(new_value);
            }
            crate::params::GovernableParam::NewWalletTxLimitPerDay => {
                params.new_wallet_tx_limit_per_day = Self::saturating_u32(new_value);
            }
            crate::params::GovernableParam::NewWalletRateLimitDurationSecs => {
                params.new_wallet_rate_limit_duration_secs = Self::saturating_u64(new_value);
            }
            crate::params::GovernableParam::GovernanceProposalCost => {
                params.governance_proposal_cost = new_value;
            }
            crate::params::GovernableParam::GovernanceMaxRounds => {
                params.governance_max_rounds = Self::saturating_u32(new_value);
            }
            crate::params::GovernableParam::GovernanceProposalWindowSecs => {
                params.governance_proposal_window_secs = Self::saturating_u64(new_value);
            }
            crate::params::GovernableParam::GovernancePropagationBufferSecs => {
                params.governance_propagation_buffer_secs = Self::saturating_u64(new_value);
            }
            crate::params::GovernableParam::MinWorkDifficulty => {
                params.min_work_difficulty = Self::saturating_u64(new_value);
            }
            crate::params::GovernableParam::MinVerificationAgeSecs => {
                params.min_verification_age_secs = Self::saturating_u64(new_value);
            }
            crate::params::GovernableParam::NeitherPenaltyCooldownSecs => {
                params.neither_penalty_cooldown_secs = Self::saturating_u64(new_value);
            }
            crate::params::GovernableParam::NeitherPenaltyThresholdBps => {
                params.neither_penalty_threshold_bps = Self::saturating_u32(new_value);
            }
            crate::params::GovernableParam::NeitherWindowAssignments => {
                params.neither_window_assignments = Self::saturating_u32(new_value);
            }
            crate::params::GovernableParam::MinVerifierBrnBalance => {
                params.min_verifier_brn_balance = new_value;
            }
            crate::params::GovernableParam::EmergencyVotingDurationSecs => {
                params.emergency_voting_duration_secs = Self::saturating_u64(new_value);
            }
            crate::params::GovernableParam::EmergencyQuorumBps => {
                params.emergency_quorum_bps = Self::saturating_u32(new_value);
            }
            crate::params::GovernableParam::EmergencySupermajorityBps => {
                params.emergency_supermajority_bps = Self::saturating_u32(new_value);
            }
        }
    }

    /// Activate a proposal — apply the parameter change to the protocol.
    ///
    /// Records the change in `pending_changes` so the node can propagate it
    /// to subsystem engines (e.g. BRN rate changes to wallet states).
    pub fn activate(
        &mut self,
        proposal: &Proposal,
        params: &mut ProtocolParams,
    ) -> Result<(), GovernanceError> {
        match &proposal.content {
            ProposalContent::ParameterChange { param, new_value } => {
                Self::apply_param_change(param, *new_value, params);
                self.pending_changes.push((param.clone(), *new_value));
                Ok(())
            }
            ProposalContent::ConstitutionalAmendment { .. }
            | ProposalContent::ConstitutionalDiff { .. } => {
                self.pending_amendments.push(proposal.content.clone());
                Ok(())
            }
            ProposalContent::Emergency {
                param, new_value, ..
            } => {
                Self::apply_param_change(param, *new_value, params);
                self.pending_changes.push((param.clone(), *new_value));
                Ok(())
            }
            ProposalContent::RepresentativeEviction { target, evict, .. } => {
                // Recorded for the node to apply to account state (sets/clears
                // `orv_evicted`). No ProtocolParams change.
                self.pending_evictions.push((target.clone(), *evict));
                Ok(())
            }
        }
    }

    /// Calculate an activation hash without applying or queuing any effects.
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

    /// Queue effects only if the activation's committed parameter hash matches.
    /// Failed validation must not leak effects to the periodic propagation task.
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

    /// Get pending parameter changes that have been activated but not yet propagated.
    ///
    /// The node calls this periodically to propagate changes to subsystem engines
    /// (e.g. applying BRN rate changes to all tracked wallet states).
    pub fn drain_pending_changes(&mut self) -> Vec<(crate::params::GovernableParam, u128)> {
        std::mem::take(&mut self.pending_changes)
    }

    /// Drain activated constitutional amendments that need to be applied by the node.
    pub fn drain_activated_amendments(&mut self) -> Vec<ProposalContent> {
        std::mem::take(&mut self.pending_amendments)
    }

    /// Drain activated ORV evictions/reinstatements. Each `(target, evict)` tells
    /// the node to set (`evict = true`) or clear (`false`) the target account's
    /// `orv_evicted` flag, removing/restoring it from the consensus weight set.
    pub fn drain_pending_evictions(&mut self) -> Vec<(WalletAddress, bool)> {
        std::mem::take(&mut self.pending_evictions)
    }

    /// Count effective votes for a proposal, including transitively delegated votes.
    ///
    /// A wallet that votes directly has its vote counted as-is. For wallets
    /// that delegated their vote and did NOT vote directly, their delegation
    /// chain is resolved transitively (A→B→C) and the ultimate delegate's
    /// vote is used. This implements the whitepaper rule:
    /// "If a wallet has delegated its vote, the delegate votes on its behalf."
    pub fn count_effective_exploration_votes(
        &self,
        proposal_hash: &TxHash,
        delegation_engine: &DelegationEngine,
        all_verified_wallets: &[WalletAddress],
    ) -> (u32, u32, u32) {
        let empty = HashMap::new();
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

    /// Count effective promotion votes for a proposal, including transitively delegated votes.
    ///
    /// Same logic as [`count_effective_exploration_votes`] but for the promotion phase.
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
        direct_votes: &HashMap<WalletAddress, GovernanceVote>,
        delegation_engine: &DelegationEngine,
        all_verified_wallets: &[WalletAddress],
        proposal: Option<&Proposal>,
    ) -> (u32, u32, u32) {
        let eligible: std::collections::HashSet<_> = all_verified_wallets.iter().collect();
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
        let (mut yea, mut nay, mut abstain) = (0, 0, 0);
        for wallet in &eligible {
            let vote = direct_votes.get(*wallet).copied().or_else(|| {
                if let Some(vote) = delegation_engine.key_ballot(wallet) {
                    return vote;
                }
                let delegate = delegation_engine.resolve_for_ballot(
                    wallet,
                    proposal.map(|p| &p.hash),
                    parameter,
                    category,
                    |person| eligible.contains(person),
                )?;
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
    /// The node should call this after a successful `try_advance` from Exploration
    /// or Promotion to obtain the participation rate, then pass it to `update_ema`.
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

    /// Update the participation EMA after a vote phase completes.
    /// Tezos formula: new_ema = (8 * old_ema + 2 * current_participation) / 10
    ///
    /// The node must call this after a successful `try_advance` for Exploration
    /// and Promotion phases to keep the adaptive quorum up to date.
    pub fn update_ema(params: &mut ProtocolParams, participation_bps: u32) {
        let old = params.governance_ema_participation_bps as u64;
        let current = participation_bps as u64;
        let new_ema = (8 * old + 2 * current) / 10;
        params.governance_ema_participation_bps = new_ema as u32;
    }

    /// Get all proposals currently in the Proposal or Exploration phase,
    /// sorted by endorsement count (highest first).
    ///
    /// Useful for displaying competing proposals in the same governance period
    /// so stakeholders can compare support levels.
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
    /// Must be called by the node when advancing to Exploration or Promotion so
    /// that `total_eligible_voters` reflects the count at phase-entry time.
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

    /// Helper to create a proposal in the Proposal phase.
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

    /// Helper to create an emergency proposal.
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

    /// Helper to create unique TxHash from a seed byte.
    fn unique_hash(seed: u8) -> TxHash {
        TxHash::new([seed; 32])
    }

    /// Helper to create unique wallet addresses for voters.
    fn voter_wallet(id: u32) -> WalletAddress {
        WalletAddress::new(format!("brst_{:0>75}", id))
    }

    // ── Submit / Endorse / Vote ────────────────────────────────────────

    /// Helper: submit a proposal with unlimited BRN (for tests not focused on cost).
    fn submit(engine: &mut GovernanceEngine, proposal: Proposal) -> TxHash {
        let params = default_params();
        engine
            .submit_proposal(proposal, u128::MAX, true, &params)
            .unwrap()
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

        // A votes Yea directly; B and C delegate to A and do NOT vote.
        engine
            .cast_exploration_vote(
                &hash,
                &a,
                GovernanceVote::Yea,
                Timestamp::new(1000),
                &params,
            )
            .unwrap();
        let mut del = crate::delegation::DelegationEngine::new(10);
        del.delegate(&b, &a).unwrap();
        del.delegate(&c, &a).unwrap();

        let proposal = engine.get_proposal(&hash).unwrap();

        // No delegation context → only A's direct vote is counted.
        let (yea, _, _, _) = engine.tally_inputs(proposal, GovernancePhase::Exploration, None);
        assert_eq!(yea, 1);

        // With delegation, A carries B and C's votes too → 3 effective Yea, and
        // the denominator is the live verified-wallet count. No double counting:
        // B and C did not vote directly.
        let (yea, nay, abstain, eligible) = engine.tally_inputs(
            proposal,
            GovernancePhase::Exploration,
            Some((&del, &(1..=10).map(voter_wallet).collect::<Vec<_>>())),
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

    // ── Proposal → Exploration ────────────────────────────────────────

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

    // ── Exploration → Cooldown ────────────────────────────────────────

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

    // ── Cooldown → Promotion ──────────────────────────────────────────

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

    // ── Promotion → Activation ────────────────────────────────────────

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

    // ── Adaptive Quorum ───────────────────────────────────────────────

    #[test]
    fn test_adaptive_quorum_uses_base_when_participation_low() {
        let engine = GovernanceEngine::new();
        // base = 3000 (30%), EMA participation = 2000 (20%)
        // adjusted = 2000 * 0.8 = 1600 → max(3000, 1600) = 3000
        let result = engine.adaptive_quorum(3000, 2000);
        assert_eq!(result, 3000);
    }

    #[test]
    fn test_adaptive_quorum_uses_ema_when_participation_high() {
        let engine = GovernanceEngine::new();
        // base = 3000 (30%), EMA participation = 8000 (80%)
        // adjusted = 8000 * 0.8 = 6400 → max(3000, 6400) = 6400
        let result = engine.adaptive_quorum(3000, 8000);
        assert_eq!(result, 6400);
    }

    #[test]
    fn test_adaptive_quorum_exact_boundary() {
        let engine = GovernanceEngine::new();
        // base = 3000, EMA = 3750 → adjusted = 3750 * 0.8 = 3000 → max(3000, 3000) = 3000
        let result = engine.adaptive_quorum(3000, 3750);
        assert_eq!(result, 3000);
    }

    #[test]
    fn test_adaptive_quorum_zero_participation() {
        let engine = GovernanceEngine::new();
        let result = engine.adaptive_quorum(3000, 0);
        assert_eq!(result, 3000);
    }

    // ── Full 5-Phase Lifecycle (Happy Path) ───────────────────────────

    #[test]
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

    // ── Full lifecycle using engine methods ───────────────────────────

    #[test]
    fn test_full_lifecycle_via_engine_methods() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();

        // Submit proposal
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

        // Store the updated proposal back
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

    // ── Activate ──────────────────────────────────────────────────────

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
        // Eviction is an ordinary governance action → normal governance
        // supermajority/quorum (a values choice, not a consti-level change).
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

    // ── Proposal Withdrawal (6.4) ─────────────────────────────────────

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

    // ── Emergency Governance (6.3) ────────────────────────────────────

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

        // Set exploration votes (95% yea to meet 95% supermajority)
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

        // Submit
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

        // Activate — apply the parameter change
        let mut protocol_params = default_params();
        engine.activate(&proposal, &mut protocol_params).unwrap();
        assert_eq!(protocol_params.brn_rate, 500);
    }

    // ── Fix 2: ConstiSupermajorityBps requires 90% threshold ──────────

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
    fn test_consti_supermajority_change_passes_at_90_percent() {
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

    // ── Fix 5: Governance Proposal BRN Cost ───────────────────────────

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
    fn test_submit_proposal_exact_brn_cost() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let proposal = make_proposal(1000, 0);

        // Exact cost should succeed
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

    // ── Fix 5B: New GovernableParam variants applied correctly ────────

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
    fn test_activate_new_wallet_tx_limit() {
        let mut engine = GovernanceEngine::new();
        let mut params = default_params();

        let mut proposal = make_proposal(0, 10);
        proposal.phase = GovernancePhase::Activation;
        proposal.content = ProposalContent::ParameterChange {
            param: crate::params::GovernableParam::NewWalletTxLimitPerDay,
            new_value: 25,
        };

        engine.activate(&proposal, &mut params).unwrap();
        assert_eq!(params.new_wallet_tx_limit_per_day, 25);
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

    // ── EMA Update ───────────────────────────────────────────────────

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
                                                          // new_ema = (8 * 5000 + 2 * 10000) / 10 = (40000 + 20000) / 10 = 6000
        assert_eq!(params.governance_ema_participation_bps, 6000);
    }

    #[test]
    fn test_update_ema_low_participation_drags_down() {
        let mut params = default_params();
        params.governance_ema_participation_bps = 8000;
        GovernanceEngine::update_ema(&mut params, 2000);
        // new_ema = (8 * 8000 + 2 * 2000) / 10 = (64000 + 4000) / 10 = 6800
        assert_eq!(params.governance_ema_participation_bps, 6800);
    }

    #[test]
    fn test_update_ema_same_participation_stays_stable() {
        let mut params = default_params();
        params.governance_ema_participation_bps = 5000;
        GovernanceEngine::update_ema(&mut params, 5000);
        // new_ema = (8 * 5000 + 2 * 5000) / 10 = 50000 / 10 = 5000
        assert_eq!(params.governance_ema_participation_bps, 5000);
    }

    #[test]
    fn test_update_ema_from_zero() {
        let mut params = default_params();
        params.governance_ema_participation_bps = 0;
        GovernanceEngine::update_ema(&mut params, 7000);
        // new_ema = (0 + 2 * 7000) / 10 = 14000 / 10 = 1400
        assert_eq!(params.governance_ema_participation_bps, 1400);
    }

    // ── Competing Proposals ──────────────────────────────────────────

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

    // ── Snapshot Eligible Voters ──────────────────────────────────────

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

    // ── Proposal Competition ────────────────────────────────────────

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
        let delegation = crate::delegation::DelegationEngine::new(10);
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

    // ── Failure Retry ───────────────────────────────────────────────

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

    // ── Delegation Tally ────────────────────────────────────────────

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
            HashMap::from([(people[2].clone(), GovernanceVote::Yea)]),
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
        let mut votes = HashMap::from([
            (people[1].clone(), GovernanceVote::Yea),
            (people[2].clone(), GovernanceVote::Nay),
            (people[3].clone(), GovernanceVote::Abstain),
            (people[4].clone(), GovernanceVote::Yea),
        ]);
        let count = |del: &DelegationEngine, direct: &HashMap<_, _>| {
            GovernanceEngine::count_votes_with_delegation(direct, del, &people, Some(&proposal))
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
        let votes = HashMap::from([(c.clone(), GovernanceVote::Yea)]);
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
        let mut delegation_engine = crate::delegation::DelegationEngine::new(10);

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
        let mut delegation_engine = crate::delegation::DelegationEngine::new(10);

        let proposal = make_proposal(1000, 0);
        let hash = submit(&mut engine, proposal);
        engine.get_proposal_mut(&hash).unwrap().phase = GovernancePhase::Exploration;

        let delegate = voter_wallet(1);
        let delegator = voter_wallet(2);

        delegation_engine.delegate(&delegator, &delegate).unwrap();

        // Delegate votes Yea
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
        let direct = HashMap::from([
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
        let delegation_engine = crate::delegation::DelegationEngine::new(10);

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
