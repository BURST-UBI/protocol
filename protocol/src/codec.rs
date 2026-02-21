//! Message codec — framing and serialization for the wire protocol.
//!
//! Uses bincode for efficient binary serialization with 4-byte big-endian
//! length-prefix framing.

use crate::ProtocolError;

/// Maximum message size in bytes.
pub const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024; // 16 MiB

/// Encode a message for transmission (4-byte big-endian length prefix + bincode body).
pub fn encode(message: &impl serde::Serialize) -> Result<Vec<u8>, ProtocolError> {
    let body = bincode::serialize(message).map_err(|e| ProtocolError::Malformed(e.to_string()))?;
    if body.len() > MAX_MESSAGE_SIZE {
        return Err(ProtocolError::MessageTooLarge {
            size: body.len(),
            max: MAX_MESSAGE_SIZE,
        });
    }
    let len_bytes = (body.len() as u32).to_be_bytes();
    let mut result = Vec::with_capacity(4 + body.len());
    result.extend_from_slice(&len_bytes);
    result.extend_from_slice(&body);
    Ok(result)
}

/// Decode a message from raw bincode bytes (no length prefix).
pub fn decode<T: serde::de::DeserializeOwned>(data: &[u8]) -> Result<T, ProtocolError> {
    bincode::deserialize(data).map_err(|e| ProtocolError::Malformed(e.to_string()))
}

/// Decode a framed message (4-byte big-endian length prefix + bincode body).
/// Returns the decoded message and the number of bytes consumed.
pub fn decode_framed<T: serde::de::DeserializeOwned>(
    data: &[u8],
) -> Result<(T, usize), ProtocolError> {
    if data.len() < 4 {
        return Err(ProtocolError::Malformed(
            "insufficient data for length prefix".into(),
        ));
    }
    let len = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
    if len > MAX_MESSAGE_SIZE {
        return Err(ProtocolError::MessageTooLarge {
            size: len,
            max: MAX_MESSAGE_SIZE,
        });
    }
    if data.len() < 4 + len {
        return Err(ProtocolError::Malformed(format!(
            "insufficient data: need {} bytes, got {}",
            4 + len,
            data.len()
        )));
    }
    let message = decode::<T>(&data[4..4 + len])?;
    Ok((message, 4 + len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct TestMessage {
        value: u32,
        text: String,
    }

    #[test]
    fn test_encode_decode_roundtrip() {
        let msg = TestMessage {
            value: 42,
            text: "hello".to_string(),
        };
        let encoded = encode(&msg).unwrap();
        assert!(encoded.len() >= 4);

        let (decoded, consumed) = decode_framed::<TestMessage>(&encoded).unwrap();
        assert_eq!(decoded, msg);
        assert_eq!(consumed, encoded.len());
    }

    #[test]
    fn test_encode_size_limit() {
        let large_msg = TestMessage {
            value: 0,
            text: "x".repeat(MAX_MESSAGE_SIZE + 1),
        };
        let result = encode(&large_msg);
        assert!(result.is_err());
        match result.unwrap_err() {
            ProtocolError::MessageTooLarge { size, max } => {
                assert_eq!(max, MAX_MESSAGE_SIZE);
                assert!(size > MAX_MESSAGE_SIZE);
            }
            _ => panic!("expected MessageTooLarge error"),
        }
    }

    #[test]
    fn test_decode_framed_insufficient_length_prefix() {
        let data = vec![0u8, 0, 0]; // only 3 bytes
        let result = decode_framed::<TestMessage>(&data);
        assert!(result.is_err());
        match result.unwrap_err() {
            ProtocolError::Malformed(msg) => {
                assert!(msg.contains("insufficient data for length prefix"));
            }
            _ => panic!("expected Malformed error"),
        }
    }

    #[test]
    fn test_decode_framed_insufficient_body() {
        let mut data = vec![0u8; 8];
        data[0..4].copy_from_slice(&100u32.to_be_bytes()); // length = 100
                                                           // but we only have 4 more bytes
        let result = decode_framed::<TestMessage>(&data);
        assert!(result.is_err());
        match result.unwrap_err() {
            ProtocolError::Malformed(msg) => {
                assert!(msg.contains("insufficient data"));
            }
            _ => panic!("expected Malformed error"),
        }
    }

    #[test]
    fn test_decode_framed_too_large_length() {
        let mut data = vec![0u8; 8];
        let huge_len = (MAX_MESSAGE_SIZE as u32 + 1).to_be_bytes();
        data[0..4].copy_from_slice(&huge_len);
        let result = decode_framed::<TestMessage>(&data);
        assert!(result.is_err());
        match result.unwrap_err() {
            ProtocolError::MessageTooLarge { size, max } => {
                assert_eq!(max, MAX_MESSAGE_SIZE);
                assert!(size > MAX_MESSAGE_SIZE);
            }
            _ => panic!("expected MessageTooLarge error"),
        }
    }

    #[test]
    fn test_decode_framed_empty_message() {
        let msg = TestMessage {
            value: 0,
            text: String::new(),
        };
        let encoded = encode(&msg).unwrap();
        let (decoded, _) = decode_framed::<TestMessage>(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_decode_raw_bincode() {
        let msg = TestMessage {
            value: 99,
            text: "raw".to_string(),
        };
        let body = bincode::serialize(&msg).unwrap();
        let decoded: TestMessage = decode(&body).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_bincode_is_more_compact_than_json() {
        let msg = TestMessage {
            value: 42,
            text: "hello".to_string(),
        };
        let bincode_encoded = encode(&msg).unwrap();
        let json_bytes = serde_json::to_vec(&msg).unwrap();
        // bincode body (without 4-byte prefix) should be smaller than JSON
        assert!((bincode_encoded.len() - 4) < json_bytes.len());
    }
}
