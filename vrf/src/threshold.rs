//! Placeholder for BURST-operated threshold randomness.
//!
//! A multiparty protocol where a committee collectively produces randomness.
//! Security depends on the committee, corruption threshold and key-management
//! protocol. This placeholder does not provide those guarantees.
//!

use crate::{RandomOutput, VrfError, VrfProvider};

/// Placeholder provider; both generation and verification return Unavailable.
pub struct ThresholdVrf {
    /// Reserved threshold setting; no protocol currently enforces it.
    pub threshold: usize,
    /// Reserved committee size; no membership protocol is implemented.
    pub committee_size: usize,
}

impl ThresholdVrf {
    pub fn new(threshold: usize, committee_size: usize) -> Self {
        Self {
            threshold,
            committee_size,
        }
    }
}

impl VrfProvider for ThresholdVrf {
    fn get_randomness(&self, _context: &[u8]) -> Result<RandomOutput, VrfError> {
        Err(VrfError::Unavailable(
            "threshold VRF not yet implemented (Phase 3)".into(),
        ))
    }

    fn verify(&self, _context: &[u8], _output: &RandomOutput) -> Result<bool, VrfError> {
        Err(VrfError::Unavailable(
            "threshold VRF not yet implemented (Phase 3)".into(),
        ))
    }

    fn name(&self) -> &str {
        "threshold-vrf"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unimplemented_threshold_provider_fails_closed() {
        let provider = ThresholdVrf::new(2, 3);
        assert!(provider.get_randomness(b"context").is_err());
    }
}
