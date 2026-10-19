use super::*;
use crate::confirmed_replay::scratch_records::ScratchRecords;

// Retransmitted votes advance counters without changing economic replay inputs.
// Keep every captured high-water mark; removal or rollback still fails closed.
pub(super) struct SequenceGuard {
    rows: ScratchRecords<u64>,
    count: u64,
}

pub(super) fn visit(
    batch: &LmdbWriteTransaction<'_>,
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
    pub(super) fn capture(batch: &LmdbWriteTransaction<'_>) -> Result<Self, StoreError> {
        let mut guard = Self {
            rows: ScratchRecords::new(batch.storage_directory())?,
            count: 0,
        };
        visit(batch, |key, sequence| {
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

    pub(super) fn verify(&self, batch: &LmdbWriteTransaction<'_>) -> Result<(), StoreError> {
        let mut retained = 0u64;
        visit(batch, |key, current| {
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
        let guard = SequenceGuard::capture(&batch).unwrap();
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
