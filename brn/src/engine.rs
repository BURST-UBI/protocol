//! Core BRN computation engine.

use crate::error::BrnError;
use crate::stake::{Stake, StakeId, StakeKind};
use crate::state::{BrnWalletState, RateHistory};
use burst_types::{Timestamp, WalletAddress};
use std::collections::HashMap;

/// BRN balances, burns and stakes backed by a shared rate history.
#[derive(Clone)]
pub struct BrnEngine {
    next_stake_id: StakeId,
    /// Tracked wallet states for global operations.
    pub wallets: crate::wallet_states::WalletStates,
    /// Global rate history shared by all wallets.
    pub rate_history: RateHistory,
}

mod prepared;

impl BrnEngine {
    /// Included in the node's atomic economic checkpoint.
    pub fn next_stake_id(&self) -> StakeId {
        self.next_stake_id
    }

    /// Replace the allocator from a nonzero big-endian u64. Callers validate the
    /// snapshot and retained stake IDs; this only checks encoding and nonzero.
    /// A coordinated rewind may lower the value.
    pub fn restore_stake_sequence(&mut self, bytes: &[u8]) -> Result<(), BrnError> {
        let next = u64::from_be_bytes(bytes.try_into()
            .map_err(|_| BrnError::Other("invalid persisted stake sequence".into()))?);
        if next == 0 {
            return Err(BrnError::Other("invalid persisted stake sequence".into()));
        }
        self.next_stake_id = next;
        Ok(())
    }

    pub fn new() -> Self {
        Self {
            next_stake_id: 1,
            wallets: Default::default(),
            rate_history: RateHistory::default(),
        }
    }

    /// Create a new engine with an explicit initial rate and genesis time.
    pub fn with_rate(initial_rate: u128, genesis: Timestamp) -> Self {
        Self {
            next_stake_id: 1,
            wallets: Default::default(),
            rate_history: RateHistory::new(initial_rate, genesis),
        }
    }

    /// Compute the available BRN balance for a wallet at a given time.
    ///
    /// `available = Σ(rate × eligible duration) − burned − active stakes`
    ///
    /// This includes unacknowledged stake returns. Use the wallet state's
    /// `spendable_balance_checked` for spending admission. Arithmetic failure
    /// produces zero in this convenience method.
    ///
    /// All values are deterministic integers: rates are `u128` raw units per
    /// second, timestamps are `u64` whole seconds, and all arithmetic is
    /// integer multiply/add/sub.
    pub fn compute_balance(&self, state: &BrnWalletState, now: Timestamp) -> u128 {
        state.available_balance(&self.rate_history, now)
    }

    /// Compute available BRN, reporting overflow or an overdrawn state as an error.
    /// This does not subtract unacknowledged stake returns.
    pub fn compute_balance_checked(
        &self,
        state: &BrnWalletState,
        now: Timestamp,
    ) -> Result<u128, BrnError> {
        state
            .available_balance_checked(&self.rate_history, now)
            .ok_or(BrnError::Overflow)
    }

    /// Consume spendable BRN. The caller applies any corresponding TRST payment.
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
            .spendable_balance_checked(&self.rate_history, now)
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

    /// Lock spendable BRN and allocate a stake ID. The caller supplies the correct
    /// wallet state and checks verification or challenge eligibility. This does
    /// not insert the returned stake into a store.
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
            .spendable_balance_checked(&self.rate_history, now)
            .ok_or(BrnError::Overflow)?;
        if available < amount {
            return Err(BrnError::InsufficientBalance {
                needed: amount,
                available,
            });
        }
        let next_stake_id = self
            .next_stake_id
            .checked_add(1)
            .ok_or(BrnError::Overflow)?;
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
        self.next_stake_id = next_stake_id;
        Ok(stake)
    }

    /// Remove the stake from the locked total and mark it resolved.
    /// The caller validates the outcome and records any pending release evidence.
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

    /// Move the stake from the locked total to cumulative burns and mark it resolved.
    /// The caller decides whether the protocol outcome requires forfeiture.
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
        let total_staked = state
            .total_staked
            .checked_sub(stake.amount)
            .ok_or(BrnError::Overflow)?;
        let total_burned = state
            .total_burned
            .checked_add(stake.amount)
            .ok_or(BrnError::Overflow)?;
        state.total_staked = total_staked;
        state.total_burned = total_burned;
        stake.resolved = true;
        Ok(())
    }

    /// Append a rate change without visiting wallets. Persistent tree updates
    /// take O(log k) work for k rate segments, including shared snapshots.
    pub fn apply_rate_change(
        &mut self,
        new_rate: u128,
        change_at: Timestamp,
    ) -> Result<(), BrnError> {
        self.rate_history.apply_rate_change(new_rate, change_at)
    }

    /// Insert or replace a tracked wallet without validating its accounting state.
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

    /// Resume accrual using the shared historical rates. The `_rate` argument
    /// is ignored. Eligibility and transition timing are caller responsibilities.
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

    /// Return the last stored rate; no query-time selection is performed.
    pub fn current_rate(&self) -> u128 {
        self.rate_history.current_rate()
    }
}

impl BrnEngine {
    /// Write allocator, Bincode rate history and each tracked wallet separately.
    /// This does not delete obsolete wallets or replace normalized-format rows.
    /// It is not an atomic node checkpoint; an error can follow earlier writes.
    pub fn save_to_store(&self, store: &dyn burst_store::BrnStore) -> Result<(), BrnError> {
        let id_bytes = self.next_stake_id.to_be_bytes();
        store
            .put_meta(b"next_stake_id", &id_bytes)
            .map_err(|e| BrnError::Other(e.to_string()))?;

        let rate_bytes =
            bincode::serialize(&self.rate_history).map_err(|e| BrnError::Other(e.to_string()))?;
        store
            .put_meta(b"rate_history", &rate_bytes)
            .map_err(|e| BrnError::Other(e.to_string()))?;

        for (addr, state) in &self.wallets {
            let bytes = bincode::serialize(state).map_err(|e| BrnError::Other(e.to_string()))?;
            store
                .put_wallet_state(addr, &bytes)
                .map_err(|e| BrnError::Other(e.to_string()))?;
        }
        Ok(())
    }

    /// Restore engine state from a BRN store.
    pub fn load_from_store(store: &dyn burst_store::BrnStore) -> Result<Self, BrnError> {
        let snapshot = store
            .read_snapshot()
            .map_err(|e| BrnError::Other(e.to_string()))?;
        Self::load_from_snapshot(snapshot.as_ref())
    }

    /// Restore from the supplied storage view, using normalized wallet rows when
    /// available. The backend provides snapshot consistency. Layout and rate
    /// checks do not establish wallet eligibility or economic validity.
    pub fn load_from_snapshot(snapshot: &dyn burst_store::brn::BrnSnapshot) -> Result<Self, BrnError> {
        let id_bytes = snapshot
            .get_meta(b"next_stake_id")
            .map_err(|e| BrnError::Other(e.to_string()))?;
        let rates = crate::rate_storage::read_snapshot(snapshot)?;
        if let Some(headers) = snapshot.wallet_record_headers().map_err(|error| BrnError::Other(error.to_string()))? {
            let mut engine = Self::from_decoded_entries(id_bytes, rates,
                std::iter::empty::<Result<(WalletAddress, &[u8]), BrnError>>())?;
            for row in headers {
                let (address, header) = row.map_err(|error| BrnError::Other(error.to_string()))?;
                if id_bytes.is_none() { return Err(BrnError::Other("incomplete persisted BRN metadata".into())); }
                let prefix = crate::wallet_storage::row_prefix(&address)?;
                let children = snapshot.meta_entries(&prefix)
                    .map_err(|error| BrnError::Other(error.to_string()))?;
                let records = std::iter::once(Ok((crate::wallet_storage::HEADER, header)))
                    .chain(children.map(|row| {
                        let (key, value) = row.map_err(|error| BrnError::Other(error.to_string()))?;
                        let key = key.strip_prefix(prefix.as_slice())
                            .ok_or_else(|| BrnError::Other("foreign BRN wallet record".into()))?;
                        Ok((key, value))
                    }));
                let wallet = crate::wallet_storage::decode(records)?
                    .ok_or_else(|| BrnError::Other("missing BRN wallet header".into()))?;
                if engine.wallets.insert(address, wallet).is_some() {
                    return Err(BrnError::Other("duplicate persisted BRN wallet".into()));
                }
            }
            return Ok(engine);
        }
        let entries = snapshot
            .wallet_entries()
            .map_err(|e| BrnError::Other(e.to_string()))?;
        Self::from_decoded_entries(
            id_bytes,
            rates,
            entries.map(|row| row.map_err(|e| BrnError::Other(e.to_string()))),
        )
    }

    /// Decode a caller-owned storage snapshot, including a rewound writer.
    pub fn from_persisted(
        id_bytes: Option<&[u8]>,
        rate_bytes: Option<&[u8]>,
        entries: Vec<(WalletAddress, Vec<u8>)>,
    ) -> Result<Self, BrnError> {
        Self::from_persisted_entries(id_bytes, rate_bytes, entries.into_iter().map(Ok))
    }

    /// Decode wallet entries incrementally and retain the resulting map in memory.
    /// Require complete metadata when wallets exist and reject duplicate addresses.
    pub fn from_persisted_entries<B: AsRef<[u8]>>(
        id_bytes: Option<&[u8]>,
        rate_bytes: Option<&[u8]>,
        entries: impl IntoIterator<Item = Result<(WalletAddress, B), BrnError>>,
    ) -> Result<Self, BrnError> {
        let rates = rate_bytes.map(|bytes| bincode::deserialize(bytes)
            .map_err(|e| BrnError::Other(e.to_string()))).transpose()?;
        Self::from_decoded_entries(id_bytes, rates, entries)
    }

    /// Restore either local rate encoding without serializing a decoded history.
    pub fn from_decoded_entries<B: AsRef<[u8]>>(
        id_bytes: Option<&[u8]>,
        rates: Option<RateHistory>,
        entries: impl IntoIterator<Item = Result<(WalletAddress, B), BrnError>>,
    ) -> Result<Self, BrnError> {
        let next_stake_id = match id_bytes {
            Some(bytes) => u64::from_be_bytes(bytes.try_into()
                .map_err(|_| BrnError::Other("invalid persisted stake sequence".into()))?),
            None => 1,
        };
        if id_bytes.is_some() != rates.is_some() {
            return Err(BrnError::Other("incomplete persisted BRN metadata".into()));
        }
        let rate_history = rates.unwrap_or_default();
        let mut engine = Self::from_state(next_stake_id, rate_history, HashMap::new())?;
        for entry in entries {
            let (addr, bytes) = entry?;
            if id_bytes.is_none() {
                return Err(BrnError::Other("incomplete persisted BRN metadata".into()));
            }
            let state: BrnWalletState = BrnWalletState::from_persisted_bytes(bytes.as_ref())
                .map_err(|e| BrnError::Other(e.to_string()))?;
            if engine.wallets.insert(addr, state).is_some() {
                return Err(BrnError::Other("duplicate persisted BRN wallet".into()));
            }
        }
        Ok(engine)
    }

    /// Build from decoded state after checking the nonzero allocator and rate
    /// segment boundaries. Wallet balances, pause histories, stake identities
    /// and release evidence are not validated here.
    pub fn from_state(
        next_stake_id: StakeId,
        rate_history: RateHistory,
        wallets: HashMap<WalletAddress, BrnWalletState>,
    ) -> Result<Self, BrnError> {
        if next_stake_id == 0 {
            return Err(BrnError::Other("invalid persisted stake sequence".into()));
        }
        let mut segments = rate_history.segments().iter().peekable();
        if segments.peek().is_none() {
            return Err(BrnError::Other("invalid persisted BRN rate history".into()));
        }
        while let Some(segment) = segments.next() {
            let invalid = match segments.peek() {
                Some(next) => segment.end != Some(next.start) || next.start < segment.start,
                None => segment.end.is_some(),
            };
            if invalid {
                return Err(BrnError::Other("invalid persisted BRN rate history".into()));
            }
        }
        Ok(Self {
            next_stake_id,
            wallets: wallets.into(),
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

    #[test]
    fn streamed_restoration_preserves_metadata_rules_and_stops_on_read_failure() {
        let id = 9u64.to_be_bytes();
        let rates = bincode::serialize(&RateHistory::new(7, Timestamp::new(0))).unwrap();
        let wallet = BrnWalletState::new(Timestamp::new(10));
        let bytes = bincode::serialize(&wallet).unwrap();
        let address = test_address(1);
        let borrowed = BrnEngine::from_persisted_entries(
            Some(&id),
            Some(&rates),
            [Ok((address.clone(), bytes.as_slice()))],
        )
        .unwrap();
        assert_eq!(borrowed.wallets[&address], wallet);
        assert_eq!(borrowed.next_stake_id(), 9);
        assert!(BrnEngine::from_persisted_entries::<&[u8]>(None, None, [])
            .unwrap()
            .wallets
            .is_empty());
        for (id, rates) in [
            (None, None),
            (Some(id.as_slice()), None),
            (None, Some(rates.as_slice())),
        ] {
            assert!(BrnEngine::from_persisted_entries(
                id,
                rates,
                [Ok((address.clone(), bytes.as_slice()))]
            )
            .is_err());
        }
        let rows = [
            Ok((address, bytes.as_slice())),
            Err(BrnError::Other("injected row failure".into())),
        ]
        .into_iter()
        .chain(std::iter::once_with(|| panic!("read past failed row")));
        let error = BrnEngine::from_persisted_entries(Some(&id), Some(&rates), rows)
            .err()
            .unwrap();
        assert!(error.to_string().contains("injected row failure"));
    }

    #[test]
    fn decoded_and_persisted_restoration_share_validation_and_balances() {
        let address = test_address(1);
        let mut rates = RateHistory::new(7, Timestamp::new(0));
        rates.apply_rate_change(11, Timestamp::new(100)).unwrap();
        let mut wallet = BrnWalletState::new(Timestamp::new(10));
        wallet.total_burned = 17;
        wallet.total_staked = 3;
        let wallets = HashMap::from([(address.clone(), wallet.clone())]);
        let direct = BrnEngine::from_state(9, rates.clone(), wallets.clone()).unwrap();
        let persisted = BrnEngine::from_persisted(
            Some(&9u64.to_be_bytes()),
            Some(&bincode::serialize(&rates).unwrap()),
            vec![(address.clone(), bincode::serialize(&wallet).unwrap())],
        )
        .unwrap();
        assert_eq!(direct.next_stake_id(), persisted.next_stake_id());
        assert_eq!(direct.wallets, persisted.wallets);
        assert_eq!(direct.rate_history, persisted.rate_history);
        assert_eq!(
            direct
                .compute_balance_checked(&direct.wallets[&address], Timestamp::new(120))
                .unwrap(),
            830
        );
        for (id, rates) in [
            (0, rates.clone()),
            (9, RateHistory::from_segments(Vec::new())),
            (9, {
                let mut invalid = rates.segments().iter().cloned().collect::<Vec<_>>();
                invalid[0].end = None;
                RateHistory::from_segments(invalid)
            }),
        ] {
            assert!(BrnEngine::from_state(id, rates.clone(), wallets.clone()).is_err());
            assert!(BrnEngine::from_persisted(
                Some(&id.to_be_bytes()),
                Some(&bincode::serialize(&rates).unwrap()),
                vec![(address.clone(), bincode::serialize(&wallet).unwrap())],
            )
            .is_err());
        }
        assert!(BrnEngine::from_persisted(
            Some(&9u64.to_be_bytes()),
            Some(&bincode::serialize(&rates).unwrap()),
            vec![
                (address.clone(), bincode::serialize(&wallet).unwrap()),
                (address, bincode::serialize(&wallet).unwrap())
            ],
        )
        .is_err());
    }

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
    fn exhausted_stake_sequence_does_not_lock_a_balance() {
        let mut engine = make_engine(10);
        engine.next_stake_id = u64::MAX;
        let mut state = BrnWalletState::new(test_timestamp(0));
        let result = engine.stake(
            &test_address(1),
            &mut state,
            1,
            StakeKind::Challenge {
                target_wallet: test_address(2),
            },
            test_timestamp(10),
        );
        assert!(matches!(result, Err(BrnError::Overflow)));
        assert_eq!(state.total_staked, 0);
        assert_eq!(engine.next_stake_id, u64::MAX);
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
        engine
            .return_stake(&staker, &mut state, &mut stake)
            .unwrap();
        assert_eq!(engine.compute_balance(&state, now), 1000);
        assert!(stake.resolved);
    }

    #[test]
    fn forfeit_overflow_leaves_wallet_and_stake_unchanged() {
        let mut engine = make_engine(10);
        let staker = test_address(1);
        let mut state = BrnWalletState::new(test_timestamp(1000));
        let mut stake = engine.stake(&staker, &mut state, 1,
            StakeKind::Challenge { target_wallet: test_address(99) },
            test_timestamp(1100)).unwrap();
        state.total_burned = u128::MAX;
        let before = state.clone();
        let stake_before = bincode::serialize(&stake).unwrap();
        assert!(matches!(engine.forfeit_stake(&staker, &mut state, &mut stake),
            Err(BrnError::Overflow)));
        assert_eq!(state, before);
        assert_eq!(bincode::serialize(&stake).unwrap(), stake_before);
        state.total_burned = u128::MAX - 1;
        engine.forfeit_stake(&staker, &mut state, &mut stake).unwrap();
        assert_eq!(state.total_burned, u128::MAX);
        assert_eq!(state.total_staked, 0);
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
        engine
            .forfeit_stake(&staker, &mut state, &mut stake)
            .unwrap();
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

        engine.apply_rate_change(20, test_timestamp(1100)).unwrap();

        // Old accrual: 100s * 10 = 1000, New accrual: 50s * 20 = 1000 → total 2000
        assert_eq!(engine.compute_balance(&state, test_timestamp(1150)), 2000);

        assert_eq!(engine.rate_history.segments().len(), 2);
        assert_eq!(engine.rate_history.segments()[0].rate, 10);
        assert_eq!(engine.rate_history.segments()[1].rate, 20);
    }

    #[test]
    fn rate_change_updates_tracked_wallet_balances() {
        let mut engine = make_engine(10);

        for i in 0u32..1000 {
            let addr = WalletAddress::new(format!("brst_{:0>60}", i));
            let state = BrnWalletState::new(test_timestamp(1000));
            engine.track_wallet(addr, state);
        }

        engine.apply_rate_change(20, test_timestamp(2000)).unwrap();

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

        engine
            .return_stake(&staker, &mut state, &mut stake)
            .unwrap();
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
        let bal_before =
            engine.compute_balance(engine.get_wallet(&addr).unwrap(), test_timestamp(1500));
        assert_eq!(bal_before, 5000);

        engine
            .deactivate_wallet(&addr, test_timestamp(1500))
            .unwrap();

        // With no further burns or stakes, stopping accrual preserves this balance.
        let bal_after =
            engine.compute_balance(engine.get_wallet(&addr).unwrap(), test_timestamp(2000));
        assert_eq!(bal_after, 5000);
    }
}
