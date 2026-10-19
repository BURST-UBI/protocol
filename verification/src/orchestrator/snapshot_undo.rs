//! Rewind journal-owned entries without replacing unrelated local progress.
//! This supplies no finality authority; the caller must check the dependency cut.

use super::*;
use std::hash::Hash;

pub(crate) fn equivalent<T: Serialize>(left: T, right: T) -> Result<bool, String> {
    Ok(bincode::serialize(&left).map_err(|e| e.to_string())?
        == bincode::serialize(&right).map_err(|e| e.to_string())?)
}

pub(crate) fn rewind_map<K: Clone + Eq + Hash, V: Clone + Serialize>(
    before: &HashMap<K, V>,
    after: &HashMap<K, V>,
    current: &mut HashMap<K, V>,
) -> Result<(), String> {
    rewind_map_by(before, after, current, |left, right| {
        equivalent(left, right)
    })
}

pub(crate) fn rewind_map_by<K: Clone + Eq + Hash, V: Clone>(
    before: &HashMap<K, V>,
    after: &HashMap<K, V>,
    current: &mut HashMap<K, V>,
    equal: impl Fn(Option<&V>, Option<&V>) -> Result<bool, String>,
) -> Result<(), String> {
    for key in before
        .keys()
        .chain(after.keys())
        .collect::<std::collections::HashSet<_>>()
    {
        if equal(before.get(key), after.get(key))? {
            continue;
        }
        if !equal(current.get(key), after.get(key))? {
            return Err("verification undo conflicts with a changed entry".into());
        }
        match before.get(key) {
            Some(value) => {
                current.insert(key.clone(), value.clone());
            }
            None => {
                current.remove(key);
            }
        }
    }
    Ok(())
}

pub(crate) fn rewind_value<T: Clone + Serialize>(
    before: &T,
    after: &T,
    current: &mut T,
) -> Result<(), String> {
    if !equivalent(before, after)? {
        if !equivalent(&*current, after)? {
            return Err("verification undo conflicts with changed rules".into());
        }
        *current = before.clone();
    }
    Ok(())
}

#[derive(Serialize)]
struct Votes<'a>(#[serde(serialize_with = "crate::state::ordered_votes")] &'a [VerifierVote]);

impl OrchestratorSnapshot {
    fn targets(&self) -> impl Iterator<Item = &WalletAddress> {
        let Self {
            states,
            active_challenges,
            prior_votes,
            selections,
            attempts,
            active_appeals,
            penalized_verifiers: _,
            neither_tracker: _,
        } = self;
        states
            .keys()
            .chain(active_challenges.keys())
            .chain(prior_votes.keys())
            .chain(selections.keys())
            .chain(active_appeals.keys())
            .chain(attempts.targets())
    }

    fn same_target(&self, other: &Self, target: &WalletAddress) -> Result<bool, String> {
        Ok(
            equivalent(self.states.get(target), other.states.get(target))?
                && equivalent(
                    self.active_challenges.get(target),
                    other.active_challenges.get(target),
                )?
                && equivalent(
                    self.prior_votes.get(target).map(|v| Votes(v)),
                    other.prior_votes.get(target).map(|v| Votes(v)),
                )?
                && equivalent(self.selections.get(target), other.selections.get(target))?
                && equivalent(
                    self.active_appeals.get(target),
                    other.active_appeals.get(target),
                )?
                && self.attempts.same_target(&other.attempts, target),
        )
    }

    /// Restore disjoint wallet/attempt entries and verifier windows. Overlapping
    /// progress must be included in the caller's rollback closure or rejected.
    pub fn rewind(before: &[u8], after: &[u8], current: &[u8]) -> Result<Vec<u8>, String> {
        const MAX_BYTES: usize = 64 << 20;
        if [before, after, current]
            .iter()
            .any(|bytes| bytes.len() > MAX_BYTES)
        {
            return Err("verification undo exceeds size limit".into());
        }
        let before = Self::decode(before).map_err(|e| e.to_string())?;
        let after = Self::decode(after).map_err(|e| e.to_string())?;
        let mut current = Self::decode(current).map_err(|e| e.to_string())?;
        if !before.neither_tracker.same_rules(&after.neither_tracker)
            && current.encode().map_err(|e| e.to_string())?
                != after.encode().map_err(|e| e.to_string())?
        {
            return Err(
                "undo of governed verification rules requires their complete dependent state"
                    .into(),
            );
        }
        for target in before
            .targets()
            .chain(after.targets())
            .collect::<std::collections::HashSet<_>>()
        {
            if !before.same_target(&after, target)? && !current.same_target(&after, target)? {
                return Err("verification undo conflicts with the target's later progress".into());
            }
        }
        for verifier in before
            .penalized_verifiers
            .keys()
            .chain(after.penalized_verifiers.keys())
            .chain(current.penalized_verifiers.keys())
            .collect::<std::collections::HashSet<_>>()
        {
            let prior = before.penalized_verifiers.get(verifier);
            let next = after.penalized_verifiers.get(verifier);
            if (prior != next
                || !before
                    .neither_tracker
                    .same_history(&after.neither_tracker, verifier)?)
                && (current.penalized_verifiers.get(verifier) != next
                    || !current
                        .neither_tracker
                        .same_history(&after.neither_tracker, verifier)?)
            {
                return Err(
                    "verification undo conflicts with the verifier's later progress".into(),
                );
            }
        }
        rewind_map(&before.states, &after.states, &mut current.states)?;
        rewind_map(
            &before.active_challenges,
            &after.active_challenges,
            &mut current.active_challenges,
        )?;
        rewind_map(
            &before.penalized_verifiers,
            &after.penalized_verifiers,
            &mut current.penalized_verifiers,
        )?;
        rewind_map_by(
            &before.prior_votes,
            &after.prior_votes,
            &mut current.prior_votes,
            |left, right| {
                equivalent(
                    left.map(|votes| Votes(votes)),
                    right.map(|votes| Votes(votes)),
                )
            },
        )?;
        rewind_map(
            &before.selections,
            &after.selections,
            &mut current.selections,
        )?;
        rewind_map(
            &before.active_appeals,
            &after.active_appeals,
            &mut current.active_appeals,
        )?;
        current.attempts.rewind(&before.attempts, &after.attempts)?;
        current
            .neither_tracker
            .rewind(&before.neither_tracker, &after.neither_tracker)?;
        current.validate_appeals().map_err(|e| e.to_string())?;
        let bytes = current.encode().map_err(|e| e.to_string())?;
        if bytes.len() > MAX_BYTES {
            return Err("restored verification snapshot exceeds size limit".into());
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(seed: u8) -> WalletAddress {
        burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[seed; 32]).public)
    }

    fn endorse(state: &mut VerificationOrchestrator, target: u8, endorser: u8) {
        let params = ProtocolParams {
            endorsement_burn_amount: 1,
            endorsement_threshold: 3,
            ..ProtocolParams::default()
        };
        state
            .process_endorsement_at(
                &address(target),
                &address(endorser),
                1,
                &params,
                Timestamp::new(100),
            )
            .unwrap();
    }

    fn encoded(state: &VerificationOrchestrator) -> Vec<u8> {
        state.snapshot().encode().unwrap()
    }

    #[test]
    fn rewind_preserves_independent_endorsements_attempts_and_verifier_history() {
        let before = VerificationOrchestrator::new();
        let mut after = before.clone();
        endorse(&mut after, 10, 1);
        after
            .attempts
            .bind_vote(&address(10), &address(1), BlockHash::new([1; 32]))
            .unwrap();
        after
            .neither_tracker
            .record_vote(&address(1), Vote::Neither);
        let mut current = after.clone();
        endorse(&mut current, 11, 2);
        current
            .attempts
            .bind_vote(&address(11), &address(2), BlockHash::new([2; 32]))
            .unwrap();
        current
            .neither_tracker
            .record_vote(&address(2), Vote::Legitimate);
        let mut expected = before.clone();
        endorse(&mut expected, 11, 2);
        expected
            .attempts
            .bind_vote(&address(11), &address(2), BlockHash::new([2; 32]))
            .unwrap();
        expected
            .neither_tracker
            .record_vote(&address(2), Vote::Legitimate);
        assert_eq!(
            OrchestratorSnapshot::rewind(&encoded(&before), &encoded(&after), &encoded(&current))
                .unwrap(),
            encoded(&expected)
        );
        let removed_second =
            OrchestratorSnapshot::rewind(&encoded(&after), &encoded(&current), &encoded(&expected))
                .unwrap();
        assert_eq!(removed_second, encoded(&before));
    }

    #[test]
    fn rewind_rejects_overlapping_wallet_attempt_and_chronological_history() {
        let before = VerificationOrchestrator::new();
        let mut after = before.clone();
        endorse(&mut after, 10, 1);
        after
            .attempts
            .bind_vote(&address(10), &address(1), BlockHash::new([1; 32]))
            .unwrap();
        after
            .neither_tracker
            .record_vote(&address(1), Vote::Neither);
        after
            .neither_tracker
            .record_vote(&address(1), Vote::Legitimate);
        for mutation in 0..4 {
            let mut current = after.clone();
            match mutation {
                0 => endorse(&mut current, 10, 2),
                1 => current
                    .attempts
                    .bind_vote(&address(10), &address(2), BlockHash::new([1; 32]))
                    .unwrap(),
                2 => {
                    current.neither_tracker.reset(&address(1));
                    current
                        .neither_tracker
                        .record_vote(&address(1), Vote::Legitimate);
                    current
                        .neither_tracker
                        .record_vote(&address(1), Vote::Neither);
                }
                _ => {
                    current.prior_votes.insert(address(10), Vec::new());
                }
            }
            assert!(OrchestratorSnapshot::rewind(
                &encoded(&before),
                &encoded(&after),
                &encoded(&current)
            )
            .is_err());
        }
    }

    #[test]
    fn rewind_keeps_cooldowns_and_verifier_history_in_one_conflict_scope() {
        let before = VerificationOrchestrator::new();
        for cooldown_first in [false, true] {
            let mut after = before.clone();
            if cooldown_first {
                after.penalized_verifiers.insert(address(1), 1000);
            } else {
                after
                    .neither_tracker
                    .record_vote(&address(1), Vote::Neither);
            }
            let mut current = after.clone();
            if cooldown_first {
                current
                    .neither_tracker
                    .record_vote(&address(1), Vote::Legitimate);
            } else {
                current.penalized_verifiers.insert(address(1), 1000);
            }
            assert!(OrchestratorSnapshot::rewind(
                &encoded(&before),
                &encoded(&after),
                &encoded(&current)
            )
            .is_err());
        }
    }

    #[test]
    fn rewind_checks_formats_rules_and_restored_window_validity() {
        let before = VerificationOrchestrator::new();
        let mut after = before.clone();
        after.neither_tracker.configure(2500, 2);
        assert_eq!(
            OrchestratorSnapshot::rewind(&encoded(&before), &encoded(&after), &encoded(&after))
                .unwrap(),
            encoded(&before)
        );
        let mut later_target = after.clone();
        endorse(&mut later_target, 10, 1);
        assert!(OrchestratorSnapshot::rewind(
            &encoded(&before),
            &encoded(&after),
            &encoded(&later_target)
        )
        .is_err());
        let mut current = after.clone();
        current.neither_tracker.configure(3000, 2);
        assert!(OrchestratorSnapshot::rewind(
            &encoded(&before),
            &encoded(&after),
            &encoded(&current)
        )
        .is_err());
        let mut invalid = encoded(&before);
        invalid.push(0);
        for (a, b, c) in [
            (&invalid, encoded(&after), encoded(&after)),
            (&encoded(&before), invalid.clone(), encoded(&after)),
            (&encoded(&before), encoded(&after), invalid.clone()),
        ] {
            assert!(OrchestratorSnapshot::rewind(a, &b, &c).is_err());
        }
        let mut narrow = before.clone();
        narrow.neither_tracker.configure(5000, 1);
        current
            .neither_tracker
            .record_vote(&address(1), Vote::Neither);
        current
            .neither_tracker
            .record_vote(&address(1), Vote::Legitimate);
        // Reverting a rule change cannot leave a later independent window
        // longer than the restored rule permits.
        let mut wide = narrow.clone();
        wide.neither_tracker.configure(3000, 2);
        assert!(OrchestratorSnapshot::rewind(
            &encoded(&narrow),
            &encoded(&wide),
            &encoded(&current)
        )
        .is_err());
    }
}
