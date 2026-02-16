//! Challenge engine — any verified wallet can contest another's legitimacy.

use burst_types::{Timestamp, WalletAddress};
use serde::{Deserialize, Serialize};

/// An active challenge against a wallet.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Challenge {
    pub challenger: WalletAddress,
    pub target: WalletAddress,
    pub stake_amount: u128,
    pub initiated_at: Timestamp,
}

/// The outcome of a resolved challenge.
#[derive(Clone, Debug)]
pub enum ChallengeOutcome {
    /// Fraud confirmed: wallet is revoked, challenger is rewarded.
    FraudConfirmed {
        /// TRST reward amount for the challenger.
        reward: u128,
    },
    /// Challenge rejected: challenger loses their stake.
    Rejected,
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

    /// Resolve a challenge based on the re-verification vote outcome.
    pub fn resolve(
        &self,
        _challenge: &Challenge,
        _fraud_confirmed: bool,
    ) -> ChallengeOutcome {
        todo!("compute reward if fraud, forfeit challenger stake if not")
    }
}
