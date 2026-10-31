//! Transaction identifiers and hexadecimal parsing shared with BlockHash.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A 32-byte transaction hash.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TxHash([u8; 32]);

impl TxHash {
    pub const ZERO: Self = Self([0u8; 32]);

    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn is_zero(&self) -> bool {
        self.0 == [0u8; 32]
    }
}

impl fmt::Debug for TxHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TxHash(")?;
        for b in &self.0[..4] {
            write!(f, "{:02x}", b)?;
        }
        write!(f, ")")
    }
}

impl fmt::Display for TxHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.0 {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseHashError;

impl fmt::Display for ParseHashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected exactly 64 hexadecimal characters")
    }
}

impl std::error::Error for ParseHashError {}

fn parse_bytes(value: &str) -> Result<[u8; 32], ParseHashError> {
    if value.len() != 64 {
        return Err(ParseHashError);
    }
    fn digit(byte: u8) -> Result<u8, ParseHashError> {
        match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            b'A'..=b'F' => Ok(byte - b'A' + 10),
            _ => Err(ParseHashError),
        }
    }
    let mut bytes = [0; 32];
    for (out, pair) in bytes.iter_mut().zip(value.as_bytes().as_chunks::<2>().0) {
        *out = digit(pair[0])? * 16 + digit(pair[1])?;
    }
    Ok(bytes)
}

impl std::str::FromStr for TxHash {
    type Err = ParseHashError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_bytes(value).map(Self::new)
    }
}

impl std::str::FromStr for crate::BlockHash {
    type Err = ParseHashError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_bytes(value).map(Self::new)
    }
}

#[cfg(test)]
mod parsing_tests {
    use super::*;
    #[test]
    fn exact_hex_grammar_for_both_hash_types() {
        let upper = "AB".repeat(32);
        assert_eq!(upper.parse::<TxHash>().unwrap().as_bytes(), &[0xab; 32]);
        assert_eq!(
            upper.parse::<crate::BlockHash>().unwrap().as_bytes(),
            &[0xab; 32]
        );
        for bad in [
            "0".repeat(63),
            "0".repeat(65),
            "gg".repeat(32),
            "é".repeat(32),
            format!(" {}", "0".repeat(63)),
            format!("0x{}", "0".repeat(62)),
        ] {
            assert!(bad.parse::<TxHash>().is_err());
            assert!(bad.parse::<crate::BlockHash>().is_err());
        }
    }
}
