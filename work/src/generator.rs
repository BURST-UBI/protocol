//! PoW generation (CPU).

use crate::{WorkError, WorkNonce};
use burst_types::BlockHash;

/// Generates proof-of-work for a block.
pub struct WorkGenerator;

impl WorkGenerator {
    /// Generate a work nonce that meets the minimum difficulty.
    ///
    /// Iterates nonces until `hash(block_hash || nonce)` meets the threshold.
    pub fn generate(
        &self,
        _block_hash: &BlockHash,
        _min_difficulty: u64,
    ) -> Result<WorkNonce, WorkError> {
        todo!("iterate nonces, compute Blake2b(block_hash || nonce), check difficulty")
    }
}
