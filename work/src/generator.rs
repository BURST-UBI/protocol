//! PoW generation (multi-threaded CPU).

use std::sync::atomic::{AtomicU64, Ordering};

use rayon::prelude::*;

use crate::{WorkError, WorkNonce};
use burst_crypto::blake2b_256;
use burst_types::BlockHash;

/// Generates proof-of-work for a block using all available CPU cores.
pub struct WorkGenerator;

/// Batch size per thread before checking cancellation flag.
const BATCH_SIZE: u64 = 4096;

impl WorkGenerator {
    /// Generate a work nonce that meets the minimum difficulty.
    ///
    /// Splits the nonce space across all available CPU cores via rayon.
    /// The first thread to find a valid nonce signals the others to stop.
    pub fn generate(
        &self,
        block_hash: &BlockHash,
        min_difficulty: u64,
    ) -> Result<WorkNonce, WorkError> {
        if min_difficulty == 0 {
            return Ok(WorkNonce(0));
        }

        let hash_bytes: [u8; 32] = *block_hash.as_bytes();
        let found = AtomicU64::new(u64::MAX);
        let num_threads = rayon::current_num_threads().max(1);

        (0..num_threads).into_par_iter().for_each(|thread_id| {
            let mut input = [0u8; 40];
            input[0..32].copy_from_slice(&hash_bytes);

            let mut nonce = thread_id as u64;
            let stride = num_threads as u64;

            loop {
                if found.load(Ordering::Relaxed) != u64::MAX {
                    return;
                }

                let end = nonce.saturating_add(BATCH_SIZE * stride);
                while nonce < end {
                    input[32..40].copy_from_slice(&nonce.to_le_bytes());
                    let hash = blake2b_256(&input);
                    let work_value = u64::from_le_bytes(hash[0..8].try_into().unwrap());
                    if work_value >= min_difficulty {
                        found.store(nonce, Ordering::Relaxed);
                        return;
                    }
                    nonce = nonce.wrapping_add(stride);
                }
            }
        });

        let result = found.load(Ordering::Relaxed);
        if result == u64::MAX {
            Err(WorkError::Cancelled)
        } else {
            Ok(WorkNonce(result))
        }
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

        assert!(super::validate_work_inner(
            block_hash.as_bytes(),
            0,
            min_difficulty
        ));

        let nonce = generator.generate(&block_hash, min_difficulty).unwrap();
        assert_eq!(nonce.0, 0);
    }

    #[test]
    fn test_work_difficulty_computation() {
        let block_hash = BlockHash::new([0xAA; 32]);

        let nonce = 12345;
        let work_value = compute_work_value(block_hash.as_bytes(), nonce);

        assert!(super::validate_work_inner(
            block_hash.as_bytes(),
            nonce,
            work_value
        ));

        if work_value > 0 {
            assert!(super::validate_work_inner(
                block_hash.as_bytes(),
                nonce,
                work_value - 1
            ));
        }

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
