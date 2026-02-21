//! Conflict detection — identifies forks in account chains.

use burst_types::{BlockHash, WalletAddress};

/// Detects conflicting blocks (forks) in account chains.
pub struct ConflictDetector;

impl ConflictDetector {
    /// Check if two blocks represent a fork (same account, same previous, different hash).
    ///
    /// Two blocks form a fork if:
    /// - They have the same previous block hash (`previous_a == previous_b`)
    /// - They have different block hashes (`block_a != block_b`)
    /// - Both belong to the same account (account parameter ensures this)
    pub fn is_fork(
        &self,
        _account: &WalletAddress,
        block_a: &BlockHash,
        block_b: &BlockHash,
        previous_a: &BlockHash,
        previous_b: &BlockHash,
    ) -> bool {
        // Fork: same previous block, but different block hashes
        previous_a == previous_b && block_a != block_b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_hash(bytes: [u8; 32]) -> BlockHash {
        BlockHash::new(bytes)
    }

    #[test]
    fn test_same_previous_different_blocks_is_fork() {
        let detector = ConflictDetector;
        let account = WalletAddress::new("brst_test_account".to_string());

        let previous = make_hash([1u8; 32]);
        let block_a = make_hash([2u8; 32]);
        let block_b = make_hash([3u8; 32]);

        assert!(detector.is_fork(&account, &block_a, &block_b, &previous, &previous));
    }

    #[test]
    fn test_same_previous_same_block_not_fork() {
        let detector = ConflictDetector;
        let account = WalletAddress::new("brst_test_account".to_string());

        let previous = make_hash([1u8; 32]);
        let block = make_hash([2u8; 32]);

        // Same previous, same block = duplicate, not a fork
        assert!(!detector.is_fork(&account, &block, &block, &previous, &previous));
    }

    #[test]
    fn test_different_previous_not_fork() {
        let detector = ConflictDetector;
        let account = WalletAddress::new("brst_test_account".to_string());

        let previous_a = make_hash([1u8; 32]);
        let previous_b = make_hash([2u8; 32]);
        let block_a = make_hash([3u8; 32]);
        let block_b = make_hash([4u8; 32]);

        // Different previous blocks = not a fork (different branches)
        assert!(!detector.is_fork(&account, &block_a, &block_b, &previous_a, &previous_b));
    }
}
