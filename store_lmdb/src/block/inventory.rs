use super::*;
use crate::write_batch::checkpoint_projection::CheckpointBlock;

pub struct IndexedBlock<'a> {
    pub hash: BlockHash,
    pub account: WalletAddress,
    pub height: u64,
    pub bytes: &'a [u8],
}

pub(crate) fn entries<'a>(
    txn: &'a heed::RoTxn<'_>,
    blocks: Database<Bytes, Bytes>,
    heights: Database<Bytes, Bytes>,
    reverse: Database<Bytes, Bytes>,
    max_rows: usize,
    max_bytes: usize,
) -> Result<impl Iterator<Item = Result<IndexedBlock<'a>, StoreError>> + 'a, StoreError> {
    let invalid = |message: &str| StoreError::Corruption(message.into());
    let count = blocks.len(txn).map_err(LmdbError::from)?;
    if count > max_rows as u64
        || heights.len(txn).map_err(LmdbError::from)? != count
        || reverse.len(txn).map_err(LmdbError::from)? != count
    {
        return Err(invalid("retained block inventory count or index mismatch"));
    }
    let mut total = 0usize;
    Ok(blocks
        .iter(txn)
        .map_err(LmdbError::from)?
        .map(move |entry| {
            let (hash, bytes) = entry.map_err(LmdbError::from)?;
            let hash = BlockHash::new(
                hash.try_into()
                    .map_err(|_| invalid("invalid retained block hash"))?,
            );
            let key = reverse
                .get(txn, hash.as_bytes())
                .map_err(LmdbError::from)?
                .ok_or_else(|| invalid("retained block has no reverse index"))?;
            if hash.is_zero() || bytes.is_empty() || key.len() <= 8 {
                return Err(invalid("invalid retained block row"));
            }
            if heights.get(txn, key).map_err(LmdbError::from)? != Some(hash.as_bytes().as_slice()) {
                return Err(invalid("conflicting retained block indexes"));
            }
            let (account, height) = key.split_at(key.len() - 8);
            let height = u64::from_be_bytes(height.try_into().expect("checked index length"));
            let account = std::str::from_utf8(account)
                .map_err(|_| invalid("invalid retained block account encoding"))?;
            if !account.starts_with(WalletAddress::PREFIX)
                || account.len() <= WalletAddress::PREFIX.len()
            {
                return Err(invalid("invalid retained block account"));
            }
            if height == 0 {
                return Err(invalid("zero retained block height"));
            }
            // Bound bodies and all three tables' keys/values before allocating.
            total = total
                .checked_add(bytes.len())
                .and_then(|n| key.len().checked_mul(2).and_then(|k| n.checked_add(k)))
                .and_then(|n| n.checked_add(96))
                .filter(|n| *n <= max_bytes)
                .ok_or_else(|| invalid("retained block inventory exceeds byte limit"))?;
            Ok(IndexedBlock {
                hash,
                account: WalletAddress::new(account.to_owned()),
                height,
                bytes,
            })
        }))
}

pub(crate) fn read(
    txn: &heed::RoTxn<'_>,
    blocks: Database<Bytes, Bytes>,
    heights: Database<Bytes, Bytes>,
    reverse: Database<Bytes, Bytes>,
    max_rows: usize,
    max_bytes: usize,
) -> Result<Vec<CheckpointBlock>, StoreError> {
    entries(txn, blocks, heights, reverse, max_rows, max_bytes)?
        .map(|row| {
            let row = row?;
            Ok(CheckpointBlock {
                hash: row.hash,
                account: row.account,
                height: row.height,
                bytes: row.bytes.to_vec(),
            })
        })
        .collect()
}

impl LmdbBlockStore {
    pub fn indexed_block_entries<'a>(
        &self,
        txn: &'a LmdbReadTransaction<'_>,
        max_rows: usize,
        max_bytes: usize,
    ) -> Result<impl Iterator<Item = Result<IndexedBlock<'a>, StoreError>> + 'a, StoreError> {
        entries(
            &txn.txn,
            self.blocks_db,
            self.height_db,
            self.block_height_db,
            max_rows,
            max_bytes,
        )
    }

    /// Inventory physical rows, including sparse histories, from one read view.
    /// Index bijection is checked; block signatures and ancestry are not.
    pub fn indexed_blocks_txn(
        &self,
        txn: &LmdbReadTransaction<'_>,
        max_rows: usize,
        max_bytes: usize,
    ) -> Result<Vec<CheckpointBlock>, StoreError> {
        read(
            &txn.txn,
            self.blocks_db,
            self.height_db,
            self.block_height_db,
            max_rows,
            max_bytes,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LmdbStore;

    fn fixture() -> (tempfile::TempDir, LmdbStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 64, 16 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_block_with_account(
                &BlockHash::new([1; 32]),
                &[1, 2],
                &WalletAddress::new("brst_test"),
                u64::MAX,
            )
            .unwrap();
        batch.commit().unwrap();
        (dir, store)
    }

    #[test]
    fn sparse_inventory_is_bounded_and_consistent() {
        let (_dir, store) = fixture();
        let blocks = store.block_store();
        let txn = store.tx_begin_read().unwrap();
        let rows = blocks.indexed_blocks_txn(&txn, 1, 1024).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].height, u64::MAX);
        assert_eq!(rows[0].bytes, [1, 2]);
        assert!(blocks.indexed_blocks_txn(&txn, 0, 1024).is_err());
        assert!(blocks.indexed_blocks_txn(&txn, 1, 1).is_err());
        let borrowed = blocks
            .indexed_block_entries(&txn, 1, 1024)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(borrowed[0].bytes, [1, 2]);
        assert!(blocks
            .indexed_block_entries(&txn, 1, 1)
            .unwrap()
            .next()
            .unwrap()
            .is_err());
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_block_with_account(
                &BlockHash::new([2; 32]),
                &[3],
                &WalletAddress::new("brst_other"),
                1,
            )
            .unwrap();
        assert_eq!(batch.indexed_blocks(2, 1024).unwrap().len(), 2);
        assert_eq!(
            batch
                .indexed_block_entries(2, 1024)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
                .len(),
            2
        );
        batch.commit().unwrap();
        assert_eq!(borrowed[0].bytes, [1, 2]);
        assert_eq!(
            blocks.indexed_block_entries(&txn, 1, 1024).unwrap().count(),
            1
        );
        assert_eq!(blocks.indexed_blocks_txn(&txn, 1, 1024).unwrap().len(), 1);
        drop(txn);
        assert_eq!(
            blocks
                .indexed_blocks_txn(&store.tx_begin_read().unwrap(), 2, 1024)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn inventory_rejects_orphan_conflicting_and_malformed_rows() {
        for corruption in 0..8 {
            let (_dir, store) = fixture();
            let blocks = store.block_store();
            let mut txn = blocks.env.write_txn().unwrap();
            let hash = [1; 32];
            let key = height_key(&WalletAddress::new("brst_test"), u64::MAX);
            match corruption {
                0 => {
                    blocks.height_db.delete(&mut txn, &key).unwrap();
                }
                1 => {
                    blocks.block_height_db.delete(&mut txn, &hash).unwrap();
                }
                2 => {
                    blocks.blocks_db.put(&mut txn, &[2; 32], &[1]).unwrap();
                }
                3 => {
                    blocks.height_db.put(&mut txn, &key, &[2; 32]).unwrap();
                }
                4 => {
                    blocks.blocks_db.put(&mut txn, &hash, &[]).unwrap();
                }
                5..=7 => {
                    let bad_key = match corruption {
                        5 => height_key(&WalletAddress::new("brst_test"), 0),
                        6 => [b"invalid".as_slice(), &1u64.to_be_bytes()].concat(),
                        _ => [b"\xff".as_slice(), &1u64.to_be_bytes()].concat(),
                    };
                    blocks.height_db.delete(&mut txn, &key).unwrap();
                    blocks.height_db.put(&mut txn, &bad_key, &hash).unwrap();
                    blocks
                        .block_height_db
                        .put(&mut txn, &hash, &bad_key)
                        .unwrap();
                }
                _ => unreachable!(),
            }
            txn.commit().unwrap();
            assert!(
                blocks
                    .indexed_blocks_txn(&store.tx_begin_read().unwrap(), 4, 1024)
                    .is_err(),
                "corruption {corruption}"
            );
            assert!(
                store
                    .tx_begin_write()
                    .unwrap()
                    .indexed_blocks(4, 1024)
                    .is_err(),
                "writer corruption {corruption}"
            );
            let batch = store.tx_begin_write().unwrap();
            let result = batch
                .indexed_block_entries(4, 1024)
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>());
            assert!(result.is_err(), "borrowed writer corruption {corruption}");
        }
    }
}
