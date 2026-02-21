//! Wallet address derivation from public keys.
//!
//! Address format: `brst_` + base32(public_key, 52 chars) + base32(checksum, 8 chars)
//!
//! Checksum: first 5 bytes of Blake2b-256(public_key).
//! Base32 alphabet: `13456789abcdefghijkmnopqrstuwxyz` (Nano-style, avoids ambiguous chars).
//! Total address length: 5 (prefix) + 52 + 8 = 65 characters.

use burst_types::{PublicKey, WalletAddress};

/// Base32 alphabet (32 chars, avoids visually ambiguous 0/O, 2/Z, l/I, v).
const BASE32_ALPHABET: &[u8; 32] = b"13456789abcdefghijkmnopqrstuwxyz";

/// Reverse lookup table: ASCII byte → 5-bit value (0xFF = invalid).
const BASE32_DECODE: [u8; 128] = {
    let mut table = [0xFFu8; 128];
    let alpha = BASE32_ALPHABET;
    let mut i = 0;
    while i < 32 {
        table[alpha[i] as usize] = i as u8;
        i += 1;
    }
    table
};

/// Expected length of the encoded part (after `brst_`): 52 pubkey + 8 checksum.
const ENCODED_LEN: usize = 60;
/// Prefix for all BURST addresses.
const PREFIX: &str = "brst_";
/// Number of base32 characters for the public key (256 bits → ceil(256/5) = 52).
const PUBKEY_CHARS: usize = 52;
/// Number of base32 characters for the checksum (40 bits → 40/5 = 8).
const _CHECKSUM_CHARS: usize = 8;

/// Encode a byte slice as base32 using the BURST alphabet.
fn encode_base32(bytes: &[u8]) -> String {
    let total_bits = bytes.len() * 8;
    let num_chars = total_bits.div_ceil(5);
    let mut result = String::with_capacity(num_chars);

    let mut buffer: u64 = 0;
    let mut bits_in_buffer = 0;

    for &byte in bytes {
        buffer = (buffer << 8) | byte as u64;
        bits_in_buffer += 8;
        while bits_in_buffer >= 5 {
            bits_in_buffer -= 5;
            let idx = ((buffer >> bits_in_buffer) & 0x1F) as usize;
            result.push(BASE32_ALPHABET[idx] as char);
        }
    }
    // Remaining bits (padded with zeros on the right).
    if bits_in_buffer > 0 {
        let idx = ((buffer << (5 - bits_in_buffer)) & 0x1F) as usize;
        result.push(BASE32_ALPHABET[idx] as char);
    }

    result
}

/// Decode a base32 string into a fixed-size byte array. Returns `None` on
/// invalid characters or wrong length. Zero-allocation.
fn decode_base32_fixed<const N: usize>(s: &str) -> Option<[u8; N]> {
    let mut buffer: u64 = 0;
    let mut bits_in_buffer = 0;
    let mut result = [0u8; N];
    let mut pos = 0;

    for c in s.bytes() {
        if c >= 128 {
            return None;
        }
        let val = BASE32_DECODE[c as usize];
        if val == 0xFF {
            return None;
        }
        buffer = (buffer << 5) | val as u64;
        bits_in_buffer += 5;
        if bits_in_buffer >= 8 {
            bits_in_buffer -= 8;
            if pos < N {
                result[pos] = (buffer >> bits_in_buffer) as u8;
                pos += 1;
            }
        }
    }

    if pos < N {
        return None;
    }
    Some(result)
}

/// Derive a `brst_`-prefixed wallet address from a public key.
///
/// Process:
/// 1. Compute checksum = Blake2b-256(public_key)[0..5]
/// 2. Encode public_key as 52 base32 characters
/// 3. Encode checksum as 8 base32 characters
/// 4. Address = "brst_" + encoded_pubkey + encoded_checksum
pub fn derive_address(public_key: &PublicKey) -> WalletAddress {
    let pubkey_encoded = encode_base32(public_key.as_bytes());
    let hash = crate::blake2b_256(public_key.as_bytes());
    let checksum_encoded = encode_base32(&hash[..5]);
    let address = format!("{}{}{}", PREFIX, pubkey_encoded, checksum_encoded);
    WalletAddress::new(address)
}

/// Extract the public key bytes from a valid BURST address.
///
/// Returns `None` if the address is malformed or has an invalid checksum.
pub fn decode_address(address: &str) -> Option<[u8; 32]> {
    if !address.starts_with(PREFIX) {
        return None;
    }
    let encoded = &address[PREFIX.len()..];
    if encoded.len() != ENCODED_LEN {
        return None;
    }

    let pubkey_encoded = &encoded[..PUBKEY_CHARS];
    let checksum_encoded = &encoded[PUBKEY_CHARS..];

    let pubkey_bytes: [u8; 32] = decode_base32_fixed(pubkey_encoded)?;
    let checksum_bytes: [u8; 5] = decode_base32_fixed(checksum_encoded)?;

    let expected_checksum = &crate::blake2b_256(&pubkey_bytes)[..5];
    if checksum_bytes != *expected_checksum {
        return None;
    }

    Some(pubkey_bytes)
}

/// Validate that an address string is well-formed and its checksum is correct.
pub fn validate_address(address: &str) -> bool {
    decode_address(address).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::generate_keypair;

    #[test]
    fn derive_and_validate() {
        let kp = generate_keypair();
        let addr = derive_address(&kp.public);
        assert!(addr.as_str().starts_with("brst_"));
        assert_eq!(addr.as_str().len(), 65);
        assert!(validate_address(addr.as_str()));
    }

    #[test]
    fn derive_is_deterministic() {
        let kp = crate::keys::keypair_from_seed(&[7u8; 32]);
        let a1 = derive_address(&kp.public);
        let a2 = derive_address(&kp.public);
        assert_eq!(a1.as_str(), a2.as_str());
    }

    #[test]
    fn decode_roundtrip() {
        let kp = generate_keypair();
        let addr = derive_address(&kp.public);
        let decoded = decode_address(addr.as_str()).unwrap();
        assert_eq!(decoded, *kp.public.as_bytes());
    }

    #[test]
    fn invalid_prefix_rejected() {
        assert!(!validate_address(
            "nano_1234567890abcdefghijkmnopqrstuwxyz1234567890abcdefghijk"
        ));
    }

    #[test]
    fn invalid_checksum_rejected() {
        let kp = generate_keypair();
        let addr = derive_address(&kp.public);
        let mut bad = addr.as_str().to_string();
        let last = bad.pop().unwrap();
        let replacement = if last == '1' { '3' } else { '1' };
        bad.push(replacement);
        assert!(!validate_address(&bad));
    }

    #[test]
    fn wrong_length_rejected() {
        assert!(!validate_address("brst_tooshort"));
        assert!(!validate_address("brst_"));
    }

    #[test]
    fn base32_encode_decode_roundtrip() {
        let data = [0xDE, 0xAD, 0xBE, 0xEF, 0x42];
        let encoded = encode_base32(&data);
        let decoded: [u8; 5] = decode_base32_fixed(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn different_keys_different_addresses() {
        let k1 = generate_keypair();
        let k2 = generate_keypair();
        assert_ne!(
            derive_address(&k1.public).as_str(),
            derive_address(&k2.public).as_str()
        );
    }
}
