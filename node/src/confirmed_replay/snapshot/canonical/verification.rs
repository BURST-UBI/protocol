//! Canonical verification fields encoded individually without a whole-state Value tree.
use super::*;
use burst_verification::orchestrator::{ordered_prior_votes, SnapshotMap, SnapshotView};
use burst_verification::VerificationOrchestrator;

struct ReadMap<M>(M);
impl<M: SnapshotMap> MapSource for ReadMap<M> {
    type Key = M::Key;
    type Value = M::Value;
    fn len(&self) -> usize {
        self.0.len()
    }
    fn entries(&self) -> impl Iterator<Item = (&Self::Key, &Self::Value)> {
        self.0.entries()
    }
}

fn measure_map<M: MapSource>(
    map: &M,
    mut output: &mut dyn Write,
    depth: usize,
    poll: &dyn Fn() -> Result<(), ReplayError>,
    value: impl Fn(&M::Value, &mut dyn Write, usize) -> Result<(), ReplayError>,
) -> Result<(), ReplayError> {
    check_depth(depth)?;
    let mut output = BoundedWriter {
        output: &mut output,
        written: 0,
        limit: MAX_SNAPSHOT_BYTES as usize,
    };
    collection_header(0xa0, map.len(), &mut output)?;
    for (key, row) in map.entries() {
        poll()?;
        encode_at_depth(key, &mut output, depth + 1)?;
        value(row, &mut output, depth + 1)?;
    }
    Ok(())
}

fn equal_map<M: SnapshotMap>(
    actual: M,
    expected: M,
    encode_value: impl Fn(&M::Value) -> Result<Vec<u8>, ReplayError>,
) -> Result<bool, ReplayError> {
    if actual.len() != expected.len() {
        return Ok(false);
    }
    for (key, value) in actual.entries() {
        let Some(other) = expected.get(key) else {
            return Ok(false);
        };
        if encode_value(value)? != encode_value(other)? {
            return Ok(false);
        }
    }
    Ok(true)
}

struct PollingMap<'a, M> {
    map: &'a ReadMap<M>,
    poll: &'a dyn Fn() -> Result<(), ReplayError>,
    measure_only: bool,
}
impl<M: SnapshotMap> Field for PollingMap<'_, M> {
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        self.encode_field_at(output, None, 1)
    }
    fn encode_field_at(
        &self,
        output: &mut dyn Write,
        directory: Option<&std::path::Path>,
        depth: usize,
    ) -> Result<(), ReplayError> {
        (self.poll)()?;
        if self.measure_only {
            return measure_map(
                self.map,
                output,
                depth,
                self.poll,
                |value, mut output, depth| encode_at_depth(value, &mut output, depth),
            );
        }
        encode_map(
            self.map,
            output,
            directory,
            depth,
            |value, mut output, child_depth| {
                (self.poll)()?;
                encode_at_depth(value, &mut output, child_depth)
            },
        )
    }
}

struct PriorVotes<'a, M> {
    map: ReadMap<M>,
    poll: &'a dyn Fn() -> Result<(), ReplayError>,
    measure_only: bool,
}
impl<M: SnapshotMap<Value = Vec<burst_verification::state::VerifierVote>>> Field
    for PriorVotes<'_, M>
{
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        self.encode_field_at(output, None, 1)
    }
    fn encode_field_at(
        &self,
        output: &mut dyn Write,
        directory: Option<&std::path::Path>,
        depth: usize,
    ) -> Result<(), ReplayError> {
        (self.poll)()?;
        if self.measure_only {
            return measure_map(
                &self.map,
                output,
                depth,
                self.poll,
                |value, mut output, depth| {
                    encode_at_depth(&ordered_prior_votes(value), &mut output, depth)
                },
            );
        }
        encode_map(
            &self.map,
            output,
            directory,
            depth,
            |value, mut output, child_depth| {
                (self.poll)()?;
                encode_at_depth(&ordered_prior_votes(value), &mut output, child_depth)
            },
        )
    }
}

struct Neither<'a, 'b> {
    view: &'a SnapshotView<'b>,
    poll: &'a dyn Fn() -> Result<(), ReplayError>,
    measure_only: bool,
}
impl Field for Neither<'_, '_> {
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        self.encode_field_at(output, None, 1)
    }
    fn encode_field_at(
        &self,
        mut output: &mut dyn Write,
        directory: Option<&std::path::Path>,
        depth: usize,
    ) -> Result<(), ReplayError> {
        let history = ReadMap(self.view.neither_history());
        let (threshold, window) = self.view.neither_rules();
        write_record_at_depth(
            &[
                (
                    "history",
                    &PollingMap {
                        map: &history,
                        poll: self.poll,
                        measure_only: self.measure_only,
                    },
                ),
                ("penalty_threshold_bps", &threshold),
                ("window_assignments", &window),
            ],
            &mut output,
            directory,
            depth,
        )
    }
}

struct Attempts<'a, 'b> {
    view: &'a SnapshotView<'b>,
    poll: &'a dyn Fn() -> Result<(), ReplayError>,
    measure_only: bool,
}
impl Field for Attempts<'_, '_> {
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        self.encode_field_at(output, None, 1)
    }
    fn encode_field_at(
        &self,
        mut output: &mut dyn Write,
        directory: Option<&std::path::Path>,
        depth: usize,
    ) -> Result<(), ReplayError> {
        let votes = ReadMap(self.view.vote_contexts());
        let challenges = ReadMap(self.view.challenge_contexts());
        write_record_at_depth(
            &[
                (
                    "votes",
                    &PollingMap {
                        map: &votes,
                        poll: self.poll,
                        measure_only: self.measure_only,
                    },
                ),
                (
                    "challenges",
                    &PollingMap {
                        map: &challenges,
                        poll: self.poll,
                        measure_only: self.measure_only,
                    },
                ),
            ],
            &mut output,
            directory,
            depth,
        )
    }
}

pub(in crate::confirmed_replay::snapshot) struct Verification<'a, 'b>(
    pub SnapshotView<'a>,
    pub &'b dyn Fn() -> Result<(), ReplayError>,
);
impl Field for Verification<'_, '_> {
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        self.encode_field_at(output, None, 1)
    }
    fn encode_field_in(
        &self,
        output: &mut dyn Write,
        directory: Option<&std::path::Path>,
    ) -> Result<(), ReplayError> {
        self.encode_field_at(output, directory, 1)
    }
    fn encode_field_at(
        &self,
        output: &mut dyn Write,
        directory: Option<&std::path::Path>,
        depth: usize,
    ) -> Result<(), ReplayError> {
        self.encode(output, directory, depth, false)
    }
}

impl Verification<'_, '_> {
    fn encode(
        &self,
        output: &mut dyn Write,
        directory: Option<&std::path::Path>,
        depth: usize,
        preflight: bool,
    ) -> Result<(), ReplayError> {
        self.encode_mode(output, directory, depth, preflight, false)
    }

    // A discarded map stream has the same exact encoded size regardless of key
    // order. SnapshotMap keys are wallet/string keys with injective CBOR encoding;
    // individual values still pass canonical normalization and depth checks.
    fn encode_mode(
        &self,
        mut output: &mut dyn Write,
        directory: Option<&std::path::Path>,
        depth: usize,
        preflight: bool,
        measure_only: bool,
    ) -> Result<(), ReplayError> {
        check_depth(depth)?;
        (self.1)()?;
        let view = &self.0;
        // Check serialized source size separately from canonical output size.
        write_bounded(view, &mut std::io::sink(), MAX_SNAPSHOT_BYTES as usize)?;
        (self.1)()?;
        let states = ReadMap(view.states());
        let challenges = ReadMap(view.challenges());
        let penalties = ReadMap(view.penalties());
        let selections = ReadMap(view.selections());
        let appeals = ReadMap(view.appeals());
        let prior_votes = PriorVotes {
            map: ReadMap(view.prior_votes()),
            poll: self.1,
            measure_only,
        };
        record_at_depth(
            &[
                (
                    "states",
                    &PollingMap {
                        map: &states,
                        poll: self.1,
                        measure_only: measure_only,
                    },
                ),
                (
                    "active_challenges",
                    &PollingMap {
                        map: &challenges,
                        poll: self.1,
                        measure_only: measure_only,
                    },
                ),
                (
                    "penalized_verifiers",
                    &PollingMap {
                        map: &penalties,
                        poll: self.1,
                        measure_only: measure_only,
                    },
                ),
                (
                    "neither_tracker",
                    &Neither {
                        view,
                        poll: self.1,
                        measure_only,
                    },
                ),
                ("prior_votes", &prior_votes),
                (
                    "selections",
                    &PollingMap {
                        map: &selections,
                        poll: self.1,
                        measure_only: measure_only,
                    },
                ),
                (
                    "attempts",
                    &Attempts {
                        view,
                        poll: self.1,
                        measure_only,
                    },
                ),
                (
                    "active_appeals",
                    &PollingMap {
                        map: &appeals,
                        poll: self.1,
                        measure_only: measure_only,
                    },
                ),
            ],
            &mut output,
            directory,
            depth,
            preflight,
        )
    }

    fn compare(
        &self,
        other: Verification<'_, '_>,
        directory: &std::path::Path,
    ) -> Result<bool, ReplayError> {
        // Validate both complete representations before returning even a mismatch.
        // Count canonical bytes without sorting top-level map keys or staging values.
        self.encode_mode(&mut std::io::sink(), Some(directory), 0, false, true)?;
        other.encode_mode(&mut std::io::sink(), Some(directory), 0, false, true)?;
        let a = &self.0;
        let b = &other.0;
        Ok(a.neither_rules() == b.neither_rules()
            && equal_map(a.states(), b.states(), encode)?
            && equal_map(a.challenges(), b.challenges(), encode)?
            && equal_map(a.penalties(), b.penalties(), encode)?
            && equal_map(a.selections(), b.selections(), encode)?
            && equal_map(a.appeals(), b.appeals(), encode)?
            && equal_map(a.neither_history(), b.neither_history(), encode)?
            && equal_map(a.vote_contexts(), b.vote_contexts(), encode)?
            && equal_map(a.challenge_contexts(), b.challenge_contexts(), encode)?
            && equal_map(a.prior_votes(), b.prior_votes(), |votes| {
                encode(&ordered_prior_votes(votes))
            })?)
    }
}

/// Check encoding size and depth. This does not authenticate votes or validate
/// verification lifecycle rules. Polling occurs between rows and encoding passes.
pub(in crate::confirmed_replay::snapshot) fn validate_verification_with_poll(
    state: &VerificationOrchestrator,
    poll: &dyn Fn() -> Result<(), ReplayError>,
) -> Result<(), ReplayError> {
    Verification(state.snapshot_view(), poll).encode_mode(
        &mut std::io::sink(),
        None,
        0,
        false,
        true,
    )
}

#[cfg(test)]
pub(in crate::confirmed_replay::snapshot) fn encode_verification_into(
    state: &VerificationOrchestrator,
    output: &mut impl Write,
    directory: Option<&std::path::Path>,
) -> Result<(), ReplayError> {
    encode_verification_with_poll(state, output, directory, &|| Ok(()))
}

#[cfg(test)]
pub(in crate::confirmed_replay::snapshot) fn encode_verification_with_poll(
    state: &VerificationOrchestrator,
    output: &mut impl Write,
    directory: Option<&std::path::Path>,
    poll: &dyn Fn() -> Result<(), ReplayError>,
) -> Result<(), ReplayError> {
    Verification(state.snapshot_view(), poll).encode(output, directory, 0, true)
}

pub(in crate::confirmed_replay::snapshot) fn equivalent_verification(
    actual: SnapshotView<'_>,
    expected: SnapshotView<'_>,
    directory: &std::path::Path,
) -> Result<bool, ReplayError> {
    Verification(actual, &|| Ok(())).compare(Verification(expected, &|| Ok(())), directory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::{Timestamp, WalletAddress};
    use burst_verification::{state::VerifierVote, Vote};

    #[test]
    fn streamed_verification_matches_owned_root_and_nested_snapshots_and_scratch_limits() {
        for count in [0, 2, 4097] {
            let mut state = VerificationOrchestrator::new();
            for index in 0..count {
                state.initialize_genesis(&WalletAddress::new(format!("brst_streamed_{index}")));
            }
            if count > 0 {
                let owner = WalletAddress::new("brst_streamed_0");
                let verifier = WalletAddress::new("brst_verifier");
                let mut snapshot = state.snapshot();
                snapshot.penalized_verifiers.insert(verifier.clone(), 99);
                snapshot
                    .neither_tracker
                    .record_vote(&verifier, Vote::Neither);
                snapshot.prior_votes.insert(
                    owner,
                    vec![
                        VerifierVote {
                            verifier,
                            vote: Vote::Legitimate,
                            stake_amount: 12,
                            timestamp: Timestamp::new(7),
                        },
                        VerifierVote {
                            verifier: WalletAddress::new("brst_a_verifier"),
                            vote: Vote::Neither,
                            stake_amount: 3,
                            timestamp: Timestamp::new(6),
                        },
                    ],
                );
                state = VerificationOrchestrator::restore(snapshot);
            }
            let owned = state.snapshot();
            let expected = encode(&owned).unwrap();
            let mut measured = BoundedWriter {
                output: std::io::sink(),
                written: 0,
                limit: MAX_SNAPSHOT_BYTES as usize,
            };
            Verification(owned.snapshot_view(), &|| Ok(()))
                .encode_mode(&mut measured, None, 0, false, true)
                .unwrap();
            assert_eq!(
                measured.written,
                expected.len(),
                "unordered measurement preserves exact canonical size"
            );
            let mut too_small = BoundedWriter {
                output: std::io::sink(),
                written: 0,
                limit: expected.len() - 1,
            };
            assert!(Verification(state.snapshot_view(), &|| Ok(()))
                .encode_mode(&mut too_small, None, 0, false, true)
                .is_err());
            use crate::confirmed_replay::snapshot::verification_state::Verification as CheckpointVerification;
            let borrowed = CheckpointVerification::Borrowed(&state);
            assert_eq!(encode(&borrowed).unwrap(), expected);
            assert_eq!(
                bincode::serialize(&borrowed).unwrap(),
                bincode::serialize(&owned).unwrap()
            );
            let decoded: CheckpointVerification<'_> =
                ciborium::from_reader(expected.as_slice()).unwrap();
            assert!(matches!(&decoded, CheckpointVerification::Owned(_)));
            assert_eq!(encode(&decoded).unwrap(), expected);
            let restored = VerificationOrchestrator::restore(decoded.into_owned());
            assert_eq!(encode(&restored.snapshot_view()).unwrap(), expected);
            let directory = tempfile::tempdir().unwrap();
            let budget = crate::confirmed_replay::scratch_file::ScratchLimit::acquire(
                directory.path(),
                8 << 20,
            )
            .unwrap();
            let mut actual = Vec::new();
            encode_verification_into(&state, &mut actual, Some(directory.path())).unwrap();
            assert_eq!(actual, expected);
            let mut owned_bytes = Vec::new();
            Verification(owned.snapshot_view(), &|| Ok(()))
                .encode(&mut owned_bytes, Some(directory.path()), 0, true)
                .unwrap();
            assert_eq!(owned_bytes, expected);
            assert!(equivalent_verification(
                owned.snapshot_view(),
                state.snapshot_view(),
                directory.path()
            )
            .unwrap());
            let mut changed = owned.clone();
            changed
                .penalized_verifiers
                .insert(WalletAddress::new("brst_extra_penalty"), 123);
            assert!(!equivalent_verification(
                changed.snapshot_view(),
                state.snapshot_view(),
                directory.path()
            )
            .unwrap());
            if count > 0 {
                let mut reordered = owned.clone();
                for votes in reordered.prior_votes.values_mut() {
                    votes.reverse();
                }
                assert!(equivalent_verification(
                    reordered.snapshot_view(),
                    state.snapshot_view(),
                    directory.path()
                )
                .unwrap());
                let key = WalletAddress::new("brst_streamed_0");
                let value = reordered.states.remove(&key).unwrap();
                reordered
                    .states
                    .insert(WalletAddress::new("brst_changed_key"), value);
                assert!(
                    !equivalent_verification(
                        reordered.snapshot_view(),
                        state.snapshot_view(),
                        directory.path()
                    )
                    .unwrap(),
                    "same cardinality cannot hide a different key"
                );
                let mut changed = owned.clone();
                changed.prior_votes.get_mut(&key).unwrap()[0].stake_amount += 1;
                assert!(!equivalent_verification(
                    changed.snapshot_view(),
                    state.snapshot_view(),
                    directory.path()
                )
                .unwrap());
            }
            assert_eq!(budget.used_temporary_bytes().unwrap(), 0);
            #[derive(Serialize)]
            struct Outer<T> {
                verification: T,
            }
            let expected_nested = encode(&Outer {
                verification: &owned,
            })
            .unwrap();
            let mut nested = Vec::new();
            encode_record_in(
                &[(
                    "verification",
                    &Verification(state.snapshot_view(), &|| Ok(())),
                )],
                &mut nested,
                Some(directory.path()),
            )
            .unwrap();
            assert_eq!(nested, expected_nested);
            let mut owned_nested = Vec::new();
            encode_record_in(
                &[(
                    "verification",
                    &Verification(owned.snapshot_view(), &|| Ok(())),
                )],
                &mut owned_nested,
                Some(directory.path()),
            )
            .unwrap();
            assert_eq!(owned_nested, expected_nested);
            assert_eq!(budget.used_temporary_bytes().unwrap(), 0);
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
            if count > 4096 {
                let polls = std::cell::Cell::new(0);
                let mut cancelled = Vec::new();
                let error = encode_verification_with_poll(
                    &state,
                    &mut cancelled,
                    Some(directory.path()),
                    &|| {
                        polls.set(polls.get() + 1);
                        if polls.get() == 37 {
                            Err(invalid("cancelled map value"))
                        } else {
                            Ok(())
                        }
                    },
                )
                .unwrap_err();
                assert!(error.to_string().contains("cancelled map value"));
                assert!(cancelled.is_empty());
                assert_eq!(budget.used_temporary_bytes().unwrap(), 0);
                drop(budget);
                let tiny = crate::confirmed_replay::scratch_file::ScratchLimit::acquire(
                    directory.path(),
                    1,
                )
                .unwrap();
                let mut rejected = Vec::new();
                assert!(equivalent_verification(
                    owned.snapshot_view(),
                    state.snapshot_view(),
                    directory.path()
                )
                .unwrap());
                validate_verification_with_poll(&state, &|| Ok(())).unwrap();
                let polls = std::cell::Cell::new(0);
                assert!(validate_verification_with_poll(&state, &|| {
                    polls.set(polls.get() + 1);
                    if polls.get() == 37 {
                        Err(invalid("cancelled measurement"))
                    } else {
                        Ok(())
                    }
                })
                .unwrap_err()
                .to_string()
                .contains("cancelled measurement"));
                assert!(
                    encode_verification_into(&state, &mut rejected, Some(directory.path()))
                        .is_err()
                );
                assert!(rejected.is_empty());
                assert!(encode_record_in(
                    &[(
                        "verification",
                        &Verification(owned.snapshot_view(), &|| Ok(()))
                    )],
                    &mut rejected,
                    Some(directory.path())
                )
                .is_err());
                assert!(rejected.is_empty());
                assert_eq!(tiny.used_temporary_bytes().unwrap(), 0);
                assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
                let missing = directory.path().join("missing");
                assert!(encode_verification_into(&state, &mut rejected, Some(&missing)).is_err());
                assert!(rejected.is_empty());
            }
        }
    }
}
