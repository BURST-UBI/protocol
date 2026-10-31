use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("unsupported protocol version: {0}")]
    UnsupportedVersion(u16),

    #[error("message too large: {size} > {max}")]
    MessageTooLarge { size: usize, max: usize },

    #[error("malformed message: {0}")]
    Malformed(String),

    #[error("handshake failed: {0}")]
    HandshakeFailed(String),

    #[error("IO error: {0}")]
    Io(String),
}
