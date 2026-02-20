//! Argon2id encrypted keystore for Ed25519 private keys.
//!
//! Encrypts a 32-byte Ed25519 secret key with a user-chosen password:
//! 1. Argon2id derives a 32-byte encryption key from the password + random salt
//! 2. AES-256-GCM encrypts the secret key with a random nonce
//! 3. The result is stored as a JSON file with all parameters for future decryption

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::error::WalletError;

/// Argon2id parameters: 64 MB memory, 3 iterations, 1 lane of parallelism.
const ARGON2_MEMORY_KIB: u32 = 65536; // 64 MB
const ARGON2_ITERATIONS: u32 = 3;
const ARGON2_PARALLELISM: u32 = 1;
const ARGON2_OUTPUT_LEN: usize = 32;

/// Salt length in bytes.
const SALT_LEN: usize = 32;
/// AES-GCM nonce length in bytes (96 bits).
const NONCE_LEN: usize = 12;

/// The top-level keystore file structure, serializable to/from JSON.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeystoreFile {
    pub version: u32,
    pub crypto: KeystoreCrypto,
}

/// The crypto section of the keystore, containing all encryption parameters.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeystoreCrypto {
    pub cipher: String,
    pub kdf: String,
    pub kdf_params: KdfParams,
    /// Hex-encoded salt.
    pub salt: String,
    /// Hex-encoded nonce.
    pub nonce: String,
    /// Hex-encoded ciphertext.
    pub ciphertext: String,
}

/// KDF parameters for Argon2id.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KdfParams {
    pub memory: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

/// Encrypt a 32-byte Ed25519 secret key with a password using Argon2id + AES-256-GCM.
pub fn encrypt_keystore(
    secret_key: &[u8; 32],
    password: &str,
) -> Result<KeystoreFile, WalletError> {
    let mut rng = rand::thread_rng();

    // Generate random salt and nonce
    let mut salt = [0u8; SALT_LEN];
    rng.fill_bytes(&mut salt);

    let mut nonce_bytes = [0u8; NONCE_LEN];
    rng.fill_bytes(&mut nonce_bytes);

    // Derive encryption key via Argon2id
    let derived_key = derive_key(password, &salt)?;

    // Encrypt with AES-256-GCM
    let cipher = Aes256Gcm::new_from_slice(&derived_key)
        .map_err(|e| WalletError::Key(format!("AES key init failed: {}", e)))?;

    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, secret_key.as_ref())
        .map_err(|e| WalletError::Key(format!("encryption failed: {}", e)))?;

    Ok(KeystoreFile {
        version: 1,
        crypto: KeystoreCrypto {
            cipher: "aes-256-gcm".to_string(),
            kdf: "argon2id".to_string(),
            kdf_params: KdfParams {
                memory: ARGON2_MEMORY_KIB,
                iterations: ARGON2_ITERATIONS,
                parallelism: ARGON2_PARALLELISM,
            },
            salt: hex_encode(&salt),
            nonce: hex_encode(&nonce_bytes),
            ciphertext: hex_encode(&ciphertext),
        },
    })
}

/// Decrypt a keystore file with the given password, returning the 32-byte secret key.
pub fn decrypt_keystore(
    keystore: &KeystoreFile,
    password: &str,
) -> Result<[u8; 32], WalletError> {
    if keystore.version != 1 {
        return Err(WalletError::Key(format!(
            "unsupported keystore version: {}",
            keystore.version
        )));
    }

    let salt = hex_decode(&keystore.crypto.salt)
        .map_err(|e| WalletError::Key(format!("invalid salt hex: {}", e)))?;
    let nonce_bytes = hex_decode(&keystore.crypto.nonce)
        .map_err(|e| WalletError::Key(format!("invalid nonce hex: {}", e)))?;
    let ciphertext = hex_decode(&keystore.crypto.ciphertext)
        .map_err(|e| WalletError::Key(format!("invalid ciphertext hex: {}", e)))?;

    if nonce_bytes.len() != NONCE_LEN {
        return Err(WalletError::Key(format!(
            "invalid nonce length: expected {}, got {}",
            NONCE_LEN,
            nonce_bytes.len()
        )));
    }

    // Derive the same encryption key from password + salt
    let derived_key = derive_key(password, &salt)?;

    // Decrypt with AES-256-GCM
    let cipher = Aes256Gcm::new_from_slice(&derived_key)
        .map_err(|e| WalletError::Key(format!("AES key init failed: {}", e)))?;

    let nonce = Nonce::from_slice(&nonce_bytes);
    let plaintext = cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|_| WalletError::Key("decryption failed: wrong password or corrupted data".to_string()))?;

    if plaintext.len() != 32 {
        return Err(WalletError::Key(format!(
            "decrypted key has wrong length: expected 32, got {}",
            plaintext.len()
        )));
    }

    let mut key = [0u8; 32];
    key.copy_from_slice(&plaintext);
    Ok(key)
}

/// Save a keystore to a JSON file.
pub fn save_keystore(keystore: &KeystoreFile, path: &Path) -> Result<(), WalletError> {
    let json = serde_json::to_string_pretty(keystore)
        .map_err(|e| WalletError::Other(format!("JSON serialization failed: {}", e)))?;
    std::fs::write(path, json)
        .map_err(|e| WalletError::Other(format!("failed to write keystore file: {}", e)))?;
    Ok(())
}

/// Load a keystore from a JSON file.
pub fn load_keystore(path: &Path) -> Result<KeystoreFile, WalletError> {
    let json = std::fs::read_to_string(path)
        .map_err(|e| WalletError::Other(format!("failed to read keystore file: {}", e)))?;
    let keystore: KeystoreFile = serde_json::from_str(&json)
        .map_err(|e| WalletError::Other(format!("invalid keystore JSON: {}", e)))?;
    Ok(keystore)
}

/// Derive a 32-byte key from a password and salt using Argon2id.
fn derive_key(password: &str, salt: &[u8]) -> Result<[u8; 32], WalletError> {
    let params = Params::new(
        ARGON2_MEMORY_KIB,
        ARGON2_ITERATIONS,
        ARGON2_PARALLELISM,
        Some(ARGON2_OUTPUT_LEN),
    )
    .map_err(|e| WalletError::Key(format!("Argon2 params error: {}", e)))?;

    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

    let mut output = [0u8; 32];
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut output)
        .map_err(|e| WalletError::Key(format!("Argon2 hashing failed: {}", e)))?;

    Ok(output)
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("odd-length hex string".to_string());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|e| format!("invalid hex at position {}: {}", i, e))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let secret_key = [42u8; 32];
        let password = "test-password-123";

        let keystore = encrypt_keystore(&secret_key, password).unwrap();
        let decrypted = decrypt_keystore(&keystore, password).unwrap();

        assert_eq!(decrypted, secret_key);
    }

    #[test]
    fn wrong_password_fails() {
        let secret_key = [42u8; 32];
        let keystore = encrypt_keystore(&secret_key, "correct-password").unwrap();
        let result = decrypt_keystore(&keystore, "wrong-password");
        assert!(result.is_err());
    }

    #[test]
    fn keystore_version_is_1() {
        let keystore = encrypt_keystore(&[0u8; 32], "pass").unwrap();
        assert_eq!(keystore.version, 1);
    }

    #[test]
    fn keystore_crypto_fields() {
        let keystore = encrypt_keystore(&[0u8; 32], "pass").unwrap();
        assert_eq!(keystore.crypto.cipher, "aes-256-gcm");
        assert_eq!(keystore.crypto.kdf, "argon2id");
        assert_eq!(keystore.crypto.kdf_params.memory, 65536);
        assert_eq!(keystore.crypto.kdf_params.iterations, 3);
        assert_eq!(keystore.crypto.kdf_params.parallelism, 1);
    }

    #[test]
    fn keystore_serializes_to_json() {
        let keystore = encrypt_keystore(&[1u8; 32], "pass").unwrap();
        let json = serde_json::to_string_pretty(&keystore).unwrap();
        assert!(json.contains("\"version\": 1"));
        assert!(json.contains("\"cipher\": \"aes-256-gcm\""));
        assert!(json.contains("\"kdf\": \"argon2id\""));
    }

    #[test]
    fn save_and_load_roundtrip() {
        let secret_key = [99u8; 32];
        let password = "file-test";
        let keystore = encrypt_keystore(&secret_key, password).unwrap();

        let dir = std::env::temp_dir().join("burst-keystore-test");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test-keystore.json");

        save_keystore(&keystore, &path).unwrap();
        let loaded = load_keystore(&path).unwrap();
        let decrypted = decrypt_keystore(&loaded, password).unwrap();

        assert_eq!(decrypted, secret_key);

        // Cleanup
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn different_passwords_produce_different_ciphertext() {
        let secret_key = [7u8; 32];
        let ks1 = encrypt_keystore(&secret_key, "password1").unwrap();
        let ks2 = encrypt_keystore(&secret_key, "password2").unwrap();
        // Different salts ensure different ciphertexts even with same key
        assert_ne!(ks1.crypto.ciphertext, ks2.crypto.ciphertext);
    }

    #[test]
    fn load_nonexistent_file_fails() {
        let result = load_keystore(Path::new("/tmp/nonexistent-burst-keystore.json"));
        assert!(result.is_err());
    }

    #[test]
    fn unsupported_version_rejected() {
        let mut keystore = encrypt_keystore(&[0u8; 32], "pass").unwrap();
        keystore.version = 99;
        let result = decrypt_keystore(&keystore, "pass");
        assert!(result.is_err());
    }
}
