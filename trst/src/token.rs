//! TRST token representation.

use burst_types::{Timestamp, TrstState, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// A TRST token — the fundamental unit of transferable currency.
///
/// Each token tracks its provenance via `origin` and `link`. Per
/// IMPLEMENTATION_DECISIONS 6.17(b), merged tokens do NOT carry flattened
/// ancestry — their `origin` is the merge transaction hash, and constituent
/// origins are discovered by following the merger graph.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrstToken {
    /// Unique identifier for this token instance (hash of the creating tx).
    pub id: TxHash,

    /// Amount of TRST in this token.
    pub amount: u128,

    /// Hash of the original burn transaction that created this TRST.
    /// For merged tokens, this is the merge transaction hash (whitepaper:
    /// "Future transactions from the merged token use the merge transaction's
    /// hash as the new origin").
    pub origin: TxHash,

    /// Hash of the immediately preceding transaction.
    /// For the first send after burn, `link == origin`.
    pub link: TxHash,

    /// The wallet that currently holds this token.
    pub holder: WalletAddress,

    /// Timestamp of the transaction that created this token instance
    /// (the burn, or the merge for merged tokens).
    pub origin_timestamp: Timestamp,

    /// Effective origin timestamp for expiry computation.
    /// For simple tokens, equals `origin_timestamp`.
    /// For merged tokens, equals `min(constituent effective_origin_timestamps)`,
    /// ensuring the merged token expires at the earliest constituent's expiry.
    pub effective_origin_timestamp: Timestamp,

    /// Current state of this token.
    pub state: TrstState,

    /// The wallet that originally burned BRN to create this TRST.
    /// For merged tokens, the wallet that performed the merge (a merge is a
    /// self-operation; constituent originators live in the merger graph).
    pub origin_wallet: WalletAddress,

    /// When `state == Revoked`: the burn origin whose revocation caused it.
    /// This is what makes un-revocation (6.15b) O(k) — restoring an origin
    /// restores exactly the tokens tagged with it. `None` otherwise.
    #[serde(default)]
    pub revoked_origin: Option<TxHash>,
}

impl TrstToken {
    /// Check whether this token has expired given the current time and expiry period.
    ///
    /// Expiry is always computed from the effective origin timestamp and the
    /// CURRENT governance expiry period (6.9) — never cached permanently, so a
    /// governance change can make non-transferable TRST transferable again.
    pub fn is_expired(&self, now: Timestamp, expiry_secs: u64) -> bool {
        self.effective_origin_timestamp
            .has_expired(expiry_secs, now)
    }

    /// Whether this token can be transferred right now.
    pub fn is_transferable(&self, now: Timestamp, expiry_secs: u64) -> bool {
        self.state.is_transferable() && !self.is_expired(now, expiry_secs)
    }

    /// The absolute expiry timestamp for this token.
    pub fn earliest_expiry(&self, expiry_secs: u64) -> Timestamp {
        Timestamp::new(
            self.effective_origin_timestamp
                .as_secs()
                .saturating_add(expiry_secs),
        )
    }

    /// Compute the current value of this token as a fraction of face value
    /// based on time remaining before expiry (value demurrage).
    ///
    /// Returns a value in `[0, 10_000]` basis points:
    /// - `10_000` = full face value (just minted or no expiry)
    /// - `0` = expired
    ///
    /// Uses linear decay (6.13a): `value_bps = (time_remaining / expiry_period) * 10_000`
    pub fn current_value_bps(&self, now: Timestamp, expiry_secs: u64) -> u64 {
        if expiry_secs == 0 || self.state == TrstState::Revoked {
            return 0;
        }
        if self.state == TrstState::Expired {
            return 0;
        }
        let age_secs = now
            .as_secs()
            .saturating_sub(self.effective_origin_timestamp.as_secs());
        if age_secs >= expiry_secs {
            return 0;
        }
        let remaining = expiry_secs - age_secs;
        ((remaining as u128 * 10_000) / expiry_secs as u128) as u64
    }

    /// Compute the demurrage-adjusted effective value of this token.
    /// A 1000 TRST token halfway through its expiry period has
    /// an effective value of 500 TRST.
    pub fn effective_value(&self, now: Timestamp, expiry_secs: u64) -> u128 {
        let bps = self.current_value_bps(now, expiry_secs) as u128;
        self.amount.saturating_mul(bps) / 10_000
    }
}
