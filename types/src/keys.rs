//! Cryptographic key types for wallet identity and signing.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A 32-byte Ed25519 public key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PublicKey(pub [u8; 32]);

/// A 32-byte Ed25519 private key (secret scalar).
///
/// This type intentionally does not implement `Debug` or `Serialize` to prevent
/// accidental exposure.
#[derive(Clone)]
pub struct PrivateKey(pub [u8; 32]);

/// A 64-byte Ed25519 signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature(pub [u8; 64]);

impl Serialize for Signature {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for Signature {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes: Vec<u8> = Deserialize::deserialize(deserializer)?;
        let arr: [u8; 64] = bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("expected exactly 64 bytes for Signature"))?;
        Ok(Signature(arr))
    }
}

/// An Ed25519 key pair (public + private).
pub struct KeyPair {
    pub public: PublicKey,
    pub private: PrivateKey,
}

impl KeyPair {
    /// Generate a new random key pair.
    pub fn generate() -> Self {
        todo!("generate Ed25519 key pair using ed25519-dalek")
    }

    /// Reconstruct a key pair from a private key.
    pub fn from_private(_private: PrivateKey) -> Self {
        todo!("derive public key from private key")
    }
}

impl PublicKey {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl Signature {
    pub fn as_bytes(&self) -> &[u8; 64] {
        &self.0
    }
}
