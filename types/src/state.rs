//! State enums for wallets and TRST tokens.

use serde::{Deserialize, Serialize};

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
}

impl WalletState {
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
        matches!(self, Self::Verified)
    }
}

/// The state of a TRST token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrstState {
    /// Active and transferable.
    Active,
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
}
