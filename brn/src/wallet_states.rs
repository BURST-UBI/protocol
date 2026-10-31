use crate::BrnWalletState;
use burst_types::WalletAddress;
use serde::Serialize;
use std::{collections::HashMap, ops::Index, sync::Arc};

const MAX_RECORDED_ROWS: usize = 65_536;
const MAX_RECORDED_BYTES: usize = 64 << 20;

/// Wallet state with optional before-images for one isolated replay transition.
/// The map shares unchanged tree nodes and wallet values across clones. An active
/// recording is cloned separately, including its captured bytes. Serialization
/// and equality include wallet values only, not the recording.
#[derive(Clone, Debug, Default)]
pub struct WalletStates {
    values: imbl::OrdMap<WalletAddress, Arc<BrnWalletState>>,
    recording: Option<Recording>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::Timestamp;

    fn changes(states: &WalletStates) -> Vec<(WalletAddress, Option<Vec<u8>>, Option<Vec<u8>>)> {
        let mut rows = Vec::new();
        states
            .visit_recorded(|key, before, after| {
                rows.push((key.clone(), before.map(<[u8]>::to_vec), after));
                Ok(())
            })
            .unwrap();
        rows.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        rows
    }

    #[test]
    fn records_first_before_and_final_after_without_unchanged_wallets() {
        let mut states = WalletStates::default();
        for i in 0..1024 {
            states.insert(
                WalletAddress::new(format!("brst_wallet_{i}")),
                BrnWalletState::new(Timestamp::EPOCH),
            );
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
        assert_eq!(
            rows[0],
            (
                key.clone(),
                Some(before),
                Some(bincode::serialize(&states[&key]).unwrap())
            )
        );
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
        assert_eq!(
            bincode::serialize(&states).unwrap(),
            bincode::serialize(&states.iter().collect::<std::collections::BTreeMap<_, _>>())
                .unwrap()
        );
        states.record_transition(false);
        assert!(states.visit_recorded(|_, _, _| Ok(())).is_err());
    }

    #[test]
    fn snapshots_share_unrelated_wallets_and_emit_exact_semantic_changes() {
        let key = |n| WalletAddress::new(format!("brst_wallet_{n:05}"));
        let mut before = WalletStates::default();
        for n in 0..4096 {
            before.insert(key(n), BrnWalletState::new(Timestamp::EPOCH));
        }
        let mut after = before.clone();
        assert!(before.values.ptr_eq(&after.values));
        after.get_mut(&key(10)).unwrap().total_burned = 5;
        after.remove(&key(20));
        after.insert(key(5000), BrnWalletState::new(Timestamp::new(100)));
        after.get_mut(&key(30)).unwrap().total_staked = 7;
        after.get_mut(&key(30)).unwrap().total_staked = 0;
        for n in 0..4096 {
            if ![10, 20, 30].contains(&n) {
                assert!(std::ptr::eq(
                    before.get(&key(n)).unwrap(),
                    after.get(&key(n)).unwrap()
                ));
            }
        }
        assert_eq!(before[&key(10)].total_burned, 0);
        let mut changes = Vec::new();
        after
            .visit_changes(&before, |key, old, new| {
                changes.push((key.clone(), old.cloned(), new.cloned()));
                Ok(())
            })
            .unwrap();
        assert_eq!(changes.len(), 3);
        assert_eq!(
            changes[0],
            (
                key(10),
                before.get(&key(10)).cloned(),
                after.get(&key(10)).cloned()
            )
        );
        assert_eq!(changes[1], (key(20), before.get(&key(20)).cloned(), None));
        assert_eq!(
            changes[2],
            (key(5000), None, after.get(&key(5000)).cloned())
        );
        let independent: WalletStates = before
            .iter()
            .map(|(key, state)| (key.clone(), state.clone()))
            .collect::<HashMap<_, _>>()
            .into();
        let mut rebuilt_changes = Vec::new();
        after
            .visit_changes(&independent, |key, old, new| {
                rebuilt_changes.push((key.clone(), old.cloned(), new.cloned()));
                Ok(())
            })
            .unwrap();
        assert_eq!(rebuilt_changes, changes);
        let decoded: HashMap<WalletAddress, BrnWalletState> =
            bincode::deserialize(&bincode::serialize(&after).unwrap()).unwrap();
        assert_eq!(WalletStates::from(decoded), after);
    }

    #[test]
    fn capture_overflow_cannot_publish_partial_changes() {
        let mut states = WalletStates::default();
        states.record_transition(true);
        states.recording.as_mut().unwrap().bytes = MAX_RECORDED_BYTES;
        states.insert(
            WalletAddress::new("brst_new"),
            BrnWalletState::new(Timestamp::EPOCH),
        );
        assert!(states.recording.as_ref().unwrap().before.is_empty());
        let mut emitted = false;
        assert!(states
            .visit_recorded(|_, _, _| {
                emitted = true;
                Ok(())
            })
            .is_err());
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
        Self {
            values: values
                .into_iter()
                .map(|(key, value)| (key, Arc::new(value)))
                .collect(),
            recording: None,
        }
    }
}

impl Serialize for WalletStates {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.iter())
    }
}

impl Index<&WalletAddress> for WalletStates {
    type Output = BrnWalletState;
    fn index(&self, address: &WalletAddress) -> &Self::Output {
        self.get(address).expect("missing BRN wallet")
    }
}

pub struct Iter<'a> {
    inner: imbl::ordmap::Iter<
        'a,
        WalletAddress,
        Arc<BrnWalletState>,
        imbl::shared_ptr::DefaultSharedPtr,
    >,
}

impl<'a> Iterator for Iter<'a> {
    type Item = (&'a WalletAddress, &'a BrnWalletState);
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(key, value)| (key, value.as_ref()))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<'a> IntoIterator for &'a WalletStates {
    type Item = (&'a WalletAddress, &'a BrnWalletState);
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl WalletStates {
    pub fn len(&self) -> usize {
        self.values.len()
    }
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
    pub fn get(&self, address: &WalletAddress) -> Option<&BrnWalletState> {
        self.values.get(address).map(Arc::as_ref)
    }
    pub fn contains_key(&self, address: &WalletAddress) -> bool {
        self.values.contains_key(address)
    }
    pub fn iter(&self) -> Iter<'_> {
        Iter {
            inner: self.values.iter(),
        }
    }
    pub fn keys(&self) -> impl Iterator<Item = &WalletAddress> {
        self.values.keys()
    }
    pub fn values(&self) -> impl Iterator<Item = &BrnWalletState> {
        self.values.values().map(Arc::as_ref)
    }

    /// Compare persistent snapshots, skipping unchanged shared tree nodes.
    /// Unrelated independently loaded maps still require a full comparison.
    pub fn visit_changes(
        &self,
        before: &Self,
        mut emit: impl FnMut(
            &WalletAddress,
            Option<&BrnWalletState>,
            Option<&BrnWalletState>,
        ) -> Result<(), String>,
    ) -> Result<(), String> {
        use imbl::ordmap::DiffItem;
        for change in before.values.diff(&self.values) {
            match change {
                DiffItem::Add(key, value) => emit(key, None, Some(value))?,
                DiffItem::Remove(key, value) => emit(key, Some(value), None)?,
                DiffItem::Update {
                    old: (_, old),
                    new: (key, new),
                } => emit(key, Some(old), Some(new))?,
            }
        }
        Ok(())
    }

    /// Discard the previous recording and start an empty one, or disable it.
    /// Wallet values are unchanged; this is not rollback.
    pub fn record_transition(&mut self, enabled: bool) {
        self.recording = enabled.then(Recording::default);
    }

    /// Clear an enabled recording without enabling a disabled recorder.
    pub fn reset_recorded_transition(&mut self) {
        if self.recording.is_some() {
            self.recording = Some(Recording::default());
        }
    }

    // Capture each address once. A capture failure is retained for visit_recorded;
    // the requested wallet mutation still proceeds in this isolated candidate.
    fn remember(&mut self, address: &WalletAddress) {
        let Some(recording) = &mut self.recording else {
            return;
        };
        if recording.error.is_some() || recording.before.contains_key(address) {
            return;
        }
        let capture = || -> Result<Option<Vec<u8>>, String> {
            if recording.before.len() >= MAX_RECORDED_ROWS {
                return Err("BRN mutation row budget exceeded".into());
            }
            let state = self.values.get(address).map(Arc::as_ref);
            let size = state
                .map(bincode::serialized_size)
                .transpose()
                .map_err(|error| error.to_string())?
                .unwrap_or(0);
            let available = MAX_RECORDED_BYTES
                .saturating_sub(recording.bytes)
                .saturating_sub(address.as_str().len());
            if address.as_str().len() > MAX_RECORDED_BYTES.saturating_sub(recording.bytes)
                || size > available as u64
            {
                return Err("BRN mutation byte budget exceeded".into());
            }
            state
                .map(bincode::serialize)
                .transpose()
                .map_err(|error| error.to_string())
        };
        match capture() {
            Ok(before) => {
                recording.bytes += address.as_str().len() + before.as_ref().map_or(0, Vec::len);
                recording.before.insert(address.clone(), before);
            }
            Err(error) => recording.error = Some(error),
        }
    }

    pub fn insert(
        &mut self,
        address: WalletAddress,
        state: BrnWalletState,
    ) -> Option<BrnWalletState> {
        self.remember(&address);
        self.values
            .insert(address, Arc::new(state))
            .map(Arc::unwrap_or_clone)
    }

    pub fn remove(&mut self, address: &WalletAddress) -> Option<BrnWalletState> {
        self.remember(address);
        self.values.remove(address).map(Arc::unwrap_or_clone)
    }

    pub fn get_mut(&mut self, address: &WalletAddress) -> Option<&mut BrnWalletState> {
        self.remember(address);
        self.values.get_mut(address).map(Arc::make_mut)
    }

    /// Emit changed before/after values in unspecified address order.
    /// A prior capture error fails before any callback. Encoding or callback
    /// errors during traversal can follow earlier callbacks, so the caller must
    /// stage their effects and discard them on error. This does not clear the journal.
    pub fn visit_recorded(
        &self,
        mut emit: impl FnMut(&WalletAddress, Option<&[u8]>, Option<Vec<u8>>) -> Result<(), String>,
    ) -> Result<(), String> {
        let recording = self
            .recording
            .as_ref()
            .ok_or("BRN transition recording is disabled")?;
        if let Some(error) = &recording.error {
            return Err(error.clone());
        }
        for (address, before) in &recording.before {
            let after = self.get(address);
            if after
                .map(bincode::serialized_size)
                .transpose()
                .map_err(|error| error.to_string())?
                .unwrap_or(0)
                > MAX_RECORDED_BYTES as u64
            {
                return Err("BRN mutation after-image exceeds byte budget".into());
            }
            let after = after
                .map(bincode::serialize)
                .transpose()
                .map_err(|error| error.to_string())?;
            if before != &after {
                emit(address, before.as_deref(), after)?;
            }
        }
        Ok(())
    }
}
