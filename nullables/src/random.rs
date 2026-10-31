//! Cyclic test outputs with no cryptographic proof verification.

use burst_vrf::{RandomOutput, VrfError, VrfProvider};
use std::sync::Mutex;

/// Test implementation of VrfProvider. Cycles through configured values, ignores
/// context, returns empty proofs and accepts every output in verify.
pub struct NullRandom {
    outputs: Mutex<Vec<[u8; 32]>>,
    index: Mutex<usize>,
}

impl NullRandom {
    /// Configure the output cycle. The sequence must be nonempty; requesting
    /// randomness from an empty sequence panics.
    pub fn new(outputs: Vec<[u8; 32]>) -> Self {
        Self {
            outputs: Mutex::new(outputs),
            index: Mutex::new(0),
        }
    }

    /// Create with a single value that will be returned for every call.
    pub fn constant(value: [u8; 32]) -> Self {
        Self::new(vec![value])
    }
}

impl VrfProvider for NullRandom {
    fn get_randomness(&self, _context: &[u8]) -> Result<RandomOutput, VrfError> {
        let outputs = self.outputs.lock().unwrap();
        let mut idx = self.index.lock().unwrap();
        let current = *idx % outputs.len();
        *idx += 1;
        Ok(RandomOutput {
            value: outputs[current],
            proof: Vec::new(),
            round: current as u64,
        })
    }

    fn verify(&self, _context: &[u8], _output: &RandomOutput) -> Result<bool, VrfError> {
        Ok(true) // Always valid in test mode
    }

    fn name(&self) -> &str {
        "null-random"
    }
}
