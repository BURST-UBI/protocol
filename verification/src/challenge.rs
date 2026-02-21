//! Challenge engine — any verified wallet can contest another's legitimacy.

use burst_types::{Timestamp, WalletAddress};
use serde::{Deserialize, Serialize};

/// Maximum time (in seconds) a challenge can remain open before auto-expiring.
/// 7 days. If verifiers fail to vote in time, the challenge resolves in favor
/// of the challenged wallet.
pub const CHALLENGE_TIMEOUT_SECS: u64 = 7 * 24 * 3600;

/// An active challenge against a wallet.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Challenge {
    pub challenger: WalletAddress,
    pub target: WalletAddress,
    pub stake_amount: u128,
    pub initiated_at: Timestamp,
}

pub struct ChallengeEngine;

impl ChallengeEngine {
    /// Initiate a challenge against a target wallet.
    ///
    /// The challenger stakes BRN (handled by BRN engine externally).
    pub fn initiate(
        &self,
        challenger: WalletAddress,
        target: WalletAddress,
        stake_amount: u128,
        now: Timestamp,
    ) -> Challenge {
        Challenge {
            challenger,
            target,
            stake_amount,
            initiated_at: now,
        }
    }

    /// Whether a challenge has timed out (auto-resolves in favor of target).
    pub fn is_timed_out(&self, challenge: &Challenge, now: Timestamp) -> bool {
        let elapsed = now
            .as_secs()
            .saturating_sub(challenge.initiated_at.as_secs());
        elapsed >= CHALLENGE_TIMEOUT_SECS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> WalletAddress {
        WalletAddress::new(&format!("brst_{s}"))
    }

    #[test]
    fn initiate_challenge() {
        let engine = ChallengeEngine;
        let c = engine.initiate(
            addr("challenger"),
            addr("target"),
            500,
            Timestamp::new(1000),
        );
        assert_eq!(c.challenger, addr("challenger"));
        assert_eq!(c.target, addr("target"));
        assert_eq!(c.stake_amount, 500);
        assert_eq!(c.initiated_at, Timestamp::new(1000));
    }

    #[test]
    fn challenge_not_timed_out_before_deadline() {
        let engine = ChallengeEngine;
        let c = engine.initiate(addr("a"), addr("b"), 100, Timestamp::new(1000));
        let still_within = Timestamp::new(1000 + CHALLENGE_TIMEOUT_SECS - 1);
        assert!(!engine.is_timed_out(&c, still_within));
    }

    #[test]
    fn challenge_timed_out_at_exactly_deadline() {
        let engine = ChallengeEngine;
        let c = engine.initiate(addr("a"), addr("b"), 100, Timestamp::new(0));
        let at_deadline = Timestamp::new(CHALLENGE_TIMEOUT_SECS);
        assert!(engine.is_timed_out(&c, at_deadline));
    }

    #[test]
    fn challenge_timed_out_well_past_deadline() {
        let engine = ChallengeEngine;
        let c = engine.initiate(addr("a"), addr("b"), 100, Timestamp::new(0));
        let way_past = Timestamp::new(CHALLENGE_TIMEOUT_SECS * 10);
        assert!(engine.is_timed_out(&c, way_past));
    }
}
