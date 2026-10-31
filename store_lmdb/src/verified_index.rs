//! Derived electorate membership. Account rows alone own undo; membership follows
//! their transactional writes, including rewind and checkpoint replacement.
use crate::LmdbError;
use burst_store::{account::AccountInfo, StoreError};
use burst_types::WalletAddress;
use heed::{types::Bytes, Database, RoTxn, RwTxn};
use std::ops::Bound;
type Db = Database<Bytes, Bytes>;

fn verified(key: &[u8], bytes: &[u8]) -> Result<bool, LmdbError> {
    let account: AccountInfo = bincode::deserialize(bytes)?;
    if account.address.as_str().as_bytes() != key {
        return Err(LmdbError::Heed(
            "verified index account key mismatch".into(),
        ));
    }
    Ok(account.state.is_verified())
}

pub(crate) fn update(
    txn: &mut RwTxn<'_>,
    accounts: Db,
    index: Db,
    key: &[u8],
    after: Option<&[u8]>,
) -> Result<bool, LmdbError> {
    let before = accounts
        .get(txn, key)?
        .map(|bytes| verified(key, bytes))
        .transpose()?
        .unwrap_or(false);
    let next = after
        .map(|bytes| verified(key, bytes))
        .transpose()?
        .unwrap_or(false);
    if index.get(txn, key)? != before.then_some(&[1][..]) {
        return Err(LmdbError::Heed(
            "verified index disagrees with account".into(),
        ));
    }
    if before != next {
        if next {
            index.put(txn, key, &[1])?;
        } else {
            index.delete(txn, key)?;
        }
    }
    Ok(before != next)
}

/// Reconcile membership in the caller's write transaction. Batches of 256 bound
/// temporary row storage, not total work or the lifetime of the transaction.
pub(crate) fn rebuild(txn: &mut RwTxn<'_>, accounts: Db, index: Db) -> Result<(), LmdbError> {
    // Remove stale or malformed index rows without rewriting correct membership.
    // Check against the old primary view before ordinary account writes resume.
    let mut cursor: Option<Vec<u8>> = None;
    loop {
        let bounds = (
            cursor.as_deref().map_or(Bound::Unbounded, Bound::Excluded),
            Bound::<&[u8]>::Unbounded,
        );
        let rows = index
            .range(txn, &bounds)?
            .take(256)
            .map(|row| {
                let (key, marker) = row?;
                let eligible = accounts
                    .get(txn, key)?
                    .map(|bytes| verified(key, bytes))
                    .transpose()?
                    .unwrap_or(false);
                Ok((key.to_vec(), eligible && marker == [1]))
            })
            .collect::<Result<Vec<_>, LmdbError>>()?;
        if rows.is_empty() {
            break;
        }
        cursor = rows.last().map(|(key, _)| key.clone());
        for (key, keep) in rows {
            if !keep {
                index.delete(txn, &key)?;
            }
        }
    }
    let mut cursor: Option<Vec<u8>> = None;
    loop {
        let bounds = (
            cursor.as_deref().map_or(Bound::Unbounded, Bound::Excluded),
            Bound::<&[u8]>::Unbounded,
        );
        let rows = accounts
            .range(txn, &bounds)?
            .take(256)
            .map(|row| {
                let (key, bytes) = row?;
                Ok((key.to_vec(), verified(key, bytes)?))
            })
            .collect::<Result<Vec<_>, LmdbError>>()?;
        if rows.is_empty() {
            break;
        }
        cursor = rows.last().map(|(key, _)| key.clone());
        for (key, eligible) in rows {
            if eligible && index.get(txn, &key)? != Some(&[1][..]) {
                index.put(txn, &key, &[1])?;
            }
        }
    }
    Ok(())
}

/// Read index addresses and check marker bytes. Does not cross-check account
/// rows or validate address checksums.
pub(crate) fn addresses(txn: &RoTxn<'_>, index: Db) -> Result<Vec<WalletAddress>, StoreError> {
    let mut result = Vec::new();
    for row in index.iter(txn).map_err(LmdbError::from)? {
        let (key, value) = row.map_err(LmdbError::from)?;
        if value != [1] {
            return Err(StoreError::Corruption(
                "invalid verified index marker".into(),
            ));
        }
        let address =
            std::str::from_utf8(key).map_err(|e| StoreError::Corruption(e.to_string()))?;
        result.push(WalletAddress::new(address));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebuild_repairs_index_atomically_and_preserves_clean_pages() {
        let directory = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let mut txn = store.env().write_txn().unwrap();
        for n in 0..600 {
            let mut account =
                AccountInfo::unopened(WalletAddress::new(format!("brst_index_{n:04}")));
            if n % 2 == 0 {
                account.state = burst_types::WalletState::Verified;
            }
            store
                .accounts_db
                .put(
                    &mut txn,
                    account.address.as_str().as_bytes(),
                    &bincode::serialize(&account).unwrap(),
                )
                .unwrap();
            if n % 3 == 0 {
                store
                    .verified_accounts_db
                    .put(&mut txn, account.address.as_str().as_bytes(), b"bad")
                    .unwrap();
            }
        }
        store
            .verified_accounts_db
            .put(&mut txn, b"orphan", &[1])
            .unwrap();
        txn.commit().unwrap();
        let original = {
            let txn = store.env().read_txn().unwrap();
            store
                .verified_accounts_db
                .iter(&txn)
                .unwrap()
                .map(|row| {
                    let (k, v) = row.unwrap();
                    (k.to_vec(), v.to_vec())
                })
                .collect::<Vec<_>>()
        };
        for commit in [false, true] {
            let mut txn = store.env().write_txn().unwrap();
            rebuild(&mut txn, store.accounts_db, store.verified_accounts_db).unwrap();
            assert_eq!(store.verified_accounts_db.len(&txn).unwrap(), 300);
            for n in 0..600 {
                assert_eq!(
                    store
                        .verified_accounts_db
                        .get(&txn, format!("brst_index_{n:04}").as_bytes())
                        .unwrap(),
                    (n % 2 == 0).then_some(&[1][..])
                );
            }
            if commit {
                txn.commit().unwrap();
            } else {
                drop(txn);
                let txn = store.env().read_txn().unwrap();
                let actual = store
                    .verified_accounts_db
                    .iter(&txn)
                    .unwrap()
                    .map(|row| {
                        let (k, v) = row.unwrap();
                        (k.to_vec(), v.to_vec())
                    })
                    .collect::<Vec<_>>();
                assert_eq!(actual, original);
            }
        }
        let before = store.env().read_txn().unwrap().id();
        let mut txn = store.env().write_txn().unwrap();
        rebuild(&mut txn, store.accounts_db, store.verified_accounts_db).unwrap();
        txn.commit().unwrap();
        // LMDB does not publish a new transaction when no database page changed.
        assert_eq!(store.env().read_txn().unwrap().id(), before);
        store.force_sync().unwrap();
        drop(store);
        let store = crate::LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let txn = store.env().read_txn().unwrap();
        assert_eq!(store.verified_accounts_db.len(&txn).unwrap(), 300);
    }
}
