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
    pub fn select(
        &self,
        _vrf: &dyn VrfProvider,
        _eligible_verifiers: &[WalletAddress],
        _seed_context: &[u8],
        _count: usize,
    ) -> Vec<WalletAddress> {
        todo!("use VRF randomness to deterministically select verifiers")
    }
}
