//! PoW validation.

use burst_types::BlockHash;

/// Validate that a work nonce meets the minimum difficulty for a given block.
pub fn validate_work(_block_hash: &BlockHash, _nonce: u64, _min_difficulty: u64) -> bool {
    todo!("compute Blake2b(block_hash || nonce), check result >= min_difficulty")
}
