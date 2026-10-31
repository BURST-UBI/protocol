//! Reference extraction only; recovered inventory validation and transitive
//! resolution must precede retirement. No phase or age suppresses a reference.
use super::*;

#[derive(Debug, PartialEq, Eq)]
pub enum Reference<'a> {
    Account(&'a WalletAddress),
    Block(BlockHash),
    Checkpoint(BlockHash),
    CommitteeContext(BlockHash),
    Outcome(BlockHash),
    Parameters(BlockHash),
    Boundary(Timestamp),
}

pub(super) fn vote_roots(
    vote: &VerifierVote,
    visit: &mut impl FnMut(Reference<'_>) -> Result<(), String>,
) -> Result<(), String> {
    visit(Reference::Account(&vote.verifier))?;
    visit(Reference::Boundary(vote.timestamp))
}

pub(super) fn target_roots(
    target: &Target,
    visit: &mut impl FnMut(Reference<'_>) -> Result<(), String>,
) -> Result<(), String> {
    visit(Reference::Account(&target.address))?;
    if let Some(state) = &target.state {
        visit(Reference::Account(&state.target))?;
        visit(Reference::Boundary(state.started_at))?;
        for endorsement in state.endorsements.iter() {
            visit(Reference::Account(&endorsement.endorser))?;
            visit(Reference::Boundary(endorsement.timestamp))?;
        }
        for account in state
            .endorser_set
            .iter()
            .chain(state.selected_verifiers.iter())
            .chain(state.excluded_verifiers.iter())
        {
            visit(Reference::Account(account))?;
        }
        for vote in &state.votes {
            vote_roots(vote, visit)?;
        }
    }
    if let Some(challenge) = &target.challenge {
        visit(Reference::Account(&challenge.target))?;
        visit(Reference::Account(&challenge.challenger))?;
        visit(Reference::Boundary(challenge.initiated_at))?;
    }
    if let Some(appeal) = &target.appeal {
        visit(Reference::Outcome(appeal.reviewed_outcome))?;
    }
    if let Some(votes) = &target.votes {
        for vote in &votes.0 {
            vote_roots(vote, visit)?;
        }
    }
    if let Some(selection) = &target.selection {
        visit(Reference::Account(selection.request.target()))?;
        visit(Reference::Block(selection.request.trigger()))?;
        visit(Reference::Parameters(selection.request.params_hash()))?;
        visit(Reference::Boundary(Timestamp::new(
            selection.request.cutoff(),
        )))?;
        for account in selection.request.storage_pool() {
            visit(Reference::Account(account))?;
        }
        if let Some((checkpoint, _)) = &selection.certified {
            visit(Reference::Checkpoint(checkpoint.hash()))?;
        }
    }
    if let Some(contexts) = &target.vote_contexts {
        for (account, context) in contexts {
            visit(Reference::Account(&WalletAddress::new(account)))?;
            visit(Reference::CommitteeContext(*context))?;
        }
    }
    if let Some(admission) = target.challenge_context {
        visit(Reference::Block(admission))?;
    }
    Ok(())
}

impl VerificationRecord {
    /// Visit direct references, including repeated references and terminal states.
    /// The caller validates and resolves their transitive dependencies before deletion.
    pub fn visit_retention_references(
        &self,
        mut visit: impl FnMut(Reference<'_>) -> Result<(), String>,
    ) -> Result<(), String> {
        match &self.0 {
            Record::Rules { .. } => Ok(()),
            Record::Target(target) => target_roots(target, &mut visit),
            Record::Verifier {
                address, penalty, ..
            } => {
                visit(Reference::Account(address))?;
                if let Some(until) = penalty {
                    visit(Reference::Boundary(Timestamp::new(*until)))?;
                }
                Ok(())
            }
        }
    }
}

pub use super::normalized::visit_retention_references as visit_normalized_target_references;

#[cfg(test)]
mod tests {
    use super::*;
    fn hash(n: u8) -> BlockHash {
        BlockHash::new([n; 32])
    }
    fn address(n: u8) -> WalletAddress {
        burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[n; 32]).public)
    }

    #[test]
    fn normalized_rows_preserve_logical_roots_across_all_member_domains() {
        let target_address = address(1);
        let mut engine = VerificationOrchestrator::new();
        engine.initialize_genesis(&target_address);
        let mut record = engine.target_record(&target_address).unwrap();
        let Record::Target(target) = &mut record.0 else {
            unreachable!()
        };
        let state = target.state.as_mut().unwrap();
        state.started_at = Timestamp::new(20);
        state.endorsements.push(crate::state::Endorsement {
            endorser: address(2),
            burn_amount: 1,
            timestamp: Timestamp::new(21),
        });
        state.endorser_set.insert(address(2));
        state.selected_verifiers.push(address(3));
        let vote = VerifierVote {
            verifier: address(3),
            vote: Vote::Legitimate,
            stake_amount: 1,
            timestamp: Timestamp::new(22),
        };
        state.votes.push(vote.clone());
        state.excluded_verifiers.insert(address(4));
        target.votes = Some(PriorVotes(vec![vote]));
        target.challenge = Some(Challenge {
            challenger: address(5),
            target: target_address.clone(),
            stake_amount: 2,
            initiated_at: Timestamp::new(23),
            reason: crate::ChallengeReason::Fraud,
        });
        target.appeal = Some(AppealContext {
            reviewed_outcome: hash(6),
            previously_legitimate: true,
        });
        target.challenge_context = Some(hash(7));
        target.vote_contexts = Some([(address(3).to_string(), hash(8))].into());
        let request = SelectionRequest::new(
            target_address.clone(),
            hash(9),
            0,
            1_700_000_000,
            hash(10),
            vec![address(3)],
            1,
        )
        .unwrap();
        let checkpoint = Checkpoint {
            network: burst_types::NetworkId::Dev,
            genesis: hash(11),
            parent: hash(12),
            height: 1,
            cutoff: request.cutoff(),
            ledger_root: hash(13),
            params_hash: hash(10),
            next_roster: hash(14),
            selection_root: request.commitment().unwrap(),
            next_quorum_bps: 6700,
        };
        let checkpoint_hash = checkpoint.hash();
        target.selection = Some(SelectionRound {
            request,
            certified: Some((
                checkpoint,
                RandomOutput {
                    value: [1; 32],
                    proof: vec![],
                    round: 1,
                },
            )),
            context: Default::default(),
        });
        let mut expected = Vec::new();
        record
            .visit_retention_references(|r| {
                expected.push(format!("{r:?}"));
                Ok(())
            })
            .unwrap();
        for reference in [
            Reference::Outcome(hash(6)),
            Reference::Block(hash(7)),
            Reference::CommitteeContext(hash(8)),
            Reference::Block(hash(9)),
            Reference::Parameters(hash(10)),
            Reference::Checkpoint(checkpoint_hash),
        ] {
            assert!(expected.contains(&format!("{reference:?}")));
        }
        assert_eq!(expected.len(), 25);
        expected.sort();
        let rows = normalized::target_rows(&record).unwrap();
        assert_eq!(rows.len(), 9, "header and all eight member domains");
        let mut actual = Vec::new();
        for (key, bytes) in &rows {
            let mut count = 0;
            visit_normalized_target_references(&target_address, key, bytes, |r| {
                count += 1;
                actual.push(format!("{r:?}"));
                Ok(())
            })
            .unwrap();
            let mut keyed = Vec::new();
            normalized::visit_row_retention_references(key, bytes, |r| {
                keyed.push(format!("{r:?}"));
                Ok(())
            })
            .unwrap();
            assert_eq!(&actual[actual.len() - count..], keyed);
            for stop in 1..=count {
                let mut calls = 0;
                assert!(
                    visit_normalized_target_references(&target_address, key, bytes, |_| {
                        calls += 1;
                        if calls == stop {
                            Err("stop".into())
                        } else {
                            Ok(())
                        }
                    })
                    .is_err()
                );
                assert_eq!(calls, stop);
            }
            let mut trailing = bytes.clone();
            trailing.push(0);
            assert!(visit_normalized_target_references(
                &target_address,
                key,
                &trailing,
                |_| panic!("trailing payload")
            )
            .is_err());
            assert!(
                visit_normalized_target_references(&address(15), key, bytes, |_| panic!(
                    "wrong target"
                ))
                .is_err()
            );
        }
        actual.sort();
        assert_eq!(actual, expected);
        let verifier = VerificationRecord(Record::Verifier {
            address: address(16),
            penalty: Some(24),
            history: Some(Default::default()),
        });
        let mut references = Vec::new();
        verifier
            .visit_retention_references(|r| {
                references.push(format!("{r:?}"));
                Ok(())
            })
            .unwrap();
        assert_eq!(
            references,
            [
                format!("{:?}", Reference::Account(&address(16))),
                format!("{:?}", Reference::Boundary(Timestamp::new(24)))
            ]
        );
    }
}
