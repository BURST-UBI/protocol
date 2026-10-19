//! Lossless parameter values for governance authoring. This describes storage
//! widths, not additional constitutional restrictions on governance.
use super::GovernableParam;
use burst_types::ProtocolParams;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    BrnPerSecond,
    Brn,
    Trst,
    Seconds,
    BasisPoints,
    Count,
    Difficulty,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParameterValue {
    pub name: String,
    pub current: String,
    pub maximum: String,
    pub unit: Unit,
    pub legacy: bool,
}

pub fn snapshot(params: &ProtocolParams) -> Vec<ParameterValue> {
    GovernableParam::ALL
        .iter()
        .map(|param| param.value_view(params))
        .collect()
}

impl GovernableParam {
    pub fn value_view(&self, params: &ProtocolParams) -> ParameterValue {
        macro_rules! values {
            ($($variant:ident => $field:ident : $ty:ty, $unit:ident;)+) => {
                match self { $(Self::$variant => (
                    params.$field as u128, <$ty>::MAX as u128, Unit::$unit
                ),)+ }
            };
        }
        let (current, maximum, unit) = values! {
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
            NewWalletTxLimitPerDay => new_wallet_tx_limit_per_day: u32, Count;
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
        };
        ParameterValue {
            name: self.name().into(),
            current: current.to_string(),
            maximum: maximum.to_string(),
            unit,
            legacy: matches!(
                self,
                Self::GovernancePromotionDurationSecs
                    | Self::GovernanceEmaParticipationBps
                    | Self::GovernanceMaxRounds
                    | Self::GovernanceProposalWindowSecs
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_is_complete_lossless_and_marks_inactive_fields() {
        let params = ProtocolParams {
            brn_rate: u128::MAX,
            trst_expiry_secs: u64::MAX,
            num_verifiers: u32::MAX,
            ..ProtocolParams::burst_defaults()
        };
        let rows = snapshot(&params);
        let encoded = serde_json::to_string(&params).unwrap();
        let object: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        let fields: std::collections::BTreeSet<_> = object.as_object().unwrap().keys().collect();
        let governed: std::collections::BTreeSet<_> = rows.iter().map(|row| &row.name).collect();
        assert_eq!(
            fields, governed,
            "every stored numeric field needs a governance mapping"
        );
        assert_eq!(rows.len(), GovernableParam::ALL.len());
        for row in &rows {
            let param = GovernableParam::from_name(&row.name).unwrap();
            assert_eq!(row, &param.value_view(&params));
            assert!(row.current.parse::<u128>().unwrap() <= row.maximum.parse::<u128>().unwrap());
        }
        assert_eq!(rows.iter().filter(|r| r.legacy).count(), 4);
        assert_eq!(rows[0].current, u128::MAX.to_string());
        assert_eq!(rows[1].current, u64::MAX.to_string());
        assert_eq!(
            bincode::serialize(&GovernableParam::MinWorkDifficulty).unwrap(),
            32u32.to_le_bytes()
        );
        assert_eq!(
            bincode::serialize(&GovernableParam::MinVerificationAgeSecs).unwrap(),
            33u32.to_le_bytes()
        );
        assert_eq!(
            bincode::serialize(&GovernableParam::NeitherPenaltyCooldownSecs).unwrap(),
            34u32.to_le_bytes()
        );
        assert_eq!(
            bincode::serialize(&GovernableParam::NeitherPenaltyThresholdBps).unwrap(),
            35u32.to_le_bytes()
        );
        assert_eq!(
            bincode::serialize(&GovernableParam::NeitherWindowAssignments).unwrap(),
            36u32.to_le_bytes()
        );
        assert_eq!(
            bincode::serialize(&GovernableParam::MinVerifierBrnBalance).unwrap(),
            37u32.to_le_bytes()
        );
        assert_eq!(
            bincode::serialize(&GovernableParam::EmergencyVotingDurationSecs).unwrap(),
            38u32.to_le_bytes()
        );
        assert_eq!(
            bincode::serialize(&GovernableParam::EmergencyQuorumBps).unwrap(),
            39u32.to_le_bytes()
        );
        assert_eq!(
            bincode::serialize(&GovernableParam::EmergencySupermajorityBps).unwrap(),
            40u32.to_le_bytes()
        );
        let bytes = bincode::serialize(&rows).unwrap();
        assert_eq!(
            bincode::deserialize::<Vec<ParameterValue>>(&bytes).unwrap(),
            rows
        );
    }
}
