//! Amount-ordered pending lookups. Keys contain destination, a zero separator,
//! complemented big-endian amount, then hash. This sorts larger amounts first
//! and uses ascending hashes to break ties.

use super::*;
use burst_store::pending::PendingPage;

fn prefix(destination: &[u8]) -> Vec<u8> {
    let mut bytes = destination.to_vec();
    bytes.push(0);
    bytes
}

pub(crate) fn key(pending: &[u8], amount: u128) -> Result<Vec<u8>, LmdbError> {
    if pending.len() <= 32 {
        return Err(LmdbError::Heed(
            "invalid pending key for amount index".into(),
        ));
    }
    let (destination, hash) = pending.split_at(pending.len() - 32);
    let mut key = prefix(destination);
    key.extend_from_slice(&(!amount).to_be_bytes());
    key.extend_from_slice(hash);
    Ok(key)
}

/// Rebuild all rows in the caller's transaction, buffering 1024 at a time.
pub(crate) fn rebuild(
    txn: &mut heed::RwTxn<'_>,
    pending_db: Database<Bytes, Bytes>,
    amount_db: Database<Bytes, Bytes>,
) -> Result<(), LmdbError> {
    amount_db.clear(txn)?;
    let mut after: Option<Vec<u8>> = None;
    loop {
        let bounds = (
            after.as_deref().map_or(Bound::Unbounded, Bound::Excluded),
            Bound::<&[u8]>::Unbounded,
        );
        let rows = pending_db
            .range(txn, &bounds)?
            .take(1024)
            .map(|row| {
                let (primary, bytes) = row?;
                let info: PendingInfo = bincode::deserialize(bytes)?;
                Ok((key(primary, info.amount)?, primary.to_vec()))
            })
            .collect::<Result<Vec<_>, LmdbError>>()?;
        if rows.is_empty() {
            break;
        }
        after = rows.last().map(|(_, primary)| primary.clone());
        for (index, primary) in rows {
            amount_db.put(txn, &index, &primary)?;
        }
    }
    Ok(())
}

/// Seek by amount/hash or skip `offset` index rows, then fetch at most `count`
/// payloads. One extra index row establishes `has_more` without reading its
/// payload. Each call opens a new snapshot; pages are not a fixed ledger view.
pub(super) fn page(
    store: &LmdbPendingStore,
    destination: &WalletAddress,
    threshold: u128,
    mut offset: u64,
    after: Option<(u128, TxHash)>,
    count: u32,
) -> Result<PendingPage, StoreError> {
    let lower = prefix(destination.as_str().as_bytes());
    let mut upper = lower.clone();
    upper.extend_from_slice(&(!threshold).to_be_bytes());
    upper.extend_from_slice(&[0xff; 32]);
    if after.is_some_and(|(amount, _)| amount < threshold) {
        return Ok(PendingPage {
            entries: Vec::new(),
            has_more: false,
        });
    }
    let seek = after.map(|(amount, hash)| {
        let mut key = lower.clone();
        key.extend_from_slice(&(!amount).to_be_bytes());
        key.extend_from_slice(hash.as_bytes());
        key
    });
    let txn = store.env.read_txn().map_err(LmdbError::from)?;
    let bounds = (
        seek.as_deref()
            .map_or(Bound::Included(lower.as_slice()), Bound::Excluded),
        Bound::Included(upper.as_slice()),
    );
    let mut entries = Vec::new();
    let mut has_more = false;
    for row in store
        .pending_amount_db
        .range(&txn, &bounds)
        .map_err(LmdbError::from)?
    {
        let (index, primary) = row.map_err(LmdbError::from)?;
        if index.len() != lower.len() + 48 || !index.starts_with(&lower) {
            return Err(StoreError::Corruption(
                "invalid pending amount index key".into(),
            ));
        }
        if offset > 0 {
            offset -= 1;
            continue;
        }
        if entries.len() == count as usize {
            has_more = true;
            break;
        }
        let bytes = store
            .pending_db
            .get(&txn, primary)
            .map_err(LmdbError::from)?
            .ok_or_else(|| StoreError::Corruption("pending amount index has no transfer".into()))?;
        let info: PendingInfo = bincode::deserialize(bytes).map_err(LmdbError::from)?;
        if key(primary, info.amount)? != index {
            return Err(StoreError::Corruption(
                "pending amount index differs from transfer".into(),
            ));
        }
        let hash = TxHash::new(
            index[index.len() - 32..]
                .try_into()
                .expect("checked hash length"),
        );
        entries.push((hash, info));
    }
    Ok(PendingPage { entries, has_more })
}
