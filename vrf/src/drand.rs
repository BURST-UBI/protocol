//! Phase 1: drand — external randomness beacon from the League of Entropy.
//!
//! The chain approved in implementation decision 19.1 emits every 30 seconds.
//! This module provides an HTTP client to fetch and verify drand beacons,
//! including full BLS12-381 signature verification against the network's
//! distributed public key.

use crate::{RandomOutput, VrfError, VrfProvider};
use bincode::Options;
use futures_util::{stream::FuturesUnordered, StreamExt};
use sha2::{Digest, Sha256};

mod policy;
mod verified_signatures;
pub use policy::DrandPolicy;

/// Default drand mainnet relay URL.
pub const DRAND_MAINNET_URL: &str = "https://api.drand.sh";
pub const MAX_DRAND_RELAYS: usize = 4;
pub const DRAND_MAINNET_CHAIN: &str =
    "8990e7a9aaed2ffed73dbd7092123d6f289930540d7651336225dc172e51b2ce";
const DRAND_MAINNET_PUBKEY: &str = "868f005eb8e6e4ca0a47c8a77ceaa5309a47978a7c71bc5cce96366b5d7a569937c529eeda66c7293784a9402801af31";
const MAX_BEACON_BYTES: usize = 4096;

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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone)]
pub struct DrandVerifier {
    pub_key_bytes: Vec<u8>,
    scheme: DrandScheme,
}

impl DrandVerifier {
    /// Create a verifier with a pinned public key in the selected scheme's group.
    pub fn new(pub_key_hex: &str, scheme: DrandScheme) -> Result<Self, VrfError> {
        let pub_key_bytes = hex::decode(pub_key_hex)
            .map_err(|e| VrfError::InvalidPublicKey(format!("hex decode: {e}")))?;
        match scheme {
            DrandScheme::Unchained => {
                blst::min_sig::PublicKey::key_validate(&pub_key_bytes).map(|_| ())
            }
            DrandScheme::Chained => {
                blst::min_pk::PublicKey::key_validate(&pub_key_bytes).map(|_| ())
            }
        }
        .map_err(|e| VrfError::InvalidPublicKey(format!("invalid public key: {e:?}")))?;
        Ok(Self {
            pub_key_bytes,
            scheme,
        })
    }

    /// Create a verifier for the drand quicknet (unchained) network.
    pub fn quicknet() -> Result<Self, VrfError> {
        Self::new(DRAND_QUICKNET_PUBKEY_HEX, DrandScheme::Unchained)
    }

    pub fn mainnet() -> Result<Self, VrfError> {
        Self::new(DRAND_MAINNET_PUBKEY, DrandScheme::Chained)
    }

    fn require_beacon(&self, beacon: &DrandBeacon) -> Result<(), VrfError> {
        if !self.verify_beacon(beacon)? {
            return Err(VrfError::BlsVerification(format!(
                "beacon round {} failed BLS signature verification",
                beacon.round,
            )));
        }
        Ok(())
    }

    /// Verify a drand beacon using full BLS12-381 signature verification.
    ///
    /// Returns `Ok(true)` if both the randomness derivation and BLS signature
    /// are valid, `Ok(false)` if either check fails.
    pub fn verify_beacon(&self, beacon: &DrandBeacon) -> Result<bool, VrfError> {
        if beacon.round == 0 {
            return Err(VrfError::InvalidProof("drand rounds start at 1".into()));
        }
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
                if beacon.previous_signature.is_some() {
                    return Err(VrfError::InvalidProof(
                        "unchained beacon includes a previous signature".into(),
                    ));
                }
                let hash = Sha256::digest(beacon.round.to_be_bytes());
                Ok(hash.to_vec())
            }
            DrandScheme::Chained => {
                let prev_sig = beacon.previous_signature.as_deref().ok_or_else(|| {
                    VrfError::InvalidProof("chained scheme requires previous_signature".into())
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
    /// For unchained/quicknet: signature on G1, public key on G2 (min_sig).
    fn verify_bls(&self, sig_bytes: &[u8], message: &[u8]) -> Result<bool, VrfError> {
        verified_signatures::verify(self.scheme, &self.pub_key_bytes, sig_bytes, message, || {
            self.verify_bls_uncached(sig_bytes, message)
        })
    }

    fn verify_bls_uncached(&self, sig_bytes: &[u8], message: &[u8]) -> Result<bool, VrfError> {
        match self.scheme {
            DrandScheme::Unchained => {
                use blst::min_sig::{PublicKey, Signature};

                let pk = PublicKey::from_bytes(&self.pub_key_bytes).map_err(|e| {
                    VrfError::InvalidPublicKey(format!("G2 point deserialization: {e:?}"))
                })?;
                let sig = Signature::from_bytes(sig_bytes).map_err(|e| {
                    VrfError::InvalidSignature(format!("G1 point deserialization: {e:?}"))
                })?;

                let result = sig.verify(true, message, DRAND_QUICKNET_DST, &[], &pk, true);

                Ok(result == blst::BLST_ERROR::BLST_SUCCESS)
            }
            DrandScheme::Chained => {
                use blst::min_pk::{PublicKey, Signature};

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
    let sig_bytes =
        hex::decode(&beacon.signature).map_err(|e| VrfError::InvalidProof(e.to_string()))?;
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
    pub fn time_of_round(&self, round: u64) -> Result<u64, VrfError> {
        if self.period == 0 || round == 0 {
            return Err(VrfError::InvalidProof("zero drand period or round".into()));
        }
        (round - 1)
            .checked_mul(self.period)
            .and_then(|offset| self.genesis_time.checked_add(offset))
            .ok_or_else(|| VrfError::InvalidProof("drand round time overflow".into()))
    }

    pub fn current_round(&self, now: u64) -> Result<u64, VrfError> {
        if self.period == 0 {
            return Err(VrfError::InvalidProof("zero drand period".into()));
        }
        if now < self.genesis_time {
            return Ok(0);
        }
        ((now - self.genesis_time) / self.period)
            .checked_add(1)
            .ok_or_else(|| VrfError::InvalidProof("drand round overflow".into()))
    }

    /// First round strictly after an agreed request cutoff, not an observer's clock.
    pub fn round_after(&self, cutoff: u64) -> Result<u64, VrfError> {
        let round = self
            .current_round(cutoff)?
            .checked_add(1)
            .ok_or_else(|| VrfError::InvalidProof("drand round overflow".into()))?;
        self.time_of_round(round)?;
        Ok(round)
    }

    pub fn is_round_available(&self, round: u64, now: u64) -> bool {
        self.time_of_round(round).is_ok_and(|at| at <= now)
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
    /// Optional bounded mirror set, used only for exact-round requests.
    relays: Vec<String>,
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
    /// Create a client for the chain approved in implementation decision 19.1.
    pub fn new() -> Self {
        Self::mainnet().expect("the pinned mainnet public key is valid")
    }

    pub fn mainnet() -> Result<Self, VrfError> {
        let policy = DrandPolicy::mainnet()?;
        Ok(Self::with_chain(DRAND_MAINNET_URL, DRAND_MAINNET_CHAIN)
            .with_verifier(policy.verifier.clone())
            .with_chain_info(policy.chain_info().clone()))
    }

    pub fn chain_info(&self) -> Option<&ChainInfo> {
        self.chain_info.as_ref()
    }

    /// Create a custom relay client. Attach a verifier before fetching beacons.
    pub fn with_url(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            relays: Vec::new(),
            client: reqwest::Client::new(),
            chain_hash: None,
            verifier: None,
            chain_info: None,
            cached_beacon: None,
        }
    }

    /// Create a custom chain client. Attach that chain's verifier before fetching.
    pub fn with_chain(base_url: &str, chain_hash: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            relays: Vec::new(),
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
            relays: Vec::new(),
            client: reqwest::Client::new(),
            chain_hash: Some(
                "52db9ba70e0cc0f6eaf7803dd07447a1f5477735fd3f661792ba94600c84e971".into(),
            ),
            verifier: Some(verifier),
            chain_info: Some(ChainInfo {
                public_key: hex::decode(DRAND_QUICKNET_PUBKEY_HEX).expect("pinned key hex"),
                period: 3,
                genesis_time: 1692803367,
                chain_hash: "52db9ba70e0cc0f6eaf7803dd07447a1f5477735fd3f661792ba94600c84e971"
                    .into(),
                scheme: DrandScheme::Unchained,
            }),
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

    /// Mirrors affect availability only; the chain, verifier and requested round
    /// remain pinned. HTTP is permitted only for loopback test/local relays.
    pub fn with_relays(mut self, relays: &[String]) -> Result<Self, VrfError> {
        self.relays = Self::validate_relays(relays)?;
        if self.chain_hash.is_none() || self.verifier.is_none() {
            return Err(VrfError::Unavailable(
                "relay failover requires a pinned chain and verifier".into(),
            ));
        }
        self.client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| VrfError::DrandFetch(error.to_string()))?;
        Ok(self)
    }

    pub fn validate_relays(relays: &[String]) -> Result<Vec<String>, VrfError> {
        if relays.is_empty() || relays.len() > MAX_DRAND_RELAYS {
            return Err(VrfError::Unavailable(format!(
                "configure 1 to {MAX_DRAND_RELAYS} drand relays"
            )));
        }
        let mut urls = Vec::with_capacity(relays.len());
        for relay in relays {
            let url = reqwest::Url::parse(relay)
                .map_err(|_| VrfError::Unavailable("invalid drand relay URL".into()))?;
            let loopback = url
                .host_str()
                .and_then(|host| {
                    host.trim_start_matches('[')
                        .trim_end_matches(']')
                        .parse::<std::net::IpAddr>()
                        .ok()
                })
                .is_some_and(|ip| ip.is_loopback());
            if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
                || url.host().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(VrfError::Unavailable("drand relays require HTTPS (or loopback HTTP), without credentials, query or fragment".into()));
            }
            let normalized = url.as_str().trim_end_matches('/').to_string();
            if urls.contains(&normalized) {
                return Err(VrfError::Unavailable("duplicate drand relay URL".into()));
            }
            urls.push(normalized);
        }
        Ok(urls)
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
        if !self.relays.is_empty() {
            return self.fetch_round_from_relays(round, &self.relays).await;
        }
        if round == 0 {
            return Err(VrfError::InvalidProof("drand rounds start at 1".into()));
        }
        let url = format!("{}/public/{}", self.api_prefix(), round);
        let beacon = self.fetch_beacon_from(&url).await?;
        if round == 0 || beacon.round != round {
            return Err(VrfError::InvalidProof(format!(
                "requested round {round}, received {}",
                beacon.round
            )));
        }
        self.maybe_verify(&beacon)?;
        Ok(beacon)
    }

    /// Try mirrors for the same pinned chain and exact round. A failure never
    /// substitutes latest randomness, another round, or an unverified response.
    pub async fn fetch_round_from_relays(
        &self,
        round: u64,
        relays: &[String],
    ) -> Result<DrandBeacon, VrfError> {
        if round == 0 {
            return Err(VrfError::InvalidProof("drand rounds start at 1".into()));
        }
        let chain = self.chain_hash.as_ref().ok_or_else(|| {
            VrfError::Unavailable("relay failover requires a pinned chain".into())
        })?;
        if self.verifier.is_none() {
            return Err(VrfError::Unavailable(
                "relay failover requires a pinned verifier".into(),
            ));
        }
        let relays = Self::validate_relays(relays)?;
        let mut last_error = VrfError::Unavailable("no drand relays configured".into());
        // Poll a bounded set together. A stalled primary cannot prevent a
        // healthy mirror from supplying the same authenticated round. Dropping
        // this set cancels losing requests, including on worker shutdown.
        let mut requests: FuturesUnordered<_> = relays
            .into_iter()
            .map(|relay| async move {
                let url = format!("{}/{chain}/public/{round}", relay.trim_end_matches('/'));
                let beacon = self.fetch_beacon_from(&url).await?;
                if beacon.round != round {
                    return Err(VrfError::InvalidProof(format!(
                        "requested round {round}, received {}",
                        beacon.round
                    )));
                }
                self.maybe_verify(&beacon)?;
                Ok(beacon)
            })
            .collect();
        while let Some(result) = requests.next().await {
            match result {
                Ok(beacon) => return Ok(beacon),
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }

    async fn fetch_beacon_from(&self, url: &str) -> Result<DrandBeacon, VrfError> {
        let mut resp = self
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

        if resp
            .content_length()
            .is_some_and(|length| length > MAX_BEACON_BYTES as u64)
        {
            return Err(VrfError::DrandFetch(
                "beacon exceeds response size limit".into(),
            ));
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| VrfError::DrandFetch(e.to_string()))?
        {
            if chunk.len() > MAX_BEACON_BYTES.saturating_sub(body.len()) {
                return Err(VrfError::DrandFetch(
                    "beacon exceeds response size limit".into(),
                ));
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|e| VrfError::DrandFetch(e.to_string()))
    }

    /// Require full BLS verification; a signature hash alone is not authentication.
    fn maybe_verify(&self, beacon: &DrandBeacon) -> Result<(), VrfError> {
        let verifier = self.verifier.as_ref().ok_or_else(|| {
            VrfError::Unavailable("custom drand clients require a pinned BLS verifier".into())
        })?;
        if let Some(info) = &self.chain_info {
            if info.period == 0
                || self.chain_hash.as_ref() != Some(&info.chain_hash)
                || verifier.pub_key_bytes != info.public_key
                || verifier.scheme != info.scheme
            {
                return Err(VrfError::InvalidProof(
                    "drand chain metadata does not match pinned verification policy".into(),
                ));
            }
        }
        verifier.require_beacon(beacon)
    }

    /// Fetch the latest beacon with caching — returns the cached beacon if
    /// the current round hasn't changed since the last fetch.
    pub async fn fetch_latest_cached(&mut self) -> Result<DrandBeacon, VrfError> {
        if let Some(ref info) = self.chain_info {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let current_round = info.current_round(now)?;
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
                    available_at: info.time_of_round(beacon.round)?,
                });
            }
        }
        Ok(())
    }

    /// Verify against the approved mainnet key. Other chains need an explicit verifier.
    pub fn verify_beacon(beacon: &DrandBeacon) -> Result<bool, VrfError> {
        DrandPolicy::mainnet()?.verifier.verify_beacon(beacon)
    }

    /// Convert a drand beacon into a [`RandomOutput`] for use in verifier selection.
    pub fn beacon_to_random_output(beacon: &DrandBeacon) -> Result<RandomOutput, VrfError> {
        let randomness_bytes =
            hex::decode(&beacon.randomness).map_err(|e| VrfError::InvalidProof(e.to_string()))?;

        let value: [u8; 32] = randomness_bytes.try_into().map_err(|_| {
            VrfError::InvalidProof("beacon randomness must contain exactly 32 bytes".into())
        })?;

        // Preserve the previous signature too; chained beacons cannot be
        // authenticated from the current signature alone.
        let proof =
            bincode::serialize(beacon).map_err(|e| VrfError::InvalidProof(e.to_string()))?;

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
        if self.verifier.is_some() {
            return policy::verify_output(output, |beacon| self.maybe_verify(beacon));
        }

        Err(VrfError::Unavailable(
            "custom drand clients require a pinned BLS verifier".into(),
        ))
    }

    fn name(&self) -> &str {
        "drand"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // https://api.drand.sh/8990e7a9aaed2ffed73dbd7092123d6f289930540d7651336225dc172e51b2ce/public/1
    fn mainnet_round_one() -> DrandBeacon {
        DrandBeacon {
            round: 1,
            randomness: "101297f1ca7dc44ef6088d94ad5fb7ba03455dc33d53ddb412bbc4564ed986ec".into(),
            signature: "8d61d9100567de44682506aea1a7a6fa6e5491cd27a0a0ed349ef6910ac5ac20ff7bc3e09d7c046566c9f7f3c6f3b10104990e7cb424998203d8f7de586fb7fa5f60045417a432684f85093b06ca91c769f0e7ca19268375e659c2a2352b4655".into(),
            previous_signature: Some("176f93498eac9ca337150b46d21dd58673ea4e3581185f869672e59fa4cb390a".into()),
        }
    }

    #[test]
    fn replay_policy_matches_client_without_transport_or_key_reconstruction() {
        let policy = DrandPolicy::mainnet().unwrap();
        assert!(std::ptr::eq(policy, DrandPolicy::mainnet().unwrap()));
        assert_eq!(policy.chain_info().chain_hash, DRAND_MAINNET_CHAIN);
        assert_eq!(policy.chain_info().period, 30);
        assert_eq!(policy.chain_info().genesis_time, 1595431050);
        assert_eq!(policy.chain_info().round_after(1595431050).unwrap(), 2);
        let client = DrandClient::mainnet().unwrap();
        let beacon = mainnet_round_one();
        let valid = DrandClient::beacon_to_random_output(&beacon).unwrap();
        assert!(policy.verify(&valid).unwrap());
        assert!(client.verify(&[], &valid).unwrap());
        for case in 0..6 {
            let mut output = valid.clone();
            match case {
                0 => output.round += 1,
                1 => output.value[0] ^= 1,
                2 => output.proof.push(0),
                3 => {
                    output.proof.pop();
                }
                4 => {
                    let mut changed = beacon.clone();
                    changed.previous_signature = None;
                    output = DrandClient::beacon_to_random_output(&changed).unwrap();
                }
                _ => output = DrandClient::beacon_to_random_output(&quicknet_round_one()).unwrap(),
            }
            let local = policy.verify(&output);
            let network = client.verify(&[], &output);
            assert!(!matches!(local, Ok(true)), "case {case}");
            assert_eq!(local.is_err(), network.is_err(), "case {case}");
            assert_eq!(local.ok(), network.ok(), "case {case}");
        }
    }

    #[test]
    fn approved_mainnet_chain_and_complete_proof_verify() {
        let client = DrandClient::new();
        assert_eq!(client.chain_hash.as_deref(), Some(DRAND_MAINNET_CHAIN));
        let beacon = mainnet_round_one();
        client.maybe_verify(&beacon).unwrap();
        let mut output = DrandClient::beacon_to_random_output(&beacon).unwrap();
        assert!(client.verify(&[], &output).unwrap());
        output.round = 2;
        assert!(!client.verify(&[], &output).unwrap());
        output.round = 1;
        output.proof.push(0);
        assert!(client.verify(&[], &output).is_err());
        let quicknet = DrandClient::quicknet().unwrap();
        assert!(quicknet.maybe_verify(&beacon).is_err());
    }

    #[test]
    fn proof_verification_enforces_the_same_pinned_metadata_as_fetching() {
        let mut client = DrandClient::new();
        let beacon = mainnet_round_one();
        let output = DrandClient::beacon_to_random_output(&beacon).unwrap();
        client.chain_info.as_mut().unwrap().chain_hash = "another-chain".into();
        assert!(client.maybe_verify(&beacon).is_err());
        assert!(client.verify(&[], &output).is_err());
    }

    #[test]
    fn round_mapping_is_strictly_future_checked_and_independent_of_local_time() {
        let info = DrandClient::new().chain_info().unwrap().clone();
        let genesis = info.genesis_time;
        assert_eq!(info.round_after(genesis - 1).unwrap(), 1);
        assert_eq!(info.round_after(genesis).unwrap(), 2);
        assert_eq!(info.round_after(genesis + 29).unwrap(), 2);
        assert_eq!(info.round_after(genesis + 30).unwrap(), 3);
        assert_eq!(info.time_of_round(2).unwrap(), genesis + 30);
        assert!(!info.is_round_available(0, u64::MAX));
        assert!(info.time_of_round(u64::MAX).is_err());
        assert!(info.round_after(u64::MAX).is_err());
        let invalid = ChainInfo { period: 0, ..info };
        assert!(invalid.current_round(genesis).is_err());
        assert!(invalid.round_after(genesis).is_err());
    }

    async fn relay(
        status: u16,
        body: String,
        requested_round: u64,
    ) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                request.push(socket.read_u8().await.unwrap());
                assert!(request.len() <= 8192);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let expected =
                format!("GET /{DRAND_MAINNET_CHAIN}/public/{requested_round} HTTP/1.1\r\n");
            assert!(request.starts_with(expected.as_bytes()));
            let response = format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            // An oversized response may be rejected before the body is consumed.
            let _ = socket.write_all(response.as_bytes()).await;
        });
        (url, task)
    }

    #[tokio::test]
    async fn relay_failure_wrong_round_and_forgery_do_not_change_the_request() {
        let good = mainnet_round_one();
        let mut wrong_round = good.clone();
        wrong_round.round = 2;
        let mut forged = good.clone();
        forged.signature = "aa".repeat(96);
        forged.randomness = hex::encode(Sha256::digest(hex::decode(&forged.signature).unwrap()));
        let (down, down_task) = relay(503, "unavailable".into(), 1).await;
        let (wrong, wrong_task) = relay(200, serde_json::to_string(&wrong_round).unwrap(), 1).await;
        let (fake, fake_task) = relay(200, serde_json::to_string(&forged).unwrap(), 1).await;
        let (valid, valid_task) = relay(200, serde_json::to_string(&good).unwrap(), 1).await;
        let client = DrandClient::new();
        let beacon = client
            .fetch_round_from_relays(1, &[down, wrong, fake, valid])
            .await
            .unwrap();
        assert_eq!(beacon.round, 1);
        assert_eq!(beacon.randomness, good.randomness);
        for task in [down_task, wrong_task, fake_task, valid_task] {
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn unavailable_oversized_and_unexpected_round_responses_fail_closed() {
        let client = DrandClient::new();
        for (status, body) in [
            (404, "not yet available".into()),
            (200, "x".repeat(MAX_BEACON_BYTES + 1)),
            (200, serde_json::to_string(&mainnet_round_one()).unwrap()),
        ] {
            let (url, task) = relay(status, body, 2).await;
            assert!(client.fetch_round_from_relays(2, &[url]).await.is_err());
            task.await.unwrap();
        }
        assert!(client.fetch_round_from_relays(1, &[]).await.is_err());
        assert!(client.fetch_round(0).await.is_err());
    }

    #[test]
    fn relay_configuration_is_bounded_and_cannot_embed_credentials_or_downgrade_remote_http() {
        let validate = |urls: &[&str]| {
            DrandClient::validate_relays(
                &urls.iter().map(|url| url.to_string()).collect::<Vec<_>>(),
            )
        };
        assert_eq!(
            validate(&["https://api.drand.sh/"]).unwrap(),
            vec![DRAND_MAINNET_URL]
        );
        assert!(validate(&["http://127.0.0.1:9000", "http://[::1]:9001"]).is_ok());
        for urls in [
            vec![],
            vec!["http://example.com"],
            vec!["file:///tmp/beacon"],
            vec!["https://user:secret@example.com"],
            vec!["https://example.com?token=secret"],
            vec!["https://example.com#fragment"],
            vec!["https://api.drand.sh", "https://api.drand.sh/"],
            vec!["https://example.com"; MAX_DRAND_RELAYS + 1],
        ] {
            assert!(
                validate(&urls).is_err(),
                "invalid relay configuration accepted"
            );
        }
        assert!(
            DrandClient::with_chain(DRAND_MAINNET_URL, DRAND_MAINNET_CHAIN)
                .with_relays(&[DRAND_MAINNET_URL.into()])
                .is_err()
        );
    }

    async fn read_relay_request(socket: &mut tokio::net::TcpStream) {
        use tokio::io::AsyncReadExt;
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
            assert!(request.len() <= 8192);
        }
        assert!(request
            .starts_with(format!("GET /{DRAND_MAINNET_CHAIN}/public/1 HTTP/1.1\r\n").as_bytes()));
    }

    #[tokio::test]
    async fn healthy_mirror_bypasses_stalled_body_and_cancels_the_losing_request() {
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let stalled = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let healthy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relays = [
            format!("http://{}", stalled.local_addr().unwrap()),
            format!("http://{}", healthy.local_addr().unwrap()),
        ];
        let (started, seen) = tokio::sync::oneshot::channel();
        let slow = tokio::spawn(async move {
            let (mut socket, _) = stalled.accept().await.unwrap();
            read_relay_request(&mut socket).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\n\r\n{")
                .await
                .unwrap();
            started.send(()).unwrap();
            let mut byte = [0];
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(3), socket.read(&mut byte))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
        });
        let good = mainnet_round_one();
        let expected = good.randomness.clone();
        let fast = tokio::spawn(async move {
            let (mut socket, _) = healthy.accept().await.unwrap();
            read_relay_request(&mut socket).await;
            seen.await.unwrap();
            let body = serde_json::to_string(&good).unwrap();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let client = DrandClient::mainnet()
            .unwrap()
            .with_relays(&relays)
            .unwrap();
        let beacon = tokio::time::timeout(Duration::from_secs(2), client.fetch_round(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(beacon.randomness, expected);
        slow.await.unwrap();
        fast.await.unwrap();
    }

    #[tokio::test]
    async fn configured_relays_do_not_follow_redirects() {
        use std::time::Duration;
        use tokio::io::AsyncWriteExt;
        let destination = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let redirect = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", redirect.local_addr().unwrap());
        let location = format!("http://{}/unexpected", destination.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = redirect.accept().await.unwrap();
            read_relay_request(&mut socket).await;
            socket.write_all(format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        });
        let client = DrandClient::mainnet().unwrap().with_relays(&[url]).unwrap();
        assert!(client.fetch_round(1).await.is_err());
        server.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), destination.accept())
                .await
                .is_err()
        );
    }

    // Immutable public vector from the pinned quicknet chain's /public/1.
    fn quicknet_round_one() -> DrandBeacon {
        DrandBeacon {
            round: 1,
            randomness: "1466a6cd24e327188770752f6134001c64d6efcc590ccc26b721611ad96f165a".into(),
            signature: "b55e7cb2d5c613ee0b2e28d6750aabbb78c39dcc96bd9d38c2c2e12198df95571de8e8e402a0cc48871c7089a2b3af4b".into(),
            previous_signature: None,
        }
    }

    #[test]
    fn authentic_quicknet_beacon_passes_bls_and_changed_round_fails() {
        let verifier = DrandVerifier::quicknet().unwrap();
        let mut beacon = quicknet_round_one();
        assert!(verifier.verify_beacon(&beacon).unwrap());
        beacon.round = 2;
        assert!(!verifier.verify_beacon(&beacon).unwrap());
    }

    #[test]
    fn warmed_signature_cache_preserves_full_beacon_validation() {
        let verifier = DrandVerifier::quicknet().unwrap();
        let beacon = quicknet_round_one();
        assert!(verifier.verify_beacon(&beacon).unwrap());
        assert!(verifier.verify_beacon(&beacon).unwrap());
        let mut changed = beacon.clone();
        changed.randomness = "00".repeat(32);
        assert!(!verifier.verify_beacon(&changed).unwrap());
        changed = beacon.clone();
        changed.previous_signature = Some("ab".repeat(48));
        assert!(verifier.verify_beacon(&changed).is_err());
        let other_key = blst::min_sig::SecretKey::key_gen(&[17; 32], &[]).unwrap();
        let other = DrandVerifier::new(
            &hex::encode(other_key.sk_to_pk().to_bytes()),
            DrandScheme::Unchained,
        )
        .unwrap();
        assert!(!other.verify_beacon(&beacon).unwrap());
        changed = beacon.clone();
        changed.signature = "00".repeat(48);
        changed.randomness = hex::encode(Sha256::digest([0; 48]));
        assert!(!matches!(verifier.verify_beacon(&changed), Ok(true)));
        assert!(verifier.verify_beacon(&beacon).unwrap());
    }

    #[test]
    fn chained_beacons_use_g1_public_keys_and_g2_signatures() {
        let secret = blst::min_pk::SecretKey::key_gen(&[7; 32], &[]).unwrap();
        let verifier = DrandVerifier::new(
            &hex::encode(secret.sk_to_pk().to_bytes()),
            DrandScheme::Chained,
        )
        .unwrap();
        let mut beacon = DrandBeacon {
            round: 5,
            randomness: String::new(),
            signature: String::new(),
            previous_signature: Some("ab".repeat(96)),
        };
        let message = verifier.beacon_message(&beacon).unwrap();
        let signature = secret
            .sign(
                &message,
                b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_NUL_",
                &[],
            )
            .to_bytes();
        beacon.signature = hex::encode(signature);
        beacon.randomness = hex::encode(Sha256::digest(signature));
        assert!(verifier.verify_beacon(&beacon).unwrap());
        beacon.previous_signature = Some("cd".repeat(96));
        assert!(!verifier.verify_beacon(&beacon).unwrap());
    }

    #[test]
    fn malformed_randomness_is_not_padded_or_truncated() {
        let mut beacon = quicknet_round_one();
        for length in [0, 1, 31, 33, 64] {
            beacon.randomness = "ab".repeat(length);
            assert!(DrandClient::beacon_to_random_output(&beacon).is_err());
        }
    }

    #[test]
    fn test_default_client_creation() {
        let client = DrandClient::new();
        assert_eq!(client.base_url, DRAND_MAINNET_URL);
        assert!(client.chain_hash.is_some());
        assert!(client.verifier.is_some());
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
    fn convenience_verifier_does_not_accept_hash_only_proofs() {
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

        assert!(!matches!(DrandClient::verify_beacon(&beacon), Ok(true)));
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
    fn self_consistent_signature_hash_is_not_a_valid_beacon() {
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

        assert!(!matches!(client.verify(&[], &output), Ok(true)));
        let custom = DrandClient::with_url("http://127.0.0.1");
        assert!(custom.verify(&[], &output).is_err());
        assert!(custom.maybe_verify(&quicknet_round_one()).is_err());
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
        let verifier = DrandVerifier::mainnet().unwrap();
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
        let verifier = DrandVerifier::mainnet().unwrap();
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
        // Deserialization failure also rejects the forged beacon.
        if let Ok(valid) = result {
            assert!(!valid, "forged beacon must not pass BLS verification");
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
