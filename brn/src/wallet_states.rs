use crate::BrnWalletState;
use burst_types::WalletAddress;
use serde::Serialize;
use std::{collections::HashMap, ops::Deref};

const MAX_RECORDED_ROWS: usize = 65_536;
const MAX_RECORDED_BYTES: usize = 64 << 20;

/// Wallet storage with optional before-images for one isolated replay transition.
/// Read access cannot bypass the mutation recorder.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(transparent)]
pub struct WalletStates {
    values: HashMap<WalletAddress, BrnWalletState>,
    #[serde(skip)]
    recording: Option<Recording>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::Timestamp;

    fn changes(states: &WalletStates) -> Vec<(WalletAddress, Option<Vec<u8>>, Option<Vec<u8>>)> {
        let mut rows = Vec::new();
        states.visit_recorded(|key, before, after| {
            rows.push((key.clone(), before.map(<[u8]>::to_vec), after));
            Ok(())
        }).unwrap();
        rows.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        rows
    }

    #[test]
    fn records_first_before_and_final_after_without_unchanged_wallets() {
        let mut states = WalletStates::default();
        for i in 0..1024 {
            states.insert(WalletAddress::new(format!("brst_wallet_{i}")), BrnWalletState::new(Timestamp::EPOCH));
        }
        let key = WalletAddress::new("brst_wallet_1");
        let removed = WalletAddress::new("brst_wallet_2");
        let transient = WalletAddress::new("brst_transient");
        let before = bincode::serialize(&states[&key]).unwrap();
        states.record_transition(true);
        let mut detached = states.remove(&key).unwrap();
        detached.total_burned = 3;
        states.insert(key.clone(), detached);
        states.get_mut(&key).unwrap().total_staked = 4;
        states.remove(&removed);
        states.insert(transient.clone(), BrnWalletState::new(Timestamp::EPOCH));
        states.remove(&transient);
        let rows = changes(&states);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (key.clone(), Some(before), Some(bincode::serialize(&states[&key]).unwrap())));
        assert_eq!(rows[1].0, removed);
        assert!(rows[1].1.is_some());
        assert!(rows[1].2.is_none());
        assert_eq!(states.recording.as_ref().unwrap().before.len(), 3);
        let mut fork = states.clone();
        fork.record_transition(true);
        fork.get_mut(&key).unwrap().total_burned = 9;
        assert_eq!(states[&key].total_burned, 3);
        assert_eq!(changes(&fork).len(), 1);
        states.record_transition(true);
        assert!(changes(&states).is_empty());
    }

    #[test]
    fn compensation_and_storage_encoding_ignore_recording() {
        let key = WalletAddress::new("brst_wallet");
        let mut states = WalletStates::default();
        states.insert(key.clone(), BrnWalletState::new(Timestamp::EPOCH));
        let original = states.clone();
        states.record_transition(true);
        states.get_mut(&key).unwrap().total_burned += 7;
        states.get_mut(&key).unwrap().total_burned -= 7;
        assert!(changes(&states).is_empty());
        assert_eq!(states, original);
        assert_eq!(bincode::serialize(&states).unwrap(), bincode::serialize(&states.values).unwrap());
        states.record_transition(false);
        assert!(states.visit_recorded(|_, _, _| Ok(())).is_err());
    }

    #[test]
    fn capture_overflow_cannot_publish_partial_changes() {
        let mut states = WalletStates::default();
        states.record_transition(true);
        states.recording.as_mut().unwrap().bytes = MAX_RECORDED_BYTES;
        states.insert(WalletAddress::new("brst_new"), BrnWalletState::new(Timestamp::EPOCH));
        assert!(states.recording.as_ref().unwrap().before.is_empty());
        let mut emitted = false;
        assert!(states.visit_recorded(|_, _, _| { emitted = true; Ok(()) }).is_err());
        assert!(!emitted);
    }
}

#[derive(Clone, Debug, Default)]
struct Recording {
    before: HashMap<WalletAddress, Option<Vec<u8>>>,
    bytes: usize,
    error: Option<String>,
}

impl PartialEq for WalletStates {
    fn eq(&self, other: &Self) -> bool {
        self.values == other.values
    }
}

impl Eq for WalletStates {}

impl From<HashMap<WalletAddress, BrnWalletState>> for WalletStates {
    fn from(values: HashMap<WalletAddress, BrnWalletState>) -> Self {
        Self { values, recording: None }
    }
}

impl Deref for WalletStates {
    type Target = HashMap<WalletAddress, BrnWalletState>;

    fn deref(&self) -> &Self::Target {
        &self.values
    }
}

impl<'a> IntoIterator for &'a WalletStates {
    type Item = (&'a WalletAddress, &'a BrnWalletState);
    type IntoIter = std::collections::hash_map::Iter<'a, WalletAddress, BrnWalletState>;

    fn into_iter(self) -> Self::IntoIter {
        self.values.iter()
    }
}

impl WalletStates {
    /// Start a fresh transition, or disable recording outside recovery.
    pub fn record_transition(&mut self, enabled: bool) {
        self.recording = enabled.then(Recording::default);
    }

    fn remember(&mut self, address: &WalletAddress) {
        let Some(recording) = &mut self.recording else { return };
        if recording.error.is_some() || recording.before.contains_key(address) {
            return;
        }
        let capture = || -> Result<Option<Vec<u8>>, String> {
            if recording.before.len() >= MAX_RECORDED_ROWS {
                return Err("BRN mutation row budget exceeded".into());
            }
            let state = self.values.get(address);
            let size = state.map(bincode::serialized_size).transpose()
                .map_err(|error| error.to_string())?.unwrap_or(0);
            let available = MAX_RECORDED_BYTES.saturating_sub(recording.bytes)
                .saturating_sub(address.as_str().len());
            if address.as_str().len() > MAX_RECORDED_BYTES.saturating_sub(recording.bytes)
                || size > available as u64
            {
                return Err("BRN mutation byte budget exceeded".into());
            }
            state.map(bincode::serialize).transpose().map_err(|error| error.to_string())
        };
        match capture() {
            Ok(before) => {
                recording.bytes += address.as_str().len() + before.as_ref().map_or(0, Vec::len);
                recording.before.insert(address.clone(), before);
            }
            Err(error) => recording.error = Some(error),
        }
    }

    pub fn insert(&mut self, address: WalletAddress, state: BrnWalletState) -> Option<BrnWalletState> {
        self.remember(&address);
        self.values.insert(address, state)
    }

    pub fn remove(&mut self, address: &WalletAddress) -> Option<BrnWalletState> {
        self.remember(address);
        self.values.remove(address)
    }

    pub fn get_mut(&mut self, address: &WalletAddress) -> Option<&mut BrnWalletState> {
        self.remember(address);
        self.values.get_mut(address)
    }

    /// Failed capture invalidates the candidate; it must never publish a partial journal.
    pub fn visit_recorded(
        &self,
        mut emit: impl FnMut(&WalletAddress, Option<&[u8]>, Option<Vec<u8>>) -> Result<(), String>,
    ) -> Result<(), String> {
        let recording = self.recording.as_ref().ok_or("BRN transition recording is disabled")?;
        if let Some(error) = &recording.error {
            return Err(error.clone());
        }
        for (address, before) in &recording.before {
            let after = self.values.get(address);
            if after.map(bincode::serialized_size).transpose()
                .map_err(|error| error.to_string())?.unwrap_or(0) > MAX_RECORDED_BYTES as u64
            {
                return Err("BRN mutation after-image exceeds byte budget".into());
            }
            let after = after.map(bincode::serialize).transpose().map_err(|error| error.to_string())?;
            if before != &after {
                emit(address, before.as_deref(), after)?;
            }
        }
        Ok(())
    }
}
