use crate::VerificationError;

/// Persisted creator-authority phase. Once complete, lower account counts and
/// higher thresholds cannot reopen it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BootstrapPhase {
    Active,
    Complete,
}

pub const MAX_BOOTSTRAP_HUMANS: u32 = 50;

impl BootstrapPhase {
    pub const KEY: &'static [u8] = b"bootstrap_phase/v1";

    pub fn at_genesis(threshold: u32) -> Self {
        let mut phase = Self::Active;
        phase.observe(1, threshold);
        phase
    }

    pub fn observe(&mut self, verified_count: u64, threshold: u32) {
        if verified_count >= u64::from(threshold.min(MAX_BOOTSTRAP_HUMANS)) {
            *self = Self::Complete;
        }
    }

    pub fn permits_creator(self, verified_count: u64, threshold: u32) -> bool {
        self == Self::Active && verified_count < u64::from(threshold.min(MAX_BOOTSTRAP_HUMANS))
    }

    pub fn encoding(self) -> [u8; 2] {
        [1, u8::from(self == Self::Complete)]
    }

    pub fn decode(bytes: Option<&[u8]>) -> Result<Self, VerificationError> {
        match bytes {
            Some([1, 0]) => Ok(Self::Active),
            Some([1, 1]) => Ok(Self::Complete),
            None => Err(VerificationError::Other(
                "bootstrap phase history is missing; use a fresh ledger or explicit authenticated replay".into(),
            )),
            Some(_) => Err(VerificationError::Other("invalid bootstrap phase record".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_exit_survives_revocation_parameter_changes_and_serialization() {
        let mut phase = BootstrapPhase::at_genesis(50);
        phase.observe(49, 50);
        assert_eq!(phase, BootstrapPhase::Active);
        phase.observe(50, 50);
        let mut restored = BootstrapPhase::decode(Some(&phase.encoding())).unwrap();
        restored.observe(1, 1000);
        assert_eq!(restored, BootstrapPhase::Complete);
        assert_eq!(BootstrapPhase::at_genesis(0), BootstrapPhase::Complete);
        assert_eq!(BootstrapPhase::at_genesis(1), BootstrapPhase::Complete);
    }

    #[test]
    fn raising_threshold_cannot_extend_authority_beyond_fifty() {
        let mut phase = BootstrapPhase::at_genesis(200);
        assert!(phase.permits_creator(49, 200));
        assert!(!phase.permits_creator(50, 200));
        phase.observe(50, 200);
        assert_eq!(phase, BootstrapPhase::Complete);
        assert!(!phase.permits_creator(1, 1000));
        assert!(!BootstrapPhase::Active.permits_creator(20, 20));
    }

    #[test]
    fn lowering_threshold_exits_once_and_missing_history_is_not_a_fresh_launch() {
        let mut phase = BootstrapPhase::at_genesis(50);
        phase.observe(20, 20);
        phase.observe(20, 200);
        assert_eq!(phase, BootstrapPhase::Complete);
        for bytes in [
            None,
            Some(&[][..]),
            Some(&[0, 0]),
            Some(&[1, 2]),
            Some(&[1, 1, 0]),
        ] {
            assert!(BootstrapPhase::decode(bytes).is_err());
        }
    }
}
