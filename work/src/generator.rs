//! Parallel native and synchronous WebAssembly proof-of-work generation.

#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(not(target_arch = "wasm32"))]
use rayon::prelude::*;

use crate::work_value;
use crate::{WorkError, WorkNonce};
use burst_types::BlockHash;

/// Generates proof-of-work. Browser callers should run local generation in a
/// Web Worker; the wallet RPC client can request work from its node instead.
pub struct WorkGenerator;

/// Batch size per thread before checking cancellation flag.
#[cfg(not(target_arch = "wasm32"))]
const BATCH_SIZE: u64 = 4096;

impl WorkGenerator {
    #[cfg(target_arch = "wasm32")]
    pub fn generate(
        &self,
        block_hash: &BlockHash,
        min_difficulty: u64,
    ) -> Result<WorkNonce, WorkError> {
        (0..=u64::MAX)
            .find(|nonce| work_value(block_hash, *nonce) >= min_difficulty)
            .map(WorkNonce)
            .ok_or(WorkError::Cancelled)
    }

    /// Generate a work nonce that meets the minimum difficulty.
    ///
    /// Workers stride through nonces using the current Rayon pool. A valid result
    /// signals other workers to stop at their next batch boundary.
    /// This entry point has no deadline; use generate_until for bounded work.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn generate(
        &self,
        block_hash: &BlockHash,
        min_difficulty: u64,
    ) -> Result<WorkNonce, WorkError> {
        self.generate_while(block_hash, min_difficulty, || true)
    }

    /// Check the supplied cancellation flag and deadline between batches.
    /// The caller must set the flag when abandoning the request; dropping its
    /// own future does not by itself signal this generator.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn generate_until(
        &self,
        block_hash: &BlockHash,
        min_difficulty: u64,
        cancelled: &std::sync::atomic::AtomicBool,
        deadline: std::time::Instant,
    ) -> Result<WorkNonce, WorkError> {
        self.generate_while(block_hash, min_difficulty, || {
            !cancelled.load(Ordering::Relaxed) && std::time::Instant::now() < deadline
        })
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn generate_while(
        &self,
        block_hash: &BlockHash,
        min_difficulty: u64,
        running: impl Fn() -> bool + Sync,
    ) -> Result<WorkNonce, WorkError> {
        if !running() {
            return Err(WorkError::Cancelled);
        }
        if min_difficulty == 0 {
            return Ok(WorkNonce(0));
        }

        let found = AtomicU64::new(u64::MAX);
        let num_threads = rayon::current_num_threads().max(1);

        (0..num_threads).into_par_iter().for_each(|thread_id| {
            let mut nonce = thread_id as u64;
            let stride = num_threads as u64;

            loop {
                if found.load(Ordering::Relaxed) != u64::MAX || !running() {
                    return;
                }

                for _ in 0..BATCH_SIZE {
                    if work_value(block_hash, nonce) >= min_difficulty {
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

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::BlockHash;

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn cancelled_and_expired_work_stops_without_searching() {
        use std::sync::atomic::AtomicBool;
        use std::time::{Duration, Instant};
        let hash = BlockHash::new([19; 32]);
        let generator = WorkGenerator;
        assert!(generator.generate_until(&hash, u64::MAX, &AtomicBool::new(true), Instant::now() + Duration::from_secs(1)).is_err());
        assert!(generator.generate_until(&hash, u64::MAX, &AtomicBool::new(false), Instant::now()).is_err());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn active_search_observes_deadline() {
        use std::sync::atomic::AtomicBool;
        use std::time::{Duration, Instant};
        let start = Instant::now();
        let result = WorkGenerator.generate_until(&BlockHash::new([19; 32]), u64::MAX,
            &AtomicBool::new(false), start + Duration::from_millis(10));
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn test_generate_work() {
        let generator = WorkGenerator;
        let block_hash = BlockHash::new([0x42; 32]);
        let min_difficulty = 1000;

        let nonce = generator.generate(&block_hash, min_difficulty).unwrap();

        assert!(crate::validate_work(
            &block_hash,
            nonce.0,
            min_difficulty
        ));
    }

    #[test]
    fn test_zero_difficulty() {
        let generator = WorkGenerator;
        let block_hash = BlockHash::new([0u8; 32]);
        let min_difficulty = 0;

        assert!(crate::validate_work(
            &block_hash,
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
        let work_value = crate::work_value(&block_hash, nonce);

        assert!(crate::validate_work(
            &block_hash,
            nonce,
            work_value
        ));

        if work_value > 0 {
            assert!(crate::validate_work(
                &block_hash,
                nonce,
                work_value - 1
            ));
        }

        assert!(!crate::validate_work(
            &block_hash,
            nonce,
            work_value + 1
        ));
    }

}
