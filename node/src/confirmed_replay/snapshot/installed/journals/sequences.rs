use super::*;
use crate::confirmed_replay::scratch_records::ScratchRecords;

/// Captured minimum signing sequences. Existing counters may increase and new
/// signers may appear, but captured counters must not disappear or decrease.
pub(super) struct SequenceGuard {
    rows: ScratchRecords<u64>,
    count: u64,
}

pub(super) fn visit_view(
    batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    mut visitor: impl FnMut(&[u8], u64) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    for row in batch.checkpoint_metadata_entries(b"consensus_sequence/")? {
        let (key, bytes) = row?;
        std::str::from_utf8(key)
            .map_err(invalid)?
            .strip_prefix("consensus_sequence/v1/")
            .filter(|signer| burst_crypto::decode_address(signer).is_some())
            .ok_or_else(|| invalid("invalid sequence signer or version"))?;
        let sequence = u64::from_be_bytes(
            bytes
                .try_into()
                .map_err(|_| invalid("invalid signer counter encoding"))?,
        );
        if sequence == 0 {
            return Err(invalid("zero persisted signing sequence"));
        }
        visitor(key, sequence)?;
    }
    Ok(())
}

impl SequenceGuard {
    #[cfg(test)]
    pub(super) fn capture(batch: &LmdbWriteTransaction<'_>) -> Result<Self, StoreError> {
        Self::capture_view(&batch.read_view())
    }

    #[cfg(test)]
    pub(super) fn capture_view(
        batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    ) -> Result<Self, StoreError> {
        Self::capture_view_with_poll(batch, &|_| Ok(()))
    }

    pub(super) fn capture_view_with_poll(
        batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<Self, StoreError> {
        let mut guard = Self {
            rows: ScratchRecords::new(batch.storage_directory())?,
            count: 0,
        };
        visit_view(batch, |key, sequence| {
            poll("journal:sequence-capture")?;
            if guard.rows.put(burst_crypto::blake2b_256(key), &sequence)? {
                return Err(invalid("duplicate signing sequence observation"));
            }
            guard.count = guard
                .count
                .checked_add(1)
                .ok_or_else(|| invalid("sequence count overflow"))?;
            Ok(())
        })?;
        Ok(guard)
    }

    #[cfg(test)]
    pub(super) fn verify(&self, batch: &LmdbWriteTransaction<'_>) -> Result<(), StoreError> {
        self.verify_view(&batch.read_view())
    }

    #[cfg(test)]
    pub(super) fn verify_view(
        &self,
        batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    ) -> Result<(), StoreError> {
        self.verify_view_with_poll(batch, &|_| Ok(()))
    }

    pub(super) fn verify_view_with_poll(
        &self,
        batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let mut retained = 0u64;
        visit_view(batch, |key, current| {
            poll("metadata:signing-sequence")?;
            if let Some(previous) = self.rows.get(&burst_crypto::blake2b_256(key))? {
                if current < previous {
                    return Err(invalid("signing sequence decreased during recovery"));
                }
                retained = retained
                    .checked_add(1)
                    .ok_or_else(|| invalid("sequence count overflow"))?;
            }
            Ok(())
        })?;
        if retained != self.count {
            return Err(invalid("signing sequence disappeared during recovery"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequences_may_advance_but_never_disappear_or_decrease() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let signer = |seed| {
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[seed; 32]).public)
        };
        let key = format!("consensus_sequence/v1/{}", signer(1));
        let other = format!("consensus_sequence/v1/{}", signer(2));
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta(&key, &12u64.to_be_bytes()).unwrap();
        let capture_reached = std::cell::Cell::new(false);
        let interrupted = SequenceGuard::capture_view_with_poll(&batch.read_view(), &|stage| {
            assert_eq!(stage, "journal:sequence-capture");
            capture_reached.set(true);
            Err(StoreError::Backend("sequence capture sentinel".into()))
        });
        assert!(
            matches!(interrupted, Err(ref error) if error.to_string().contains("sequence capture sentinel"))
        );
        assert!(capture_reached.get());
        let guard = SequenceGuard::capture(&batch).unwrap();
        let reached = std::cell::Cell::new(false);
        assert!(guard
            .verify_view_with_poll(&batch.read_view(), &|stage| {
                assert_eq!(stage, "metadata:signing-sequence");
                reached.set(true);
                Err(StoreError::Backend("sequence checkpoint sentinel".into()))
            })
            .unwrap_err()
            .to_string()
            .contains("sequence checkpoint sentinel"));
        assert!(reached.get());
        for sequence in [12u64, 13, u64::MAX] {
            batch.put_meta(&key, &sequence.to_be_bytes()).unwrap();
            guard.verify(&batch).unwrap();
        }
        batch.put_meta(&other, &1u64.to_be_bytes()).unwrap();
        guard.verify(&batch).unwrap();
        for bytes in [
            11u64.to_be_bytes().to_vec(),
            vec![1],
            0u64.to_be_bytes().to_vec(),
        ] {
            batch.put_meta(&key, &bytes).unwrap();
            assert!(guard.verify(&batch).is_err());
        }
        batch.delete_meta(&key).unwrap();
        assert!(guard.verify(&batch).is_err());
        batch.put_meta(&key, &12u64.to_be_bytes()).unwrap();
        batch
            .put_meta("consensus_sequence/v2/invalid", &1u64.to_be_bytes())
            .unwrap();
        let before = batch.checkpoint_metadata(b"").unwrap();
        assert!(guard.verify(&batch).is_err());
        assert_eq!(batch.checkpoint_metadata(b"").unwrap(), before);
    }
}
