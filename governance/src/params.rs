//! All governable protocol parameters.
//!
//! Every parameter can be changed via the four-phase governance process,
//! including the governance parameters themselves (self-governing thresholds).

use crate::error::GovernanceError;
use burst_types::ProtocolParams;
use serde::{Deserialize, Serialize};

pub mod view;

/// Enum of all protocol parameters that can be changed by governance vote.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GovernableParam {
    // The General Equation
    BrnRate,
    TrstExpirySecs,

    // Verification
    EndorsementThreshold,
    EndorsementBurnAmount,
    NumVerifiers,
    VerificationThresholdBps,
    VerifierStakeAmount,
    MaxRevotes,

    // Challenges
    ChallengeStakeAmount,

    // Governance (self-governing!)
    GovernanceProposalDurationSecs,
    GovernanceExplorationDurationSecs,
    GovernanceCooldownDurationSecs,
    GovernancePromotionDurationSecs,
    GovernanceSupermajorityBps,
    GovernanceQuorumBps,
    GovernanceProposalEndorsements,
    GovernanceEmaParticipationBps,

    // Consti
    ConstiSupermajorityBps,
    ConstiQuorumBps,

    // Verification
    VerificationTimeoutSecs,
    ChallengeDurationSecs,
    ChallengeRewardBps,
    ChallengeRewardCap,

    // Economic
    NewWalletSpendingLimit,
    NewWalletLimitDurationSecs,
    BootstrapExitThreshold,
    NewWalletTxLimitPerDay,
    NewWalletRateLimitDurationSecs,

    // Governance (cost)
    GovernanceProposalCost,

    // Governance (competition & retry)
    GovernanceMaxRounds,
    GovernanceProposalWindowSecs,

    // Governance (propagation buffer)
    GovernancePropagationBufferSecs,

    // Anti-Spam
    MinWorkDifficulty,

    // Append new variants to preserve existing serialized discriminants.
    MinVerificationAgeSecs,
    NeitherPenaltyCooldownSecs,
    NeitherPenaltyThresholdBps,
    NeitherWindowAssignments,
    MinVerifierBrnBalance,
    EmergencyVotingDurationSecs,
    EmergencyQuorumBps,
    EmergencySupermajorityBps,
}

impl GovernableParam {
    pub const ALL: &'static [Self] = &[
        Self::BrnRate,
        Self::TrstExpirySecs,
        Self::EndorsementThreshold,
        Self::EndorsementBurnAmount,
        Self::NumVerifiers,
        Self::VerificationThresholdBps,
        Self::VerifierStakeAmount,
        Self::MaxRevotes,
        Self::ChallengeStakeAmount,
        Self::GovernanceProposalDurationSecs,
        Self::GovernanceExplorationDurationSecs,
        Self::GovernanceCooldownDurationSecs,
        Self::GovernancePromotionDurationSecs,
        Self::GovernanceSupermajorityBps,
        Self::GovernanceQuorumBps,
        Self::GovernanceProposalEndorsements,
        Self::GovernanceEmaParticipationBps,
        Self::ConstiSupermajorityBps,
        Self::ConstiQuorumBps,
        Self::VerificationTimeoutSecs,
        Self::ChallengeDurationSecs,
        Self::ChallengeRewardBps,
        Self::ChallengeRewardCap,
        Self::NewWalletSpendingLimit,
        Self::NewWalletLimitDurationSecs,
        Self::BootstrapExitThreshold,
        Self::NewWalletTxLimitPerDay,
        Self::NewWalletRateLimitDurationSecs,
        Self::GovernanceProposalCost,
        Self::GovernanceMaxRounds,
        Self::GovernanceProposalWindowSecs,
        Self::GovernancePropagationBufferSecs,
        Self::MinWorkDifficulty,
        Self::MinVerificationAgeSecs,
        Self::NeitherPenaltyCooldownSecs,
        Self::NeitherPenaltyThresholdBps,
        Self::NeitherWindowAssignments,
        Self::MinVerifierBrnBalance,
        Self::EmergencyVotingDurationSecs,
        Self::EmergencyQuorumBps,
        Self::EmergencySupermajorityBps,
    ];

    /// Parse the canonical parameter name used by wallets and RPC.
    pub fn from_name(name: &str) -> Result<Self, GovernanceError> {
        match name {
            "brn_rate" => Ok(Self::BrnRate),
            "trst_expiry_secs" => Ok(Self::TrstExpirySecs),
            "endorsement_threshold" => Ok(Self::EndorsementThreshold),
            "endorsement_burn_amount" => Ok(Self::EndorsementBurnAmount),
            "num_verifiers" => Ok(Self::NumVerifiers),
            "verification_threshold_bps" => Ok(Self::VerificationThresholdBps),
            "verifier_stake_amount" => Ok(Self::VerifierStakeAmount),
            "max_revotes" => Ok(Self::MaxRevotes),
            "challenge_stake_amount" => Ok(Self::ChallengeStakeAmount),
            "governance_proposal_duration_secs" => Ok(Self::GovernanceProposalDurationSecs),
            "governance_exploration_duration_secs" => Ok(Self::GovernanceExplorationDurationSecs),
            "governance_cooldown_duration_secs" => Ok(Self::GovernanceCooldownDurationSecs),
            "governance_promotion_duration_secs" => Ok(Self::GovernancePromotionDurationSecs),
            "governance_supermajority_bps" => Ok(Self::GovernanceSupermajorityBps),
            "governance_quorum_bps" => Ok(Self::GovernanceQuorumBps),
            "governance_proposal_endorsements" => Ok(Self::GovernanceProposalEndorsements),
            "governance_ema_participation_bps" => Ok(Self::GovernanceEmaParticipationBps),
            "consti_supermajority_bps" => Ok(Self::ConstiSupermajorityBps),
            "consti_quorum_bps" => Ok(Self::ConstiQuorumBps),
            "verification_timeout_secs" => Ok(Self::VerificationTimeoutSecs),
            "challenge_duration_secs" => Ok(Self::ChallengeDurationSecs),
            "challenge_reward_bps" => Ok(Self::ChallengeRewardBps),
            "challenge_reward_cap" => Ok(Self::ChallengeRewardCap),
            "new_wallet_spending_limit" => Ok(Self::NewWalletSpendingLimit),
            "new_wallet_limit_duration_secs" => Ok(Self::NewWalletLimitDurationSecs),
            "bootstrap_exit_threshold" => Ok(Self::BootstrapExitThreshold),
            "new_wallet_tx_limit_per_day" => Ok(Self::NewWalletTxLimitPerDay),
            "new_wallet_rate_limit_duration_secs" => Ok(Self::NewWalletRateLimitDurationSecs),
            "governance_proposal_cost" => Ok(Self::GovernanceProposalCost),
            "governance_max_rounds" => Ok(Self::GovernanceMaxRounds),
            "governance_proposal_window_secs" => Ok(Self::GovernanceProposalWindowSecs),
            "governance_propagation_buffer_secs" => Ok(Self::GovernancePropagationBufferSecs),
            "min_work_difficulty" => Ok(Self::MinWorkDifficulty),
            "min_verification_age_secs" => Ok(Self::MinVerificationAgeSecs),
            "neither_penalty_cooldown_secs" => Ok(Self::NeitherPenaltyCooldownSecs),
            "neither_penalty_threshold_bps" => Ok(Self::NeitherPenaltyThresholdBps),
            "neither_window_assignments" => Ok(Self::NeitherWindowAssignments),
            "min_verifier_brn_balance" => Ok(Self::MinVerifierBrnBalance),
            "emergency_voting_duration_secs" => Ok(Self::EmergencyVotingDurationSecs),
            "emergency_quorum_bps" => Ok(Self::EmergencyQuorumBps),
            "emergency_supermajority_bps" => Ok(Self::EmergencySupermajorityBps),
            _ => Err(GovernanceError::Other(format!(
                "unknown governable parameter: '{name}'"
            ))),
        }
    }

    /// Canonical delegation category; this is policy, not a UI label.
    pub fn category(&self) -> &'static str {
        match self {
            Self::BrnRate
            | Self::TrstExpirySecs
            | Self::NewWalletSpendingLimit
            | Self::NewWalletLimitDurationSecs => "economic",
            Self::EndorsementThreshold
            | Self::EndorsementBurnAmount
            | Self::NumVerifiers
            | Self::VerificationThresholdBps
            | Self::VerifierStakeAmount
            | Self::MaxRevotes
            | Self::ChallengeStakeAmount
            | Self::VerificationTimeoutSecs
            | Self::ChallengeDurationSecs
            | Self::ChallengeRewardBps
            | Self::ChallengeRewardCap
            | Self::BootstrapExitThreshold
            | Self::MinVerificationAgeSecs
            | Self::NeitherPenaltyCooldownSecs
            | Self::NeitherPenaltyThresholdBps
            | Self::NeitherWindowAssignments
            | Self::MinVerifierBrnBalance => "verification",
            Self::GovernanceProposalDurationSecs
            | Self::GovernanceExplorationDurationSecs
            | Self::GovernanceCooldownDurationSecs
            | Self::GovernancePromotionDurationSecs
            | Self::GovernanceSupermajorityBps
            | Self::GovernanceQuorumBps
            | Self::GovernanceProposalEndorsements
            | Self::GovernanceEmaParticipationBps
            | Self::GovernanceProposalCost
            | Self::GovernanceMaxRounds
            | Self::GovernanceProposalWindowSecs
            | Self::GovernancePropagationBufferSecs
            | Self::EmergencyVotingDurationSecs
            | Self::EmergencyQuorumBps
            | Self::EmergencySupermajorityBps => "governance",
            Self::ConstiSupermajorityBps | Self::ConstiQuorumBps => "constitution",
            Self::MinWorkDifficulty
            | Self::NewWalletTxLimitPerDay
            | Self::NewWalletRateLimitDurationSecs => "network",
        }
    }
    /// Human-readable name of this parameter.
    pub fn name(&self) -> &'static str {
        match self {
            Self::BrnRate => "brn_rate",
            Self::TrstExpirySecs => "trst_expiry_secs",
            Self::EndorsementThreshold => "endorsement_threshold",
            Self::EndorsementBurnAmount => "endorsement_burn_amount",
            Self::NumVerifiers => "num_verifiers",
            Self::VerificationThresholdBps => "verification_threshold_bps",
            Self::VerifierStakeAmount => "verifier_stake_amount",
            Self::MaxRevotes => "max_revotes",
            Self::ChallengeStakeAmount => "challenge_stake_amount",
            Self::GovernanceProposalDurationSecs => "governance_proposal_duration_secs",
            Self::GovernanceExplorationDurationSecs => "governance_exploration_duration_secs",
            Self::GovernanceCooldownDurationSecs => "governance_cooldown_duration_secs",
            Self::GovernancePromotionDurationSecs => "governance_promotion_duration_secs",
            Self::GovernanceSupermajorityBps => "governance_supermajority_bps",
            Self::GovernanceQuorumBps => "governance_quorum_bps",
            Self::GovernanceProposalEndorsements => "governance_proposal_endorsements",
            Self::GovernanceEmaParticipationBps => "governance_ema_participation_bps",
            Self::ConstiSupermajorityBps => "consti_supermajority_bps",
            Self::ConstiQuorumBps => "consti_quorum_bps",
            Self::VerificationTimeoutSecs => "verification_timeout_secs",
            Self::ChallengeDurationSecs => "challenge_duration_secs",
            Self::ChallengeRewardBps => "challenge_reward_bps",
            Self::ChallengeRewardCap => "challenge_reward_cap",
            Self::NewWalletSpendingLimit => "new_wallet_spending_limit",
            Self::NewWalletLimitDurationSecs => "new_wallet_limit_duration_secs",
            Self::BootstrapExitThreshold => "bootstrap_exit_threshold",
            Self::NewWalletTxLimitPerDay => "new_wallet_tx_limit_per_day",
            Self::NewWalletRateLimitDurationSecs => "new_wallet_rate_limit_duration_secs",
            Self::GovernanceProposalCost => "governance_proposal_cost",
            Self::GovernanceMaxRounds => "governance_max_rounds",
            Self::GovernanceProposalWindowSecs => "governance_proposal_window_secs",
            Self::GovernancePropagationBufferSecs => "governance_propagation_buffer_secs",
            Self::MinWorkDifficulty => "min_work_difficulty",
            Self::MinVerificationAgeSecs => "min_verification_age_secs",
            Self::NeitherPenaltyCooldownSecs => "neither_penalty_cooldown_secs",
            Self::NeitherPenaltyThresholdBps => "neither_penalty_threshold_bps",
            Self::NeitherWindowAssignments => "neither_window_assignments",
            Self::MinVerifierBrnBalance => "min_verifier_brn_balance",
            Self::EmergencyVotingDurationSecs => "emergency_voting_duration_secs",
            Self::EmergencyQuorumBps => "emergency_quorum_bps",
            Self::EmergencySupermajorityBps => "emergency_supermajority_bps",
        }
    }
}

/// Trait for governable parameters that can be applied to ProtocolParams.
pub trait GovernableParamTrait {
    /// Apply a new value to the protocol parameters.
    fn apply(&self, params: &mut ProtocolParams, value: u128) -> Result<(), GovernanceError>;
}

/// BRN accrual rate parameter.
pub struct BrnRateParam;

impl GovernableParamTrait for BrnRateParam {
    fn apply(&self, params: &mut ProtocolParams, value: u128) -> Result<(), GovernanceError> {
        params.brn_rate = value;
        Ok(())
    }
}

/// TRST expiry period parameter.
pub struct TrstExpiryParam;

impl GovernableParamTrait for TrstExpiryParam {
    fn apply(&self, params: &mut ProtocolParams, value: u128) -> Result<(), GovernanceError> {
        params.trst_expiry_secs = value as u64;
        Ok(())
    }
}
