//! PoW validation.

use blake2::{digest::consts::U8, Blake2b, Digest};
use burst_types::BlockHash;

/// Check the nonce against the supplied work root and inclusive threshold.
/// The caller must select the correct root; this does not validate a block.
pub fn validate_work(root: &BlockHash, nonce: u64, min_difficulty: u64) -> bool {
    work_value(root, nonce) >= min_difficulty
}

/// Nano-style Blake2b-64(nonce_le || root), interpreted as little-endian u64.
/// An eight-byte Blake2b digest is not a truncated 32-byte digest.
pub fn work_value(root: &BlockHash, nonce: u64) -> u64 {
    let mut hasher = Blake2b::<U8>::new();
    hasher.update(nonce.to_le_bytes());
    hasher.update(root.as_bytes());
    u64::from_le_bytes(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkGenerator;
    use burst_types::BlockHash;

    #[test]
    fn matches_pinned_rsnano_difficulty_vector() {
        // references/rsnano-node/types/src/difficulty.rs::difficulty_for_root
        let mut root = [0; 32];
        root[24..].copy_from_slice(&123u64.to_be_bytes());
        assert_eq!(
            work_value(&BlockHash::new(root), 456),
            10_978_371_542_656_683_347
        );
    }

    #[test]
    fn test_generated_nonce_passes_validation() {
        let generator = WorkGenerator;
        let block_hash = BlockHash::new([0xDE; 32]);
        let min_difficulty = 5000;

        let nonce = generator.generate(&block_hash, min_difficulty).unwrap();

        assert!(validate_work(&block_hash, nonce.0, min_difficulty));
    }

    #[test]
    fn test_zero_nonce_with_zero_difficulty() {
        let block_hash = BlockHash::new([0u8; 32]);

        assert!(validate_work(&block_hash, 0, 0));
    }

    #[test]
    fn test_invalid_nonce_fails() {
        let block_hash = BlockHash::new([0xFF; 32]);
        let min_difficulty = u64::MAX;

        assert!(!validate_work(&block_hash, 12345, min_difficulty));
    }

    #[test]
    fn test_work_difficulty_threshold() {
        let block_hash = BlockHash::new([0x42; 32]);

        let generator = WorkGenerator;
        let target_difficulty = 10000;
        let nonce = generator.generate(&block_hash, target_difficulty).unwrap();

        assert!(validate_work(&block_hash, nonce.0, target_difficulty));

        assert!(validate_work(&block_hash, nonce.0, target_difficulty - 1));

        let work_value = work_value(&block_hash, nonce.0);

        assert!(!validate_work(&block_hash, nonce.0, work_value + 1));
    }

    #[test]
    fn test_different_block_hashes_produce_different_work() {
        let hash1 = BlockHash::new([0x11; 32]);
        let hash2 = BlockHash::new([0x22; 32]);
        let min_difficulty = 10000;

        let generator = WorkGenerator;
        let nonce1 = generator.generate(&hash1, min_difficulty).unwrap();
        let nonce2 = generator.generate(&hash2, min_difficulty).unwrap();

        assert!(validate_work(&hash1, nonce1.0, min_difficulty));
        assert!(validate_work(&hash2, nonce2.0, min_difficulty));

        let work_value1 = work_value(&hash1, nonce1.0);
        assert!(work_value1 >= min_difficulty);

        let work_value2 = work_value(&hash2, nonce1.0);

        assert_ne!(work_value1, work_value2);
    }
}
