//! Endorsement engine — manages the endorsement phase of verification.

use crate::error::VerificationError;
use crate::state::{Endorsement, VerificationState};
use burst_types::{Timestamp, WalletAddress};

pub struct EndorsementEngine;

impl EndorsementEngine {
    /// Submit an endorsement for a target wallet.
    ///
    /// The endorser permanently burns their BRN (handled by BRN engine externally).
    pub fn submit_endorsement(
        &self,
        state: &mut VerificationState,
        endorser: WalletAddress,
        burn_amount: u128,
        now: Timestamp,
    ) -> Result<(), VerificationError> {
        state.endorsements.push(Endorsement {
            endorser,
            burn_amount,
            timestamp: now,
        });
        Ok(())
    }

    /// Check whether the endorsement threshold has been met.
    pub fn check_threshold(&self, state: &VerificationState, threshold: u32) -> bool {
        state.endorsements.len() as u32 >= threshold
    }

    /// Total BRN burned in endorsements for this wallet.
    pub fn total_burned(&self, state: &VerificationState) -> u128 {
        state.endorsements.iter().map(|e| e.burn_amount).sum()
    }
}
