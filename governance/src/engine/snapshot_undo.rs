//! Reconcile local undo images. This does not authorize a fork or an activation.
use super::*;
use bincode::Options;
use std::collections::HashSet;

pub(super) const MAX_BYTES: usize = 64 << 20;
type BallotScopes<'a> = std::collections::HashMap<
    TxHash,
    std::collections::HashMap<(&'a WalletAddress, &'a burst_types::BlockHash), GovernanceVote>,
>;

pub(super) fn decode(bytes: &[u8]) -> Result<GovernanceEngine, String> {
    if bytes.len() > MAX_BYTES {
        return Err("governance undo exceeds size limit".into());
    }
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_BYTES as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(|error| error.to_string())
}

fn restore_entry<V: Clone>(map: &mut OrdMap<TxHash, V>, before: &OrdMap<TxHash, V>, id: TxHash) {
    match before.get(&id) {
        Some(value) => {
            map.insert(id, value.clone());
        }
        None => {
            map.remove(&id);
        }
    }
}

impl GovernanceEngine {
    fn proposal_ids(&self) -> impl Iterator<Item = TxHash> + '_ {
        // Exhaustive destructuring makes a new persistent field require review.
        let Self {
            proposals,
            exploration_votes,
            promotion_votes,
            endorsement_counts,
            pending_changes: _,
            pending_amendments: _,
            pending_evictions: _,
            timings,
            key_ballots,
        } = self;
        proposals
            .keys()
            .chain(exploration_votes.keys())
            .chain(promotion_votes.keys())
            .chain(endorsement_counts.keys())
            .chain(timings.keys())
            .copied()
            .chain(key_ballots.keys().map(|key| key.0))
    }

    fn same_proposal(&self, other: &Self, id: TxHash) -> bool {
        self.proposals.get(&id) == other.proposals.get(&id)
            && self.exploration_votes.get(&id) == other.exploration_votes.get(&id)
            && self.promotion_votes.get(&id) == other.promotion_votes.get(&id)
            && self.endorsement_counts.get(&id) == other.endorsement_counts.get(&id)
            && self.timings.get(&id) == other.timings.get(&id)
    }

    fn ballot_scopes(&self) -> BallotScopes<'_> {
        let mut scopes = BallotScopes::new();
        for ((proposal, voter, key), vote) in &self.key_ballots {
            scopes
                .entry(*proposal)
                .or_default()
                .insert((voter, key), *vote);
        }
        scopes
    }

    /// Undo only changed proposal scopes. Votes, timing and secondary ballots
    /// belong to the proposal they affect. Later changes in that scope require
    /// the caller to roll back their dependencies first.
    /// A change to any pending-effect queue instead requires the complete current
    /// engine to equal `after`. This reconciles stored state, not economic effects
    /// that a node may already have applied outside the engine.
    pub fn rewind_snapshot(before: &[u8], after: &[u8], current: &[u8]) -> Result<Vec<u8>, String> {
        let before = decode(before)?;
        let after = decode(after)?;
        let mut current = decode(current)?;
        if before.pending_changes != after.pending_changes
            || before.pending_amendments != after.pending_amendments
            || before.pending_evictions != after.pending_evictions
        {
            if current != after {
                return Err("governance rule undo requires its complete dependent state".into());
            }
            current = before;
        } else {
            let before_ballots = before.ballot_scopes();
            let after_ballots = after.ballot_scopes();
            let current_ballots = current.ballot_scopes();
            let ids: HashSet<_> = before.proposal_ids().chain(after.proposal_ids()).collect();
            let changed: HashSet<_> = ids
                .into_iter()
                .filter(|id| {
                    !before.same_proposal(&after, *id)
                        || before_ballots.get(id) != after_ballots.get(id)
                })
                .collect();
            for id in &changed {
                if !current.same_proposal(&after, *id)
                    || current_ballots.get(id) != after_ballots.get(id)
                {
                    return Err(
                        "governance undo conflicts with the proposal's later progress".into(),
                    );
                }
            }
            for id in &changed {
                restore_entry(&mut current.proposals, &before.proposals, *id);
                restore_entry(
                    &mut current.exploration_votes,
                    &before.exploration_votes,
                    *id,
                );
                restore_entry(&mut current.promotion_votes, &before.promotion_votes, *id);
                restore_entry(
                    &mut current.endorsement_counts,
                    &before.endorsement_counts,
                    *id,
                );
                restore_entry(&mut current.timings, &before.timings, *id);
            }
            let removed: Vec<_> = current
                .key_ballots
                .keys()
                .filter(|key| changed.contains(&key.0))
                .cloned()
                .collect();
            for key in removed {
                current.key_ballots.remove(&key);
            }
            current.key_ballots.extend(
                before
                    .key_ballots
                    .into_iter()
                    .filter(|(key, _)| changed.contains(&key.0)),
            );
        }
        let bytes = bincode::serialize(&current).map_err(|error| error.to_string())?;
        if bytes.len() > MAX_BYTES {
            return Err("restored governance undo exceeds size limit".into());
        }
        Ok(bytes)
    }
}
