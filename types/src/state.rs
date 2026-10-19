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

impl TrstBasis {
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

/// Why a wallet is being challenged. The same challenge procedure (stake →
/// re-verification → verifier vote) removes a verified wallet either way; the
/// reason only decides what happens to the wallet's TRST when the challenge is
/// upheld (whitepaper §Handling Bad Actors vs §Unverification Without Revocation).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ChallengeReason {
    /// The wallet is a fake / non-unique human. Upheld → wallet unverified AND
    /// all TRST it originated is revoked (via the merger graph).
    #[default]
    Fraud,
    /// The wallet's holder is dead / prolonged-inactive (a real person who
    /// legitimately earned their TRST). Upheld → wallet deactivated (BRN accrual
    /// and transaction rights stop) but TRST is NOT revoked. The challenger is
    /// still rewarded for catching it (stake returned + correct verifiers paid).
    Inactivity,
}

/// Sentinel placed in a Challenge block's `origin` field to mark it as an
/// Inactivity (benign) challenge. A Challenge block never references a TRST
/// origin, so this field is otherwise unused — encoding the reason here needs no
/// new block field and no change to any block hash. Any other value (including
/// the default zero) means a Fraud challenge.
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
    /// Wallet has been verified as a unique human.
    Verified,
    /// Wallet is under active challenge review.
    Challenged,
    /// Wallet was found fraudulent; all originated TRST revoked.
    Revoked,
    /// Deactivated — formerly verified, now inactive. BRN stops accruing,
    /// transactions blocked, but originated TRST is NOT revoked.
    /// Used for dead wallets, extended inactivity, or voluntary deactivation.
    Deactivated,
}

impl WalletState {
    /// A pending accusation does not revoke verified-human rights (decisions
    /// 15.11 and 17.3). Only a resolved outcome may do that.
    pub fn is_verified(&self) -> bool {
        matches!(self, Self::Verified | Self::Challenged)
    }

    /// Whether this wallet is allowed to transact (send/receive).
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
    /// Active and transferable.
    Active,
    /// Pending — sent but not yet received; non-transferable until accepted.
    Pending,
    /// Expired — non-transferable but visible (virtue points / reputation).
    Expired,
    /// Revoked — originating wallet found fraudulent; immediately non-transferable.
    Revoked,
}

impl TrstState {
    /// Whether this TRST can be transferred.
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
