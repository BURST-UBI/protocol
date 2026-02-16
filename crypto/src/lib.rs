//! Cryptographic primitives for the BURST protocol.
//!
//! - **Ed25519** for signing and signature verification (same as Nano)
//! - **Blake2b** for hashing (block hashes, transaction hashes)
//! - **X25519** for Diffie-Hellman key exchange (delegation key encryption)
//! - Address derivation with `brst_` prefix

pub mod address;
pub mod encryption;
pub mod hash;
pub mod keys;
pub mod sign;

pub use address::derive_address;
pub use hash::{blake2b_256, hash_block, hash_transaction};
pub use keys::generate_keypair;
pub use sign::{sign_message, verify_signature};
