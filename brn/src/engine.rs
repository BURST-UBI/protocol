//! Core BRN computation engine.

use crate::error::BrnError;
use crate::stake::{Stake, StakeId, StakeKind};
use crate::state::{BrnWalletState, RateSegment};
use burst_types::Timestamp;

/// The BRN engine — computes balances, records burns, manages stakes.
pub struct BrnEngine {
    next_stake_id: StakeId,
}

impl BrnEngine {
    pub fn new() -> Self {
        Self { next_stake_id: 1 }
    }

    /// Compute the available BRN balance for a wallet at a given time.
    ///
    /// `BRN(w) = Σ(rate_i × duration_i) − total_burned − total_staked`
    pub fn compute_balance(&self, state: &BrnWalletState, now: Timestamp) -> u128 {
        state.available_balance(now)
    }

    /// Record a BRN burn (consuming BRN to mint TRST for a provider).
    ///
    /// Returns an error if the wallet has insufficient available BRN.
    pub fn record_burn(
        &self,
        state: &mut BrnWalletState,
        amount: u128,
        now: Timestamp,
    ) -> Result<(), BrnError> {
        let available = state.available_balance(now);
        if available < amount {
            return Err(BrnError::InsufficientBalance {
                needed: amount,
                available,
            });
        }
        state.total_burned += amount;
        Ok(())
    }

    /// Lock BRN as a temporary stake (for verification or challenge).
    ///
    /// Returns the stake ID. The BRN is locked until the stake is resolved.
    pub fn stake(
        &mut self,
        state: &mut BrnWalletState,
        amount: u128,
        kind: StakeKind,
        now: Timestamp,
    ) -> Result<Stake, BrnError> {
        let available = state.available_balance(now);
        if available < amount {
            return Err(BrnError::InsufficientBalance {
                needed: amount,
                available,
            });
        }
        state.total_staked += amount;
        let stake = Stake {
            id: self.next_stake_id,
            amount,
            kind,
            created_at: now,
            resolved: false,
        };
        self.next_stake_id += 1;
        Ok(stake)
    }

    /// Return a stake (successful outcome — BRN is unlocked).
    pub fn return_stake(
        &self,
        state: &mut BrnWalletState,
        stake: &mut Stake,
    ) -> Result<(), BrnError> {
        if stake.resolved {
            return Err(BrnError::StakeAlreadyResolved(stake.id));
        }
        state.total_staked = state.total_staked.saturating_sub(stake.amount);
        stake.resolved = true;
        Ok(())
    }

    /// Forfeit a stake (staker voted against the outcome — BRN is lost).
    pub fn forfeit_stake(
        &self,
        state: &mut BrnWalletState,
        stake: &mut Stake,
    ) -> Result<(), BrnError> {
        if stake.resolved {
            return Err(BrnError::StakeAlreadyResolved(stake.id));
        }
        // Staked BRN is gone: move from staked to burned.
        state.total_staked = state.total_staked.saturating_sub(stake.amount);
        state.total_burned += stake.amount;
        stake.resolved = true;
        Ok(())
    }

    /// Apply a rate change at a specific timestamp.
    ///
    /// Closes the current rate segment and opens a new one with the new rate.
    pub fn apply_rate_change(
        &self,
        state: &mut BrnWalletState,
        new_rate: u128,
        change_at: Timestamp,
    ) {
        if let Some(current) = state.rate_segments.last_mut() {
            current.end = Some(change_at);
        }
        state.rate_segments.push(RateSegment {
            rate: new_rate,
            start: change_at,
            end: None,
        });
    }
}

impl Default for BrnEngine {
    fn default() -> Self {
        Self::new()
    }
}
