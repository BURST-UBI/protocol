use thiserror::Error;

#[derive(Debug, Error)]
pub enum NodeError {
    #[error("ledger error: {0}")]
    Ledger(#[from] burst_ledger::LedgerError),

    #[error("network error: {0}")]
    Network(#[from] burst_network::NetworkError),

    #[error("verification error: {0}")]
    Verification(#[from] burst_verification::VerificationError),

    #[error("governance error: {0}")]
    Governance(#[from] burst_governance::GovernanceError),

    #[error("consensus error: {0}")]
    Consensus(#[from] burst_consensus::ConsensusError),

    #[error("node not initialized")]
    NotInitialized,

    #[error("{0}")]
    Other(String),
}
