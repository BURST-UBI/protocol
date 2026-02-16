//! Message codec — framing and serialization for the wire protocol.

use crate::ProtocolError;

/// Maximum message size in bytes.
pub const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024; // 16 MiB

/// Encode a message for transmission (length-prefixed JSON).
pub fn encode(_message: &impl serde::Serialize) -> Result<Vec<u8>, ProtocolError> {
    todo!("serialize to JSON, prepend 4-byte length prefix")
}

/// Decode a message from raw bytes.
pub fn decode<T: serde::de::DeserializeOwned>(data: &[u8]) -> Result<T, ProtocolError> {
    serde_json::from_slice(data).map_err(|e| ProtocolError::Malformed(e.to_string()))
}
