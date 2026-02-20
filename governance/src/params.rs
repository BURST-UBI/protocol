//! All governable protocol parameters.
//!
//! Every parameter can be changed via the 5-phase governance process,
//! including the governance parameters themselves (self-governing thresholds).

use crate::error::GovernanceError;
use burst_types::ProtocolParams;
use serde::{Deserialize, Serialize};

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
    EndorserRewardBps,

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
}

impl GovernableParam {
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
            Self::EndorserRewardBps => "endorser_reward_bps",
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
