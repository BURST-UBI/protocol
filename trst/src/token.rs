//! TRST token representation.

use burst_types::{Timestamp, TrstState, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// A TRST token — the fundamental unit of transferable currency.
///
/// Each token tracks its full provenance via `origin` and `link`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrstToken {
    /// Unique identifier for this token instance (hash of the creating tx).
    pub id: TxHash,

    /// Amount of TRST in this token.
    pub amount: u128,

    /// Hash of the original burn transaction that created this TRST.
    /// Determines expiry date and the original minter (for revocation).
    pub origin: TxHash,

    /// Hash of the immediately preceding transaction.
    /// For the first send after burn, `link == origin`.
    pub link: TxHash,

    /// The wallet that currently holds this token.
    pub holder: WalletAddress,

    /// Timestamp of the origin burn (copied from origin tx).
    pub origin_timestamp: Timestamp,

    /// Current state of this token.
    pub state: TrstState,

    /// The wallet that originally burned BRN to create this TRST.
    pub origin_wallet: WalletAddress,

    /// For merged tokens: the proportions from each origin.
    /// Maps origin TxHash → proportion of this token's amount from that origin.
    /// Empty for non-merged tokens (100% from `self.origin`).
    pub origin_proportions: Vec<OriginProportion>,
}

/// Tracks what fraction of a merged token came from a specific origin.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OriginProportion {
    pub origin: TxHash,
    pub origin_wallet: WalletAddress,
    pub amount: u128,
}

impl TrstToken {
    /// Check whether this token has expired given the current time and expiry period.
    pub fn is_expired(&self, now: Timestamp, expiry_secs: u64) -> bool {
        self.origin_timestamp.has_expired(expiry_secs, now)
    }

    /// Whether this token can be transferred right now.
    pub fn is_transferable(&self, now: Timestamp, expiry_secs: u64) -> bool {
        self.state.is_transferable() && !self.is_expired(now, expiry_secs)
    }

    /// The earliest expiry among all origin proportions (for merged tokens).
    pub fn earliest_expiry(&self, expiry_secs: u64) -> Timestamp {
        // For merged tokens, the expiry is the earliest origin timestamp + expiry period.
        // For simple tokens, it's just origin_timestamp + expiry_secs.
        Timestamp::new(self.origin_timestamp.as_secs().saturating_add(expiry_secs))
    }
}
