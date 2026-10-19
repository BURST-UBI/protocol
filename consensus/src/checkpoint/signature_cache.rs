use burst_types::{PublicKey, Signature};
use std::{
    collections::{HashSet, VecDeque},
    sync::{Mutex, OnceLock},
};

const CAPACITY: usize = 4096;
const MAX_PAYLOAD: usize = 1024;

#[derive(Default)]
struct VerifiedSignatures {
    entries: HashSet<[u8; 32]>,
    order: VecDeque<[u8; 32]>,
}

impl VerifiedSignatures {
    fn insert(&mut self, key: [u8; 32]) {
        if self.entries.contains(&key) {
            return;
        }
        if self.order.len() == CAPACITY {
            self.entries
                .remove(&self.order.pop_front().expect("full cache"));
        }
        self.entries.insert(key);
        self.order.push_back(key);
    }
}

fn key(payload: &[u8], signature: &Signature, public_key: &PublicKey) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(130 + payload.len());
    bytes.extend_from_slice(b"BURST/checkpoint-signature-cache/v1\0");
    bytes.extend_from_slice(&public_key.0);
    bytes.extend_from_slice(&signature.0);
    bytes.extend_from_slice(payload);
    burst_crypto::blake2b_256(&bytes)
}

/// Cache only strict Ed25519 success, never roster membership or quorum.
/// A miss, oversized payload or unavailable cache always runs strict verification.
pub(super) fn verify(payload: &[u8], signature: &Signature, public_key: &PublicKey) -> bool {
    if payload.len() > MAX_PAYLOAD {
        return burst_crypto::verify_signature(payload, signature, public_key);
    }
    static CACHE: OnceLock<Mutex<VerifiedSignatures>> = OnceLock::new();
    let cache = CACHE.get_or_init(Mutex::default);
    let key = key(payload, signature, public_key);
    if cache.lock().is_ok_and(|cache| cache.entries.contains(&key)) {
        return true;
    }
    if !burst_crypto::verify_signature(payload, signature, public_key) {
        return false;
    }
    if let Ok(mut cache) = cache.lock() {
        cache.insert(key);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_success_binds_message_key_and_signature_and_keeps_strict_rules() {
        let signer = burst_crypto::keypair_from_seed(&[31; 32]);
        let other = burst_crypto::keypair_from_seed(&[32; 32]);
        let payload = b"checkpoint precommit";
        let signature = burst_crypto::sign_message(payload, &signer.private);
        for _ in 0..2 {
            assert!(verify(payload, &signature, &signer.public));
            assert!(!verify(b"checkpoint prevote", &signature, &signer.public));
            assert!(!verify(payload, &signature, &other.public));
            let mut changed = signature.clone();
            changed.0[0] ^= 1;
            assert!(!verify(payload, &changed, &signer.public));
        }
        let oversized = vec![7; MAX_PAYLOAD + 1];
        let signature = burst_crypto::sign_message(&oversized, &signer.private);
        assert!(verify(&oversized, &signature, &signer.public));
        assert!(!verify(&oversized, &signature, &other.public));
        let mut identity = [0; 32];
        identity[0] = 1;
        let mut forged = [0; 64];
        forged[..32].copy_from_slice(&identity);
        assert!(!verify(payload, &Signature(forged), &PublicKey(identity)));
    }

    #[test]
    fn successful_signature_cache_evicts_without_growing_or_duplicating_entries() {
        let mut cache = VerifiedSignatures::default();
        for n in 0..=CAPACITY {
            let key = burst_crypto::blake2b_256(&n.to_be_bytes());
            cache.insert(key);
            cache.insert(key);
            assert!(cache.entries.len() <= CAPACITY);
            assert_eq!(cache.entries.len(), cache.order.len());
        }
        assert!(!cache
            .entries
            .contains(&burst_crypto::blake2b_256(&0usize.to_be_bytes())));
        assert!(cache
            .entries
            .contains(&burst_crypto::blake2b_256(&CAPACITY.to_be_bytes())));
    }
}
