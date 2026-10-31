//! Convert positional holding journals to stable holding keys using the portfolio
//! state immediately after each block. The caller must supply that historical
//! state, order the journals and commit converted records atomically.
use super::*;
use burst_trst::{engine::{holding_rows, WalletRecord}, WalletPortfolio};

pub struct ConvertedJournal {
    pub journal: BlockUndo,
    /// Only affected holders. Apply to the context provider after the whole
    /// conversion succeeds; None means the portfolio was absent before it.
    pub before_portfolios: BTreeMap<WalletAddress, Option<WalletPortfolio>>,
}

/// Convert one positional journal using caller-verified portfolios from just
/// after that block.
/// Nonholding changes are retained byte-for-byte. Context bytes are a separate
/// shared bound across affected wallets; the journal retains existing undo limits.
/// The caller must also bound total work and retained journals across the migration.
pub fn convert<'a>(
    journal: BlockUndo,
    mut after_portfolio: impl FnMut(&WalletAddress) -> Result<Option<&'a WalletPortfolio>, StoreError>,
    max_context_bytes: usize,
) -> Result<ConvertedJournal, StoreError> {
    if max_context_bytes > MAX_BYTES { return Err(invalid("migration context exceeds hard limit")); }
    if bincode::serialized_size(&journal).map_err(serialization)? > MAX_BYTES as u64 {
        return Err(invalid("oversized migration journal"));
    }
    let encoded = bincode::serialize(&journal).map_err(serialization)?;
    let mut journal = BlockUndo::decode(&journal.block, &encoded)?;
    drop(encoded);
    let mut grouped = BTreeMap::<WalletAddress, Vec<(String, Option<Vec<u8>>, Option<Vec<u8>>)>>::new();
    let mut retained = Vec::new();
    for change in journal.changes {
        if change.table == UndoTable::Meta {
            if change.key == b"trst_engine_wallets" || change.key.starts_with(holding_rows::PREFIX.as_bytes()) {
                return Err(invalid("migration requires positional-only holding journal images"));
            }
            if change.key.starts_with(WalletRecord::STORAGE_PREFIX.as_bytes()) {
                let key = std::str::from_utf8(&change.key).map_err(|_| invalid("invalid positional journal key"))?;
                let mut holder = None;
                for bytes in change.before.iter().chain(change.after.iter()) {
                    match WalletRecord::from_storage_entry(key, bytes).map_err(StoreError::Corruption)? {
                        WalletRecord::Portfolio { holder: found, .. } | WalletRecord::Holding { holder: found, .. } => {
                            if holder.as_ref().is_some_and(|old| old != &found) {
                                return Err(invalid("positional journal changes holder"));
                            }
                            holder = Some(found);
                        }
                        // Origin/invalidation metadata keeps its encoding, but
                        // still must be bound to a valid canonical key/version.
                        _ => (),
                    }
                }
                if let Some(holder) = holder {
                    grouped.entry(holder).or_default().push((key.to_owned(), change.before, change.after));
                    continue;
                }
            }
        }
        retained.push(change);
    }
    let mut journal_bytes = bincode::serialized_size(&(journal.block, &journal.account, journal.height, &retained))
        .map_err(serialization)?;
    let mut before_portfolios = BTreeMap::new();
    let mut remaining = max_context_bytes;
    for (holder, changes) in grouped {
        let after = after_portfolio(&holder)?;
        let before = holding_rows::positional::rewind_portfolio(&holder, after, changes, remaining)
            .map_err(StoreError::Corruption)?;
        // Charge returned owned contexts while each temporary encoded context
        // is released before the next wallet. This is not an allocator/RSS claim.
        let bytes = bincode::serialized_size(&(&holder, &before)).map_err(serialization)?;
        remaining = remaining.checked_sub(usize::try_from(bytes).map_err(|_| invalid("context size overflow"))?)
            .ok_or_else(|| invalid("aggregate migration contexts exceed budget"))?;
        holding_rows::visit_delta(&holder, before.as_ref(), after, StoreError::Corruption, |old, new| {
            let old = old.map(|row| row.storage_entry()).transpose().map_err(StoreError::Corruption)?;
            let new = new.map(|row| row.storage_entry()).transpose().map_err(StoreError::Corruption)?;
            let key = new.as_ref().or(old.as_ref()).ok_or_else(|| invalid("empty stable migration delta"))?.0.clone();
            if old.as_ref().is_some_and(|row| row.0 != key) { return Err(invalid("stable migration changed key")); }
            let change = UndoChange { table: UndoTable::Meta, key: key.into_bytes(),
                before: old.map(|row| row.1), after: new.map(|row| row.1) };
            journal_bytes = journal_bytes.checked_add(bincode::serialized_size(&change).map_err(serialization)?)
                .filter(|bytes| *bytes <= MAX_BYTES as u64)
                .ok_or_else(|| invalid("converted journal exceeds byte limit"))?;
            retained.push(change);
            if retained.len() > MAX_CHANGES { return Err(invalid("converted journal exceeds change limit")); }
            Ok(())
        })?;
        before_portfolios.insert(holder, before);
    }
    retained.sort_by(|a, b| (a.table, &a.key).cmp(&(b.table, &b.key)));
    journal.changes = retained;
    if bincode::serialized_size(&journal).map_err(serialization)? > MAX_BYTES as u64 {
        return Err(invalid("converted journal exceeds byte limit"));
    }
    let encoded = bincode::serialize(&journal).map_err(serialization)?;
    let journal = BlockUndo::decode(&journal.block, &encoded)?;
    Ok(ConvertedJournal { journal, before_portfolios })
}

fn serialization(error: impl ToString) -> StoreError { StoreError::Serialization(error.to_string()) }

#[cfg(test)]
mod tests;
