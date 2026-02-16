//! The main BURST node struct.

use crate::config::NodeConfig;
use crate::error::NodeError;
use burst_brn::BrnEngine;
use burst_governance::GovernanceEngine;
use burst_trst::TrstEngine;

/// A running BURST node.
pub struct BurstNode {
    pub config: NodeConfig,
    pub brn_engine: BrnEngine,
    pub trst_engine: TrstEngine,
    pub governance: GovernanceEngine,
    // pub peer_manager: PeerManager,
    // pub ledger: ...,
    // pub verification: ...,
    // pub consensus: ...,
}

impl BurstNode {
    /// Create and initialize a new BURST node.
    pub async fn new(config: NodeConfig) -> Result<Self, NodeError> {
        Ok(Self {
            config,
            brn_engine: BrnEngine::new(),
            trst_engine: TrstEngine::new(),
            governance: GovernanceEngine,
        })
    }

    /// Start the node — begin listening for connections and processing blocks.
    pub async fn start(&mut self) -> Result<(), NodeError> {
        todo!("start P2P listener, begin sync, start block processor")
    }

    /// Stop the node gracefully.
    pub async fn stop(&mut self) -> Result<(), NodeError> {
        todo!("disconnect peers, flush storage, shutdown")
    }

    /// Process an incoming block/transaction.
    pub async fn process_transaction(
        &mut self,
        _tx: burst_transactions::Transaction,
    ) -> Result<(), NodeError> {
        todo!("validate transaction, apply to ledger, broadcast to peers")
    }

    /// Get the current protocol parameters.
    pub fn params(&self) -> &burst_types::ProtocolParams {
        &self.config.params
    }
}
