//! Cryptographic primitives for the BURST protocol.
//!
//! - **Ed25519** for signing and signature verification (same as Nano)
//! - **Blake2b** for hashing (block hashes, transaction hashes)
//! - **X25519** for Diffie-Hellman key exchange (delegation key encryption)
//! - Address derivation with `brst_` prefix and base32 encoding

pub mod address;
pub mod encryption;
pub mod hash;
pub mod keys;
pub mod mnemonic;
pub mod sign;

pub use address::{decode_address, derive_address, validate_address};
pub use encryption::{decrypt_delegation_key, encrypt_delegation_key};
pub use hash::{blake2b_256, blake2b_256_multi, hash_block, hash_transaction};
pub use keys::{
    ed25519_private_to_x25519, ed25519_public_to_x25519, generate_keypair, keypair_from_private,
    keypair_from_seed, public_from_private,
};
pub use mnemonic::{generate_mnemonic, keypair_from_mnemonic, validate_mnemonic, MnemonicError};
pub use sign::{sign_message, verify_signature};
