use super::*;
use bincode::Options;

const MAX_BYTES: usize = 64 << 20;
type Scopes<'a> = HashMap<&'a WalletAddress, HashMap<&'a DelegationScope, &'a WalletAddress>>;

fn decode(bytes: &[u8]) -> Result<DelegationSnapshot, String> {
    if bytes.len() > MAX_BYTES {
        return Err("delegation undo exceeds size limit".into());
    }
    let snapshot: DelegationSnapshot = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_BYTES as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(|error| error.to_string())?;
    if snapshot.delegations.iter().any(|(from, to)| from == to)
        || snapshot
            .scoped_delegations
            .iter()
            .any(|((from, scope), to)| from == to || *scope == DelegationScope::Global)
    {
        return Err("invalid delegation undo graph".into());
    }
    Ok(snapshot)
}

impl DelegationSnapshot {
    fn owners(&self) -> impl Iterator<Item = &WalletAddress> {
        let Self {
            delegations,
            scoped_delegations,
            max_depth: _,
        } = self;
        delegations
            .keys()
            .chain(scoped_delegations.keys().map(|(owner, _)| owner))
    }

    fn scopes(&self) -> Scopes<'_> {
        let mut scopes = Scopes::new();
        for ((owner, scope), delegate) in &self.scoped_delegations {
            scopes.entry(owner).or_default().insert(scope, delegate);
        }
        scopes
    }

    /// Reconcile an owner's global/scoped changes together. Cycles and depth
    /// exhaustion keep their ordinary resolution behavior (no delegated vote).
    /// The caller still has to authorize the rollback and close dependencies.
    pub fn rewind(before: &[u8], after: &[u8], current: &[u8]) -> Result<Vec<u8>, String> {
        let before = decode(before)?;
        let after = decode(after)?;
        let mut current = decode(current)?;
        if before.max_depth != after.max_depth {
            if current != after {
                return Err("delegation rule undo requires its complete dependent state".into());
            }
            current = before;
        } else {
            let prior = before.scopes();
            let next = after.scopes();
            let live = current.scopes();
            let owners: HashSet<_> = before.owners().chain(after.owners()).cloned().collect();
            let changed: HashSet<_> = owners
                .into_iter()
                .filter(|owner| {
                    before.delegations.get(owner) != after.delegations.get(owner)
                        || prior.get(owner) != next.get(owner)
                })
                .collect();
            for owner in &changed {
                if current.delegations.get(owner) != after.delegations.get(owner)
                    || live.get(owner) != next.get(owner)
                {
                    return Err("delegation undo conflicts with the owner's later progress".into());
                }
            }
            for owner in &changed {
                match before.delegations.get(owner) {
                    Some(delegate) => {
                        current.delegations.insert(owner.clone(), delegate.clone());
                    }
                    None => {
                        current.delegations.remove(owner);
                    }
                }
            }
            current
                .scoped_delegations
                .retain(|(owner, _), _| !changed.contains(owner));
            current.scoped_delegations.extend(
                before
                    .scoped_delegations
                    .into_iter()
                    .filter(|((owner, _), _)| changed.contains(owner)),
            );
        }
        let bytes = bincode::serialize(&current).map_err(|error| error.to_string())?;
        if bytes.len() > MAX_BYTES {
            return Err("restored delegation undo exceeds size limit".into());
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests;
