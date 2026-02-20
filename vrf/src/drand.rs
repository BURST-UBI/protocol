//! Phase 1: drand — external randomness beacon from the League of Entropy.
//!
//! drand emits publicly verifiable random values every 30 seconds.
//! This module provides an HTTP client to fetch and verify drand beacons,
//! including full BLS12-381 signature verification against the network's
//! distributed public key.

use crate::{RandomOutput, VrfError, VrfProvider};
use sha2::{Digest, Sha256};

/// Default drand mainnet relay URL.
const DRAND_MAINNET_URL: &str = "https://api.drand.sh";

/// Domain separation tag used by drand's quicknet (unchained) scheme.
/// This is the DST for BLS signatures on G1 with SHA-256 hash-to-curve.
const DRAND_QUICKNET_DST: &[u8] = b"BLS_SIG_BLS12381G1_XMD:SHA-256_SSWU_RO_NUL_";

/// Distributed public key for the drand quicknet network (G2 point, compressed).
///
/// Chain hash: `52db9ba70e0cc0f6eaf7803dd07447a1f5477735fd3f661792ba94600c84e971`
///
/// This can be fetched from `https://api.drand.sh/52db9ba.../info` and verified
/// against the League of Entropy's published keys.
const DRAND_QUICKNET_PUBKEY_HEX: &str = concat!(
    "83cf0f2896adee7eb8b5f01fcad3912212c437e0073e911fb90022d3e760183c",
    "8c4b450b6a0a6c3ac6a5776a2d1064510d1fec758c921cc22b0e17e63aaf4bcb",
    "5ed66304de9cf809bd274ca73bab4af5a6e9c76a4bc09e76eae8991ef5ece45a",
);

/// The drand scheme used for beacon verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrandScheme {
    /// Unchained scheme (quicknet): message = SHA-256(round as BE u64).
    /// Signature on G1, public key on G2.
    Unchained,
    /// Chained scheme: message = SHA-256(previous_signature || round as BE u64).
    /// Signature on G2, public key on G1.
    Chained,
}

/// A drand beacon response containing the randomness for a given round.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct DrandBeacon {
    /// The round number of this beacon.
    pub round: u64,
    /// Hex-encoded randomness value.
    pub randomness: String,
    /// Hex-encoded BLS signature over the round message.
    pub signature: String,
    /// Hex-encoded BLS signature from the previous round (chained scheme only).
    #[serde(default)]
    pub previous_signature: Option<String>,
}

/// BLS12-381 verifier for drand beacons.
///
/// Performs full cryptographic verification:
/// 1. Checks that `randomness == SHA-256(signature)` (drand's derivation rule)
/// 2. Verifies the BLS signature against the network's distributed public key
pub struct DrandVerifier {
    pub_key_bytes: Vec<u8>,
    scheme: DrandScheme,
}

impl DrandVerifier {
    /// Create a verifier with a custom public key (hex-encoded compressed G2 point).
    pub fn new(pub_key_hex: &str, scheme: DrandScheme) -> Result<Self, VrfError> {
        let pub_key_bytes = hex::decode(pub_key_hex)
            .map_err(|e| VrfError::InvalidPublicKey(format!("hex decode: {e}")))?;
        Ok(Self {
            pub_key_bytes,
            scheme,
        })
    }

    /// Create a verifier for the drand quicknet (unchained) network.
    pub fn quicknet() -> Result<Self, VrfError> {
        Self::new(DRAND_QUICKNET_PUBKEY_HEX, DrandScheme::Unchained)
    }

    /// Verify a drand beacon using full BLS12-381 signature verification.
    ///
    /// Returns `Ok(true)` if both the randomness derivation and BLS signature
    /// are valid, `Ok(false)` if either check fails.
    pub fn verify_beacon(&self, beacon: &DrandBeacon) -> Result<bool, VrfError> {
        let sig_bytes = hex::decode(&beacon.signature)
            .map_err(|e| VrfError::InvalidSignature(format!("hex decode: {e}")))?;
        let randomness_bytes = hex::decode(&beacon.randomness)
            .map_err(|e| VrfError::InvalidProof(format!("randomness hex decode: {e}")))?;

        // Step 1: Verify randomness == SHA-256(signature)
        let computed_randomness = Sha256::digest(&sig_bytes);
        if computed_randomness.as_slice() != randomness_bytes.as_slice() {
            return Ok(false);
        }

        // Step 2: Construct the message that was signed
        let message = self.beacon_message(beacon)?;

        // Step 3: Verify BLS signature
        self.verify_bls(&sig_bytes, &message)
    }

    /// Construct the message that the beacon round signed.
    fn beacon_message(&self, beacon: &DrandBeacon) -> Result<Vec<u8>, VrfError> {
        match self.scheme {
            DrandScheme::Unchained => {
                let hash = Sha256::digest(beacon.round.to_be_bytes());
                Ok(hash.to_vec())
            }
            DrandScheme::Chained => {
                let prev_sig = beacon
                    .previous_signature
                    .as_deref()
                    .ok_or_else(|| {
                        VrfError::InvalidProof(
                            "chained scheme requires previous_signature".into(),
                        )
                    })?;
                let prev_bytes = hex::decode(prev_sig)
                    .map_err(|e| VrfError::InvalidProof(format!("prev sig hex: {e}")))?;
                let mut hasher = Sha256::new();
                hasher.update(&prev_bytes);
                hasher.update(beacon.round.to_be_bytes());
                Ok(hasher.finalize().to_vec())
            }
        }
    }

    /// Verify a BLS12-381 signature using the `blst` crate.
    ///
    /// For unchained/quicknet: signature on G1, public key on G2 (min_pk scheme).
    fn verify_bls(&self, sig_bytes: &[u8], message: &[u8]) -> Result<bool, VrfError> {
        match self.scheme {
            DrandScheme::Unchained => {
                use blst::min_pk::{PublicKey, Signature};

                let pk = PublicKey::from_bytes(&self.pub_key_bytes).map_err(|e| {
                    VrfError::InvalidPublicKey(format!("G2 point deserialization: {e:?}"))
                })?;
                let sig = Signature::from_bytes(sig_bytes).map_err(|e| {
                    VrfError::InvalidSignature(format!("G1 point deserialization: {e:?}"))
                })?;

                let result =
                    sig.verify(true, message, DRAND_QUICKNET_DST, &[], &pk, true);

                Ok(result == blst::BLST_ERROR::BLST_SUCCESS)
            }
            DrandScheme::Chained => {
                use blst::min_sig::{PublicKey, Signature};

                let pk = PublicKey::from_bytes(&self.pub_key_bytes).map_err(|e| {
                    VrfError::InvalidPublicKey(format!("G1 point deserialization: {e:?}"))
                })?;
                let sig = Signature::from_bytes(sig_bytes).map_err(|e| {
                    VrfError::InvalidSignature(format!("G2 point deserialization: {e:?}"))
                })?;

                let dst = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_NUL_";
                let result = sig.verify(true, message, dst, &[], &pk, true);

                Ok(result == blst::BLST_ERROR::BLST_SUCCESS)
            }
        }
    }
}

/// Simplified verification: only checks `randomness == SHA-256(signature)`.
///
/// This does NOT verify the BLS signature and should only be used for testing
/// or when the `blst` library is unavailable. An attacker can forge beacons
/// that pass this check.
pub fn verify_beacon_simple(beacon: &DrandBeacon) -> Result<bool, VrfError> {
    let sig_bytes = hex::decode(&beacon.signature)
        .map_err(|e| VrfError::InvalidProof(e.to_string()))?;
    let hash = Sha256::digest(&sig_bytes);
    let expected = hex::encode(hash);
    Ok(expected == beacon.randomness)
}

/// Metadata about a drand chain, used for round-to-time mapping and
/// future-round rejection.
#[derive(Debug, Clone)]
pub struct ChainInfo {
    /// Network's distributed public key (compressed).
    pub public_key: Vec<u8>,
    /// Seconds between rounds (typically 3 for quicknet).
    pub period: u64,
    /// UNIX timestamp of round 1.
    pub genesis_time: u64,
    /// Hex-encoded chain hash.
    pub chain_hash: String,
    /// Signature scheme used by this chain.
    pub scheme: DrandScheme,
}

impl ChainInfo {
    pub fn time_of_round(&self, round: u64) -> u64 {
        self.genesis_time + (round.saturating_sub(1)) * self.period
    }

    pub fn current_round(&self, now: u64) -> u64 {
        if now < self.genesis_time {
            return 0;
        }
        ((now - self.genesis_time) / self.period) + 1
    }

    pub fn is_round_available(&self, round: u64, now: u64) -> bool {
        self.time_of_round(round) <= now
    }
}

/// HTTP client for fetching randomness from a drand relay.
///
/// drand beacons are publicly verifiable and produced by the League of Entropy
/// distributed key generation network. Each beacon contains a BLS signature
/// that can be verified against the network's public key.
pub struct DrandClient {
    /// Base URL of the drand HTTP relay.
    base_url: String,
    /// Reusable HTTP client.
    client: reqwest::Client,
    /// The chain hash identifying which drand network to use (optional filter).
    chain_hash: Option<String>,
    /// Optional verifier for full BLS signature checking.
    verifier: Option<DrandVerifier>,
    /// Chain metadata for round/time calculations.
    chain_info: Option<ChainInfo>,
    /// Cached beacon to avoid redundant fetches within the same round.
    cached_beacon: Option<(u64, DrandBeacon)>,
}

impl DrandClient {
    /// Create a new client pointing at the drand mainnet relay (no BLS verification).
    pub fn new() -> Self {
        Self {
            base_url: DRAND_MAINNET_URL.to_string(),
            client: reqwest::Client::new(),
            chain_hash: None,
            verifier: None,
            chain_info: None,
            cached_beacon: None,
        }
    }

    /// Create a client pointing at a custom relay URL.
    pub fn with_url(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
            chain_hash: None,
            verifier: None,
            chain_info: None,
            cached_beacon: None,
        }
    }

    /// Create a client with a specific chain hash for network selection.
    pub fn with_chain(base_url: &str, chain_hash: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
            chain_hash: Some(chain_hash.to_string()),
            verifier: None,
            chain_info: None,
            cached_beacon: None,
        }
    }

    /// Create a client configured for drand quicknet with full BLS verification.
    pub fn quicknet() -> Result<Self, VrfError> {
        let verifier = DrandVerifier::quicknet()?;
        Ok(Self {
            base_url: DRAND_MAINNET_URL.to_string(),
            client: reqwest::Client::new(),
            chain_hash: Some(
                "52db9ba70e0cc0f6eaf7803dd07447a1f5477735fd3f661792ba94600c84e971".into(),
            ),
            verifier: Some(verifier),
            chain_info: None,
            cached_beacon: None,
        })
    }

    /// Attach a BLS verifier to this client so fetched beacons are fully verified.
    pub fn with_verifier(mut self, verifier: DrandVerifier) -> Self {
        self.verifier = Some(verifier);
        self
    }

    /// Attach chain info for round-to-time calculations and future-round rejection.
    pub fn with_chain_info(mut self, info: ChainInfo) -> Self {
        self.chain_info = Some(info);
        self
    }

    /// Build the API path prefix, incorporating chain_hash if set.
    fn api_prefix(&self) -> String {
        match &self.chain_hash {
            Some(hash) => format!("{}/{}", self.base_url, hash),
            None => self.base_url.clone(),
        }
    }

    /// Fetch the latest beacon from drand.
    pub async fn fetch_latest(&self) -> Result<DrandBeacon, VrfError> {
        let url = format!("{}/public/latest", self.api_prefix());
        let beacon = self.fetch_beacon_from(&url).await?;
        self.maybe_verify(&beacon)?;
        Ok(beacon)
    }

    /// Fetch a specific round from drand.
    pub async fn fetch_round(&self, round: u64) -> Result<DrandBeacon, VrfError> {
        let url = format!("{}/public/{}", self.api_prefix(), round);
        let beacon = self.fetch_beacon_from(&url).await?;
        self.maybe_verify(&beacon)?;
        Ok(beacon)
    }

    async fn fetch_beacon_from(&self, url: &str) -> Result<DrandBeacon, VrfError> {
        let resp = self
            .client
            .get(url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| VrfError::DrandFetch(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(VrfError::DrandFetch(format!(
                "HTTP {} from {}",
                resp.status(),
                url
            )));
        }

        resp.json()
            .await
            .map_err(|e| VrfError::DrandFetch(e.to_string()))
    }

    /// If a verifier is attached, perform full BLS verification.
    fn maybe_verify(&self, beacon: &DrandBeacon) -> Result<(), VrfError> {
        if let Some(ref verifier) = self.verifier {
            if !verifier.verify_beacon(beacon)? {
                return Err(VrfError::BlsVerification(format!(
                    "beacon round {} failed BLS signature verification",
                    beacon.round,
                )));
            }
        }
        Ok(())
    }

    /// Fetch the latest beacon with caching — returns the cached beacon if
    /// the current round hasn't changed since the last fetch.
    pub async fn fetch_latest_cached(&mut self) -> Result<DrandBeacon, VrfError> {
        if let Some(ref info) = self.chain_info {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let current_round = info.current_round(now);
            if let Some((cached_round, ref beacon)) = self.cached_beacon {
                if cached_round == current_round {
                    return Ok(beacon.clone());
                }
            }
        }

        let beacon = self.fetch_latest().await?;
        self.cached_beacon = Some((beacon.round, beacon.clone()));
        Ok(beacon)
    }

    /// Validate that a beacon's round is not from the future.
    pub fn validate_round_timing(&self, beacon: &DrandBeacon) -> Result<(), VrfError> {
        if let Some(ref info) = self.chain_info {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if !info.is_round_available(beacon.round, now) {
                return Err(VrfError::FutureRound {
                    round: beacon.round,
                    available_at: info.time_of_round(beacon.round),
                });
            }
        }
        Ok(())
    }

    /// Verify a drand beacon (simplified SHA-256 check only).
    ///
    /// **Deprecated**: Use [`DrandVerifier::verify_beacon`] for full BLS verification.
    pub fn verify_beacon(beacon: &DrandBeacon) -> Result<bool, VrfError> {
        verify_beacon_simple(beacon)
    }

    /// Convert a drand beacon into a [`RandomOutput`] for use in verifier selection.
    pub fn beacon_to_random_output(beacon: &DrandBeacon) -> Result<RandomOutput, VrfError> {
        let randomness_bytes = hex::decode(&beacon.randomness)
            .map_err(|e| VrfError::InvalidProof(e.to_string()))?;

        let mut value = [0u8; 32];
        let len = randomness_bytes.len().min(32);
        value[..len].copy_from_slice(&randomness_bytes[..len]);

        let proof = hex::decode(&beacon.signature)
            .map_err(|e| VrfError::InvalidProof(e.to_string()))?;

        Ok(RandomOutput {
            value,
            proof,
            round: beacon.round,
        })
    }
}

impl Default for DrandClient {
    fn default() -> Self {
        Self::new()
    }
}

impl VrfProvider for DrandClient {
    fn get_randomness(&self, _context: &[u8]) -> Result<RandomOutput, VrfError> {
        Err(VrfError::Unavailable(
            "drand requires async fetch — use fetch_latest() or fetch_round()".into(),
        ))
    }

    fn verify(&self, _context: &[u8], output: &RandomOutput) -> Result<bool, VrfError> {
        // If we have a verifier, reconstruct the beacon and do full BLS verification.
        if let Some(ref verifier) = self.verifier {
            let beacon = DrandBeacon {
                round: output.round,
                randomness: hex::encode(output.value),
                signature: hex::encode(&output.proof),
                previous_signature: None,
            };
            return verifier.verify_beacon(&beacon);
        }

        // Fallback: SHA-256 of the proof (signature) should yield the randomness value.
        let hash = Sha256::digest(&output.proof);
        let mut expected = [0u8; 32];
        expected.copy_from_slice(&hash);
        Ok(expected == output.value)
    }

    fn name(&self) -> &str {
        "drand"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_client_creation() {
        let client = DrandClient::new();
        assert_eq!(client.base_url, DRAND_MAINNET_URL);
        assert!(client.chain_hash.is_none());
    }

    #[test]
    fn test_custom_url_client() {
        let client = DrandClient::with_url("https://custom.drand.sh/");
        assert_eq!(client.base_url, "https://custom.drand.sh");
    }

    #[test]
    fn test_chain_hash_client() {
        let client = DrandClient::with_chain("https://api.drand.sh", "abc123");
        assert_eq!(client.chain_hash.as_deref(), Some("abc123"));
        assert_eq!(client.api_prefix(), "https://api.drand.sh/abc123");
    }

    #[test]
    fn test_quicknet_client_creation() {
        let client = DrandClient::quicknet().expect("quicknet client");
        assert!(client.verifier.is_some());
        assert!(client.chain_hash.is_some());
    }

    #[test]
    fn test_verify_beacon_simple_valid() {
        let sig_hex = "deadbeef".to_string();
        let sig_bytes = hex::decode(&sig_hex).unwrap();
        let hash = Sha256::digest(&sig_bytes);
        let randomness = hex::encode(hash);

        let beacon = DrandBeacon {
            round: 1,
            randomness,
            signature: sig_hex,
            previous_signature: None,
        };

        assert!(verify_beacon_simple(&beacon).unwrap());
    }

    #[test]
    fn test_verify_beacon_simple_invalid() {
        let beacon = DrandBeacon {
            round: 1,
            randomness: "0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(),
            signature: "deadbeef".to_string(),
            previous_signature: None,
        };

        assert!(!verify_beacon_simple(&beacon).unwrap());
    }

    #[test]
    fn test_verify_beacon_deprecated_wrapper() {
        let sig_hex = "deadbeef".to_string();
        let sig_bytes = hex::decode(&sig_hex).unwrap();
        let hash = Sha256::digest(&sig_bytes);
        let randomness = hex::encode(hash);

        let beacon = DrandBeacon {
            round: 1,
            randomness,
            signature: sig_hex,
            previous_signature: None,
        };

        assert!(DrandClient::verify_beacon(&beacon).unwrap());
    }

    #[test]
    fn test_beacon_to_random_output() {
        let sig_hex = "aa".repeat(48);
        let sig_bytes = hex::decode(&sig_hex).unwrap();
        let hash = Sha256::digest(&sig_bytes);
        let randomness = hex::encode(hash);

        let beacon = DrandBeacon {
            round: 42,
            randomness,
            signature: sig_hex,
            previous_signature: None,
        };

        let output = DrandClient::beacon_to_random_output(&beacon).unwrap();
        assert_eq!(output.round, 42);
        assert_eq!(output.value.len(), 32);
        assert!(!output.proof.is_empty());
    }

    #[test]
    fn test_vrf_provider_verify_fallback() {
        let client = DrandClient::new();

        let proof = b"test_signature_data".to_vec();
        let hash = Sha256::digest(&proof);
        let mut value = [0u8; 32];
        value.copy_from_slice(&hash);

        let output = RandomOutput {
            value,
            proof,
            round: 1,
        };

        assert!(client.verify(&[], &output).unwrap());
    }

    #[test]
    fn test_vrf_provider_get_randomness_returns_unavailable() {
        let client = DrandClient::new();
        let result = client.get_randomness(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_drand_verifier_creation() {
        let verifier = DrandVerifier::quicknet().expect("should create quicknet verifier");
        assert_eq!(verifier.scheme, DrandScheme::Unchained);
        assert!(!verifier.pub_key_bytes.is_empty());
    }

    #[test]
    fn test_drand_verifier_invalid_pubkey() {
        let result = DrandVerifier::new("not_valid_hex!!!", DrandScheme::Unchained);
        assert!(result.is_err());
    }

    #[test]
    fn test_beacon_message_unchained() {
        let verifier = DrandVerifier::quicknet().unwrap();
        let beacon = DrandBeacon {
            round: 1000,
            randomness: String::new(),
            signature: String::new(),
            previous_signature: None,
        };
        let msg = verifier.beacon_message(&beacon).unwrap();
        let expected = Sha256::digest(1000u64.to_be_bytes());
        assert_eq!(msg, expected.as_slice());
    }

    #[test]
    fn test_beacon_message_chained_requires_prev_sig() {
        let verifier =
            DrandVerifier::new("aabb", DrandScheme::Chained).unwrap();
        let beacon = DrandBeacon {
            round: 1,
            randomness: String::new(),
            signature: String::new(),
            previous_signature: None,
        };
        assert!(verifier.beacon_message(&beacon).is_err());
    }

    #[test]
    fn test_beacon_message_chained() {
        let verifier =
            DrandVerifier::new("aabb", DrandScheme::Chained).unwrap();
        let beacon = DrandBeacon {
            round: 5,
            randomness: String::new(),
            signature: String::new(),
            previous_signature: Some("abcd".into()),
        };
        let msg = verifier.beacon_message(&beacon).unwrap();

        let prev_bytes = hex::decode("abcd").unwrap();
        let mut hasher = Sha256::new();
        hasher.update(&prev_bytes);
        hasher.update(5u64.to_be_bytes());
        let expected = hasher.finalize();
        assert_eq!(msg, expected.as_slice());
    }

    #[test]
    fn test_bls_verification_rejects_forged_signature() {
        let verifier = DrandVerifier::quicknet().unwrap();

        let fake_sig = "aa".repeat(48);
        let fake_sig_bytes = hex::decode(&fake_sig).unwrap();
        let fake_randomness = hex::encode(Sha256::digest(&fake_sig_bytes));

        let forged = DrandBeacon {
            round: 1,
            randomness: fake_randomness,
            signature: fake_sig,
            previous_signature: None,
        };

        // SHA-256 check passes, but BLS verification must fail
        assert!(verify_beacon_simple(&forged).unwrap());
        let result = verifier.verify_beacon(&forged);
        match result {
            Ok(valid) => assert!(!valid, "forged beacon must not pass BLS verification"),
            Err(_) => {} // deserialization failure is also acceptable
        }
    }

    #[test]
    fn test_deserialization_unchained_no_prev_sig() {
        let json = r#"{"round":1000,"randomness":"abcd","signature":"ef01"}"#;
        let beacon: DrandBeacon = serde_json::from_str(json).unwrap();
        assert_eq!(beacon.round, 1000);
        assert!(beacon.previous_signature.is_none());
    }

    #[test]
    fn test_deserialization_chained_with_prev_sig() {
        let json = r#"{"round":1,"randomness":"ab","signature":"cd","previous_signature":"ef"}"#;
        let beacon: DrandBeacon = serde_json::from_str(json).unwrap();
        assert_eq!(beacon.previous_signature.as_deref(), Some("ef"));
    }
}
