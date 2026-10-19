use super::{DrandScheme, VrfError};
use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
};

const CAPACITY: usize = 256;

#[derive(PartialEq, Eq)]
struct VerifiedSignature {
    scheme: DrandScheme,
    public_key: Vec<u8>,
    signature: Vec<u8>,
    message: Vec<u8>,
}

// Process-local memoization of a pure cryptographic check, never persisted
// authority. Every input to BLS verification is part of the exact-match key.
static VERIFIED: OnceLock<Mutex<VecDeque<VerifiedSignature>>> = OnceLock::new();

pub(super) fn verify(
    scheme: DrandScheme,
    public_key: &[u8],
    signature: &[u8],
    message: &[u8],
    check: impl FnOnce() -> Result<bool, VrfError>,
) -> Result<bool, VrfError> {
    // Both supported schemes hash the beacon message to 32 bytes. Unexpected
    // shapes still go through the verifier but cannot occupy cache memory.
    let expected = match scheme {
        DrandScheme::Chained => (48, 96),
        DrandScheme::Unchained => (96, 48),
    };
    if (public_key.len(), signature.len()) != expected || message.len() != 32 {
        return check();
    }
    let key = VerifiedSignature {
        scheme,
        public_key: public_key.to_vec(),
        signature: signature.to_vec(),
        message: message.to_vec(),
    };
    verify_cached(VERIFIED.get_or_init(Mutex::default), key, check)
}

fn verify_cached(
    cache: &Mutex<VecDeque<VerifiedSignature>>,
    key: VerifiedSignature,
    check: impl FnOnce() -> Result<bool, VrfError>,
) -> Result<bool, VrfError> {
    if cache.lock().is_ok_and(|entries| entries.contains(&key)) {
        return Ok(true);
    }
    // Do not serialize expensive checks behind the cache lock. Concurrent
    // misses may repeat verification, but cannot publish an unchecked result.
    let valid = check()?;
    if valid {
        if let Ok(mut entries) = cache.lock() {
            if !entries.contains(&key) {
                if entries.len() == CAPACITY {
                    entries.pop_front();
                }
                entries.push_back(key);
            }
        }
    }
    Ok(valid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(number: u64) -> VerifiedSignature {
        VerifiedSignature {
            scheme: DrandScheme::Chained,
            public_key: vec![1; 48],
            signature: vec![2; 96],
            message: number.to_be_bytes().to_vec(),
        }
    }

    #[test]
    fn only_successes_are_reused_and_eviction_rechecks() {
        let cache = Mutex::default();
        assert!(!verify_cached(&cache, key(0), || Ok(false)).unwrap());
        assert!(verify_cached(&cache, key(0), || Err(VrfError::InvalidProof(
            "test".into()
        )))
        .is_err());
        assert!(cache.lock().unwrap().is_empty());
        assert!(verify_cached(&cache, key(0), || Ok(true)).unwrap());
        assert!(verify_cached(&cache, key(0), || panic!(
            "exact successful check was repeated"
        ))
        .unwrap());
        for number in 1..=CAPACITY as u64 {
            assert!(verify_cached(&cache, key(number), || Ok(true)).unwrap());
        }
        assert_eq!(cache.lock().unwrap().len(), CAPACITY);
        assert!(!verify_cached(&cache, key(0), || Ok(false)).unwrap());
    }

    #[test]
    fn every_cryptographic_input_is_bound() {
        let cache = Mutex::default();
        assert!(verify_cached(&cache, key(0), || Ok(true)).unwrap());
        for field in 0..4 {
            let mut changed = key(0);
            match field {
                0 => changed.scheme = DrandScheme::Unchained,
                1 => changed.public_key[0] ^= 1,
                2 => changed.signature[0] ^= 1,
                _ => changed.message[0] ^= 1,
            }
            assert!(!verify_cached(&cache, changed, || Ok(false)).unwrap());
        }
    }
}
