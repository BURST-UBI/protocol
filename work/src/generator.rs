//! PoW generation (CPU).

use crate::{WorkError, WorkNonce};
use burst_crypto::blake2b_256;
use burst_types::BlockHash;

/// Generates proof-of-work for a block.
pub struct WorkGenerator;

impl WorkGenerator {
    /// Generate a work nonce that meets the minimum difficulty.
    ///
    /// Iterates nonces until `hash(block_hash || nonce)` meets the threshold.
    /// The block hash portion of the input buffer is hoisted outside the tight
    /// loop since it never changes — only the 8-byte nonce suffix is updated.
    pub fn generate(
        &self,
        block_hash: &BlockHash,
        min_difficulty: u64,
    ) -> Result<WorkNonce, WorkError> {
        let mut input = [0u8; 40];
        input[0..32].copy_from_slice(block_hash.as_bytes());

        for nonce in 0u64.. {
            input[32..40].copy_from_slice(&nonce.to_le_bytes());
            let hash = blake2b_256(&input);
            let work_value = u64::from_le_bytes(hash[0..8].try_into().unwrap());
            if work_value >= min_difficulty {
                return Ok(WorkNonce(nonce));
            }
        }
        Err(WorkError::Cancelled)
    }
}

/// Validation helper used by tests (single-shot, not hot loop).
#[cfg(test)]
fn validate_work_inner(block_hash: &[u8; 32], nonce: u64, min_difficulty: u64) -> bool {
    let mut input = [0u8; 40];
    input[0..32].copy_from_slice(block_hash);
    input[32..40].copy_from_slice(&nonce.to_le_bytes());
    let hash = blake2b_256(&input);
    let work_value = u64::from_le_bytes(hash[0..8].try_into().unwrap());
    work_value >= min_difficulty
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::BlockHash;

    #[test]
    fn test_generate_work() {
        let generator = WorkGenerator;
        let block_hash = BlockHash::new([0x42; 32]);
        let min_difficulty = 1000;

        let nonce = generator.generate(&block_hash, min_difficulty).unwrap();

        // Verify the generated nonce passes validation
        assert!(super::validate_work_inner(
            block_hash.as_bytes(),
            nonce.0,
            min_difficulty
        ));
    }

    #[test]
    fn test_zero_difficulty() {
        let generator = WorkGenerator;
        let block_hash = BlockHash::new([0u8; 32]);
        let min_difficulty = 0;

        // With difficulty 0, nonce 0 should pass
        assert!(super::validate_work_inner(
            block_hash.as_bytes(),
            0,
            min_difficulty
        ));

        // Generator should return nonce 0 immediately
        let nonce = generator.generate(&block_hash, min_difficulty).unwrap();
        assert_eq!(nonce.0, 0);
    }

    #[test]
    fn test_work_difficulty_computation() {
        let block_hash = BlockHash::new([0xAA; 32]);

        // Test that validate_work_inner correctly checks difficulty thresholds
        let nonce = 12345;
        let work_value = compute_work_value(block_hash.as_bytes(), nonce);

        // Should pass at difficulty equal to work value
        assert!(super::validate_work_inner(
            block_hash.as_bytes(),
            nonce,
            work_value
        ));

        // Should pass at lower difficulty
        if work_value > 0 {
            assert!(super::validate_work_inner(
                block_hash.as_bytes(),
                nonce,
                work_value - 1
            ));
        }

        // Should fail at higher difficulty
        assert!(!super::validate_work_inner(
            block_hash.as_bytes(),
            nonce,
            work_value + 1
        ));
    }

    /// Helper to compute work value for testing
    fn compute_work_value(block_hash: &[u8; 32], nonce: u64) -> u64 {
        let mut input = [0u8; 40];
        input[0..32].copy_from_slice(block_hash);
        input[32..40].copy_from_slice(&nonce.to_le_bytes());
        let hash = blake2b_256(&input);
        u64::from_le_bytes([
            hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7],
        ])
    }
}
