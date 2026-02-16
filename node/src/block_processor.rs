//! Block processing pipeline.
//!
//! Validates incoming blocks, applies them to the ledger, detects conflicts,
//! and triggers consensus if needed.

use crate::error::NodeError;
use burst_ledger::StateBlock;

/// The block processing pipeline.
pub struct BlockProcessor;

impl BlockProcessor {
    /// Process a single incoming block.
    ///
    /// Steps:
    /// 1. Validate PoW
    /// 2. Validate signature
    /// 3. Validate against account chain (no gap, no fork)
    /// 4. Apply transaction effects (BRN burn, TRST transfer, etc.)
    /// 5. Update ledger state
    /// 6. If fork detected → initiate consensus vote
    pub async fn process(&self, _block: StateBlock) -> Result<ProcessResult, NodeError> {
        todo!("validate, apply, update ledger, broadcast confirmation request")
    }
}

/// Result of processing a block.
pub enum ProcessResult {
    /// Block was accepted and applied.
    Accepted,
    /// Block references an unknown previous block (need to sync).
    Gap,
    /// Block conflicts with an existing block (fork — need consensus).
    Fork,
    /// Block was rejected (invalid).
    Rejected(String),
}
