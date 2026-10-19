use super::*;
use std::sync::OnceLock;

/// Immutable chain rules for local replay. No HTTP client, relay configuration
/// or wall clock participates in checking a certified historical beacon.
pub struct DrandPolicy {
    pub(super) verifier: DrandVerifier,
    info: ChainInfo,
}

impl DrandPolicy {
    pub fn mainnet() -> Result<&'static Self, VrfError> {
        static MAINNET: OnceLock<Result<DrandPolicy, String>> = OnceLock::new();
        MAINNET
            .get_or_init(|| {
                let verifier = DrandVerifier::mainnet().map_err(|error| error.to_string())?;
                let info = ChainInfo {
                    public_key: verifier.pub_key_bytes.clone(),
                    period: 30,
                    genesis_time: 1595431050,
                    chain_hash: DRAND_MAINNET_CHAIN.into(),
                    scheme: DrandScheme::Chained,
                };
                Ok(Self { verifier, info })
            })
            .as_ref()
            .map_err(|error| VrfError::InvalidPublicKey(error.clone()))
    }

    pub fn chain_info(&self) -> &ChainInfo {
        &self.info
    }

    pub fn verify(&self, output: &RandomOutput) -> Result<bool, VrfError> {
        verify_output(output, |beacon| self.verifier.require_beacon(beacon))
    }
}

pub(super) fn verify_output(
    output: &RandomOutput,
    verify: impl FnOnce(&DrandBeacon) -> Result<(), VrfError>,
) -> Result<bool, VrfError> {
    let beacon: DrandBeacon = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_BEACON_BYTES as u64)
        .reject_trailing_bytes()
        .deserialize(&output.proof)
        .map_err(|error| VrfError::InvalidProof(error.to_string()))?;
    if beacon.round != output.round
        || hex::decode(&beacon.randomness).ok().as_deref() != Some(output.value.as_slice())
    {
        return Ok(false);
    }
    verify(&beacon)?;
    Ok(true)
}
