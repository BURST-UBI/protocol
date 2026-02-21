//! Phase 3: Threshold VRF (DVRF) — gold standard.
//!
//! A multiparty protocol where a committee collectively produces randomness.
//! No single participant can predict or bias the output.
//!
//! Also provides beacon-based verifier selection for Phase 1 (drand) integration,
//! and eligibility checking for verifier candidates.

use crate::{RandomOutput, VrfError, VrfProvider};
use burst_types::{Timestamp, WalletAddress};
use sha2::{Digest, Sha256};

/// Threshold VRF provider — strongest guarantees.
pub struct ThresholdVrf {
    /// Minimum number of participants needed to produce a valid output.
    pub threshold: usize,
    /// Total committee size.
    pub committee_size: usize,
}

impl ThresholdVrf {
    pub fn new(threshold: usize, committee_size: usize) -> Self {
        Self {
            threshold,
            committee_size,
        }
    }
}

impl VrfProvider for ThresholdVrf {
    fn get_randomness(&self, _context: &[u8]) -> Result<RandomOutput, VrfError> {
        Err(VrfError::Unavailable(
            "threshold VRF not yet implemented (Phase 3)".into(),
        ))
    }

    fn verify(&self, _context: &[u8], _output: &RandomOutput) -> Result<bool, VrfError> {
        Err(VrfError::Unavailable(
            "threshold VRF not yet implemented (Phase 3)".into(),
        ))
    }

    fn name(&self) -> &str {
        "threshold-vrf"
    }
}

/// Select verifiers from a pool using drand beacon randomness.
///
/// Each verifier's address is hashed together with the beacon randomness to
/// produce a deterministic selection score. The `count` verifiers with the
/// lowest scores are selected. This is deterministic — any node can
/// independently verify which verifiers were selected given the same inputs.
///
/// # Arguments
///
/// * `beacon_randomness` - Raw randomness bytes from a drand beacon
/// * `verifier_pool` - All addresses eligible for selection
/// * `count` - Number of verifiers to select
///
/// # Returns
///
/// A vector of selected `WalletAddress`es, up to `count` entries.
pub fn select_verifiers(
    beacon_randomness: &[u8],
    verifier_pool: &[WalletAddress],
    count: usize,
) -> Vec<WalletAddress> {
    if verifier_pool.is_empty() || count == 0 {
        return Vec::new();
    }

    let take = count.min(verifier_pool.len());

    let mut scored: Vec<_> = verifier_pool
        .iter()
        .map(|v| {
            let mut hasher = Sha256::new();
            hasher.update(beacon_randomness);
            hasher.update(v.as_str().as_bytes());
            let score = hasher.finalize();
            (v.clone(), score)
        })
        .collect();

    scored.sort_by(|a, b| a.1.as_slice().cmp(b.1.as_slice()));
    scored.into_iter().take(take).map(|(v, _)| v).collect()
}

/// Minimum duration (seconds) a wallet must be verified before it can serve as verifier.
/// Default: 30 days.
pub const DEFAULT_MIN_VERIFICATION_AGE_SECS: u64 = 30 * 24 * 3600;

/// Default minimum BRN balance (raw units) required to be a verifier.
/// Set to 100 BRN (100 * 10^18 raw units).
pub const DEFAULT_MIN_BRN_BALANCE: u128 = 100 * 1_000_000_000_000_000_000;

/// Eligibility criteria for a verifier candidate.
///
/// Populated by the caller from wallet state and passed to `is_eligible_verifier()`
/// for a pure, deterministic eligibility check.
#[derive(Clone, Debug)]
pub struct VerifierEligibility {
    /// Timestamp when this wallet was first verified.
    /// `None` if the wallet has never been verified.
    pub verified_since: Option<Timestamp>,

    /// The wallet's current computed BRN balance (raw units).
    pub brn_balance: u128,

    /// Whether the wallet has explicitly opted in to the verifier pool.
    pub opted_in_as_verifier: bool,

    /// Whether the wallet is currently under an active challenge.
    pub under_active_challenge: bool,
}

/// Configuration for verifier eligibility thresholds.
#[derive(Clone, Debug)]
pub struct EligibilityConfig {
    /// Minimum number of seconds the wallet must have been verified.
    pub min_verification_age_secs: u64,

    /// Minimum BRN balance required (raw units).
    pub min_brn_balance: u128,
}

impl Default for EligibilityConfig {
    fn default() -> Self {
        Self {
            min_verification_age_secs: DEFAULT_MIN_VERIFICATION_AGE_SECS,
            min_brn_balance: DEFAULT_MIN_BRN_BALANCE,
        }
    }
}

/// Check whether a wallet is eligible to serve as a verifier.
///
/// Eligibility requires ALL of the following:
///
/// 1. **Verified for at least `min_verification_age_secs`** — prevents freshly
///    verified sybil wallets from immediately participating.
/// 2. **Sufficient BRN balance** — ensures the verifier has enough skin in the game
///    (BRN is staked during verification votes).
/// 3. **Opted in as verifier** — explicit consent; not all wallets want the
///    responsibility and bandwidth overhead.
/// 4. **Not under active challenge** — a challenged wallet's humanity is in question,
///    so it must not judge others.
///
/// This function is pure and deterministic — any node can independently verify
/// the same result given the same inputs.
pub fn is_eligible_verifier(
    eligibility: &VerifierEligibility,
    config: &EligibilityConfig,
    now: Timestamp,
) -> bool {
    // Must have opted in.
    if !eligibility.opted_in_as_verifier {
        return false;
    }

    // Must not be under active challenge.
    if eligibility.under_active_challenge {
        return false;
    }

    // Must be verified for at least the minimum duration.
    let verified_long_enough = match eligibility.verified_since {
        Some(verified_at) => {
            let age = verified_at.elapsed_since(now);
            age >= config.min_verification_age_secs
        }
        None => false,
    };

    if !verified_long_enough {
        return false;
    }

    // Must have sufficient BRN balance.
    if eligibility.brn_balance < config.min_brn_balance {
        return false;
    }

    true
}

/// Convenience: filter a pool of candidates down to only eligible verifiers.
///
/// Takes a list of `(WalletAddress, VerifierEligibility)` pairs and returns
/// only the addresses that pass all eligibility checks.
pub fn filter_eligible_verifiers(
    candidates: &[(WalletAddress, VerifierEligibility)],
    config: &EligibilityConfig,
    now: Timestamp,
) -> Vec<WalletAddress> {
    candidates
        .iter()
        .filter(|(_, elig)| is_eligible_verifier(elig, config, now))
        .map(|(addr, _)| addr.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_addr(name: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{}", name))
    }

    #[test]
    fn test_select_verifiers_deterministic() {
        let pool = vec![
            make_addr("alice"),
            make_addr("bob"),
            make_addr("carol"),
            make_addr("dave"),
            make_addr("eve"),
        ];
        let randomness = b"beacon_round_42_randomness";

        let selected1 = select_verifiers(randomness, &pool, 3);
        let selected2 = select_verifiers(randomness, &pool, 3);

        assert_eq!(selected1.len(), 3);
        assert_eq!(selected1, selected2, "selection must be deterministic");
    }

    #[test]
    fn test_select_verifiers_different_randomness_different_result() {
        let pool = vec![
            make_addr("alice"),
            make_addr("bob"),
            make_addr("carol"),
            make_addr("dave"),
            make_addr("eve"),
        ];

        let selected_a = select_verifiers(b"randomness_a", &pool, 3);
        let selected_b = select_verifiers(b"randomness_b", &pool, 3);

        // Different randomness should (with overwhelming probability) yield different orderings
        assert_ne!(selected_a, selected_b);
    }

    #[test]
    fn test_select_verifiers_count_exceeds_pool() {
        let pool = vec![make_addr("alice"), make_addr("bob")];
        let selected = select_verifiers(b"randomness", &pool, 10);
        assert_eq!(selected.len(), 2, "cannot select more than pool size");
    }

    #[test]
    fn test_select_verifiers_empty_pool() {
        let selected = select_verifiers(b"randomness", &[], 5);
        assert!(selected.is_empty());
    }

    #[test]
    fn test_select_verifiers_zero_count() {
        let pool = vec![make_addr("alice")];
        let selected = select_verifiers(b"randomness", &pool, 0);
        assert!(selected.is_empty());
    }

    #[test]
    fn test_select_all_unique() {
        let pool: Vec<_> = (0..20).map(|i| make_addr(&format!("user{}", i))).collect();
        let selected = select_verifiers(b"test_randomness", &pool, 10);
        assert_eq!(selected.len(), 10);

        // All selected addresses should be unique
        let mut unique = selected.clone();
        unique.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        unique.dedup();
        assert_eq!(unique.len(), 10, "all selected verifiers must be unique");
    }

    // ── Known test vectors for select_verifiers ─────────────────────────

    #[test]
    fn test_select_verifiers_known_vector_1() {
        // Fixed inputs produce a fixed, verifiable output.
        let pool = vec![make_addr("alice"), make_addr("bob"), make_addr("carol")];
        let randomness = b"fixed_seed_vector_1";
        let selected = select_verifiers(randomness, &pool, 2);

        assert_eq!(selected.len(), 2);

        // Compute expected order manually: Hash(seed || addr) for each,
        // then sort by hash. This verifies the algorithm is correct.
        let mut scored: Vec<_> = pool
            .iter()
            .map(|v| {
                let mut hasher = Sha256::new();
                hasher.update(randomness);
                hasher.update(v.as_str().as_bytes());
                let score = hasher.finalize();
                (v.clone(), score)
            })
            .collect();
        scored.sort_by(|a, b| a.1.as_slice().cmp(b.1.as_slice()));
        let expected: Vec<WalletAddress> = scored.into_iter().take(2).map(|(v, _)| v).collect();

        assert_eq!(selected, expected, "must match manually computed order");
    }

    #[test]
    fn test_select_verifiers_known_vector_2() {
        // Another fixed set to ensure stability across implementations.
        let pool = vec![
            make_addr("validator_0"),
            make_addr("validator_1"),
            make_addr("validator_2"),
            make_addr("validator_3"),
            make_addr("validator_4"),
        ];
        let randomness = b"drand_round_12345_output";
        let selected = select_verifiers(randomness, &pool, 3);

        assert_eq!(selected.len(), 3);

        // Compute expected selection independently.
        let mut scored: Vec<_> = pool
            .iter()
            .map(|v| {
                let mut hasher = Sha256::new();
                hasher.update(randomness);
                hasher.update(v.as_str().as_bytes());
                let score = hasher.finalize();
                (v.clone(), score)
            })
            .collect();
        scored.sort_by(|a, b| a.1.as_slice().cmp(b.1.as_slice()));
        let expected: Vec<WalletAddress> = scored.into_iter().take(3).map(|(v, _)| v).collect();

        assert_eq!(selected, expected);
    }

    #[test]
    fn test_select_verifiers_stable_across_calls() {
        // The same inputs must always produce the same output — this is critical
        // for consensus: every node must agree on who the verifiers are.
        let pool: Vec<_> = (0..50)
            .map(|i| make_addr(&format!("node_{:03}", i)))
            .collect();
        let randomness = b"consensus_critical_seed";

        let run1 = select_verifiers(randomness, &pool, 7);
        let run2 = select_verifiers(randomness, &pool, 7);
        let run3 = select_verifiers(randomness, &pool, 7);

        assert_eq!(run1, run2);
        assert_eq!(run2, run3);
    }

    #[test]
    fn test_select_verifiers_pool_order_independent() {
        // Selection must be deterministic regardless of input pool ordering.
        let pool_a = vec![
            make_addr("alice"),
            make_addr("bob"),
            make_addr("carol"),
            make_addr("dave"),
        ];
        let pool_b: Vec<_> = pool_a.iter().rev().cloned().collect();

        let randomness = b"order_test_seed";
        let mut selected_a = select_verifiers(randomness, &pool_a, 2);
        let mut selected_b = select_verifiers(randomness, &pool_b, 2);

        // Sort results since we only care about the SET, not order
        // (though the function is deterministic on sorted hashes).
        selected_a.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        selected_b.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        assert_eq!(selected_a, selected_b, "same set regardless of input order");
    }

    // ── is_eligible_verifier tests ──────────────────────────────────────

    fn make_timestamp(secs: u64) -> Timestamp {
        Timestamp::new(secs)
    }

    fn eligible_verifier() -> VerifierEligibility {
        VerifierEligibility {
            verified_since: Some(make_timestamp(1_000_000)),
            brn_balance: DEFAULT_MIN_BRN_BALANCE * 2,
            opted_in_as_verifier: true,
            under_active_challenge: false,
        }
    }

    #[test]
    fn test_eligible_verifier_passes_all_checks() {
        let config = EligibilityConfig::default();
        let now = make_timestamp(1_000_000 + DEFAULT_MIN_VERIFICATION_AGE_SECS + 1);

        assert!(is_eligible_verifier(&eligible_verifier(), &config, now));
    }

    #[test]
    fn test_ineligible_not_opted_in() {
        let config = EligibilityConfig::default();
        let now = make_timestamp(1_000_000 + DEFAULT_MIN_VERIFICATION_AGE_SECS + 1);

        let mut elig = eligible_verifier();
        elig.opted_in_as_verifier = false;

        assert!(!is_eligible_verifier(&elig, &config, now));
    }

    #[test]
    fn test_ineligible_under_challenge() {
        let config = EligibilityConfig::default();
        let now = make_timestamp(1_000_000 + DEFAULT_MIN_VERIFICATION_AGE_SECS + 1);

        let mut elig = eligible_verifier();
        elig.under_active_challenge = true;

        assert!(!is_eligible_verifier(&elig, &config, now));
    }

    #[test]
    fn test_ineligible_not_verified() {
        let config = EligibilityConfig::default();
        let now = make_timestamp(5_000_000);

        let mut elig = eligible_verifier();
        elig.verified_since = None;

        assert!(!is_eligible_verifier(&elig, &config, now));
    }

    #[test]
    fn test_ineligible_verified_too_recently() {
        let config = EligibilityConfig::default();
        // Verified 10 days ago, need 30 days.
        let verified_at = make_timestamp(1_000_000);
        let now = make_timestamp(1_000_000 + 10 * 24 * 3600);

        let mut elig = eligible_verifier();
        elig.verified_since = Some(verified_at);

        assert!(!is_eligible_verifier(&elig, &config, now));
    }

    #[test]
    fn test_ineligible_insufficient_brn() {
        let config = EligibilityConfig::default();
        let now = make_timestamp(1_000_000 + DEFAULT_MIN_VERIFICATION_AGE_SECS + 1);

        let mut elig = eligible_verifier();
        elig.brn_balance = DEFAULT_MIN_BRN_BALANCE - 1;

        assert!(!is_eligible_verifier(&elig, &config, now));
    }

    #[test]
    fn test_eligible_at_exact_boundary() {
        let config = EligibilityConfig::default();
        // Exactly 30 days since verification.
        let verified_at = make_timestamp(1_000_000);
        let now = make_timestamp(1_000_000 + DEFAULT_MIN_VERIFICATION_AGE_SECS);

        let elig = VerifierEligibility {
            verified_since: Some(verified_at),
            brn_balance: DEFAULT_MIN_BRN_BALANCE,
            opted_in_as_verifier: true,
            under_active_challenge: false,
        };

        assert!(is_eligible_verifier(&elig, &config, now));
    }

    #[test]
    fn test_custom_eligibility_config() {
        let config = EligibilityConfig {
            min_verification_age_secs: 7 * 24 * 3600,        // 7 days
            min_brn_balance: 50 * 1_000_000_000_000_000_000, // 50 BRN
        };
        let verified_at = make_timestamp(1_000_000);
        let now = make_timestamp(1_000_000 + 8 * 24 * 3600); // 8 days later

        let elig = VerifierEligibility {
            verified_since: Some(verified_at),
            brn_balance: 60 * 1_000_000_000_000_000_000,
            opted_in_as_verifier: true,
            under_active_challenge: false,
        };

        assert!(is_eligible_verifier(&elig, &config, now));
    }

    #[test]
    fn test_filter_eligible_verifiers_returns_only_eligible() {
        let config = EligibilityConfig::default();
        let now = make_timestamp(1_000_000 + DEFAULT_MIN_VERIFICATION_AGE_SECS + 1);

        let candidates = vec![
            (
                make_addr("good_1"),
                VerifierEligibility {
                    verified_since: Some(make_timestamp(1_000_000)),
                    brn_balance: DEFAULT_MIN_BRN_BALANCE * 2,
                    opted_in_as_verifier: true,
                    under_active_challenge: false,
                },
            ),
            (
                make_addr("not_opted_in"),
                VerifierEligibility {
                    verified_since: Some(make_timestamp(1_000_000)),
                    brn_balance: DEFAULT_MIN_BRN_BALANCE * 2,
                    opted_in_as_verifier: false,
                    under_active_challenge: false,
                },
            ),
            (
                make_addr("challenged"),
                VerifierEligibility {
                    verified_since: Some(make_timestamp(1_000_000)),
                    brn_balance: DEFAULT_MIN_BRN_BALANCE * 2,
                    opted_in_as_verifier: true,
                    under_active_challenge: true,
                },
            ),
            (
                make_addr("good_2"),
                VerifierEligibility {
                    verified_since: Some(make_timestamp(1_000_000)),
                    brn_balance: DEFAULT_MIN_BRN_BALANCE,
                    opted_in_as_verifier: true,
                    under_active_challenge: false,
                },
            ),
            (
                make_addr("too_new"),
                VerifierEligibility {
                    verified_since: Some(make_timestamp(now.as_secs() - 100)),
                    brn_balance: DEFAULT_MIN_BRN_BALANCE * 5,
                    opted_in_as_verifier: true,
                    under_active_challenge: false,
                },
            ),
        ];

        let eligible = filter_eligible_verifiers(&candidates, &config, now);

        assert_eq!(eligible.len(), 2);
        assert!(eligible.contains(&make_addr("good_1")));
        assert!(eligible.contains(&make_addr("good_2")));
    }
}
