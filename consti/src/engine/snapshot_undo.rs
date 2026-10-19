use super::*;
use bincode::Options;
use std::collections::HashSet;

const MAX_BYTES: usize = 64 << 20;

fn decode(bytes: &[u8]) -> Result<ConstiEngine, String> {
    if bytes.len() > MAX_BYTES {
        return Err("constitution undo exceeds size limit".into());
    }
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_BYTES as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(|error| error.to_string())
}

fn restore<V: Clone>(current: &mut HashMap<TxHash, V>, before: &HashMap<TxHash, V>, id: TxHash) {
    match before.get(&id) {
        Some(value) => {
            current.insert(id, value.clone());
        }
        None => {
            current.remove(&id);
        }
    }
}

impl ConstiEngine {
    fn amendment_ids(&self) -> impl Iterator<Item = TxHash> + '_ {
        let Self {
            amendments,
            votes,
            document: _,
            conflicts,
        } = self;
        amendments
            .keys()
            .chain(votes.keys())
            .chain(conflicts.keys())
            .copied()
    }

    fn same_amendment(&self, other: &Self, id: TxHash) -> bool {
        self.amendments.get(&id) == other.amendments.get(&id)
            && self.votes.get(&id) == other.votes.get(&id)
            && self.conflicts.get(&id) == other.conflicts.get(&id)
    }

    /// Restore independent amendment scopes without erasing later votes or
    /// conflicts. A document change requires its entire dependent after-state.
    /// This is local reconciliation, not authority to reverse an activation.
    pub fn rewind_snapshot(before: &[u8], after: &[u8], current: &[u8]) -> Result<Vec<u8>, String> {
        let before = decode(before)?;
        let after = decode(after)?;
        let mut current = decode(current)?;
        if before.document != after.document {
            if current != after {
                return Err(
                    "constitution document undo requires its complete dependent state".into(),
                );
            }
            current = before;
        } else {
            let ids: HashSet<_> = before
                .amendment_ids()
                .chain(after.amendment_ids())
                .collect();
            let changed: Vec<_> = ids
                .into_iter()
                .filter(|id| !before.same_amendment(&after, *id))
                .collect();
            for id in &changed {
                if !current.same_amendment(&after, *id) {
                    return Err(
                        "constitution undo conflicts with the amendment's later progress".into(),
                    );
                }
            }
            for id in changed {
                restore(&mut current.amendments, &before.amendments, id);
                restore(&mut current.votes, &before.votes, id);
                restore(&mut current.conflicts, &before.conflicts, id);
            }
        }
        let bytes = bincode::serialize(&current).map_err(|error| error.to_string())?;
        if bytes.len() > MAX_BYTES {
            return Err("restored constitution undo exceeds size limit".into());
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests;
