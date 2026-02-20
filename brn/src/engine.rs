//! Core BRN computation engine.

use crate::error::BrnError;
use crate::stake::{Stake, StakeId, StakeKind};
use crate::state::{BrnWalletState, RateHistory};
use burst_types::{Timestamp, WalletAddress};
use std::collections::HashMap;

/// The BRN engine — computes balances, records burns, manages stakes.
///
/// Rate segments are stored ONCE in the global `rate_history`, not per-wallet.
/// A governance rate change is O(1) — just appends one segment to the global list.
pub struct BrnEngine {
    next_stake_id: StakeId,
    /// Tracked wallet states for global operations.
    pub wallets: HashMap<WalletAddress, BrnWalletState>,
    /// Global rate history shared by all wallets.
    pub rate_history: RateHistory,
}

impl BrnEngine {
    pub fn new() -> Self {
        Self {
            next_stake_id: 1,
            wallets: HashMap::new(),
            rate_history: RateHistory::default(),
        }
    }

    /// Create a new engine with an explicit initial rate and genesis time.
    pub fn with_rate(initial_rate: u128, genesis: Timestamp) -> Self {
        Self {
            next_stake_id: 1,
            wallets: HashMap::new(),
            rate_history: RateHistory::new(initial_rate, genesis),
        }
    }

    /// Compute the available BRN balance for a wallet at a given time.
    ///
    /// `BRN(w) = Σ(rate_i × duration_i) − total_burned − total_staked`
    ///
    /// All values are deterministic integers: rates are `u128` raw units per
    /// second, timestamps are `u64` whole seconds, and all arithmetic is
    /// integer multiply/add/sub.
    pub fn compute_balance(&self, state: &BrnWalletState, now: Timestamp) -> u128 {
        state.available_balance(&self.rate_history, now)
    }

    /// Compute the available BRN balance with checked arithmetic.
    pub fn compute_balance_checked(
        &self,
        state: &BrnWalletState,
        now: Timestamp,
    ) -> Result<u128, BrnError> {
        state
            .available_balance_checked(&self.rate_history, now)
            .ok_or(BrnError::Overflow)
    }

    /// Record a BRN burn (consuming BRN to mint TRST for a provider).
    pub fn record_burn(
        &self,
        state: &mut BrnWalletState,
        amount: u128,
        now: Timestamp,
    ) -> Result<(), BrnError> {
        if amount == 0 {
            return Err(BrnError::ZeroAmount);
        }
        let available = state
            .available_balance_checked(&self.rate_history, now)
            .ok_or(BrnError::Overflow)?;
        if available < amount {
            return Err(BrnError::InsufficientBalance {
                needed: amount,
                available,
            });
        }
        state.total_burned = state
            .total_burned
            .checked_add(amount)
            .ok_or(BrnError::Overflow)?;
        Ok(())
    }

    /// Lock BRN as a temporary stake (for verification or challenge).
    pub fn stake(
        &mut self,
        staker: &WalletAddress,
        state: &mut BrnWalletState,
        amount: u128,
        kind: StakeKind,
        now: Timestamp,
    ) -> Result<Stake, BrnError> {
        if amount == 0 {
            return Err(BrnError::ZeroAmount);
        }
        let available = state
            .available_balance_checked(&self.rate_history, now)
            .ok_or(BrnError::Overflow)?;
        if available < amount {
            return Err(BrnError::InsufficientBalance {
                needed: amount,
                available,
            });
        }
        state.total_staked = state
            .total_staked
            .checked_add(amount)
            .ok_or(BrnError::Overflow)?;
        let stake = Stake {
            id: self.next_stake_id,
            staker: staker.clone(),
            amount,
            kind,
            created_at: now,
            resolved: false,
        };
        self.next_stake_id = self
            .next_stake_id
            .checked_add(1)
            .ok_or(BrnError::Overflow)?;
        Ok(stake)
    }

    /// Return a stake (successful outcome — BRN is unlocked).
    pub fn return_stake(
        &self,
        staker: &WalletAddress,
        state: &mut BrnWalletState,
        stake: &mut Stake,
    ) -> Result<(), BrnError> {
        if stake.resolved {
            return Err(BrnError::StakeAlreadyResolved(stake.id));
        }
        if stake.staker != *staker {
            return Err(BrnError::Other(format!(
                "stake {} belongs to {}, not {}",
                stake.id, stake.staker, staker
            )));
        }
        state.total_staked = state
            .total_staked
            .checked_sub(stake.amount)
            .ok_or(BrnError::Overflow)?;
        stake.resolved = true;
        Ok(())
    }

    /// Forfeit a stake (staker voted against the outcome — BRN is lost).
    pub fn forfeit_stake(
        &self,
        staker: &WalletAddress,
        state: &mut BrnWalletState,
        stake: &mut Stake,
    ) -> Result<(), BrnError> {
        if stake.resolved {
            return Err(BrnError::StakeAlreadyResolved(stake.id));
        }
        if stake.staker != *staker {
            return Err(BrnError::Other(format!(
                "stake {} belongs to {}, not {}",
                stake.id, stake.staker, staker
            )));
        }
        state.total_staked = state
            .total_staked
            .checked_sub(stake.amount)
            .ok_or(BrnError::Overflow)?;
        state.total_burned = state
            .total_burned
            .checked_add(stake.amount)
            .ok_or(BrnError::Overflow)?;
        stake.resolved = true;
        Ok(())
    }

    /// Apply a rate change at a specific timestamp — O(1).
    ///
    /// This is the key optimization: rate changes append to a single global
    /// history instead of iterating every wallet.
    pub fn apply_rate_change(
        &mut self,
        new_rate: u128,
        change_at: Timestamp,
    ) -> Result<(), BrnError> {
        self.rate_history.apply_rate_change(new_rate, change_at)
    }

    /// Register a wallet state for tracking.
    pub fn track_wallet(&mut self, address: WalletAddress, state: BrnWalletState) {
        self.wallets.insert(address, state);
    }

    /// Get a tracked wallet state.
    pub fn get_wallet(&self, address: &WalletAddress) -> Option<&BrnWalletState> {
        self.wallets.get(address)
    }

    /// Get a mutable reference to a tracked wallet state.
    pub fn get_wallet_mut(&mut self, address: &WalletAddress) -> Option<&mut BrnWalletState> {
        self.wallets.get_mut(address)
    }

    /// Stop BRN accrual for a wallet (e.g. on de-verification).
    pub fn deactivate_wallet(
        &mut self,
        address: &WalletAddress,
        at: Timestamp,
    ) -> Result<(), BrnError> {
        let state = self
            .wallets
            .get_mut(address)
            .ok_or(BrnError::WalletNotVerified)?;
        state.stop_accrual(at);
        Ok(())
    }

    /// Resume BRN accrual for a wallet (e.g. on re-verification).
    pub fn reactivate_wallet(
        &mut self,
        address: &WalletAddress,
        _rate: u128,
        at: Timestamp,
    ) -> Result<(), BrnError> {
        let state = self
            .wallets
            .get_mut(address)
            .ok_or(BrnError::WalletNotVerified)?;
        state.resume_accrual(at);
        Ok(())
    }

    /// Get the current active rate from the global history.
    pub fn current_rate(&self) -> u128 {
        self.rate_history.current_rate()
    }
}

impl BrnEngine {
    /// Persist all engine state to a BRN store.
    pub fn save_to_store(&self, store: &dyn burst_store::BrnStore) -> Result<(), BrnError> {
        let id_bytes = self.next_stake_id.to_be_bytes();
        store
            .put_meta(b"next_stake_id", &id_bytes)
            .map_err(|e| BrnError::Other(e.to_string()))?;

        let rate_bytes = bincode::serialize(&self.rate_history)
            .map_err(|e| BrnError::Other(e.to_string()))?;
        store
            .put_meta(b"rate_history", &rate_bytes)
            .map_err(|e| BrnError::Other(e.to_string()))?;

        for (addr, state) in &self.wallets {
            let bytes =
                bincode::serialize(state).map_err(|e| BrnError::Other(e.to_string()))?;
            store
                .put_wallet_state(addr, &bytes)
                .map_err(|e| BrnError::Other(e.to_string()))?;
        }
        Ok(())
    }

    /// Restore engine state from a BRN store.
    pub fn load_from_store(store: &dyn burst_store::BrnStore) -> Result<Self, BrnError> {
        let next_stake_id = match store.get_meta(b"next_stake_id") {
            Ok(Some(bytes)) if bytes.len() >= 8 => {
                u64::from_be_bytes(bytes[..8].try_into().unwrap())
            }
            _ => 1,
        };

        let rate_history = match store.get_meta(b"rate_history") {
            Ok(Some(bytes)) => bincode::deserialize(&bytes)
                .map_err(|e| BrnError::Other(e.to_string()))?,
            _ => RateHistory::default(),
        };

        let entries = store
            .iter_wallet_states()
            .map_err(|e| BrnError::Other(e.to_string()))?;
        let mut wallets = HashMap::new();
        for (addr, bytes) in entries {
            let state: BrnWalletState =
                bincode::deserialize(&bytes).map_err(|e| BrnError::Other(e.to_string()))?;
            wallets.insert(addr, state);
        }
        Ok(Self {
            next_stake_id,
            wallets,
            rate_history,
        })
    }
}

impl Default for BrnEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stake::StakeKind;

    fn test_address(n: u8) -> burst_types::WalletAddress {
        burst_types::WalletAddress::new(format!("brst_{:0>60}", n))
    }

    fn test_timestamp(secs: u64) -> Timestamp {
        Timestamp::new(secs)
    }

    fn make_engine(initial_rate: u128) -> BrnEngine {
        BrnEngine::with_rate(initial_rate, test_timestamp(0))
    }

    #[test]
    fn test_balance_computation_at_different_times() {
        let engine = make_engine(10);
        let verified_at = test_timestamp(1000);
        let state = BrnWalletState::new(verified_at);

        assert_eq!(engine.compute_balance(&state, test_timestamp(1000)), 0);
        assert_eq!(engine.compute_balance(&state, test_timestamp(1100)), 1000);
        assert_eq!(engine.compute_balance(&state, test_timestamp(2000)), 10000);
    }

    #[test]
    fn test_burn_reduces_available_balance() {
        let engine = make_engine(10);
        let verified_at = test_timestamp(1000);
        let mut state = BrnWalletState::new(verified_at);
        let now = test_timestamp(1100);

        assert_eq!(engine.compute_balance(&state, now), 1000);
        engine.record_burn(&mut state, 300, now).unwrap();
        assert_eq!(engine.compute_balance(&state, now), 700);
        engine.record_burn(&mut state, 200, now).unwrap();
        assert_eq!(engine.compute_balance(&state, now), 500);
    }

    #[test]
    fn test_insufficient_balance_burn_returns_error() {
        let engine = make_engine(10);
        let verified_at = test_timestamp(1000);
        let mut state = BrnWalletState::new(verified_at);
        let now = test_timestamp(1100);

        let result = engine.record_burn(&mut state, 1500, now);
        assert!(result.is_err());
        match result.unwrap_err() {
            BrnError::InsufficientBalance { needed, available } => {
                assert_eq!(needed, 1500);
                assert_eq!(available, 1000);
            }
            _ => panic!("Expected InsufficientBalance error"),
        }
    }

    #[test]
    fn test_staking_locks_brn() {
        let mut engine = make_engine(10);
        let verified_at = test_timestamp(1000);
        let mut state = BrnWalletState::new(verified_at);
        let now = test_timestamp(1100);
        let staker = test_address(1);

        assert_eq!(engine.compute_balance(&state, now), 1000);
        let stake = engine
            .stake(
                &staker,
                &mut state,
                400,
                StakeKind::Verification {
                    target_wallet: test_address(99),
                },
                now,
            )
            .unwrap();
        assert_eq!(engine.compute_balance(&state, now), 600);
        assert_eq!(stake.amount, 400);
        assert_eq!(stake.id, 1);
        assert!(!stake.resolved);
    }

    #[test]
    fn test_return_stake_unlocks_brn() {
        let mut engine = make_engine(10);
        let verified_at = test_timestamp(1000);
        let mut state = BrnWalletState::new(verified_at);
        let now = test_timestamp(1100);
        let staker = test_address(1);

        let mut stake = engine
            .stake(
                &staker,
                &mut state,
                400,
                StakeKind::Verification {
                    target_wallet: test_address(99),
                },
                now,
            )
            .unwrap();
        assert_eq!(engine.compute_balance(&state, now), 600);
        engine.return_stake(&staker, &mut state, &mut stake).unwrap();
        assert_eq!(engine.compute_balance(&state, now), 1000);
        assert!(stake.resolved);
    }

    #[test]
    fn test_forfeit_stake_converts_to_burned() {
        let mut engine = make_engine(10);
        let verified_at = test_timestamp(1000);
        let mut state = BrnWalletState::new(verified_at);
        let now = test_timestamp(1100);
        let staker = test_address(1);

        let mut stake = engine
            .stake(
                &staker,
                &mut state,
                400,
                StakeKind::Challenge {
                    target_wallet: test_address(99),
                },
                now,
            )
            .unwrap();
        assert_eq!(engine.compute_balance(&state, now), 600);
        assert_eq!(state.total_burned, 0);
        engine.forfeit_stake(&staker, &mut state, &mut stake).unwrap();
        assert_eq!(engine.compute_balance(&state, now), 600);
        assert_eq!(state.total_burned, 400);
        assert_eq!(state.total_staked, 0);
        assert!(stake.resolved);
    }

    #[test]
    fn test_rate_change_splitting_preserves_old_accrual() {
        let mut engine = make_engine(10);
        let verified_at = test_timestamp(1000);
        let state = BrnWalletState::new(verified_at);

        assert_eq!(engine.compute_balance(&state, test_timestamp(1100)), 1000);

        // Global rate change to 20 at time 1100
        engine.apply_rate_change(20, test_timestamp(1100)).unwrap();

        // Old accrual: 100s * 10 = 1000, New accrual: 50s * 20 = 1000 → total 2000
        assert_eq!(engine.compute_balance(&state, test_timestamp(1150)), 2000);

        assert_eq!(engine.rate_history.segments.len(), 2);
        assert_eq!(engine.rate_history.segments[0].rate, 10);
        assert_eq!(engine.rate_history.segments[1].rate, 20);
    }

    #[test]
    fn test_global_rate_change_is_o1() {
        let mut engine = make_engine(10);

        // Track 1000 wallets
        for i in 0u32..1000 {
            let addr = WalletAddress::new(format!("brst_{:0>60}", i));
            let state = BrnWalletState::new(test_timestamp(1000));
            engine.track_wallet(addr, state);
        }

        // Rate change should be O(1) — no wallet iteration
        engine
            .apply_rate_change(20, test_timestamp(2000))
            .unwrap();

        // Verify it works correctly for any wallet
        let addr = WalletAddress::new(format!("brst_{:0>60}", 42));
        let state = engine.get_wallet(&addr).unwrap();
        // 1000s at rate 10 + 500s at rate 20 = 10000 + 10000 = 20000
        assert_eq!(engine.compute_balance(state, test_timestamp(2500)), 20000);
    }

    #[test]
    fn test_double_resolve_stake_returns_error() {
        let mut engine = make_engine(10);
        let verified_at = test_timestamp(1000);
        let mut state = BrnWalletState::new(verified_at);
        let now = test_timestamp(1100);
        let staker = test_address(1);

        let mut stake = engine
            .stake(
                &staker,
                &mut state,
                400,
                StakeKind::Verification {
                    target_wallet: test_address(99),
                },
                now,
            )
            .unwrap();

        engine.return_stake(&staker, &mut state, &mut stake).unwrap();
        assert!(stake.resolved);

        let result = engine.return_stake(&staker, &mut state, &mut stake);
        assert!(result.is_err());
        match result.unwrap_err() {
            BrnError::StakeAlreadyResolved(id) => assert_eq!(id, stake.id),
            _ => panic!("Expected StakeAlreadyResolved error"),
        }

        let result = engine.forfeit_stake(&staker, &mut state, &mut stake);
        assert!(result.is_err());
        match result.unwrap_err() {
            BrnError::StakeAlreadyResolved(id) => assert_eq!(id, stake.id),
            _ => panic!("Expected StakeAlreadyResolved error"),
        }
    }

    #[test]
    fn test_deactivate_stops_accrual() {
        let mut engine = make_engine(10);
        let verified_at = test_timestamp(1000);
        let state = BrnWalletState::new(verified_at);
        let addr = test_address(1);
        engine.track_wallet(addr.clone(), state);

        // 500s at rate 10 = 5000
        let bal_before = engine
            .compute_balance(engine.get_wallet(&addr).unwrap(), test_timestamp(1500));
        assert_eq!(bal_before, 5000);

        engine.deactivate_wallet(&addr, test_timestamp(1500)).unwrap();

        // After deactivation, balance stays frozen at 5000
        let bal_after = engine
            .compute_balance(engine.get_wallet(&addr).unwrap(), test_timestamp(2000));
        assert_eq!(bal_after, 5000);
    }
}
