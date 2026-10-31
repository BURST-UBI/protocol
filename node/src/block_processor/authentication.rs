//! Immutable worker checks: format, message size, content hash, work and signature.
//! Payment epoch, timestamp, ledger dependencies and economic admission are
//! checked later. A successful result alone does not admit or confirm a block.

use super::*;

/// Select the account key, or the genesis key for Epoch/GovernanceActivation.
/// This selection does not establish that the operation is enabled or authorized.
pub(crate) fn signing_key(
    block: &StateBlock,
    genesis: &WalletAddress,
) -> Result<PublicKey, String> {
    let signer = if matches!(
        block.block_type,
        BlockType::Epoch | BlockType::GovernanceActivation
    ) {
        genesis
    } else {
        &block.account
    };
    decode_address(signer.as_str())
        .map(PublicKey)
        .ok_or_else(|| "unable to decode account address for signature verification".into())
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AuthenticationRules {
    genesis_account: WalletAddress,
    verify_signatures: bool,
    work_threshold: u64,
}

/// Owns a worker authentication result, including failures, and its exact input.
/// No mutable block access or public constructor: a hash-only cache would
/// incorrectly trust changed signatures.
pub(crate) struct BlockAuthentication {
    block: StateBlock,
    rules: AuthenticationRules,
    result: Result<(), String>,
    authentication_time: std::time::Duration,
    // Held until this candidate leaves admission, including durable publication.
    pub(crate) _preparation_reservation: Option<std::sync::Arc<tokio::sync::OwnedMutexGuard<()>>>,
    pub(crate) preparation_time: std::time::Duration,
    pub(crate) preparation_fault: Option<String>,
    pub(crate) prepared: Option<crate::economic_candidate::prepared::PreparedPayment>,
    pub(crate) retention: Option<crate::block_retention::Slot>,
    pub(crate) growth_fork_content: Option<std::sync::Arc<Vec<u8>>>,
    pub(crate) _growth_retry: Option<burst_store_lmdb::GrowthRetry>,
    pub(crate) _in_flight_payload: Option<std::sync::Arc<crate::block_validation::InFlightPayload>>,
}

impl BlockAuthentication {
    pub(crate) fn block(&self) -> &StateBlock {
        &self.block
    }

    pub(crate) fn matches(&self, rules: &AuthenticationRules) -> bool {
        self.rules == *rules
    }

    pub(crate) fn result(&self) -> &Result<(), String> {
        &self.result
    }

    pub(crate) fn worker_time(&self) -> std::time::Duration {
        self.authentication_time + self.preparation_time
    }

    pub(crate) fn authentication_time(&self) -> std::time::Duration {
        self.authentication_time
    }
}

impl AuthenticationRules {
    pub(super) fn new(
        genesis_account: WalletAddress,
        verify_signatures: bool,
        work_threshold: u64,
    ) -> Self {
        Self {
            genesis_account,
            verify_signatures,
            work_threshold,
        }
    }

    pub(crate) fn authenticate(&self, block: StateBlock) -> BlockAuthentication {
        let start = std::time::Instant::now();
        let result = self.check(&block);
        BlockAuthentication {
            block,
            rules: self.clone(),
            result,
            authentication_time: start.elapsed(),
            retention: None,
            _growth_retry: None,
            growth_fork_content: None,
            _in_flight_payload: None,
            prepared: None,
            _preparation_reservation: None,
            preparation_time: std::time::Duration::ZERO,
            preparation_fault: None,
        }
    }

    fn check(&self, block: &StateBlock) -> Result<(), String> {
        block.validate_format().map_err(str::to_owned)?;
        if !block.fits_p2p_message() {
            return Err("block exceeds P2P message limit".into());
        }
        if block.hash != block.compute_hash() {
            return Err("block hash does not match its contents".into());
        }
        // Reject cheap-work floods before expensive Ed25519 verification.
        // Admission discards this result if the installed threshold changed.
        if !block.verify_work(self.work_threshold) {
            return Err("proof-of-work does not meet minimum difficulty".into());
        }
        self.check_signature(block)
    }

    /// Reject zero signatures even when cryptographic verification is disabled.
    /// The flag controls Ed25519 verification only; it does not bypass format/work.
    pub(super) fn check_signature(&self, block: &StateBlock) -> Result<(), String> {
        if block.signature == Signature([0; 64]) {
            return Err("signature is zero (unsigned block)".into());
        }
        if !self.verify_signatures {
            return Ok(());
        }
        let public = signing_key(block, &self.genesis_account)?;
        if !verify_signature(block.hash.as_bytes(), &block.signature, &public) {
            return Err("invalid signature".into());
        }
        Ok(())
    }
}
