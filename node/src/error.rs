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

    #[error("store error: {0}")]
    Store(#[from] burst_store::StoreError),

    #[error("invalid block: {0}")]
    InvalidBlock(String),

    #[error("proof-of-work does not meet minimum difficulty")]
    WorkInvalid,

    #[error("block signature is invalid")]
    SignatureInvalid,

    #[error("config error: {0}")]
    Config(String),

    #[error("node not initialized")]
    NotInitialized,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("RPC server error: {0}")]
    Rpc(String),

    #[error("WebSocket server error: {0}")]
    WebSocket(String),

    #[error("shutdown timeout")]
    ShutdownTimeout,

    #[error("{0}")]
    Other(String),
}
