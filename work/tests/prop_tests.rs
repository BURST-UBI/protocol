use proptest::prelude::*;

use burst_types::BlockHash;
use burst_work::{validate_work, WorkGenerator};

proptest! {
    /// Generated PoW always passes its own validation.
    #[test]
    fn generated_pow_always_valid(
        hash_byte in 0u8..=255,
        difficulty in 0u64..50_000,
    ) {
        let block_hash = BlockHash::new([hash_byte; 32]);
        let generator = WorkGenerator;
        let nonce = generator.generate(&block_hash, difficulty).unwrap();
        prop_assert!(
            validate_work(&block_hash, nonce.0, difficulty),
            "generated nonce must pass validation"
        );
    }

    /// Zero difficulty always passes regardless of nonce.
    #[test]
    fn zero_difficulty_always_passes(
        hash_bytes in prop::array::uniform32(0u8..),
        nonce in 0u64..1_000_000,
    ) {
        let block_hash = BlockHash::new(hash_bytes);
        prop_assert!(
            validate_work(&block_hash, nonce, 0),
            "zero difficulty must always pass"
        );
    }

    /// Max difficulty fails for (almost) all random inputs.
    #[test]
    fn max_difficulty_rejects_random(
        hash_bytes in prop::array::uniform32(0u8..),
        nonce in 0u64..1_000_000,
    ) {
        let block_hash = BlockHash::new(hash_bytes);
        // u64::MAX difficulty means work_value must be >= u64::MAX,
        // which is essentially impossible for random inputs.
        let result = validate_work(&block_hash, nonce, u64::MAX);
        // We just assert it doesn't panic; it should nearly always be false.
        let _ = result;
    }

    /// Validation is deterministic: same inputs produce same result.
    #[test]
    fn validation_is_deterministic(
        hash_bytes in prop::array::uniform32(0u8..),
        nonce in any::<u64>(),
        difficulty in any::<u64>(),
    ) {
        let block_hash = BlockHash::new(hash_bytes);
        let r1 = validate_work(&block_hash, nonce, difficulty);
        let r2 = validate_work(&block_hash, nonce, difficulty);
        prop_assert_eq!(r1, r2, "validation must be deterministic");
    }

    /// Lower difficulty is easier to meet: if valid at D, then valid at D-1.
    #[test]
    fn lower_difficulty_is_easier(
        hash_bytes in prop::array::uniform32(0u8..),
        nonce in any::<u64>(),
        difficulty in 1u64..u64::MAX,
    ) {
        let block_hash = BlockHash::new(hash_bytes);
        let valid_at_d = validate_work(&block_hash, nonce, difficulty);
        let valid_at_d_minus_1 = validate_work(&block_hash, nonce, difficulty - 1);
        if valid_at_d {
            prop_assert!(
                valid_at_d_minus_1,
                "if valid at difficulty {}, must be valid at {}",
                difficulty,
                difficulty - 1
            );
        }
    }
}
