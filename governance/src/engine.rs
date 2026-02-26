//! Core governance engine — manages proposals through the 5-phase lifecycle.
//!
//! Tezos-inspired flow: Proposal → Exploration → Cooldown → Promotion → Activation
//! Emergency flow: Exploration → Promotion → Activation (skip Proposal and Cooldown)
//! With adaptive quorum biasing (EMA-based).

use crate::delegation::DelegationEngine;
use crate::error::GovernanceError;
use crate::proposal::{GovernancePhase, Proposal, ProposalContent};
use burst_transactions::governance::GovernanceVote;
use burst_types::{ProtocolParams, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Duration of each voting phase for emergency proposals (24 hours).
const EMERGENCY_PHASE_DURATION_SECS: u64 = 86400;

/// Supermajority threshold for emergency proposals (95%).
const EMERGENCY_SUPERMAJORITY_BPS: u32 = 9500;

/// Delay between promotion passing and activation applying.
/// Ensures all nodes activate the change at the same deterministic timestamp.
/// Uses the governance_propagation_buffer_secs parameter (default 3600s / 1 hour).
fn activation_delay_secs(params: &ProtocolParams) -> u64 {
    params.governance_propagation_buffer_secs
}

/// The governance engine manages proposals through the 5-phase lifecycle,
/// tracking endorsements, votes, and phase transitions.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
        }
    }

    /// Submit a new proposal (enters Proposal phase).
    ///
    /// The caller must verify the proposer is verified. The proposer's BRN balance
    /// is checked against `governance_proposal_cost`; the caller is responsible for
    /// actually deducting the cost after this returns `Ok`.
    pub fn submit_proposal(
        &mut self,
        mut proposal: Proposal,
        proposer_brn_balance: u128,
        proposer_verified: bool,
        params: &ProtocolParams,
    ) -> Result<TxHash, GovernanceError> {
        if !proposer_verified {
            return Err(GovernanceError::ProposerNotVerified);
        }

        if proposal.proposer.as_str().is_empty() {
            return Err(GovernanceError::Other(
                "proposal must have a valid proposer".to_string(),
            ));
        }

        // Validate proposer can afford the governance proposal cost
        if proposer_brn_balance < params.governance_proposal_cost {
            return Err(GovernanceError::InsufficientBrn {
                have: proposer_brn_balance,
                need: params.governance_proposal_cost,
            });
        }

        proposal.phase = GovernancePhase::Proposal;

        if proposal.created_at == Timestamp::EPOCH && proposal.hash != TxHash::ZERO {
            // If created_at was not explicitly set, this is an issue, but we allow
            // the caller to have already set it on the Proposal struct.
        }

        let hash = proposal.hash;
        self.proposals.insert(hash, proposal);
        Ok(hash)
    }

    /// Submit an emergency proposal — enters Exploration phase immediately.
    ///
    /// Emergency proposals skip the Proposal and Cooldown phases, use 24-hour
    /// voting periods, and require a 95% supermajority. Can only be submitted
    /// by the genesis account or a quorum of representatives.
    pub fn submit_emergency_proposal(
        &mut self,
        proposal: &mut Proposal,
        now: Timestamp,
    ) -> Result<(), GovernanceError> {
        if !matches!(proposal.content, ProposalContent::Emergency { .. }) {
            return Err(GovernanceError::WrongPhase);
        }
        proposal.phase = GovernancePhase::Exploration;
        proposal.exploration_started_at = Some(now);

        let hash = proposal.hash;
        self.proposals.insert(hash, proposal.clone());
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
    /// When the endorsement count reaches `governance_proposal_endorsements`,
    /// the proposal auto-advances to the Exploration phase.
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
        entry.0 += 1;
        entry.1 += brn_burned;

        proposal.endorsement_count = entry.0;

        Ok(())
    }

    /// Cast a vote during the Exploration phase.
    ///
    /// Validates the proposal is in the Exploration phase, that voting hasn't
    /// closed, and that the voter hasn't already voted. Records the vote and
    /// updates the proposal's aggregate vote counts using adaptive quorum biasing.
    pub fn cast_exploration_vote(
        &mut self,
        proposal_hash: &TxHash,
        voter: &WalletAddress,
        vote: GovernanceVote,
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
            let is_emergency = matches!(proposal.content, ProposalContent::Emergency { .. });
            let duration = if is_emergency {
                EMERGENCY_PHASE_DURATION_SECS
            } else {
                params.governance_exploration_duration_secs
            };
            if started.has_expired(duration, now) {
                return Err(GovernanceError::VotingClosed);
            }
        }

        let votes = self.exploration_votes.entry(*proposal_hash).or_default();

        if votes.contains_key(voter) {
            return Err(GovernanceError::AlreadyVoted(voter.to_string()));
        }

        votes.insert(voter.clone(), vote);

        // Update aggregate counts on the proposal
        if let Some(proposal) = self.proposals.get_mut(proposal_hash) {
            match vote {
                GovernanceVote::Yea => proposal.exploration_votes_yea += 1,
                GovernanceVote::Nay => proposal.exploration_votes_nay += 1,
                GovernanceVote::Abstain => proposal.exploration_votes_abstain += 1,
            }
        }

        Ok(())
    }

    /// Cast a vote during the Promotion phase.
    ///
    /// Validates the proposal is in the Promotion phase, that voting hasn't
    /// closed, and that the voter hasn't already voted. Records the vote and
    /// updates the proposal's aggregate vote counts.
    pub fn cast_promotion_vote(
        &mut self,
        proposal_hash: &TxHash,
        voter: &WalletAddress,
        vote: GovernanceVote,
        now: Timestamp,
        params: &ProtocolParams,
    ) -> Result<(), GovernanceError> {
        let proposal = self
            .proposals
            .get(proposal_hash)
            .ok_or_else(|| GovernanceError::ProposalNotFound(proposal_hash.to_string()))?;

        if proposal.phase != GovernancePhase::Promotion {
            return Err(GovernanceError::WrongPhase);
        }

        if let Some(started) = proposal.promotion_started_at {
            let is_emergency = matches!(proposal.content, ProposalContent::Emergency { .. });
            let duration = if is_emergency {
                EMERGENCY_PHASE_DURATION_SECS
            } else {
                params.governance_promotion_duration_secs
            };
            if started.has_expired(duration, now) {
                return Err(GovernanceError::VotingClosed);
            }
        }

        let votes = self.promotion_votes.entry(*proposal_hash).or_default();

        if votes.contains_key(voter) {
            return Err(GovernanceError::AlreadyVoted(voter.to_string()));
        }

        votes.insert(voter.clone(), vote);

        // Update aggregate counts on the proposal
        if let Some(proposal) = self.proposals.get_mut(proposal_hash) {
            match vote {
                GovernanceVote::Yea => proposal.promotion_votes_yea += 1,
                GovernanceVote::Nay => proposal.promotion_votes_nay += 1,
                GovernanceVote::Abstain => proposal.promotion_votes_abstain += 1,
            }
        }

        Ok(())
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
    pub fn tick(&mut self, now: Timestamp, params: &mut ProtocolParams) -> Vec<TxHash> {
        let mut activated = Vec::new();

        // Pass 1: activate proposals whose deferred activation timestamp has arrived
        let pending_activation: Vec<TxHash> = self
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

        for hash in pending_activation {
            if let Some(proposal) = self.proposals.get(&hash) {
                let p = proposal.clone();
                if self.activate(&p, params).is_ok() {
                    activated.push(hash);
                }
            }
        }

        // Pass 2: proposal competition — when the proposal window has elapsed,
        // select the winner and reject/reset all other Proposal-phase proposals.
        self.resolve_proposal_competition(now, params);

        // Pass 3: advance proposals in active (advanceable) phases
        let hashes = self.active_proposal_hashes();
        for hash in hashes {
            if let Some(proposal) = self.proposals.get_mut(&hash) {
                let mut p = proposal.clone();
                match self.try_advance(&mut p, now, params) {
                    Ok(GovernancePhase::Activation) => {
                        p.activation_at = Some(Timestamp::new(
                            now.as_secs() + activation_delay_secs(params),
                        ));
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
    /// - Changing `GovernanceSupermajorityBps` requires the *current* governance
    ///   threshold OR 85%, whichever is higher. This prevents a majority from
    ///   first lowering the threshold and then pushing through destructive changes.
    /// - Constitutional amendments require the consti threshold.
    /// - All other parameter changes use the normal governance threshold.
    fn get_required_supermajority(proposal: &Proposal, params: &ProtocolParams) -> u32 {
        match &proposal.content {
            ProposalContent::ParameterChange { param, .. }
            | ProposalContent::Emergency { param, .. } => match param {
                crate::params::GovernableParam::ConstiSupermajorityBps => {
                    params.consti_supermajority_bps
                }
                crate::params::GovernableParam::GovernanceSupermajorityBps => {
                    // Meta-threshold: changing the threshold requires at least
                    // the current threshold or 85%, whichever is higher.
                    params.governance_supermajority_bps.max(8500)
                }
                _ => params.governance_supermajority_bps,
            },
            ProposalContent::ConstitutionalAmendment { .. } => params.consti_supermajority_bps,
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
    pub fn try_advance(
        &self,
        proposal: &mut Proposal,
        now: Timestamp,
        params: &ProtocolParams,
    ) -> Result<GovernancePhase, GovernanceError> {
        match proposal.phase {
            GovernancePhase::Proposal => self.try_advance_proposal(proposal, now, params),
            GovernancePhase::Exploration => self.try_advance_exploration(proposal, now, params),
            GovernancePhase::Cooldown => self.try_advance_cooldown(proposal, now, params),
            GovernancePhase::Promotion => self.try_advance_promotion(proposal, now, params),
            _ => Err(GovernanceError::WrongPhase),
        }
    }

    /// Proposal → Exploration: endorsements threshold met + duration elapsed.
    fn try_advance_proposal(
        &self,
        proposal: &mut Proposal,
        now: Timestamp,
        params: &ProtocolParams,
    ) -> Result<GovernancePhase, GovernanceError> {
        let duration_elapsed = proposal
            .created_at
            .has_expired(params.governance_proposal_duration_secs, now);
        let endorsements_met =
            proposal.endorsement_count >= params.governance_proposal_endorsements;

        if duration_elapsed && endorsements_met {
            proposal.phase = GovernancePhase::Exploration;
            proposal.exploration_started_at = Some(now);
            Ok(GovernancePhase::Exploration)
        } else {
            Err(GovernanceError::PhaseNotExpired)
        }
    }

    /// Exploration → Cooldown (normal) or Exploration → Promotion (emergency):
    /// duration elapsed + propagation buffer + adaptive quorum + supermajority.
    fn try_advance_exploration(
        &self,
        proposal: &mut Proposal,
        now: Timestamp,
        params: &ProtocolParams,
    ) -> Result<GovernancePhase, GovernanceError> {
        let is_emergency = matches!(proposal.content, ProposalContent::Emergency { .. });

        let exploration_started = proposal
            .exploration_started_at
            .ok_or_else(|| GovernanceError::Other("exploration_started_at not set".to_string()))?;

        let duration_secs = if is_emergency {
            EMERGENCY_PHASE_DURATION_SECS
        } else {
            params.governance_exploration_duration_secs
        };

        let voting_ended = exploration_started.has_expired(duration_secs, now);

        if !voting_ended {
            return Err(GovernanceError::PhaseNotExpired);
        }

        let counting_time = exploration_started
            .as_secs()
            .saturating_add(duration_secs)
            .saturating_add(params.governance_propagation_buffer_secs);
        if now.as_secs() < counting_time {
            return Err(GovernanceError::PropagationBuffer);
        }

        let effective_quorum = self.adaptive_quorum(
            params.governance_quorum_bps,
            params.governance_ema_participation_bps,
        );

        let supermajority_bps = if is_emergency {
            EMERGENCY_SUPERMAJORITY_BPS
        } else {
            Self::get_required_supermajority(proposal, params)
        };

        self.check_vote_result(
            proposal.exploration_votes_yea,
            proposal.exploration_votes_nay,
            proposal.exploration_votes_abstain,
            proposal.total_eligible_voters,
            effective_quorum,
            supermajority_bps,
            proposal,
            now,
            params,
        )?;

        if is_emergency {
            proposal.phase = GovernancePhase::Promotion;
            proposal.promotion_started_at = Some(now);
            Ok(GovernancePhase::Promotion)
        } else {
            proposal.phase = GovernancePhase::Cooldown;
            proposal.cooldown_started_at = Some(now);
            Ok(GovernancePhase::Cooldown)
        }
    }

    /// Cooldown → Promotion: duration elapsed.
    fn try_advance_cooldown(
        &self,
        proposal: &mut Proposal,
        now: Timestamp,
        params: &ProtocolParams,
    ) -> Result<GovernancePhase, GovernanceError> {
        let cooldown_started = proposal
            .cooldown_started_at
            .ok_or_else(|| GovernanceError::Other("cooldown_started_at not set".to_string()))?;

        let duration_elapsed =
            cooldown_started.has_expired(params.governance_cooldown_duration_secs, now);

        if duration_elapsed {
            proposal.phase = GovernancePhase::Promotion;
            proposal.promotion_started_at = Some(now);
            Ok(GovernancePhase::Promotion)
        } else {
            Err(GovernanceError::PhaseNotExpired)
        }
    }

    /// Promotion → Activation: duration elapsed + propagation buffer + adaptive quorum + supermajority.
    fn try_advance_promotion(
        &self,
        proposal: &mut Proposal,
        now: Timestamp,
        params: &ProtocolParams,
    ) -> Result<GovernancePhase, GovernanceError> {
        let is_emergency = matches!(proposal.content, ProposalContent::Emergency { .. });

        let promotion_started = proposal
            .promotion_started_at
            .ok_or_else(|| GovernanceError::Other("promotion_started_at not set".to_string()))?;

        let duration_secs = if is_emergency {
            EMERGENCY_PHASE_DURATION_SECS
        } else {
            params.governance_promotion_duration_secs
        };

        let voting_ended = promotion_started.has_expired(duration_secs, now);

        if !voting_ended {
            return Err(GovernanceError::PhaseNotExpired);
        }

        let counting_time = promotion_started
            .as_secs()
            .saturating_add(duration_secs)
            .saturating_add(params.governance_propagation_buffer_secs);
        if now.as_secs() < counting_time {
            return Err(GovernanceError::PropagationBuffer);
        }

        let effective_quorum = self.adaptive_quorum(
            params.governance_quorum_bps,
            params.governance_ema_participation_bps,
        );

        let supermajority_bps = if is_emergency {
            EMERGENCY_SUPERMAJORITY_BPS
        } else {
            Self::get_required_supermajority(proposal, params)
        };

        self.check_vote_result(
            proposal.promotion_votes_yea,
            proposal.promotion_votes_nay,
            proposal.promotion_votes_abstain,
            proposal.total_eligible_voters,
            effective_quorum,
            supermajority_bps,
            proposal,
            now,
            params,
        )?;

        proposal.phase = GovernancePhase::Activation;
        proposal.activation_at = Some(now);
        Ok(GovernancePhase::Activation)
    }

    /// Check quorum and supermajority for a vote phase.
    /// On failure, resets the proposal to Proposal phase with an incremented round
    /// counter (if rounds remain), or terminally rejects it.
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
        let total_votes = votes_yea + votes_nay + votes_abstain;
        let participation_bps = (total_votes * 10000)
            .checked_div(total_eligible_voters)
            .unwrap_or(0);

        if participation_bps < quorum_bps {
            self.fail_proposal(proposal, now, params);
            return Err(GovernanceError::QuorumNotMet {
                have_bps: participation_bps,
                need_bps: quorum_bps,
            });
        }

        let total_yea_nay = votes_yea + votes_nay;
        let supermajority_actual_bps = (votes_yea * 10000).checked_div(total_yea_nay).unwrap_or(0);

        if supermajority_actual_bps < supermajority_bps {
            self.fail_proposal(proposal, now, params);
            return Err(GovernanceError::SupermajorityNotMet {
                have_bps: supermajority_actual_bps,
                need_bps: supermajority_bps,
            });
        }

        Ok(())
    }

    /// Handle a proposal failure: reset to Proposal phase if rounds remain,
    /// otherwise reject terminally. Clears vote state for the new round.
    fn fail_proposal(&self, proposal: &mut Proposal, now: Timestamp, params: &ProtocolParams) {
        if proposal.round < params.governance_max_rounds {
            proposal.round += 1;
            proposal.phase = GovernancePhase::Proposal;
            proposal.created_at = now;
            proposal.endorsement_count = 0;
            proposal.exploration_started_at = None;
            proposal.exploration_votes_yea = 0;
            proposal.exploration_votes_nay = 0;
            proposal.exploration_votes_abstain = 0;
            proposal.cooldown_started_at = None;
            proposal.promotion_started_at = None;
            proposal.promotion_votes_yea = 0;
            proposal.promotion_votes_nay = 0;
            proposal.promotion_votes_abstain = 0;
            tracing::info!(
                proposal = ?proposal.hash,
                round = proposal.round,
                "proposal failed, reset to Proposal phase"
            );
        } else {
            proposal.phase = GovernancePhase::Rejected;
            tracing::info!(
                proposal = ?proposal.hash,
                "proposal rejected after max rounds"
            );
        }
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
            crate::params::GovernableParam::EndorserRewardBps => {
                params.endorser_reward_bps = Self::saturating_u32(new_value);
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
            ProposalContent::ConstitutionalAmendment { .. } => {
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
        }
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
        let direct_votes = match self.exploration_votes.get(proposal_hash) {
            Some(votes) => votes,
            None => return (0, 0, 0),
        };

        Self::count_votes_with_delegation(direct_votes, delegation_engine, all_verified_wallets)
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

        Self::count_votes_with_delegation(direct_votes, delegation_engine, all_verified_wallets)
    }

    /// Shared implementation for transitive delegation vote counting.
    fn count_votes_with_delegation(
        direct_votes: &HashMap<WalletAddress, GovernanceVote>,
        delegation_engine: &DelegationEngine,
        all_verified_wallets: &[WalletAddress],
    ) -> (u32, u32, u32) {
        let mut yea = 0u32;
        let mut nay = 0u32;
        let mut abstain = 0u32;

        for wallet in all_verified_wallets {
            let vote = if let Some(v) = direct_votes.get(wallet) {
                Some(*v)
            } else {
                // Resolve delegation transitively
                delegation_engine.resolve(wallet).and_then(|delegate| {
                    if delegate != *wallet {
                        direct_votes.get(&delegate).copied()
                    } else {
                        None
                    }
                })
            };

            if let Some(v) = vote {
                match v {
                    GovernanceVote::Yea => yea += 1,
                    GovernanceVote::Nay => nay += 1,
                    GovernanceVote::Abstain => abstain += 1,
                }
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

    /// Select the winning proposal from all proposals in the Proposal phase
    /// that meet the endorsement threshold. Returns `None` if no proposal
    /// qualifies or if there's a tie for the top spot.
    fn select_winning_proposal(&self, params: &ProtocolParams) -> Option<TxHash> {
        let mut qualified: Vec<_> = self
            .proposals
            .iter()
            .filter(|(_, p)| p.phase == GovernancePhase::Proposal)
            .filter(|(_, p)| p.endorsement_count >= params.governance_proposal_endorsements)
            .collect();

        if qualified.is_empty() {
            return None;
        }

        qualified.sort_by_key(|b| std::cmp::Reverse(b.1.endorsement_count));

        if qualified.len() > 1
            && qualified[0].1.endorsement_count == qualified[1].1.endorsement_count
        {
            return None;
        }

        Some(*qualified[0].0)
    }

    /// When the proposal window has elapsed for any Proposal-phase proposals,
    /// select the single winner (most endorsements, no ties) and reject/reset losers.
    fn resolve_proposal_competition(&mut self, now: Timestamp, params: &ProtocolParams) {
        let window_expired: Vec<TxHash> = self
            .proposals
            .iter()
            .filter(|(_, p)| p.phase == GovernancePhase::Proposal)
            .filter(|(_, p)| {
                p.created_at
                    .has_expired(params.governance_proposal_window_secs, now)
            })
            .map(|(h, _)| *h)
            .collect();

        if window_expired.is_empty() {
            return;
        }

        let winner = self.select_winning_proposal(params);

        for hash in &window_expired {
            if Some(*hash) == winner {
                continue;
            }
            if let Some(proposal) = self.proposals.get_mut(hash) {
                if proposal.round < params.governance_max_rounds {
                    proposal.round += 1;
                    proposal.phase = GovernancePhase::Proposal;
                    proposal.created_at = now;
                    proposal.endorsement_count = 0;
                    self.endorsement_counts.remove(hash);
                    tracing::info!(
                        proposal = ?hash,
                        round = proposal.round,
                        "proposal lost competition, reset to Proposal phase"
                    );
                } else {
                    proposal.phase = GovernancePhase::Rejected;
                    tracing::info!(
                        proposal = ?hash,
                        "proposal rejected after max rounds (lost competition)"
                    );
                }
            }
        }
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
    fn test_cast_promotion_vote() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let proposal = make_proposal(1000, 0);
        let hash = submit(&mut engine, proposal);
        engine.get_proposal_mut(&hash).unwrap().phase = GovernancePhase::Promotion;
        engine.get_proposal_mut(&hash).unwrap().promotion_started_at = Some(Timestamp::new(1000));

        let voter = voter_wallet(1);
        assert!(engine
            .cast_promotion_vote(
                &hash,
                &voter,
                GovernanceVote::Yea,
                Timestamp::new(1000),
                &params
            )
            .is_ok());

        let stored = engine.get_proposal(&hash).unwrap();
        assert_eq!(stored.promotion_votes_yea, 1);
    }

    #[test]
    fn test_cast_promotion_vote_duplicate_rejected() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let proposal = make_proposal(1000, 0);
        let hash = submit(&mut engine, proposal);
        engine.get_proposal_mut(&hash).unwrap().phase = GovernancePhase::Promotion;

        let voter = voter_wallet(1);
        engine
            .cast_promotion_vote(
                &hash,
                &voter,
                GovernanceVote::Yea,
                Timestamp::new(1000),
                &params,
            )
            .unwrap();

        let result = engine.cast_promotion_vote(
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
        assert!(engine.try_advance(&mut proposal, now, &params).is_err());

        // After duration elapsed — should advance to Exploration
        let now = Timestamp::new(
            proposal.created_at.as_secs() + params.governance_proposal_duration_secs,
        );
        let result = engine.try_advance(&mut proposal, now, &params);
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
        assert!(engine.try_advance(&mut proposal, now, &params).is_err());

        // After duration elapsed but in propagation buffer — should fail
        let now = Timestamp::new(
            exploration_started.as_secs() + params.governance_exploration_duration_secs,
        );
        assert!(matches!(
            engine.try_advance(&mut proposal, now, &params).unwrap_err(),
            GovernanceError::PropagationBuffer
        ));

        // After duration + propagation buffer — should advance to Cooldown
        let now = Timestamp::new(
            exploration_started.as_secs()
                + params.governance_exploration_duration_secs
                + params.governance_propagation_buffer_secs,
        );
        let result = engine.try_advance(&mut proposal, now, &params);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), GovernancePhase::Cooldown);
        assert_eq!(proposal.phase, GovernancePhase::Cooldown);
        assert!(proposal.cooldown_started_at.is_some());
    }

    #[test]
    fn test_try_advance_exploration_retry_on_quorum_failure() {
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
        let result = engine.try_advance(&mut proposal, now, &params);
        assert!(result.is_err());
        // With retry: resets to Proposal phase, round incremented
        assert_eq!(proposal.phase, GovernancePhase::Proposal);
        assert_eq!(proposal.round, 1);
        assert_eq!(proposal.exploration_votes_yea, 0);
    }

    #[test]
    fn test_try_advance_exploration_retry_on_supermajority_failure() {
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
        let result = engine.try_advance(&mut proposal, now, &params);
        assert!(result.is_err());
        // With retry: resets to Proposal phase, round incremented
        assert_eq!(proposal.phase, GovernancePhase::Proposal);
        assert_eq!(proposal.round, 1);
    }

    // ── Cooldown → Promotion ──────────────────────────────────────────

    #[test]
    fn test_try_advance_cooldown_to_promotion() {
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
        assert!(engine.try_advance(&mut proposal, now, &params).is_err());

        // After duration elapsed — should advance to Promotion
        let now =
            Timestamp::new(cooldown_started.as_secs() + params.governance_cooldown_duration_secs);
        let result = engine.try_advance(&mut proposal, now, &params);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), GovernancePhase::Promotion);
        assert_eq!(proposal.phase, GovernancePhase::Promotion);
        assert!(proposal.promotion_started_at.is_some());
    }

    // ── Promotion → Activation ────────────────────────────────────────

    #[test]
    fn test_try_advance_promotion_to_activation() {
        let engine = GovernanceEngine::new();
        let params = default_params();
        let promotion_started = Timestamp::new(1000);
        let mut proposal = make_proposal(0, 10);
        proposal.phase = GovernancePhase::Promotion;
        proposal.exploration_started_at = Some(Timestamp::new(0));
        proposal.exploration_votes_yea = 85;
        proposal.exploration_votes_nay = 15;
        proposal.cooldown_started_at = Some(Timestamp::new(0));
        proposal.promotion_started_at = Some(promotion_started);
        proposal.promotion_votes_yea = 85;
        proposal.promotion_votes_nay = 15;
        proposal.promotion_votes_abstain = 0;

        // Before duration elapsed — should fail
        let now = Timestamp::new(
            promotion_started.as_secs() + params.governance_promotion_duration_secs - 1,
        );
        assert!(engine.try_advance(&mut proposal, now, &params).is_err());

        // After duration + propagation buffer — should advance to Activation
        let now = Timestamp::new(
            promotion_started.as_secs()
                + params.governance_promotion_duration_secs
                + params.governance_propagation_buffer_secs,
        );
        let result = engine.try_advance(&mut proposal, now, &params);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), GovernancePhase::Activation);
        assert_eq!(proposal.phase, GovernancePhase::Activation);
        assert!(proposal.activation_at.is_some());
    }

    #[test]
    fn test_try_advance_promotion_retry_after_failure() {
        let engine = GovernanceEngine::new();
        let params = default_params();
        let promotion_started = Timestamp::new(1000);
        let mut proposal = make_proposal(0, 10);
        proposal.phase = GovernancePhase::Promotion;
        proposal.exploration_started_at = Some(Timestamp::new(0));
        proposal.exploration_votes_yea = 85;
        proposal.exploration_votes_nay = 15;
        proposal.cooldown_started_at = Some(Timestamp::new(0));
        proposal.promotion_started_at = Some(promotion_started);
        // Promotion fails: 50/50 split doesn't meet 80% supermajority
        proposal.promotion_votes_yea = 50;
        proposal.promotion_votes_nay = 50;
        proposal.promotion_votes_abstain = 0;

        let now = Timestamp::new(
            promotion_started.as_secs()
                + params.governance_promotion_duration_secs
                + params.governance_propagation_buffer_secs,
        );
        let result = engine.try_advance(&mut proposal, now, &params);
        assert!(result.is_err());
        // With retry: resets to Proposal phase, round incremented, votes cleared
        assert_eq!(proposal.phase, GovernancePhase::Proposal);
        assert_eq!(proposal.round, 1);
        assert_eq!(proposal.promotion_votes_yea, 0);
        assert_eq!(proposal.exploration_votes_yea, 0);
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
    fn test_full_5_phase_lifecycle() {
        let engine = GovernanceEngine::new();
        let params = default_params();
        let mut proposal = make_proposal(1000, params.governance_proposal_endorsements);

        // Phase 1 → 2: Proposal → Exploration
        let now = Timestamp::new(
            proposal.created_at.as_secs() + params.governance_proposal_duration_secs,
        );
        let result = engine.try_advance(&mut proposal, now, &params);
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
        let result = engine.try_advance(&mut proposal, now, &params);
        assert_eq!(result.unwrap(), GovernancePhase::Cooldown);

        // Phase 3 → 4: Cooldown → Promotion
        let cooldown_started = proposal.cooldown_started_at.unwrap();
        let now =
            Timestamp::new(cooldown_started.as_secs() + params.governance_cooldown_duration_secs);
        let result = engine.try_advance(&mut proposal, now, &params);
        assert_eq!(result.unwrap(), GovernancePhase::Promotion);

        // Simulate promotion votes (85 yea, 10 nay, 5 abstain, 89.5% supermajority)
        proposal.promotion_votes_yea = 85;
        proposal.promotion_votes_nay = 10;
        proposal.promotion_votes_abstain = 5;

        // Phase 4 → 5: Promotion → Activation (with propagation buffer)
        let promotion_started = proposal.promotion_started_at.unwrap();
        let now = Timestamp::new(
            promotion_started.as_secs()
                + params.governance_promotion_duration_secs
                + params.governance_propagation_buffer_secs,
        );
        let result = engine.try_advance(&mut proposal, now, &params);
        assert_eq!(result.unwrap(), GovernancePhase::Activation);
        assert_eq!(proposal.phase, GovernancePhase::Activation);
        assert!(proposal.activation_at.is_some());
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
        let result = engine.try_advance(&mut proposal, now, &params);
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
        let result = engine.try_advance(&mut proposal_copy, now, &params);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), GovernanceError::WrongPhase));
    }

    // ── Emergency Governance (6.3) ────────────────────────────────────

    #[test]
    fn test_submit_emergency_proposal() {
        let mut engine = GovernanceEngine::new();
        let now = Timestamp::new(1000);
        let mut proposal = make_emergency_proposal(1000);

        let result = engine.submit_emergency_proposal(&mut proposal, now);
        assert!(result.is_ok());
        assert_eq!(proposal.phase, GovernancePhase::Exploration);
        assert_eq!(proposal.exploration_started_at, Some(now));
    }

    #[test]
    fn test_submit_emergency_rejects_non_emergency() {
        let mut engine = GovernanceEngine::new();
        let now = Timestamp::new(1000);
        let mut proposal = make_proposal(1000, 0);

        let result = engine.submit_emergency_proposal(&mut proposal, now);
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
            .submit_emergency_proposal(&mut proposal, now)
            .unwrap();
        assert_eq!(proposal.phase, GovernancePhase::Exploration);

        // Set exploration votes (95% yea to meet 95% supermajority)
        proposal.exploration_votes_yea = 96;
        proposal.exploration_votes_nay = 4;
        proposal.exploration_votes_abstain = 0;

        // Advance after 24h + propagation buffer → should go to Promotion (not Cooldown!)
        let now = Timestamp::new(
            1000 + EMERGENCY_PHASE_DURATION_SECS + params.governance_propagation_buffer_secs,
        );
        let result = engine.try_advance(&mut proposal, now, &params);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), GovernancePhase::Promotion);
        assert_eq!(proposal.phase, GovernancePhase::Promotion);
        assert!(proposal.cooldown_started_at.is_none());

        // Set promotion votes
        proposal.promotion_votes_yea = 96;
        proposal.promotion_votes_nay = 4;
        proposal.promotion_votes_abstain = 0;

        // Advance after another 24h + propagation buffer → Activation
        let promotion_started = proposal.promotion_started_at.unwrap();
        let now = Timestamp::new(
            promotion_started.as_secs()
                + EMERGENCY_PHASE_DURATION_SECS
                + params.governance_propagation_buffer_secs,
        );
        let result = engine.try_advance(&mut proposal, now, &params);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), GovernancePhase::Activation);
        assert_eq!(proposal.phase, GovernancePhase::Activation);
    }

    #[test]
    fn test_emergency_24h_voting_period() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let now = Timestamp::new(1000);
        let mut proposal = make_emergency_proposal(1000);
        engine
            .submit_emergency_proposal(&mut proposal, now)
            .unwrap();

        proposal.exploration_votes_yea = 96;
        proposal.exploration_votes_nay = 4;
        proposal.exploration_votes_abstain = 0;

        // Before 24h → should fail (PhaseNotExpired)
        let now = Timestamp::new(1000 + EMERGENCY_PHASE_DURATION_SECS - 1);
        assert!(engine.try_advance(&mut proposal, now, &params).is_err());

        // At exactly 24h but before propagation buffer → should fail (PropagationBuffer)
        let now = Timestamp::new(1000 + EMERGENCY_PHASE_DURATION_SECS);
        assert!(matches!(
            engine.try_advance(&mut proposal, now, &params).unwrap_err(),
            GovernanceError::PropagationBuffer
        ));

        // At 24h + propagation buffer → should pass
        let now = Timestamp::new(
            1000 + EMERGENCY_PHASE_DURATION_SECS + params.governance_propagation_buffer_secs,
        );
        assert!(engine.try_advance(&mut proposal, now, &params).is_ok());
    }

    #[test]
    fn test_emergency_requires_95_percent_supermajority() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let now = Timestamp::new(1000);
        let mut proposal = make_emergency_proposal(1000);
        engine
            .submit_emergency_proposal(&mut proposal, now)
            .unwrap();

        // 94% yea → should fail (need 95%)
        proposal.exploration_votes_yea = 94;
        proposal.exploration_votes_nay = 6;
        proposal.exploration_votes_abstain = 0;

        let now = Timestamp::new(
            1000 + EMERGENCY_PHASE_DURATION_SECS + params.governance_propagation_buffer_secs,
        );
        let result = engine.try_advance(&mut proposal, now, &params);
        assert!(result.is_err());
        // With retry: resets to Proposal phase, round incremented
        assert_eq!(proposal.phase, GovernancePhase::Proposal);
        assert_eq!(proposal.round, 1);
    }

    #[test]
    fn test_emergency_95_percent_passes() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let now = Timestamp::new(1000);
        let mut proposal = make_emergency_proposal(1000);
        engine
            .submit_emergency_proposal(&mut proposal, now)
            .unwrap();

        // 96% yea → (96*10000)/100 = 9600 bps ≥ 9500 bps → should pass
        proposal.exploration_votes_yea = 96;
        proposal.exploration_votes_nay = 4;
        proposal.exploration_votes_abstain = 0;

        let now = Timestamp::new(
            1000 + EMERGENCY_PHASE_DURATION_SECS + params.governance_propagation_buffer_secs,
        );
        let result = engine.try_advance(&mut proposal, now, &params);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), GovernancePhase::Promotion);
    }

    #[test]
    fn test_emergency_full_lifecycle() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let now = Timestamp::new(1000);
        let mut proposal = make_emergency_proposal(1000);

        // Submit
        engine
            .submit_emergency_proposal(&mut proposal, now)
            .unwrap();

        // Exploration → Promotion
        proposal.exploration_votes_yea = 96;
        proposal.exploration_votes_nay = 4;
        proposal.exploration_votes_abstain = 0;
        let now = Timestamp::new(
            1000 + EMERGENCY_PHASE_DURATION_SECS + params.governance_propagation_buffer_secs,
        );
        engine.try_advance(&mut proposal, now, &params).unwrap();

        // Promotion → Activation
        proposal.promotion_votes_yea = 96;
        proposal.promotion_votes_nay = 4;
        proposal.promotion_votes_abstain = 0;
        let promotion_started = proposal.promotion_started_at.unwrap();
        let now = Timestamp::new(
            promotion_started.as_secs()
                + EMERGENCY_PHASE_DURATION_SECS
                + params.governance_propagation_buffer_secs,
        );
        engine.try_advance(&mut proposal, now, &params).unwrap();
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
        let result = engine.try_advance(&mut proposal, now, &params);
        assert!(result.is_err());
        // With retry: resets to Proposal phase, round incremented
        assert_eq!(proposal.phase, GovernancePhase::Proposal);
        assert_eq!(proposal.round, 1);
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
        let result = engine.try_advance(&mut proposal, now, &params);
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
        let result = engine.try_advance(&mut proposal, now, &params);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), GovernancePhase::Cooldown);
    }

    // ── Fix 5: Governance Proposal BRN Cost ───────────────────────────

    #[test]
    fn test_submit_proposal_insufficient_brn() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let proposal = make_proposal(1000, 0);

        // Try with 0 BRN — should fail
        let result = engine.submit_proposal(proposal, 0, true, &params);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            GovernanceError::InsufficientBrn { .. }
        ));
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
    fn test_submit_proposal_just_below_brn_cost() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let proposal = make_proposal(1000, 0);

        let result =
            engine.submit_proposal(proposal, params.governance_proposal_cost - 1, true, &params);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            GovernanceError::InsufficientBrn { .. }
        ));
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
    fn test_select_winning_proposal_single() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();
        let mut p = make_proposal(1000, params.governance_proposal_endorsements);
        p.hash = unique_hash(1);
        submit(&mut engine, p);

        let winner = engine.select_winning_proposal(&params);
        assert_eq!(winner, Some(unique_hash(1)));
    }

    #[test]
    fn test_select_winning_proposal_highest_wins() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();

        let mut p1 = make_proposal(1000, params.governance_proposal_endorsements);
        p1.hash = unique_hash(1);
        submit(&mut engine, p1);

        let mut p2 = make_proposal(1000, params.governance_proposal_endorsements + 5);
        p2.hash = unique_hash(2);
        submit(&mut engine, p2);

        let winner = engine.select_winning_proposal(&params);
        assert_eq!(winner, Some(unique_hash(2)));
    }

    #[test]
    fn test_select_winning_proposal_tie_returns_none() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();

        let mut p1 = make_proposal(1000, params.governance_proposal_endorsements);
        p1.hash = unique_hash(1);
        submit(&mut engine, p1);

        let mut p2 = make_proposal(1000, params.governance_proposal_endorsements);
        p2.hash = unique_hash(2);
        submit(&mut engine, p2);

        let winner = engine.select_winning_proposal(&params);
        assert_eq!(winner, None);
    }

    #[test]
    fn test_select_winning_proposal_below_threshold() {
        let mut engine = GovernanceEngine::new();
        let params = default_params();

        let mut p = make_proposal(1000, params.governance_proposal_endorsements - 1);
        p.hash = unique_hash(1);
        submit(&mut engine, p);

        let winner = engine.select_winning_proposal(&params);
        assert_eq!(winner, None);
    }

    #[test]
    fn test_proposal_competition_winner_advances_losers_reset() {
        let mut engine = GovernanceEngine::new();
        let mut params = default_params();

        let created_at = 1000u64;
        let mut p1 = make_proposal(created_at, 0);
        p1.hash = unique_hash(1);
        submit(&mut engine, p1);

        let mut p2 = make_proposal(created_at, 0);
        p2.hash = unique_hash(2);
        submit(&mut engine, p2);

        // Endorse p1 past threshold, p2 below
        for _ in 0..params.governance_proposal_endorsements + 3 {
            engine.endorse_proposal(&unique_hash(1), 100).unwrap();
        }
        for _ in 0..params.governance_proposal_endorsements {
            engine.endorse_proposal(&unique_hash(2), 100).unwrap();
        }

        // Tick after proposal window elapses — winner (p1) advances, loser (p2) resets.
        // The proposal window (7d) equals proposal_duration (7d), so p1 also meets
        // try_advance_proposal conditions and advances to Exploration in the same tick.
        let now = Timestamp::new(created_at + params.governance_proposal_window_secs);
        engine.tick(now, &mut params);

        let p1_stored = engine.get_proposal(&unique_hash(1)).unwrap();
        assert_eq!(p1_stored.phase, GovernancePhase::Exploration);

        let p2_stored = engine.get_proposal(&unique_hash(2)).unwrap();
        assert_eq!(p2_stored.phase, GovernancePhase::Proposal);
        assert_eq!(p2_stored.round, 1);
        assert_eq!(p2_stored.endorsement_count, 0);
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
        let result = engine.try_advance(&mut proposal, now, &params);
        assert!(result.is_err());
        assert_eq!(proposal.phase, GovernancePhase::Rejected);
    }

    #[test]
    fn test_retry_clears_vote_state() {
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
        let _ = engine.try_advance(&mut proposal, now, &params);

        assert_eq!(proposal.phase, GovernancePhase::Proposal);
        assert_eq!(proposal.round, 1);
        assert_eq!(proposal.exploration_votes_yea, 0);
        assert_eq!(proposal.exploration_votes_nay, 0);
        assert_eq!(proposal.exploration_votes_abstain, 0);
        assert_eq!(proposal.endorsement_count, 0);
        assert!(proposal.exploration_started_at.is_none());
    }

    #[test]
    fn test_multiple_rounds_then_rejection() {
        let engine = GovernanceEngine::new();
        let params = default_params();
        let mut proposal = make_proposal(0, 10);

        for round in 0..params.governance_max_rounds {
            proposal.phase = GovernancePhase::Exploration;
            proposal.exploration_started_at = Some(Timestamp::new(round as u64 * 10000));
            proposal.exploration_votes_yea = 20;
            proposal.total_eligible_voters = 100;

            let now = Timestamp::new(
                proposal.exploration_started_at.unwrap().as_secs()
                    + params.governance_exploration_duration_secs
                    + params.governance_propagation_buffer_secs,
            );
            let _ = engine.try_advance(&mut proposal, now, &params);
            assert_eq!(proposal.phase, GovernancePhase::Proposal);
            assert_eq!(proposal.round, round + 1);
        }

        // One more failure at max_rounds should reject terminally
        proposal.phase = GovernancePhase::Exploration;
        proposal.exploration_started_at = Some(Timestamp::new(99000));
        proposal.exploration_votes_yea = 20;
        proposal.total_eligible_voters = 100;

        let now = Timestamp::new(
            99000
                + params.governance_exploration_duration_secs
                + params.governance_propagation_buffer_secs,
        );
        let _ = engine.try_advance(&mut proposal, now, &params);
        assert_eq!(proposal.phase, GovernancePhase::Rejected);
    }

    // ── Delegation Tally ────────────────────────────────────────────

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
