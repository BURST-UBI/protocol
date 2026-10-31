//! State enums for wallets and TRST tokens.

use serde::{Deserialize, Serialize};

/// Constant-size reference to a token's composition. Ancestry remains in the
/// merger graph; a restored split is a pure share of its named burn origin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrstBasis {
    #[default]
    Entire,
    Remainder(crate::TxHash),
    Single(crate::TxHash),
}

/// Namespaces for retained TRST references. These identify dependencies, not
/// authenticated history or permission to delete records. Token IDs can be
/// derived split/change identities and must not be interpreted as block hashes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrstReference<'a> {
    Account(&'a crate::WalletAddress),
    Block(crate::BlockHash),
    Token(crate::TxHash),
    Graph(crate::TxHash),
    Revision(crate::TxHash),
}

impl TrstBasis {
    pub fn visit_retention_references<'a, B>(
        self,
        mut visit: impl FnMut(TrstReference<'a>) -> std::ops::ControlFlow<B>,
    ) -> std::ops::ControlFlow<B> {
        match self {
            Self::Entire => std::ops::ControlFlow::Continue(()),
            Self::Remainder(hash) => visit(TrstReference::Revision(hash)),
            Self::Single(hash) => visit(TrstReference::Graph(hash)),
        }
    }

    pub fn key(self) -> [u8; 33] {
        let mut key = [0; 33];
        match self {
            Self::Entire => {}
            Self::Remainder(hash) => {
                key[0] = 1;
                key[1..].copy_from_slice(hash.as_bytes());
            }
            Self::Single(hash) => {
                key[0] = 2;
                key[1..].copy_from_slice(hash.as_bytes());
            }
        }
        key
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrstRevocationChunk {
    pub amount: u128,
    pub restored: bool,
}

/// Reason for a challenge. An upheld fraud challenge revokes originated TRST;
/// an upheld inactivity challenge deactivates the wallet without revoking those holdings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ChallengeReason {
    /// The wallet is alleged to represent a fake or duplicate identity.
    #[default]
    Fraud,
    /// The holder is alleged to be dead or inactive. This reason does not
    /// classify their previously issued TRST as fraudulent.
    Inactivity,
}

/// Inactivity marker in a Challenge block's `origin` field. `from_origin`
/// interprets any other value as Fraud; `to_origin` encodes Fraud as zero.
/// The field is part of the signed block, not an unsigned annotation.
pub const INACTIVITY_CHALLENGE_MARKER: [u8; 32] = [0xCC; 32];

impl ChallengeReason {
    /// Decode the challenge reason from a Challenge block's `origin` bytes.
    pub fn from_origin(origin: &[u8; 32]) -> Self {
        if *origin == INACTIVITY_CHALLENGE_MARKER {
            ChallengeReason::Inactivity
        } else {
            ChallengeReason::Fraud
        }
    }

    /// The `origin` bytes a Challenge block should carry for this reason.
    pub fn to_origin(self) -> [u8; 32] {
        match self {
            ChallengeReason::Fraud => [0u8; 32],
            ChallengeReason::Inactivity => INACTIVITY_CHALLENGE_MARKER,
        }
    }
}

/// The verification state of a wallet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WalletState {
    /// Wallet exists but has not been verified.
    Unverified,
    /// Endorsement threshold met; awaiting verifier votes.
    Endorsed,
    /// Verification voting is in progress.
    Voting,
    /// Wallet has been accepted by the identity-verification rules.
    Verified,
    /// Wallet is under active challenge review.
    Challenged,
    /// Wallet was found fraudulent; all originated TRST revoked.
    Revoked,
    /// Inactive verified identity. Accrual and verified spending rights stop;
    /// originated TRST is not revoked by deactivation.
    Deactivated,
}

impl WalletState {
    /// A pending challenge retains verified rights until its outcome is applied.
    pub fn is_verified(&self) -> bool {
        matches!(self, Self::Verified | Self::Challenged)
    }

    /// Whether this wallet has verified spending rights. Receiving TRST is
    /// checked separately and does not require this predicate.
    pub fn can_transact(&self) -> bool {
        matches!(self, Self::Verified | Self::Challenged)
    }

    /// Whether BRN accrual is active.
    pub fn accrues_brn(&self) -> bool {
        matches!(self, Self::Verified | Self::Challenged)
    }

    /// Whether this wallet can participate in governance votes.
    pub fn can_vote(&self) -> bool {
        self.is_verified()
    }
}

/// The state of a TRST token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrstState {
    /// Active stored state; transfer still requires expiry and ledger checks.
    Active,
    /// Sent but not yet received; cannot be transferred while pending.
    Pending,
    /// Expired and non-transferable; retained separately from active holdings.
    Expired,
    /// Revoked and non-transferable.
    Revoked,
}

impl TrstState {
    /// Check stored state only. This does not check age, ownership or provenance.
    pub fn is_transferable(&self) -> bool {
        matches!(self, Self::Active)
    }

    /// Whether this TRST is in a revoked state.
    pub fn is_revoked(&self) -> bool {
        matches!(self, Self::Revoked)
    }

    /// Whether this TRST is pending acceptance.
    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deactivated_cannot_transact() {
        assert!(!WalletState::Deactivated.can_transact());
    }

    #[test]
    fn deactivated_does_not_accrue_brn() {
        assert!(!WalletState::Deactivated.accrues_brn());
    }

    #[test]
    fn deactivated_cannot_vote() {
        assert!(!WalletState::Deactivated.can_vote());
    }

    #[test]
    fn verified_can_transact_and_accrue() {
        assert!(WalletState::Verified.can_transact());
        assert!(WalletState::Verified.accrues_brn());
        assert!(WalletState::Verified.can_vote());
    }

    #[test]
    fn accusation_alone_does_not_remove_verified_rights() {
        assert!(WalletState::Challenged.is_verified());
        assert!(WalletState::Challenged.can_transact());
        assert!(WalletState::Challenged.accrues_brn());
        assert!(WalletState::Challenged.can_vote());
    }

    #[test]
    fn revoked_cannot_transact() {
        assert!(!WalletState::Revoked.can_transact());
        assert!(!WalletState::Revoked.accrues_brn());
        assert!(!WalletState::Revoked.can_vote());
    }
}
