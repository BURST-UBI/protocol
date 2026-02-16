//! Phase 1: drand — external randomness beacon from the League of Entropy.
//!
//! drand emits publicly verifiable random values every 30 seconds.
//! Minimal integration code. This is the bootstrap approach.

use crate::{RandomOutput, VrfError, VrfProvider};

/// drand client — fetches randomness from the League of Entropy beacon.
pub struct DrandClient {
    /// Base URL of the drand HTTP relay (e.g., "https://api.drand.sh").
    pub relay_url: String,
    /// The chain hash identifying which drand network to use.
    pub chain_hash: String,
}

impl DrandClient {
    pub fn new(relay_url: impl Into<String>, chain_hash: impl Into<String>) -> Self {
        Self {
            relay_url: relay_url.into(),
            chain_hash: chain_hash.into(),
        }
    }

    /// Fetch the latest round from drand.
    pub async fn fetch_latest(&self) -> Result<RandomOutput, VrfError> {
        todo!("GET {}/v1/public/latest -> parse JSON -> RandomOutput", self.relay_url)
    }

    /// Fetch a specific round from drand.
    pub async fn fetch_round(&self, _round: u64) -> Result<RandomOutput, VrfError> {
        todo!("GET {}/v1/public/{} -> parse JSON -> RandomOutput", self.relay_url, _round)
    }
}

impl VrfProvider for DrandClient {
    fn get_randomness(&self, _context: &[u8]) -> Result<RandomOutput, VrfError> {
        todo!("blocking fetch or cached latest round")
    }

    fn verify(&self, _context: &[u8], _output: &RandomOutput) -> Result<bool, VrfError> {
        todo!("verify BLS signature against drand public key")
    }

    fn name(&self) -> &str {
        "drand"
    }
}
