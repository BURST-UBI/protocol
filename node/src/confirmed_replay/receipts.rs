use super::{invalid, scratch_records::ScratchRecords, ReplayError};
use crate::checkpoint_replay::SelectionReceipt;
use serde::{
    de::{DeserializeOwned, SeqAccess},
    ser::SerializeSeq,
    Deserialize, Serialize,
};
use std::{borrow::Cow, path::Path, sync::Arc};

#[derive(Clone)]
pub(super) enum OrderedHistory<T> {
    Memory(Vec<T>),
    Disk {
        rows: Arc<ScratchRecords<T>>,
        len: usize,
    },
}

pub(super) type ReceiptHistory = OrderedHistory<SelectionReceipt>;
pub(super) type CheckpointHistory = OrderedHistory<burst_types::BlockHash>;

impl<T> Default for OrderedHistory<T> {
    fn default() -> Self {
        Self::Memory(Vec::new())
    }
}

fn key(index: usize) -> [u8; 32] {
    let mut key = [0; 32];
    key[..8].copy_from_slice(&(index as u64).to_be_bytes());
    key
}

impl<T: Clone + Serialize + DeserializeOwned> OrderedHistory<T> {
    pub(super) fn len(&self) -> usize {
        match self {
            Self::Memory(rows) => rows.len(),
            Self::Disk { len, .. } => *len,
        }
    }

    pub(super) fn stage(&mut self, directory: &Path) -> Result<(), ReplayError> {
        let Self::Memory(receipts) = self else {
            return Ok(());
        };
        let mut rows = ScratchRecords::new(directory).map_err(|e| invalid(e.to_string()))?;
        for (index, receipt) in receipts.iter().enumerate() {
            rows.put(key(index), receipt)
                .map_err(|e| invalid(e.to_string()))?;
        }
        *self = Self::Disk {
            rows: Arc::new(rows),
            len: receipts.len(),
        };
        Ok(())
    }

    pub(super) fn get(&self, index: usize) -> Result<Option<Cow<'_, T>>, ReplayError> {
        match self {
            Self::Memory(rows) => Ok(rows.get(index).map(Cow::Borrowed)),
            Self::Disk { rows, len } => {
                if index >= *len {
                    return Ok(None);
                }
                rows.get(&key(index))
                    .map_err(|e| invalid(e.to_string()))?
                    .map(|row| Some(Cow::Owned(row)))
                    .ok_or_else(|| invalid("missing staged history row"))
            }
        }
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = Result<Cow<'_, T>, ReplayError>> {
        (0..self.len()).map(|index| {
            self.get(index)?
                .ok_or_else(|| invalid("missing history row"))
        })
    }

    pub(super) fn push(&mut self, receipt: T) -> Result<(), ReplayError> {
        match self {
            Self::Memory(rows) => rows.push(receipt),
            Self::Disk { rows, len } => {
                let next = len
                    .checked_add(1)
                    .ok_or_else(|| invalid("history row count overflow"))?;
                if Arc::get_mut(rows).is_none() {
                    *rows = Arc::new(rows.fork().map_err(|e| invalid(e.to_string()))?);
                }
                if Arc::get_mut(rows)
                    .expect("private history rows")
                    .put(key(*len), &receipt)
                    .map_err(|e| invalid(e.to_string()))?
                {
                    return Err(invalid("duplicate staged history position"));
                }
                *len = next;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn truncate_for_test(&self) {
        let Self::Disk { rows, .. } = self else {
            panic!("history corruption test requires staged storage");
        };
        rows.truncate_for_test();
    }

    #[cfg(test)]
    pub(super) fn shares_storage(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Disk { rows: a, .. }, Self::Disk { rows: b, .. }) => Arc::ptr_eq(a, b),
            (Self::Memory(a), Self::Memory(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(a, b)| std::ptr::eq(a, b))
            }
            _ => false,
        }
    }

    #[cfg(test)]
    pub(super) fn replace_for_test(&mut self, index: usize, receipt: T) {
        match self {
            Self::Memory(rows) => rows[index] = receipt,
            Self::Disk { rows, len } => {
                assert!(index < *len);
                if Arc::get_mut(rows).is_none() {
                    *rows = Arc::new(rows.fork().unwrap());
                }
                assert!(Arc::get_mut(rows)
                    .unwrap()
                    .put(key(index), &receipt)
                    .unwrap());
            }
        }
    }
}

impl<T: Clone + Serialize + DeserializeOwned> Serialize for OrderedHistory<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.len()))?;
        for receipt in self.iter() {
            sequence.serialize_element(receipt.map_err(serde::ser::Error::custom)?.as_ref())?;
        }
        sequence.end()
    }
}

impl<'de, T: Clone + Serialize + DeserializeOwned> Deserialize<'de> for OrderedHistory<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Rows<T>(std::marker::PhantomData<T>);
        impl<'de, T: Clone + Serialize + DeserializeOwned> serde::de::Visitor<'de> for Rows<T> {
            type Value = OrderedHistory<T>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("ordered replay history")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut input: A) -> Result<Self::Value, A::Error> {
                let mut history = OrderedHistory::default();
                history
                    .stage(&std::env::temp_dir())
                    .map_err(serde::de::Error::custom)?;
                while let Some(receipt) = input.next_element()? {
                    history.push(receipt).map_err(serde::de::Error::custom)?;
                }
                Ok(history)
            }
        }
        deserializer.deserialize_seq(Rows(std::marker::PhantomData))
    }
}
