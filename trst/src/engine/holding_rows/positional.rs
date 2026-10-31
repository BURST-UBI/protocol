//! Migration support for one historical portfolio cut, not rollback authority.
use super::*;
use crate::engine::{WalletRecord, WalletRecordReader};
use std::collections::{BTreeMap, HashSet};

/// Reconstruct the before-portfolio from a complete after-portfolio and partial
/// positional journal images. Every after-image must match; missing neighboring
/// rows are taken only from the supplied cut. Historical expiry caches are checked
/// structurally, not reclassified using today's governed period. The caller must
/// establish the cut's identity and charge a shared migration budget across wallets.
/// The byte limit covers retained encoded keys and values, not total heap use.
/// The supplied portfolio is borrowed; errors do not mutate it.
pub fn rewind_portfolio(
    holder: &WalletAddress,
    after: Option<&WalletPortfolio>,
    changes: impl IntoIterator<Item = (String, Option<Vec<u8>>, Option<Vec<u8>>)>,
    max_context_bytes: usize,
) -> Result<Option<WalletPortfolio>, String> {
    let mut rows = BTreeMap::new();
    let mut retained = 0usize;
    let mut insert = |record: WalletRecord| -> Result<(), String> {
        let (key, value) = record.storage_entry()?;
        retained = retained.checked_add(key.len()).and_then(|n| n.checked_add(value.len()))
            .filter(|n| *n <= max_context_bytes).ok_or("positional migration context exceeds budget")?;
        rows.insert(key, value);
        Ok(())
    };
    if let Some(portfolio) = after {
        validate(holder, portfolio)?;
        insert(WalletRecord::Portfolio { holder: holder.clone(),
            cached_transferable: portfolio.cached_transferable, earliest_expiry: portfolio.earliest_expiry })?;
        for (position, token) in portfolio.tokens.iter().enumerate() {
            insert(WalletRecord::Holding { holder: holder.clone(), position: position as u64,
                token: Box::new(token.clone()) })?;
        }
    }
    let mut seen = HashSet::new();
    for (key, before, after) in changes {
        if !seen.insert(key.clone()) || before == after {
            return Err("duplicate or unchanged positional migration row".into());
        }
        for bytes in before.iter().chain(after.iter()) {
            let owner = match WalletRecord::from_storage_entry(&key, bytes)? {
                WalletRecord::Portfolio { holder, .. } | WalletRecord::Holding { holder, .. } => holder,
                _ => return Err("non-portfolio positional migration row".into()),
            };
            if owner != *holder { return Err("positional migration owner mismatch".into()); }
        }
        if rows.get(&key) != after.as_ref() {
            return Err("positional migration after-image differs from supplied cut".into());
        }
        if let Some(previous) = rows.remove(&key) { retained -= key.len() + previous.len(); }
        if let Some(before) = before {
            retained = retained.checked_add(key.len()).and_then(|n| n.checked_add(before.len()))
                .filter(|n| *n <= max_context_bytes).ok_or("positional migration context exceeds budget")?;
            rows.insert(key, before);
        }
    }
    // The historical period is not supplied by the row journal. Structural
    // validation preserves its exact cache; live publication still validates its
    // separately authenticated governed period using the normal reader.
    let mut reader = WalletRecordReader::new(0);
    for (key, bytes) in rows { reader.push(WalletRecord::from_storage_entry(&key, &bytes)?)?; }
    let mut engine = reader.finish_checked(false)?;
    Ok(engine.wallets.remove(holder).map(|portfolio| portfolio.into_inner()))
}
