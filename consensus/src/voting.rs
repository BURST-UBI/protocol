//! Representative voting for conflict resolution.

use crate::representative::Representative;
use burst_types::BlockHash;
use std::collections::HashMap;

/// Manages representative votes for resolving block conflicts.
pub struct RepresentativeVoting {
    /// Votes: block_hash → total weight voting for it.
    votes: HashMap<BlockHash, u128>,
    /// Total online voting weight.
    total_online_weight: u128,
}

impl RepresentativeVoting {
    pub fn new(total_online_weight: u128) -> Self {
        Self {
            votes: HashMap::new(),
            total_online_weight,
        }
    }

    /// Record a representative's vote for a specific block.
    pub fn cast_vote(&mut self, rep: &Representative, block: BlockHash) {
        *self.votes.entry(block).or_insert(0) += rep.delegated_weight;
    }

    /// Check if a block has reached the confirmation threshold (≥ 67%).
    pub fn is_confirmed(&self, block: &BlockHash) -> bool {
        let weight = self.votes.get(block).copied().unwrap_or(0);
        // 67% threshold (6700 basis points)
        weight * 10_000 / self.total_online_weight.max(1) >= 6700
    }

    /// Get the winning block (if any has reached confirmation threshold).
    pub fn winner(&self) -> Option<BlockHash> {
        self.votes
            .iter()
            .find(|(hash, _)| self.is_confirmed(hash))
            .map(|(hash, _)| *hash)
    }
}
