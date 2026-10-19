use super::*;
use bincode::Options;
use burst_store::account::AccountInfo;

/// Reconcile only after the writer has rechecked the recovered cuts. This
/// preserves existing finality; it never cements replayed provisional blocks.
pub(in crate::confirmed_replay::snapshot::installed) fn reconcile(
    batch: &LmdbWriteTransaction<'_>,
    records: impl IntoIterator<Item = Result<ReplayedEconomicChange, StoreError>>,
) -> Result<ScratchSequence<ReplayedEconomicChange>, StoreError> {
    batch
        .account_entries()?
        .try_for_each(|account| account.map(|_| ()))?;
    let mut provisional = ScratchSequence::new(batch.storage_directory())?;
    for record in records {
        let mut record = record?;
        let account = batch.get_account(&record.account)?;
        if record.height == 0
            || record.height > account.block_count
            || batch.block_at_height(&record.account, record.height)? != Some(record.block)
            || batch.block_height(&record.account, &record.block)? != record.height
        {
            return Err(invalid(
                "replayed undo differs from indexed account history",
            ));
        }
        if record.height <= account.confirmation_height {
            continue;
        }
        for change in record
            .changes
            .iter_mut()
            .filter(|change| change.table == UndoTable::Accounts)
        {
            for bytes in [&mut change.before, &mut change.after]
                .into_iter()
                .flatten()
            {
                let mut image: AccountInfo = bincode::DefaultOptions::new()
                    .with_fixint_encoding()
                    .with_limit(MAX_BYTES as u64)
                    .reject_trailing_bytes()
                    .deserialize(bytes)
                    .map_err(invalid)?;
                let current = batch.get_account(&image.address)?;
                if change.key != image.address.as_str().as_bytes()
                    || image.confirmation_height > current.confirmation_height
                    || current.confirmation_height > image.block_count
                {
                    return Err(invalid(
                        "provisional undo crosses confirmed account history",
                    ));
                }
                image.confirmation_height = current.confirmation_height;
                *bytes = bincode::serialize(&image).map_err(invalid)?;
            }
        }
        provisional.push(&record)?;
    }
    Ok(provisional)
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::WalletAddress;

    #[test]
    fn recovered_finality_excludes_cemented_rows_and_preserves_the_remaining_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let address = WalletAddress::new("brst_finality_fixture");
        let mut before = AccountInfo::unopened(address.clone());
        before.block_count = 2;
        before.head = BlockHash::new([2; 32]);
        before.confirmation_height = 1;
        let mut after = before.clone();
        after.block_count = 3;
        after.head = BlockHash::new([3; 32]);
        let mut current = after.clone();
        current.confirmation_height = 2;
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_account_info(&current, false).unwrap();
        for account in [&before, &after] {
            batch
                .put_block_with_account(&account.head, b"fixture", &address, account.block_count)
                .unwrap();
        }
        batch.commit().unwrap();
        let record = |height| ReplayedEconomicChange {
            block: BlockHash::new([height as u8; 32]),
            account: address.clone(),
            height,
            changes: vec![UndoChange {
                table: UndoTable::Accounts,
                key: address.as_str().as_bytes().to_vec(),
                before: Some(bincode::serialize(&before).unwrap()),
                after: Some(bincode::serialize(&after).unwrap()),
            }],
        };
        let batch = store.tx_begin_write().unwrap();
        let records = reconcile(&batch, vec![Ok(record(2)), Ok(record(3))]).unwrap();
        let records = records.iter().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].height, 3);
        for bytes in [&records[0].changes[0].before, &records[0].changes[0].after] {
            let image: AccountInfo = bincode::deserialize(bytes.as_ref().unwrap()).unwrap();
            assert_eq!(image.confirmation_height, 2);
        }
        for case in 0..5 {
            let mut bad = record(3);
            match case {
                0 => bad.block = BlockHash::new([9; 32]),
                1 => bad.changes[0].key = b"foreign".to_vec(),
                2 => {
                    let mut old = before.clone();
                    old.block_count = 1;
                    bad.changes[0].before = Some(bincode::serialize(&old).unwrap());
                }
                3 => {
                    let mut old = before.clone();
                    old.confirmation_height = 3;
                    bad.changes[0].before = Some(bincode::serialize(&old).unwrap());
                }
                _ => bad.changes[0].before.as_mut().unwrap().push(0),
            }
            assert!(reconcile(&batch, vec![Ok(bad)]).is_err(), "case {case}");
        }
        assert_eq!(batch.get_account(&address).unwrap().confirmation_height, 2);
    }
}
