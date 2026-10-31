//! Governable parameter registry. Ordering is the serialized enum ABI: append only.
use crate::error::GovernanceError;
use burst_types::ProtocolParams;
use serde::{Deserialize, Serialize};
pub mod view;

macro_rules! parameters {
    ($($variant:ident => $field:ident : $ty:ty, $unit:ident;)+) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum GovernableParam { $($variant,)+ }
        impl GovernableParam {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];
            pub fn name(&self) -> &'static str {
                match self { $(Self::$variant => stringify!($field),)+ }
            }
            pub fn from_name(name: &str) -> Result<Self, GovernanceError> {
                match name {
                    $(stringify!($field) => Ok(Self::$variant),)+
                    _ => Err(GovernanceError::Other(format!("unknown governable parameter: '{name}'"))),
                }
            }
            fn value_parts(&self, params: &ProtocolParams) -> (u128, u128, view::Unit) {
                match self { $(Self::$variant => (params.$field as u128, <$ty>::MAX as u128, view::Unit::$unit),)+ }
            }
            pub(crate) fn apply(&self, params: &mut ProtocolParams, value: u128) -> Result<(), GovernanceError> {
                match self { $(Self::$variant => {
                    params.$field = <$ty>::try_from(value).map_err(|_| GovernanceError::Other(
                        concat!("parameter value exceeds ", stringify!($ty)).into()))?;
                },)+ }
                Ok(())
            }
        }
    };
}

parameters! {
    BrnRate => brn_rate: u128, BrnPerSecond;
    TrstExpirySecs => trst_expiry_secs: u64, Seconds;
    EndorsementThreshold => endorsement_threshold: u32, Count;
    EndorsementBurnAmount => endorsement_burn_amount: u128, Brn;
    NumVerifiers => num_verifiers: u32, Count;
    VerificationThresholdBps => verification_threshold_bps: u32, BasisPoints;
    VerifierStakeAmount => verifier_stake_amount: u128, Brn;
    MaxRevotes => max_revotes: u32, Count;
    ChallengeStakeAmount => challenge_stake_amount: u128, Brn;
    GovernanceProposalDurationSecs => governance_proposal_duration_secs: u64, Seconds;
    GovernanceExplorationDurationSecs => governance_exploration_duration_secs: u64, Seconds;
    GovernanceCooldownDurationSecs => governance_cooldown_duration_secs: u64, Seconds;
    GovernancePromotionDurationSecs => governance_promotion_duration_secs: u64, Seconds;
    GovernanceSupermajorityBps => governance_supermajority_bps: u32, BasisPoints;
    GovernanceQuorumBps => governance_quorum_bps: u32, BasisPoints;
    GovernanceProposalEndorsements => governance_proposal_endorsements: u32, Count;
    GovernanceEmaParticipationBps => governance_ema_participation_bps: u32, BasisPoints;
    ConstiSupermajorityBps => consti_supermajority_bps: u32, BasisPoints;
    ConstiQuorumBps => consti_quorum_bps: u32, BasisPoints;
    VerificationTimeoutSecs => verification_timeout_secs: u64, Seconds;
    ChallengeDurationSecs => challenge_duration_secs: u64, Seconds;
    ChallengeRewardBps => challenge_reward_bps: u32, BasisPoints;
    ChallengeRewardCap => challenge_reward_cap: u128, Trst;
    NewWalletSpendingLimit => new_wallet_spending_limit: u128, Trst;
    NewWalletLimitDurationSecs => new_wallet_limit_duration_secs: u64, Seconds;
    BootstrapExitThreshold => bootstrap_exit_threshold: u32, Count;
    NewWalletRateLimitDurationSecs => new_wallet_rate_limit_duration_secs: u64, Seconds;
    GovernanceProposalCost => governance_proposal_cost: u128, Brn;
    GovernanceMaxRounds => governance_max_rounds: u32, Count;
    GovernanceProposalWindowSecs => governance_proposal_window_secs: u64, Seconds;
    GovernancePropagationBufferSecs => governance_propagation_buffer_secs: u64, Seconds;
    MinWorkDifficulty => min_work_difficulty: u64, Difficulty;
    MinVerificationAgeSecs => min_verification_age_secs: u64, Seconds;
    NeitherPenaltyCooldownSecs => neither_penalty_cooldown_secs: u64, Seconds;
    NeitherPenaltyThresholdBps => neither_penalty_threshold_bps: u32, BasisPoints;
    NeitherWindowAssignments => neither_window_assignments: u32, Count;
    MinVerifierBrnBalance => min_verifier_brn_balance: u128, Brn;
    EmergencyVotingDurationSecs => emergency_voting_duration_secs: u64, Seconds;
    EmergencyQuorumBps => emergency_quorum_bps: u32, BasisPoints;
    EmergencySupermajorityBps => emergency_supermajority_bps: u32, BasisPoints;
}

impl GovernableParam {
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
            | Self::NewWalletRateLimitDurationSecs => "network",
        }
    }
}
