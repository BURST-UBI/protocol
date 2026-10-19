use super::*;

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

/// Owns the exact bytes authenticated by a worker. No mutable access or public
/// constructor: a hash-only cache would incorrectly trust changed signatures.
pub(crate) struct AuthenticatedBlock {
    block: StateBlock,
    rules: AuthenticationRules,
    result: Result<(), String>,
    authentication_time: std::time::Duration,
    pub(crate) retention: Option<crate::block_retention::Slot>,
}

impl AuthenticatedBlock {
    pub(crate) fn block(&self) -> &StateBlock {
        &self.block
    }

    pub(crate) fn matches(&self, rules: &AuthenticationRules) -> bool {
        self.rules == *rules
    }

    pub(crate) fn result(&self) -> &Result<(), String> {
        &self.result
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

    pub(crate) fn authenticate(&self, block: StateBlock) -> AuthenticatedBlock {
        let start = std::time::Instant::now();
        let result = self.check(&block);
        AuthenticatedBlock {
            block,
            rules: self.clone(),
            result,
            authentication_time: start.elapsed(),
            retention: None,
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
