//! Blake2b hashing for blocks and transactions.

use blake2::digest::consts::U32;
use blake2::{Blake2b, Digest};
use burst_types::{BlockHash, TxHash};

type Blake2b256 = Blake2b<U32>;

/// Compute a 256-bit Blake2b hash of arbitrary data.
pub fn blake2b_256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Blake2b256::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut output = [0u8; 32];
    output.copy_from_slice(&result);
    output
}

/// Hash multiple byte slices in sequence (avoids concatenation allocation).
pub fn blake2b_256_multi(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Blake2b256::new();
    for part in parts {
        hasher.update(part);
    }
    let result = hasher.finalize();
    let mut output = [0u8; 32];
    output.copy_from_slice(&result);
    output
}

/// Hash a serialized block to produce its `BlockHash`.
pub fn hash_block(block_bytes: &[u8]) -> BlockHash {
    BlockHash::new(blake2b_256(block_bytes))
}

/// Hash a serialized transaction to produce its `TxHash`.
pub fn hash_transaction(tx_bytes: &[u8]) -> TxHash {
    TxHash::new(blake2b_256(tx_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blake2b_deterministic() {
        let h1 = blake2b_256(b"hello burst");
        let h2 = blake2b_256(b"hello burst");
        assert_eq!(h1, h2);
    }

    #[test]
    fn blake2b_different_inputs() {
        let h1 = blake2b_256(b"hello");
        let h2 = blake2b_256(b"world");
        assert_ne!(h1, h2);
    }

    #[test]
    fn blake2b_empty() {
        let h = blake2b_256(b"");
        assert_ne!(h, [0u8; 32]);
    }

    #[test]
    fn blake2b_multi_equivalent() {
        let single = blake2b_256(b"helloworld");
        let multi = blake2b_256_multi(&[b"hello", b"world"]);
        assert_eq!(single, multi);
    }

    #[test]
    fn hash_block_returns_blockhash() {
        let h = hash_block(b"test block data");
        assert!(!h.is_zero());
    }

    #[test]
    fn hash_transaction_returns_txhash() {
        let h = hash_transaction(b"test tx data");
        assert!(!h.is_zero());
    }
}
