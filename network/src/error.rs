use thiserror::Error;

#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    #[error("peer {0} not found")]
    PeerNotFound(String),

    #[error("sync failed: {0}")]
    SyncFailed(String),

    #[error("clock drift too large: {drift_ms}ms")]
    ClockDrift { drift_ms: i64 },

    #[error("protocol error: {0}")]
    Protocol(#[from] burst_protocol::ProtocolError),

    #[error("IO error: {0}")]
    Io(String),
}
