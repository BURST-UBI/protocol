//! Signing, hashing, wallet addresses and key recovery.
//!
//! Signatures use ed25519-dalek; delegation envelopes use X25519 and
//! XChaCha20-Poly1305. Callers define the bytes to sign and authenticate the
//! delegation grant. Mnemonic recovery uses BURST's fixed derivation scheme.

pub mod address;
pub mod encryption;
pub mod hash;
pub mod keys;
pub mod mnemonic;
pub mod sign;

pub use address::{decode_address, derive_address, validate_address};
pub use encryption::{decrypt_delegation_key, encrypt_delegation_key};
pub use hash::{blake2b_256, blake2b_256_iter, blake2b_256_multi, hash_block, hash_transaction, Blake2b256Hasher};
pub use keys::{
    ed25519_private_to_x25519, ed25519_public_to_x25519, generate_keypair, keypair_from_private,
    keypair_from_seed, public_from_private, derive_account_key,
};
pub use mnemonic::{generate_mnemonic, keypair_from_mnemonic, validate_mnemonic, MnemonicError};
pub use sign::{sign_message, verify_signature};
