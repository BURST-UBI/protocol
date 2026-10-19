//! Verifier selection using VRF randomness.

use burst_types::WalletAddress;
use burst_vrf::VrfProvider;

/// Selects random verifiers from the pool of opted-in verified wallets.
pub struct VerifierSelector;

impl VerifierSelector {
    /// Select `count` random verifiers using VRF-derived randomness.
    ///
    /// The selection is deterministic given the same seed — any node can
    /// independently verify which verifiers were selected.
    ///
    /// Algorithm: for each eligible verifier, compute `Hash(seed || address)` to get
    /// a selection score, then pick the `count` verifiers with the lowest scores.
    pub fn select(
        &self,
        vrf: &dyn VrfProvider,
        eligible_verifiers: &[WalletAddress],
        seed_context: &[u8],
        count: usize,
    ) -> Vec<WalletAddress> {
        if eligible_verifiers.is_empty() || count == 0 {
            return Vec::new();
        }

        let seed = match vrf.get_randomness(seed_context) {
            Ok(output) if matches!(vrf.verify(seed_context, &output), Ok(true)) => output.value,
            Ok(_) => return Vec::new(),
            Err(_) => return Vec::new(),
        };

        let mut eligible_verifiers = eligible_verifiers.to_vec();
        eligible_verifiers.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        eligible_verifiers.dedup();
        if eligible_verifiers.len() < count {
            return Vec::new();
        }
        let mut scored: Vec<(usize, [u8; 32])> = eligible_verifiers
            .iter()
            .enumerate()
            .map(|(i, addr)| {
                let mut data = Vec::new();
                data.extend_from_slice(&seed);
                data.extend_from_slice(addr.as_str().as_bytes());
                let hash = burst_crypto::blake2b_256(&data);
                (i, hash)
            })
            .collect();

        // Use partial sort O(n) instead of full sort O(n log n) —
        // we only need the top `count` lowest hashes, not full ordering.
        let actual_count = count;
        if actual_count > 0 && actual_count < scored.len() {
            scored.select_nth_unstable_by_key(actual_count - 1, |a| a.1);
            scored.truncate(actual_count);
            // Sort the selected few for deterministic ordering
            scored.sort_by_key(|a| a.1);
        } else {
            scored.sort_by_key(|a| a.1);
            scored.truncate(actual_count);
        }
        scored
            .iter()
            .map(|(i, _)| eligible_verifiers[*i].clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_vrf::{RandomOutput, VrfError, VrfProvider};

    fn addr(s: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{s}"))
    }

    #[test]
    fn duplicate_addresses_cannot_fill_multiple_verifier_slots() {
        let vrf = FixedVrf { seed: [1; 32] };
        let unique = [addr("alice"), addr("bob")];
        let repeated = [addr("bob"), addr("alice"), addr("alice"), addr("bob")];
        assert_eq!(
            VerifierSelector.select(&vrf, &unique, b"request", 2),
            VerifierSelector.select(&vrf, &repeated, b"request", 2)
        );
        assert!(VerifierSelector
            .select(&vrf, &repeated, b"request", 3)
            .is_empty());
    }

    #[test]
    fn unverified_randomness_never_selects_verifiers() {
        struct InvalidProof;
        impl VrfProvider for InvalidProof {
            fn get_randomness(&self, _: &[u8]) -> Result<RandomOutput, VrfError> {
                Ok(RandomOutput {
                    value: [9; 32],
                    proof: vec![],
                    round: 1,
                })
            }
            fn verify(&self, _: &[u8], _: &RandomOutput) -> Result<bool, VrfError> {
                Ok(false)
            }
            fn name(&self) -> &str {
                "invalid-proof"
            }
        }
        assert!(VerifierSelector
            .select(&InvalidProof, &[addr("alice")], b"request", 1)
            .is_empty());
    }

    struct FixedVrf {
        seed: [u8; 32],
    }

    impl VrfProvider for FixedVrf {
        fn get_randomness(&self, _context: &[u8]) -> Result<RandomOutput, VrfError> {
            Ok(RandomOutput {
                value: self.seed,
                proof: vec![],
                round: 0,
            })
        }
        fn verify(&self, _context: &[u8], _output: &RandomOutput) -> Result<bool, VrfError> {
            Ok(true)
        }
        fn name(&self) -> &str {
            "fixed-test-vrf"
        }
    }

    struct FailingVrf;
    impl VrfProvider for FailingVrf {
        fn get_randomness(&self, _context: &[u8]) -> Result<RandomOutput, VrfError> {
            Err(VrfError::Unavailable("test".into()))
        }
        fn verify(&self, _context: &[u8], _output: &RandomOutput) -> Result<bool, VrfError> {
            Ok(false)
        }
        fn name(&self) -> &str {
            "failing"
        }
    }

    #[test]
    fn selection_is_deterministic() {
        let vrf = FixedVrf { seed: [42u8; 32] };
        let pool: Vec<WalletAddress> = (0..10).map(|i| addr(&format!("v{i}"))).collect();
        let selector = VerifierSelector;

        let r1 = selector.select(&vrf, &pool, b"ctx", 3);
        let r2 = selector.select(&vrf, &pool, b"ctx", 3);
        assert_eq!(r1, r2, "same seed + same pool must produce same selection");
    }

    #[test]
    fn selection_respects_count() {
        let vrf = FixedVrf { seed: [1u8; 32] };
        let pool: Vec<WalletAddress> = (0..20).map(|i| addr(&format!("v{i}"))).collect();
        let selector = VerifierSelector;

        let selected = selector.select(&vrf, &pool, b"ctx", 5);
        assert_eq!(selected.len(), 5);
    }

    #[test]
    fn selection_count_larger_than_pool_does_not_shrink_committee() {
        let vrf = FixedVrf { seed: [2u8; 32] };
        let pool = vec![addr("v0"), addr("v1"), addr("v2")];
        let selector = VerifierSelector;

        let selected = selector.select(&vrf, &pool, b"ctx", 10);
        assert!(selected.is_empty());
    }

    #[test]
    fn empty_pool_returns_empty() {
        let vrf = FixedVrf { seed: [0u8; 32] };
        let selector = VerifierSelector;
        let selected = selector.select(&vrf, &[], b"ctx", 5);
        assert!(selected.is_empty());
    }

    #[test]
    fn zero_count_returns_empty() {
        let vrf = FixedVrf { seed: [0u8; 32] };
        let pool = vec![addr("v0")];
        let selector = VerifierSelector;
        let selected = selector.select(&vrf, &pool, b"ctx", 0);
        assert!(selected.is_empty());
    }

    #[test]
    fn vrf_failure_returns_empty() {
        let vrf = FailingVrf;
        let pool = vec![addr("v0"), addr("v1")];
        let selector = VerifierSelector;
        let selected = selector.select(&vrf, &pool, b"ctx", 2);
        assert!(selected.is_empty());
    }

    #[test]
    fn different_seeds_produce_different_selections() {
        let vrf1 = FixedVrf { seed: [10u8; 32] };
        let vrf2 = FixedVrf { seed: [20u8; 32] };
        let pool: Vec<WalletAddress> = (0..50).map(|i| addr(&format!("v{i}"))).collect();
        let selector = VerifierSelector;

        let r1 = selector.select(&vrf1, &pool, b"ctx", 5);
        let r2 = selector.select(&vrf2, &pool, b"ctx", 5);
        assert_ne!(
            r1, r2,
            "different seeds should generally produce different selections"
        );
    }
}
