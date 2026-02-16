//! Blake2b hashing for blocks and transactions.

use burst_types::{BlockHash, TxHash};

/// Compute a 256-bit Blake2b hash of arbitrary data.
pub fn blake2b_256(_data: &[u8]) -> [u8; 32] {
    todo!("use blake2::Blake2b256::digest()")
}

/// Hash a serialized block to produce its `BlockHash`.
pub fn hash_block(block_bytes: &[u8]) -> BlockHash {
    BlockHash::new(blake2b_256(block_bytes))
}

/// Hash a serialized transaction to produce its `TxHash`.
pub fn hash_transaction(tx_bytes: &[u8]) -> TxHash {
    TxHash::new(blake2b_256(tx_bytes))
}
