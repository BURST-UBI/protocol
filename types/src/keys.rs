//! Cryptographic key types for wallet identity and signing.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// A 32-byte Ed25519 public key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PublicKey(pub [u8; 32]);

/// A 32-byte Ed25519 private key (secret scalar).
///
/// This type intentionally does not implement `Debug`, `Serialize`, or `Clone`
/// to prevent accidental exposure. Key bytes are zeroized on drop.
#[derive(Zeroize, ZeroizeOnDrop)]
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
        struct SigVisitor;

        impl<'de> serde::de::Visitor<'de> for SigVisitor {
            type Value = Signature;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "64 bytes")
            }

            fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
                let arr: [u8; 64] = v
                    .try_into()
                    .map_err(|_| E::invalid_length(v.len(), &self))?;
                Ok(Signature(arr))
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut arr = [0u8; 64];
                for (i, byte) in arr.iter_mut().enumerate() {
                    *byte = seq
                        .next_element()?
                        .ok_or_else(|| serde::de::Error::invalid_length(i, &self))?;
                }
                Ok(Signature(arr))
            }
        }

        deserializer.deserialize_bytes(SigVisitor)
    }
}

/// An Ed25519 key pair (public + private).
///
/// Use `burst_crypto::generate_keypair()` or `burst_crypto::keypair_from_private()`
/// to construct key pairs. This struct is intentionally just data.
pub struct KeyPair {
    pub public: PublicKey,
    pub private: PrivateKey,
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
