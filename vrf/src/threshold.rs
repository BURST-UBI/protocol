//! Phase 3: Threshold VRF (DVRF) — gold standard.
//!
//! A multiparty protocol where a committee collectively produces randomness.
//! No single participant can predict or bias the output.

use crate::{RandomOutput, VrfError, VrfProvider};

/// Threshold VRF provider — strongest guarantees.
pub struct ThresholdVrf {
    /// Minimum number of participants needed to produce a valid output.
    pub threshold: usize,
    /// Total committee size.
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
        todo!("collect partial evaluations from threshold participants, combine")
    }

    fn verify(&self, _context: &[u8], _output: &RandomOutput) -> Result<bool, VrfError> {
        todo!("verify threshold signature on the output")
    }

    fn name(&self) -> &str {
        "threshold-vrf"
    }
}
