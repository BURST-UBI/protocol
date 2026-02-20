//! PoW validation.

use burst_crypto::blake2b_256;
use burst_types::BlockHash;

/// Validate that a work nonce meets the minimum difficulty for a given block.
pub fn validate_work(block_hash: &BlockHash, nonce: u64, min_difficulty: u64) -> bool {
    // Same logic as generator check: concatenate block_hash + nonce LE bytes,
    // hash with Blake2b-256, interpret first 8 bytes as u64 LE, compare >= min_difficulty
    let mut input = [0u8; 40];
    input[0..32].copy_from_slice(block_hash.as_bytes());
    input[32..40].copy_from_slice(&nonce.to_le_bytes());
    
    let hash = blake2b_256(&input);
    let work_value = u64::from_le_bytes([
        hash[0], hash[1], hash[2], hash[3],
        hash[4], hash[5], hash[6], hash[7],
    ]);
    
    work_value >= min_difficulty
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkGenerator;
    use burst_types::BlockHash;

    #[test]
    fn test_generated_nonce_passes_validation() {
        let generator = WorkGenerator;
        let block_hash = BlockHash::new([0xDE; 32]);
        let min_difficulty = 5000;
        
        let nonce = generator.generate(&block_hash, min_difficulty).unwrap();
        
        // The generated nonce should pass validation
        assert!(validate_work(&block_hash, nonce.0, min_difficulty));
    }

    #[test]
    fn test_zero_nonce_with_zero_difficulty() {
        let block_hash = BlockHash::new([0u8; 32]);
        
        // Zero nonce with zero difficulty should pass
        assert!(validate_work(&block_hash, 0, 0));
    }

    #[test]
    fn test_invalid_nonce_fails() {
        let block_hash = BlockHash::new([0xFF; 32]);
        let min_difficulty = u64::MAX;
        
        // A random nonce should fail with maximum difficulty
        assert!(!validate_work(&block_hash, 12345, min_difficulty));
    }

    #[test]
    fn test_work_difficulty_threshold() {
        let block_hash = BlockHash::new([0x42; 32]);
        
        // Generate work for a specific difficulty
        let generator = WorkGenerator;
        let target_difficulty = 10000;
        let nonce = generator.generate(&block_hash, target_difficulty).unwrap();
        
        // Should pass at target difficulty
        assert!(validate_work(&block_hash, nonce.0, target_difficulty));
        
        // Should pass at lower difficulty
        assert!(validate_work(&block_hash, nonce.0, target_difficulty - 1));
        
        // Compute the actual work value to test the upper bound
        let mut input = [0u8; 40];
        input[0..32].copy_from_slice(block_hash.as_bytes());
        input[32..40].copy_from_slice(&nonce.0.to_le_bytes());
        let hash = burst_crypto::blake2b_256(&input);
        let work_value = u64::from_le_bytes([
            hash[0], hash[1], hash[2], hash[3],
            hash[4], hash[5], hash[6], hash[7],
        ]);
        
        // Should fail at a difficulty higher than the actual work value
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
        
        // Work for different hashes should be valid for their respective hashes
        assert!(validate_work(&hash1, nonce1.0, min_difficulty));
        assert!(validate_work(&hash2, nonce2.0, min_difficulty));
        
        // Compute work values to verify the hash function is working correctly
        // Work value for nonce1 with hash1 should be >= min_difficulty
        let mut input1 = [0u8; 40];
        input1[0..32].copy_from_slice(hash1.as_bytes());
        input1[32..40].copy_from_slice(&nonce1.0.to_le_bytes());
        let hash1_result = burst_crypto::blake2b_256(&input1);
        let work_value1 = u64::from_le_bytes([
            hash1_result[0], hash1_result[1], hash1_result[2], hash1_result[3],
            hash1_result[4], hash1_result[5], hash1_result[6], hash1_result[7],
        ]);
        assert!(work_value1 >= min_difficulty);
        
        // Work value for nonce1 with hash2 should be different
        let mut input2 = [0u8; 40];
        input2[0..32].copy_from_slice(hash2.as_bytes());
        input2[32..40].copy_from_slice(&nonce1.0.to_le_bytes());
        let hash2_result = burst_crypto::blake2b_256(&input2);
        let work_value2 = u64::from_le_bytes([
            hash2_result[0], hash2_result[1], hash2_result[2], hash2_result[3],
            hash2_result[4], hash2_result[5], hash2_result[6], hash2_result[7],
        ]);
        
        // The work values should be different (hash function property)
        // Note: It's possible but extremely unlikely that work_value2 >= min_difficulty
        // We just verify that the hash produces different values for different inputs
        assert_ne!(work_value1, work_value2);
    }
}
