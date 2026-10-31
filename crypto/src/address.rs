//! Wallet address derivation from public keys.
//!
//! Address format: `brst_` + base32(public_key, 52 chars) + base32(checksum, 8 chars)
//!
//! Checksum: first 5 bytes of Blake2b-256(public_key).
//! Base32 alphabet: `13456789abcdefghijkmnopqrstuwxyz`.
//! Total address length: 5 (prefix) + 52 + 8 = 65 characters.

use burst_types::{PublicKey, WalletAddress};

/// Alphabet used by the address encoding.
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

/// Decode the first N bytes without allocating. Reject invalid characters or
/// insufficient input; the caller must check exact length and unused padding.
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
/// Uses the encoding and checksum described in the module documentation.
pub fn derive_address(public_key: &PublicKey) -> WalletAddress {
    let pubkey_encoded = encode_base32(public_key.as_bytes());
    let hash = crate::blake2b_256(public_key.as_bytes());
    let checksum_encoded = encode_base32(&hash[..5]);
    let address = format!("{}{}{}", PREFIX, pubkey_encoded, checksum_encoded);
    WalletAddress::new(address)
}

/// Extract the public key bytes from a valid BURST address.
///
/// Rejects malformed encoding, nonzero padding and invalid checksums.
/// This checks address encoding, not whether the decoded key can verify signatures.
pub fn decode_address(address: &str) -> Option<[u8; 32]> {
    if !address.is_ascii() || !address.starts_with(PREFIX) {
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

    // The final public-key character carries one data bit and four zero padding
    // bits. Reject aliases: roster and vote deduplication use the address string.
    if BASE32_DECODE[pubkey_encoded.as_bytes()[PUBKEY_CHARS - 1] as usize] & 0x0F != 0 {
        return None;
    }

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
    fn address_decode_matches_canonical_reencoding_for_every_ascii_mutation() {
        fn reference(address: &str) -> Option<[u8; 32]> {
            if !address.is_ascii()
                || !address.starts_with(PREFIX)
                || address.len() != PREFIX.len() + ENCODED_LEN
            {
                return None;
            }
            let key =
                decode_base32_fixed::<32>(&address[PREFIX.len()..PREFIX.len() + PUBKEY_CHARS])?;
            (derive_address(&PublicKey(key)).as_str() == address).then_some(key)
        }
        for seed in 0u8..16 {
            let key = PublicKey(std::array::from_fn(|i| {
                seed.wrapping_mul(17).wrapping_add(i as u8)
            }));
            let address = derive_address(&key);
            assert_eq!(decode_address(address.as_str()), Some(key.0));
            for position in 0..address.as_str().len() {
                for byte in 0..=127 {
                    let mut bytes = address.as_str().as_bytes().to_vec();
                    bytes[position] = byte;
                    let mutated = String::from_utf8(bytes).unwrap();
                    assert_eq!(
                        decode_address(&mutated),
                        reference(&mutated),
                        "seed={seed} position={position} byte={byte}"
                    );
                }
                let mut unicode = address.as_str().to_owned();
                unicode.replace_range(position..position + 1, "é");
                assert_eq!(decode_address(&unicode), None);
            }
        }
    }

    #[test]
    fn nonzero_padding_cannot_alias_a_wallet_identity() {
        let kp = crate::keys::keypair_from_seed(&[7; 32]);
        let canonical = derive_address(&kp.public);
        let last = PREFIX.len() + PUBKEY_CHARS - 1;
        let canonical_index = BASE32_DECODE[canonical.as_str().as_bytes()[last] as usize];
        assert_eq!(canonical_index & 15, 0);
        for padding in 1..16 {
            let mut alias = canonical.as_str().as_bytes().to_vec();
            alias[last] = BASE32_ALPHABET[(canonical_index | padding) as usize];
            let alias = String::from_utf8(alias).unwrap();
            assert_eq!(decode_address(&alias), None);
            assert!(!validate_address(&alias));
        }
        assert_eq!(decode_address(canonical.as_str()), Some(kp.public.0));
    }

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
