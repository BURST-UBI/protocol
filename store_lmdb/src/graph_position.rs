//! Derived graph edge ordering. Primary edge rows alone own block undo;
//! rebuilding or maintaining this index never creates a shared-parent undo key.
use crate::LmdbError;
use burst_trst::merger_graph::GraphRecord;
use heed::{types::Bytes, Database, RwTxn};
use std::ops::Bound;

type Db = Database<Bytes, Bytes>;

pub(crate) fn edge_key(primary: &[u8], bytes: &[u8]) -> Result<Option<Vec<u8>>, LmdbError> {
    if !primary.starts_with(b"trst_graph/v1/3/") && !primary.starts_with(b"trst_graph/v1/4/") {
        return Ok(None);
    }
    let key = std::str::from_utf8(primary).map_err(|e| LmdbError::Serialization(e.to_string()))?;
    let record = GraphRecord::from_storage_entry(key, bytes).map_err(LmdbError::Serialization)?;
    let (kind, parent, position) = match record {
        GraphRecord::OriginEdge {
            origin, position, ..
        } => (3, origin, position),
        GraphRecord::DownstreamEdge {
            merge, position, ..
        } => (4, merge, position),
        _ => unreachable!("record decoder binds key and variant"),
    };
    let mut index = vec![kind];
    index.extend_from_slice(parent.as_bytes());
    index.extend_from_slice(&position.to_be_bytes());
    Ok(Some(index))
}

pub(crate) fn update(
    txn: &mut RwTxn<'_>,
    meta: Db,
    index: Db,
    key: &[u8],
    value: Option<&[u8]>,
) -> Result<(), LmdbError> {
    if !key.starts_with(b"trst_graph/v1/3/") && !key.starts_with(b"trst_graph/v1/4/") {
        return Ok(());
    }
    let before = meta
        .get(txn, key)?
        .map(|bytes| edge_key(key, bytes))
        .transpose()?
        .flatten();
    let after = value
        .map(|bytes| edge_key(key, bytes))
        .transpose()?
        .flatten();
    if let Some(old) = before.as_deref() {
        if index.get(txn, old)? != Some(key) {
            return Err(LmdbError::Heed(
                "graph position index disagrees with primary edge".into(),
            ));
        }
    }
    if let Some(new) = after.as_deref() {
        if index.get(txn, new)?.is_some_and(|owner| owner != key) {
            return Err(LmdbError::Heed("duplicate graph edge position".into()));
        }
    }
    if before != after {
        if let Some(old) = before {
            index.delete(txn, &old)?;
        }
    }
    if let Some(new) = after {
        index.put(txn, &new, key)?;
    }
    Ok(())
}

/// Rebuild inside the opening transaction for a new database or schema 3/4.
/// Copy at most 256 edge keys per batch; the transaction still spans the whole
/// index. The caller commits it together with the schema version.
pub(crate) fn rebuild(txn: &mut RwTxn<'_>, meta: Db, index: Db) -> Result<(), LmdbError> {
    index.clear(txn)?;
    for prefix in [
        b"trst_graph/v1/3/".as_slice(),
        b"trst_graph/v1/4/".as_slice(),
    ] {
        let mut cursor: Option<Vec<u8>> = None;
        loop {
            let bounds = (
                cursor
                    .as_deref()
                    .map_or(Bound::Included(prefix), Bound::Excluded),
                Bound::<&[u8]>::Unbounded,
            );
            let rows = meta
                .range(txn, &bounds)?
                .take_while(|r| r.as_ref().map_or(true, |(key, _)| key.starts_with(prefix)))
                .take(256)
                .map(|row| {
                    let (key, bytes) = row?;
                    Ok((key.to_vec(), edge_key(key, bytes)?.expect("edge namespace")))
                })
                .collect::<Result<Vec<_>, LmdbError>>()?;
            if rows.is_empty() {
                break;
            }
            cursor = rows.last().map(|(key, _)| key.clone());
            for (key, position) in rows {
                if index.get(txn, &position)?.is_some() {
                    return Err(LmdbError::Heed(
                        "duplicate graph edge position during migration".into(),
                    ));
                }
                index.put(txn, &position, &key)?;
            }
        }
    }
    Ok(())
}

/// Return one past the greatest surviving position, or zero for no edges.
/// Interior gaps remain unused; removing the highest edge allows its position
/// to be reused. Check the selected index entry against its primary row.
pub(crate) fn next_position(
    txn: &RwTxn<'_>,
    meta: Db,
    index: Db,
    parent: &burst_types::TxHash,
    downstream: bool,
) -> Result<u64, burst_store::StoreError> {
    let mut lower = vec![if downstream { 4 } else { 3 }];
    lower.extend_from_slice(parent.as_bytes());
    let mut upper = lower.clone();
    upper.extend_from_slice(&u64::MAX.to_be_bytes());
    let bounds = (
        Bound::Included(lower.as_slice()),
        Bound::Included(upper.as_slice()),
    );
    let row = index
        .rev_range(txn, &bounds)
        .map_err(LmdbError::from)?
        .next()
        .transpose()
        .map_err(LmdbError::from)?;
    let Some((position, primary)) = row else {
        return Ok(0);
    };
    let bytes = meta
        .get(txn, primary)
        .map_err(LmdbError::from)?
        .ok_or_else(|| LmdbError::Heed("orphan graph position index".into()))?;
    if edge_key(primary, bytes)?.as_deref() != Some(position) {
        return Err(LmdbError::Heed("invalid graph position index".into()).into());
    }
    let position = u64::from_be_bytes(
        position[33..]
            .try_into()
            .map_err(|_| LmdbError::Heed("invalid graph position length".into()))?,
    );
    position
        .checked_add(1)
        .ok_or_else(|| LmdbError::Heed("graph edge ordering exhausted".into()).into())
}

#[cfg(test)]
mod tests;
