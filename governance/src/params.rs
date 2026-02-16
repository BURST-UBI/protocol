//! All governable protocol parameters.
//!
//! Every parameter can be changed via the 4-phase governance process,
//! including the governance parameters themselves (self-governing thresholds).

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
    GovernanceVotingDurationSecs,
    GovernanceCooldownDurationSecs,
    GovernanceSupermajorityBps,
    GovernanceQuorumBps,
    GovernanceProposalEndorsements,

    // Consti
    ConstiSupermajorityBps,

    // Economic
    NewWalletSpendingLimit,
    NewWalletLimitDurationSecs,

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
            Self::GovernanceVotingDurationSecs => "governance_voting_duration_secs",
            Self::GovernanceCooldownDurationSecs => "governance_cooldown_duration_secs",
            Self::GovernanceSupermajorityBps => "governance_supermajority_bps",
            Self::GovernanceQuorumBps => "governance_quorum_bps",
            Self::GovernanceProposalEndorsements => "governance_proposal_endorsements",
            Self::ConstiSupermajorityBps => "consti_supermajority_bps",
            Self::NewWalletSpendingLimit => "new_wallet_spending_limit",
            Self::NewWalletLimitDurationSecs => "new_wallet_limit_duration_secs",
            Self::MinWorkDifficulty => "min_work_difficulty",
        }
    }
}
