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
    /// Maximum value representable by the governed field.
    pub fn maximum_value(&self) -> u128 {
        self.value_parts(&ProtocolParams::default()).1
    }

    pub fn value_view(&self, params: &ProtocolParams) -> ParameterValue {
        let (current, maximum, unit) = self.value_parts(params);
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
            31u32.to_le_bytes()
        );
        assert_eq!(
            bincode::serialize(&GovernableParam::MinVerificationAgeSecs).unwrap(),
            32u32.to_le_bytes()
        );
        assert_eq!(
            bincode::serialize(&GovernableParam::NeitherPenaltyCooldownSecs).unwrap(),
            33u32.to_le_bytes()
        );
        assert_eq!(
            bincode::serialize(&GovernableParam::NeitherPenaltyThresholdBps).unwrap(),
            34u32.to_le_bytes()
        );
        assert_eq!(
            bincode::serialize(&GovernableParam::NeitherWindowAssignments).unwrap(),
            35u32.to_le_bytes()
        );
        assert_eq!(
            bincode::serialize(&GovernableParam::MinVerifierBrnBalance).unwrap(),
            36u32.to_le_bytes()
        );
        assert_eq!(
            bincode::serialize(&GovernableParam::EmergencyVotingDurationSecs).unwrap(),
            37u32.to_le_bytes()
        );
        assert_eq!(
            bincode::serialize(&GovernableParam::EmergencyQuorumBps).unwrap(),
            38u32.to_le_bytes()
        );
        assert_eq!(
            bincode::serialize(&GovernableParam::EmergencySupermajorityBps).unwrap(),
            39u32.to_le_bytes()
        );
        let bytes = bincode::serialize(&rows).unwrap();
        assert_eq!(
            bincode::deserialize::<Vec<ParameterValue>>(&bytes).unwrap(),
            rows
        );
    }
}
