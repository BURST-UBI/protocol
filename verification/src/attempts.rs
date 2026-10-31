use crate::{ChallengeOutcomeEvent, VerificationError, VerifierOutcome};
use burst_types::{BlockHash, WalletAddress};
use imbl::OrdMap;
use serde::{Deserialize, Serialize};

/// Binds votes and challenges to attempt hashes for economic settlement.
/// These maps record caller-supplied contexts; they do not verify signatures.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct AttemptContexts {
    #[serde(serialize_with = "ordered_votes")]
    pub(crate) votes:
        crate::tracked_map::TrackedMap<WalletAddress, OrdMap<WalletAddress, BlockHash>>,
    #[serde(serialize_with = "ordered_challenges")]
    pub(crate) challenges: crate::tracked_map::TrackedMap<WalletAddress, BlockHash>,
}

fn ordered_votes<S: serde::Serializer>(
    votes: &crate::tracked_map::TrackedMap<WalletAddress, OrdMap<WalletAddress, BlockHash>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    struct Voters<'a>(&'a OrdMap<WalletAddress, BlockHash>);
    impl Serialize for Voters<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.collect_map(
                self.0
                    .iter()
                    .map(|(wallet, context)| (wallet.as_str(), context)),
            )
        }
    }
    serializer.collect_map(
        votes
            .iter()
            .map(|(target, voters)| (target.as_str(), Voters(voters))),
    )
}

fn ordered_challenges<S: serde::Serializer>(
    challenges: &crate::tracked_map::TrackedMap<WalletAddress, BlockHash>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_map(
        challenges
            .iter()
            .map(|(wallet, hash)| (wallet.as_str(), hash)),
    )
}

fn rewind_map<V: Clone + PartialEq>(
    before: &crate::tracked_map::TrackedMap<WalletAddress, V>,
    after: &crate::tracked_map::TrackedMap<WalletAddress, V>,
    current: &mut crate::tracked_map::TrackedMap<WalletAddress, V>,
) -> Result<(), String> {
    for key in before
        .keys()
        .chain(after.keys())
        .collect::<std::collections::BTreeSet<_>>()
    {
        if before.get(key) == after.get(key) {
            continue;
        }
        if current.get(key) != after.get(key) {
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

impl AttemptContexts {
    pub(crate) fn targets(&self) -> impl Iterator<Item = &WalletAddress> {
        self.votes.keys().chain(self.challenges.keys())
    }

    pub(crate) fn same_target(&self, other: &Self, target: &WalletAddress) -> bool {
        self.votes.get(target) == other.votes.get(target)
            && self.challenges.get(target) == other.challenges.get(target)
    }

    /// Restore changed entries only if they still match `after`. Earlier entries
    /// may already be restored on error; callers must use a disposable snapshot.
    pub(crate) fn rewind(&mut self, before: &Self, after: &Self) -> Result<(), String> {
        rewind_map(&before.votes, &after.votes, &mut self.votes)?;
        rewind_map(&before.challenges, &after.challenges, &mut self.challenges)
    }

    pub(crate) fn challenge_context(&self, target: &WalletAddress) -> Option<BlockHash> {
        self.challenges.get(target).copied()
    }

    pub(crate) fn vote_context(
        &self,
        target: &WalletAddress,
        voter: &WalletAddress,
    ) -> Option<BlockHash> {
        self.votes.get(target)?.get(voter).copied()
    }

    pub(crate) fn completed_panel(
        &self,
        target: &WalletAddress,
        context: BlockHash,
        selected: &[WalletAddress],
    ) -> bool {
        self.votes.get(target).is_some_and(|votes| {
            votes.values().filter(|value| **value == context).count() == selected.len()
                && selected
                    .iter()
                    .all(|voter| votes.get(voter) == Some(&context))
        })
    }

    pub(crate) fn bind_vote(
        &mut self,
        target: &WalletAddress,
        voter: &WalletAddress,
        context: BlockHash,
    ) -> Result<(), VerificationError> {
        if context.is_zero() {
            return Err(VerificationError::Other(
                "vote attempt context is missing".into(),
            ));
        }
        let voters = self.votes.entry(target.clone()).or_default();
        if voters
            .get(voter)
            .is_some_and(|previous| *previous != context)
        {
            return Err(VerificationError::Other(
                "verifier already belongs to another round of this attempt".into(),
            ));
        }
        voters.insert(voter.clone(), context);
        Ok(())
    }

    pub(crate) fn clear(&mut self, target: &WalletAddress) {
        self.votes.remove(target);
        self.challenges.remove(target);
    }

    pub(crate) fn bind_challenge(&mut self, target: &WalletAddress, context: BlockHash) {
        self.challenges.insert(target.clone(), context);
    }

    pub(crate) fn annotate_votes(&self, target: &WalletAddress, outcomes: &mut [VerifierOutcome]) {
        for outcome in outcomes {
            outcome.selection_context = self
                .votes
                .get(target)
                .and_then(|votes| votes.get(&outcome.address))
                .copied();
        }
    }

    pub(crate) fn annotate_challenge(
        &self,
        target: &WalletAddress,
        outcome: &mut ChallengeOutcomeEvent,
    ) {
        outcome.challenge_context = self.challenges.get(target).copied();
        self.annotate_votes(target, &mut outcome.verifier_outcomes);
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn ordered_context_stream_matches_legacy_sorted_maps_including_empty_targets() {
        use std::collections::BTreeMap;
        for count in [0, 1, 128] {
            let mut contexts = super::AttemptContexts::default();
            for index in (0..count).rev() {
                let target = burst_types::WalletAddress::new(format!("brst_context_{index}"));
                let voters = (0..index % 8)
                    .rev()
                    .map(|voter| {
                        (
                            burst_types::WalletAddress::new(format!("brst_voter_{voter}")),
                            burst_types::BlockHash::new([(voter + 1) as u8; 32]),
                        )
                    })
                    .collect();
                contexts.votes.insert(target.clone(), voters);
                if index % 2 == 0 {
                    contexts
                        .challenges
                        .insert(target, burst_types::BlockHash::new([9; 32]));
                }
            }
            let votes: BTreeMap<_, _> = contexts
                .votes
                .iter()
                .map(|(target, voters)| {
                    (
                        target.as_str(),
                        voters
                            .iter()
                            .map(|(wallet, context)| (wallet.as_str(), context))
                            .collect::<BTreeMap<_, _>>(),
                    )
                })
                .collect();
            let challenges: BTreeMap<_, _> = contexts
                .challenges
                .iter()
                .map(|(wallet, hash)| (wallet.as_str(), hash))
                .collect();
            let legacy = bincode::serialize(&(votes, challenges)).unwrap();
            let actual = bincode::serialize(&contexts).unwrap();
            assert_eq!(actual, legacy);
            let restored: super::AttemptContexts = bincode::deserialize(&actual).unwrap();
            assert_eq!(bincode::serialize(&restored).unwrap(), legacy);
            assert_eq!(restored.votes.len(), count as usize);
        }
    }
    use super::*;
    use crate::{compute_challenge_outcome, ChallengeResult};

    #[test]
    fn attempt_snapshot_bytes_ignore_map_insertion_and_restore_order() {
        let mut first = AttemptContexts::default();
        let mut second = AttemptContexts::default();
        for (state, reverse) in [(&mut first, false), (&mut second, true)] {
            let mut targets: Vec<_> = (0..8).collect();
            if reverse {
                targets.reverse();
            }
            for target in targets {
                let target = WalletAddress::new(format!("brst_target_{target}"));
                let mut voters: Vec<_> = (0..8).collect();
                if reverse {
                    voters.reverse();
                }
                for voter in voters {
                    state
                        .bind_vote(
                            &target,
                            &WalletAddress::new(format!("brst_voter_{voter}")),
                            BlockHash::new([voter + 1; 32]),
                        )
                        .unwrap();
                }
                state.bind_challenge(&target, BlockHash::new([9; 32]));
            }
        }
        let bytes = bincode::serialize(&first).unwrap();
        // The former HashMap-backed two-field representation must decode these
        // exact bytes; ordered wire maps remain independent of runtime storage.
        type Legacy = (
            std::collections::HashMap<
                WalletAddress,
                std::collections::HashMap<WalletAddress, BlockHash>,
            >,
            std::collections::HashMap<WalletAddress, BlockHash>,
        );
        let legacy: Legacy = bincode::deserialize(&bytes).unwrap();
        let ordered_votes: std::collections::BTreeMap<_, _> = legacy
            .0
            .iter()
            .map(|(target, voters)| {
                (
                    target.as_str(),
                    voters
                        .iter()
                        .map(|(voter, hash)| (voter.as_str(), hash))
                        .collect::<std::collections::BTreeMap<_, _>>(),
                )
            })
            .collect();
        let ordered_challenges: std::collections::BTreeMap<_, _> = legacy
            .1
            .iter()
            .map(|(target, hash)| (target.as_str(), hash))
            .collect();
        assert_eq!(
            bytes,
            bincode::serialize(&(ordered_votes, ordered_challenges)).unwrap()
        );
        assert_eq!(bytes, bincode::serialize(&second).unwrap());
        for _ in 0..8 {
            let restored: AttemptContexts = bincode::deserialize(&bytes).unwrap();
            assert_eq!(bytes, bincode::serialize(&restored).unwrap());
        }
    }

    #[test]
    fn contexts_survive_restart_and_cannot_be_rebound_within_an_attempt() {
        let target = WalletAddress::new("brst_target");
        let first = WalletAddress::new("brst_first");
        let second = WalletAddress::new("brst_second");
        let mut contexts = AttemptContexts::default();
        let a = BlockHash::new([1; 32]);
        let b = BlockHash::new([2; 32]);
        let challenge = BlockHash::new([3; 32]);
        assert!(contexts
            .bind_vote(&target, &first, BlockHash::ZERO)
            .is_err());
        contexts.bind_vote(&target, &first, a).unwrap();
        contexts.bind_vote(&target, &first, a).unwrap();
        assert!(contexts.bind_vote(&target, &first, b).is_err());
        contexts.bind_vote(&target, &second, b).unwrap();
        contexts.bind_challenge(&target, challenge);
        let bytes = bincode::serialize(&contexts).unwrap();
        let mut contexts: AttemptContexts = bincode::deserialize(&bytes).unwrap();
        let mut outcome = compute_challenge_outcome(
            &target,
            &first,
            ChallengeResult::Expired,
            50,
            &[(first.clone(), 20, true), (second.clone(), 20, true)],
        )
        .unwrap();
        contexts.annotate_challenge(&target, &mut outcome);
        assert_eq!(outcome.challenge_context, Some(challenge));
        assert_eq!(outcome.verifier_outcomes[0].selection_context, Some(a));
        assert_eq!(outcome.verifier_outcomes[1].selection_context, Some(b));
        contexts.clear(&target);
        contexts.bind_vote(&target, &first, b).unwrap();
        // A new attempt never changes a previously emitted settlement.
        assert_eq!(outcome.verifier_outcomes[0].selection_context, Some(a));
        contexts.annotate_challenge(&target, &mut outcome);
        assert_eq!(outcome.challenge_context, None);
        assert_eq!(outcome.verifier_outcomes[0].selection_context, Some(b));
        assert_eq!(outcome.verifier_outcomes[1].selection_context, None);
    }
}
