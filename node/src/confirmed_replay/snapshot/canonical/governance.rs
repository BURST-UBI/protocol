use super::*;
use burst_governance::engine::{GovernanceEngine, SnapshotMap};

struct ReadMap<'a, M>(&'a M);
impl<M: SnapshotMap> MapSource for ReadMap<'_, M> {
    type Key = M::Key;
    type Value = M::Value;
    fn len(&self) -> usize {
        self.0.len()
    }
    fn entries(&self) -> impl Iterator<Item = (&Self::Key, &Self::Value)> {
        self.0.entries()
    }
}
struct PollingMap<'a, M>(&'a M, &'a dyn Fn() -> Result<(), ReplayError>);
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
        (self.1)()?;
        encode_map(
            &ReadMap(self.0),
            output,
            directory,
            depth,
            |value, mut output, child| {
                (self.1)()?;
                encode_at_depth(value, &mut output, child)
            },
        )
    }
}
struct Votes<'a, M>(&'a M, &'a dyn Fn() -> Result<(), ReplayError>);
impl<M: SnapshotMap<Value: SnapshotMap>> Field for Votes<'_, M> {
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        self.encode_field_at(output, None, 1)
    }
    fn encode_field_at(
        &self,
        output: &mut dyn Write,
        directory: Option<&std::path::Path>,
        depth: usize,
    ) -> Result<(), ReplayError> {
        (self.1)()?;
        encode_map(
            &ReadMap(self.0),
            output,
            directory,
            depth,
            |value, output, child| {
                PollingMap(value, self.1).encode_field_at(output, directory, child)
            },
        )
    }
}
struct Effects<'a, T>(&'a [T], &'a dyn Fn() -> Result<(), ReplayError>);
impl<T: Serialize> Field for Effects<'_, T> {
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        self.encode_field_at(output, None, 1)
    }
    fn encode_field_at(
        &self,
        mut output: &mut dyn Write,
        _: Option<&std::path::Path>,
        depth: usize,
    ) -> Result<(), ReplayError> {
        check_depth(depth)?;
        let mut output = BoundedWriter {
            output: &mut output,
            written: 0,
            limit: MAX_SNAPSHOT_BYTES as usize,
        };
        collection_header(0x80, self.0.len(), &mut output)?;
        for value in self.0 {
            (self.1)()?;
            encode_at_depth(value, &mut output, depth + 1)?;
        }
        Ok(())
    }
}

struct Governance<'a>(
    &'a GovernanceEngine,
    &'a dyn Fn() -> Result<(), ReplayError>,
);
impl Field for Governance<'_> {
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        self.encode_field_at(output, None, 1)
    }
    fn encode_field_at(
        &self,
        output: &mut dyn Write,
        directory: Option<&std::path::Path>,
        depth: usize,
    ) -> Result<(), ReplayError> {
        encode_engine(self.0, output, directory, self.1, depth, false)
    }
}

fn encode_engine(
    state: &GovernanceEngine,
    mut output: &mut dyn Write,
    directory: Option<&std::path::Path>,
    poll: &dyn Fn() -> Result<(), ReplayError>,
    depth: usize,
    preflight: bool,
) -> Result<(), ReplayError> {
    check_depth(depth)?;
    poll()?;
    // Check serialized source size separately from canonical output size.
    write_bounded(state, &mut std::io::sink(), MAX_SNAPSHOT_BYTES as usize)?;
    poll()?;
    let view = state.snapshot_view();
    record_at_depth(
        &[
            ("proposals", &PollingMap(view.proposals(), poll)),
            ("exploration_votes", &Votes(view.exploration_votes(), poll)),
            ("promotion_votes", &Votes(view.promotion_votes(), poll)),
            ("endorsement_counts", &PollingMap(view.endorsements(), poll)),
            ("pending_changes", &Effects(view.pending_changes(), poll)),
            (
                "pending_amendments",
                &Effects(view.pending_amendments(), poll),
            ),
            (
                "pending_evictions",
                &Effects(view.pending_evictions(), poll),
            ),
            ("timings", &PollingMap(view.timings(), poll)),
            ("key_ballots", &PollingMap(view.key_ballots(), poll)),
        ],
        &mut output,
        directory,
        depth,
        preflight,
    )
}

pub(in crate::confirmed_replay::snapshot) fn encode_governance_with_poll(
    state: &GovernanceEngine,
    output: &mut impl Write,
    directory: Option<&std::path::Path>,
    poll: &dyn Fn() -> Result<(), ReplayError>,
) -> Result<(), ReplayError> {
    encode_engine(state, output, directory, poll, 0, true)
}

/// Compare canonical representations, retaining both complete encodings in RAM.
/// The scratch directory is used for sorting, not for these output buffers.
pub(in crate::confirmed_replay::snapshot) fn equivalent_governance(
    actual: &GovernanceEngine,
    expected: &GovernanceEngine,
    directory: &std::path::Path,
) -> Result<bool, ReplayError> {
    let mut actual_bytes = Vec::new();
    let mut expected_bytes = Vec::new();
    encode_governance_with_poll(actual, &mut actual_bytes, Some(directory), &|| Ok(()))?;
    encode_governance_with_poll(expected, &mut expected_bytes, Some(directory), &|| Ok(()))?;
    Ok(actual_bytes == expected_bytes)
}

struct Reverse<'a, M>(&'a M);
impl<M: SnapshotMap<Value: burst_governance::vote_delegation::WalletSet>> Field for Reverse<'_, M> {
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        self.encode_field_at(output, None, 1)
    }
    fn encode_field_at(
        &self,
        output: &mut dyn Write,
        directory: Option<&std::path::Path>,
        depth: usize,
    ) -> Result<(), ReplayError> {
        use burst_governance::vote_delegation::WalletSet;
        encode_map(
            &ReadMap(self.0),
            output,
            directory,
            depth,
            |value, output, child| {
                check_depth(child)?;
                let sources = value.ordered_sources();
                Sequence(&sources).encode_field_at(output, directory, child)
            },
        )
    }
}
struct Delegation<'a>(&'a burst_governance::DelegationEngine);
impl Field for Delegation<'_> {
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        self.encode_field_at(output, None, 1)
    }
    fn encode_field_at(
        &self,
        mut output: &mut dyn Write,
        directory: Option<&std::path::Path>,
        depth: usize,
    ) -> Result<(), ReplayError> {
        check_depth(depth)?;
        write_bounded(self.0, &mut std::io::sink(), MAX_SNAPSHOT_BYTES as usize)?;
        let view = self.0.snapshot_view();
        let poll = || Ok(());
        write_record_at_depth(
            &[
                ("delegations", &PollingMap(view.delegations(), &poll)),
                ("reverse_delegations", &Reverse(view.reverse())),
                ("scoped_delegations", &PollingMap(view.scoped(), &poll)),
                ("max_depth", &view.max_depth()),
                ("key_ballots", &PollingMap(view.ballots(), &poll)),
            ],
            &mut output,
            directory,
            depth,
        )
    }
}

pub(in crate::confirmed_replay::snapshot) struct CandidateGovernance<'a>(
    pub &'a Option<crate::governance_commit::CandidateGovernance>,
);
impl Field for CandidateGovernance<'_> {
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
        mut output: &mut dyn Write,
        directory: Option<&std::path::Path>,
        depth: usize,
    ) -> Result<(), ReplayError> {
        check_depth(depth)?;
        // Bound the serialized candidate as a whole before encoding its fields.
        write_bounded(self.0, &mut std::io::sink(), MAX_SNAPSHOT_BYTES as usize)?;
        let Some(state) = self.0 else {
            return encode_at_depth(self.0, &mut output, depth);
        };
        write_record_at_depth(
            &[
                ("network", &state.network),
                ("genesis", &state.genesis),
                ("governance", &Governance(&state.governance, &|| Ok(()))),
                ("delegation", &Delegation(&state.delegation)),
                ("constitution", &Constitution(&state.constitution)),
                ("params", &state.params),
            ],
            &mut output,
            directory,
            depth,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_governance::engine::GovernanceRecord;
    use burst_transactions::governance::GovernanceVote;
    use burst_types::{TxHash, WalletAddress};

    #[test]
    fn streamed_governance_preserves_bytes_nested_spills_and_cancellation() {
        let records = crate::governance_storage::tests::comparison_fixture();
        let id = TxHash::new([17; 32]);
        // Exercise outer-map and one large inner-voter-map spilling separately.
        for shape in 0..4 {
            let mut input = if shape == 0 {
                vec![records[0].clone()]
            } else {
                records.clone()
            };
            for index in 0..if shape >= 2 { 4097u64 } else { 0 } {
                if shape == 2 {
                    input.push(GovernanceRecord::Vote {
                        proposal: id,
                        promotion: false,
                        voter: WalletAddress::new(format!("brst_stream_vote_{index}")),
                        vote: GovernanceVote::Nay,
                    });
                } else {
                    let mut hash = [0; 32];
                    hash[..8].copy_from_slice(&index.to_le_bytes());
                    input.push(GovernanceRecord::VoteBucket {
                        proposal: TxHash::new(hash),
                        promotion: true,
                    });
                }
            }
            let candidate = GovernanceEngine::from_records(input.into_iter().map(Ok)).unwrap();
            assert_eq!(
                bincode::serialize(&candidate).unwrap(),
                bincode::serialize(&candidate.snapshot_view()).unwrap()
            );
            let scratch_dir = tempfile::tempdir().unwrap();
            let budget = crate::confirmed_replay::scratch_file::ScratchLimit::acquire(
                scratch_dir.path(),
                8 << 20,
            )
            .unwrap();
            let mut bytes = Vec::new();
            encode_governance_with_poll(&candidate, &mut bytes, Some(scratch_dir.path()), &|| {
                Ok(())
            })
            .unwrap();
            assert_eq!(bytes, encode(&candidate).unwrap());
            assert!(equivalent_governance(&candidate, &candidate, scratch_dir.path()).unwrap());
            assert!(!equivalent_governance(
                &candidate,
                &GovernanceEngine::new(),
                scratch_dir.path()
            )
            .unwrap());
            let mut delegation = burst_governance::DelegationEngine::new(8);
            for index in 0..if shape >= 2 { 4097 } else { 3 } {
                let owner = WalletAddress::new(format!("brst_delegate_source_{index}"));
                let target = WalletAddress::new(format!(
                    "brst_delegate_target_{}",
                    if shape == 2 { 0 } else { index }
                ));
                delegation.delegate(&owner, &target).unwrap();
            }
            delegation
                .delegate_scoped(
                    &WalletAddress::new("brst_scoped"),
                    &WalletAddress::new("brst_target"),
                    burst_governance::DelegationScope::Category("economic".into()),
                )
                .unwrap();
            delegation.project_key_ballot(WalletAddress::new("brst_revoked"), None);
            let wrapped = Some(crate::governance_commit::CandidateGovernance {
                network: burst_types::NetworkId::Dev,
                genesis: burst_types::BlockHash::ZERO,
                governance: candidate.clone(),
                delegation,
                constitution: burst_consti::ConstiEngine::bootstrap(),
                params: burst_types::ProtocolParams::default(),
            });
            #[derive(Serialize)]
            struct Outer<'a> {
                governance: &'a Option<crate::governance_commit::CandidateGovernance>,
            }
            for state in [&None, &wrapped] {
                let mut nested = Vec::new();
                encode_record_in(
                    &[("governance", &CandidateGovernance(state))],
                    &mut nested,
                    Some(scratch_dir.path()),
                )
                .unwrap();
                assert_eq!(nested, encode(&Outer { governance: state }).unwrap());
            }
            if shape == 1 {
                for depth in 60..=65 {
                    let mut legacy = Vec::new();
                    let old = encode_at_depth(&candidate, &mut legacy, depth);
                    let mut streamed = Vec::new();
                    let new = encode_engine(
                        &candidate,
                        &mut streamed,
                        Some(scratch_dir.path()),
                        &|| Ok(()),
                        depth,
                        true,
                    );
                    assert_eq!(old.is_ok(), new.is_ok(), "depth {depth}");
                    if old.is_ok() {
                        assert_eq!(legacy, streamed);
                    } else {
                        assert!(streamed.is_empty());
                    }
                }
            }
            assert_eq!(budget.used_temporary_bytes().unwrap(), 0);
            let polls = std::cell::Cell::new(0);
            let mut rejected = Vec::new();
            let error = encode_governance_with_poll(
                &candidate,
                &mut rejected,
                Some(scratch_dir.path()),
                &|| {
                    polls.set(polls.get() + 1);
                    if polls.get() == 3 {
                        Err(crate::confirmed_replay::ReplayError::Invalid(
                            "governance canonical sentinel".into(),
                        ))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();
            assert!(error.to_string().contains("governance canonical sentinel"));
            assert!(rejected.is_empty());
            assert_eq!(budget.used_temporary_bytes().unwrap(), 0);
            drop(budget);
            if shape >= 2 {
                let tiny = crate::confirmed_replay::scratch_file::ScratchLimit::acquire(
                    scratch_dir.path(),
                    1,
                )
                .unwrap();
                assert!(encode_governance_with_poll(
                    &candidate,
                    &mut rejected,
                    Some(scratch_dir.path()),
                    &|| Ok(())
                )
                .is_err());
                assert!(rejected.is_empty());
                assert!(equivalent_governance(&candidate, &candidate, scratch_dir.path()).is_err());
                assert!(encode_record_in(
                    &[("governance", &CandidateGovernance(&wrapped))],
                    &mut rejected,
                    Some(scratch_dir.path())
                )
                .is_err());
                assert!(rejected.is_empty());
                assert_eq!(tiny.used_temporary_bytes().unwrap(), 0);
            }
            assert_eq!(std::fs::read_dir(scratch_dir.path()).unwrap().count(), 0);
        }
    }
}
