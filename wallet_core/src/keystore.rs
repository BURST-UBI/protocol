//! Argon2id encrypted keystore for Ed25519 private keys.
//!
//! Encrypts a 32-byte Ed25519 secret key with a user-chosen password:
//! 1. Argon2id derives a 32-byte encryption key from the password + random salt
//! 2. AES-256-GCM encrypts the secret key with a random nonce
//! 3. The result can be serialized to JSON with the salt, nonce and fixed profile.
//!
//! A keystore contains only the secret key, not signing journals or device state.
//! Version 1 decryption accepts only the fixed profile declared below.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use rand::RngCore;
use serde::{Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;

use crate::error::WalletError;

/// Version 1 uses 64 MiB of memory, three iterations and one lane.
const V1_ARGON2_MEMORY_KIB: u32 = 65536; // 64 MiB
const V1_ARGON2_ITERATIONS: u32 = 3;
const V1_ARGON2_PARALLELISM: u32 = 1;
const V1_ARGON2_OUTPUT_LEN: usize = 32;

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
    #[serde(rename = "memory")]
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

/// Encrypt a 32-byte Ed25519 secret key with a password using Argon2id + AES-256-GCM.
pub fn encrypt_keystore(
    secret_key: &[u8; 32],
    password: &str,
) -> Result<KeystoreFile, WalletError> {
    let mut rng = rand::thread_rng();

    let mut salt = [0u8; SALT_LEN];
    rng.fill_bytes(&mut salt);

    let mut nonce_bytes = [0u8; NONCE_LEN];
    rng.fill_bytes(&mut nonce_bytes);

    let derived_key = derive_key(password, &salt)?;

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
                memory_kib: V1_ARGON2_MEMORY_KIB,
                iterations: V1_ARGON2_ITERATIONS,
                parallelism: V1_ARGON2_PARALLELISM,
            },
            salt: hex_encode(&salt),
            nonce: hex_encode(&nonce_bytes),
            ciphertext: hex_encode(&ciphertext),
        },
    })
}

/// Validate the version-1 profile and authenticate/decrypt the encrypted key.
/// Returns raw secret bytes; the caller controls their lifetime and disposal.
pub fn decrypt_keystore(keystore: &KeystoreFile, password: &str) -> Result<[u8; 32], WalletError> {
    if keystore.version != 1 {
        return Err(WalletError::Key(format!(
            "unsupported keystore version: {}",
            keystore.version
        )));
    }

    let crypto = &keystore.crypto;
    if crypto.cipher != "aes-256-gcm" || crypto.kdf != "argon2id"
        || crypto.kdf_params.memory_kib != V1_ARGON2_MEMORY_KIB
        || crypto.kdf_params.iterations != V1_ARGON2_ITERATIONS
        || crypto.kdf_params.parallelism != V1_ARGON2_PARALLELISM
    {
        return Err(WalletError::Key("unsupported version 1 keystore profile".into()));
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

    let derived_key = derive_key(password, &salt)?;

    let cipher = Aes256Gcm::new_from_slice(&derived_key)
        .map_err(|e| WalletError::Key(format!("AES key init failed: {}", e)))?;

    let nonce = Nonce::from_slice(&nonce_bytes);
    let plaintext = cipher.decrypt(nonce, ciphertext.as_ref()).map_err(|_| {
        WalletError::Key("decryption failed: wrong password or corrupted data".to_string())
    })?;

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

/// Create a new JSON file, refusing to overwrite an existing path. On Unix the
/// file is created with owner-only permissions. Syncs the file, not its parent
/// directory; a write failure can leave a partial file at the requested path.
#[cfg(not(target_arch = "wasm32"))]
pub fn save_keystore(keystore: &KeystoreFile, path: &Path) -> Result<(), WalletError> {
    use std::io::Write;
    let json = serde_json::to_string_pretty(keystore)
        .map_err(|e| WalletError::Other(format!("JSON serialization failed: {}", e)))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|e| {
        WalletError::Other(format!(
            "cannot create keystore; choose a new file path: {e}"
        ))
    })?;
    file.write_all(json.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|e| WalletError::Other(format!("failed to write keystore file: {e}")))?;
    Ok(())
}

/// Read and parse the entire JSON file. This does not validate its encryption
/// profile or authenticate its ciphertext; call `decrypt_keystore` for those checks.
/// The caller must bound input size before loading untrusted backups.
#[cfg(not(target_arch = "wasm32"))]
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
        V1_ARGON2_MEMORY_KIB,
        V1_ARGON2_ITERATIONS,
        V1_ARGON2_PARALLELISM,
        Some(V1_ARGON2_OUTPUT_LEN),
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
    if !s.is_ascii() || !s.len().is_multiple_of(2) {
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

    /// Characterizes unbounded JSON input without invoking the password KDF.
    #[test]
    fn audit_backup_import_accepts_large_ignored_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("untrusted-backup.json");
        let document = serde_json::json!({
            "version": 1,
            "crypto": {
                "cipher": "aes-256-gcm", "kdf": "argon2id",
                "kdf_params": {"memory": 65536, "iterations": 3, "parallelism": 1},
                "salt": "00".repeat(32), "nonce": "00".repeat(12),
                "ciphertext": "00".repeat(48)
            },
            "untrusted_padding": "x".repeat(1024 * 1024)
        });
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > 1024 * 1024);
        assert_eq!(load_keystore(&path).unwrap().version, 1);
        println!("FA01 reproduced: >1 MiB ignored metadata accepted by backup import; no KDF run");
    }

    #[test]
    fn unsupported_profiles_are_rejected_before_decoding_or_derivation() {
        let base = KeystoreFile { version: 1, crypto: KeystoreCrypto {
            cipher: "aes-256-gcm".into(), kdf: "argon2id".into(),
            kdf_params: KdfParams { memory_kib: V1_ARGON2_MEMORY_KIB, iterations: V1_ARGON2_ITERATIONS, parallelism: V1_ARGON2_PARALLELISM },
            salt: "invalid".into(), nonce: "invalid".into(), ciphertext: "invalid".into(),
        }};
        for field in 0..5 {
            let mut invalid = base.clone();
            match field {
                0 => invalid.crypto.cipher = "aes-128-gcm".into(),
                1 => invalid.crypto.kdf = "argon2i".into(),
                2 => invalid.crypto.kdf_params.memory_kib = u32::MAX,
                3 => invalid.crypto.kdf_params.iterations = 1,
                _ => invalid.crypto.kdf_params.parallelism = 2,
            }
            assert!(decrypt_keystore(&invalid, "test").unwrap_err().to_string().contains("unsupported version 1 keystore profile"));
        }
    }

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
        assert_eq!(keystore.crypto.kdf_params.memory_kib, 65536);
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
        assert!(
            save_keystore(&keystore, &path).is_err(),
            "must not overwrite an existing backup"
        );
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
