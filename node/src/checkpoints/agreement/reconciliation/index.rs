use super::*;
use crate::confirmed_replay::scratch_records::ScratchRecords;

#[derive(Serialize, Deserialize)]
struct Entry {
    representative: WalletAddress,
    previous: Option<u64>,
}

/// Disposable lookup positions only. Journal bytes still come from the pinned
/// transaction and are authenticated against the matching checkpoint anchor.
pub(super) struct JournalIndex {
    heads: ScratchRecords<u64>,
    entries: ScratchRecords<Entry>,
    count: u64,
}

fn key(position: u64) -> [u8; 32] {
    let mut key = [0; 32];
    key[24..].copy_from_slice(&position.to_be_bytes());
    key
}

impl JournalIndex {
    pub(super) fn new(directory: &std::path::Path) -> Result<Self, StoreError> {
        Ok(Self {
            heads: ScratchRecords::new(directory)?,
            entries: ScratchRecords::new(directory)?,
            count: 0,
        })
    }

    pub(super) fn push(
        &mut self,
        height: u64,
        representative: WalletAddress,
    ) -> Result<(), StoreError> {
        let next = self
            .count
            .checked_add(1)
            .ok_or_else(|| invalid("journal index overflow"))?;
        let previous = self.heads.get(&key(height))?;
        if self.entries.put(
            key(self.count),
            &Entry {
                representative,
                previous,
            },
        )? {
            return Err(invalid("journal index overwrote a position"));
        }
        self.heads.put(key(height), &self.count)?;
        self.count = next;
        Ok(())
    }

    pub(super) fn len(&self) -> u64 {
        self.count
    }

    pub(super) fn visit(
        &self,
        height: u64,
        mut visit: impl FnMut(&WalletAddress) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let mut position = self.heads.get(&key(height))?;
        while let Some(index) = position {
            if index >= self.count {
                return Err(invalid("journal index position out of bounds"));
            }
            let row = self
                .entries
                .get(&key(index))?
                .ok_or_else(|| invalid("missing journal index position"))?;
            if row.previous.is_some_and(|previous| previous >= index) {
                return Err(invalid("journal index is not strictly descending"));
            }
            visit(&row.representative)?;
            position = row.previous;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_heights_visit_only_present_journals_and_reject_damaged_scratch() {
        let directory = tempfile::tempdir().unwrap();
        let mut index = JournalIndex::new(directory.path()).unwrap();
        let signer = |value: u64| {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&value.to_be_bytes());
            burst_crypto::derive_address(&burst_types::PublicKey(bytes))
        };
        // Key order in LMDB is signer first, not checkpoint height.
        for value in 0..1024 {
            index
                .push(if value % 2 == 0 { 7 } else { 101 }, signer(value))
                .unwrap();
        }
        assert_eq!(index.len(), 1024);
        let mut visited = 0;
        for height in 1..=256 {
            let mut at_height = 0;
            index
                .visit(height, |representative| {
                    let expected = 1022 - at_height * 2 + u64::from(height == 101);
                    assert_eq!(*representative, signer(expected));
                    at_height += 1;
                    visited += 1;
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                at_height,
                if height == 7 || height == 101 { 512 } else { 0 }
            );
        }
        assert_eq!(visited, 1024);
        let mut callbacks = 0;
        assert!(index
            .visit(7, |_| {
                callbacks += 1;
                Err(invalid("stop"))
            })
            .is_err());
        assert_eq!(callbacks, 1);
        index.entries.truncate_for_test();
        assert!(index.visit(7, |_| Ok(())).is_err());
        index.heads.truncate_for_test();
        assert!(index.visit(99, |_| Ok(())).is_err());
    }
}
