//! Governance row references, independent of phase and current voting power.
//! Extraction does not authenticate rows or authorize retirement.
use crate::{engine::GovernanceRecord, ProposalContent};
use burst_types::{BlockHash, Timestamp, TxHash, WalletAddress};

#[derive(Debug, PartialEq, Eq)]
pub enum Reference<'a> {
    Account(&'a WalletAddress),
    Proposal(TxHash),
    Block(BlockHash),
    Genesis(BlockHash),
    DelegationScope(BlockHash),
    /// Hash of the grant's signed payload, not its carrier ledger block.
    Grant(BlockHash),
    Boundary(Timestamp),
}

fn content_references(
    content: &ProposalContent,
    visit: &mut impl FnMut(Reference<'_>) -> Result<(), String>,
) -> Result<(), String> {
    match content {
        ProposalContent::RepresentativeEviction { target, .. } => visit(Reference::Account(target)),
        ProposalContent::ParameterChange { .. }
        | ProposalContent::Emergency { .. }
        | ProposalContent::ConstitutionalAmendment { .. }
        | ProposalContent::ConstitutionalDiff { .. } => Ok(()),
    }
}

impl GovernanceRecord {
    pub fn visit_retention_references(
        &self,
        mut visit: impl FnMut(Reference<'_>) -> Result<(), String>,
    ) -> Result<(), String> {
        match self {
            Self::Proposal(proposal) => {
                visit(Reference::Proposal(proposal.hash))?;
                visit(Reference::Account(&proposal.proposer))?;
                visit(Reference::Boundary(proposal.created_at))?;
                for at in [
                    proposal.exploration_started_at,
                    proposal.cooldown_started_at,
                    proposal.promotion_started_at,
                    proposal.activation_at,
                ]
                .into_iter()
                .flatten()
                {
                    visit(Reference::Boundary(at))?;
                }
                content_references(&proposal.content, &mut visit)
            }
            Self::VoteBucket { proposal, .. }
            | Self::Endorsement { proposal, .. }
            | Self::Timing { proposal, .. } => visit(Reference::Proposal(*proposal)),
            Self::Vote {
                proposal, voter, ..
            } => {
                visit(Reference::Proposal(*proposal))?;
                visit(Reference::Account(voter))
            }
            Self::KeyBallot {
                proposal,
                owner,
                grant,
                ..
            } => {
                visit(Reference::Proposal(*proposal))?;
                visit(Reference::Account(owner))?;
                visit(Reference::Grant(*grant))
            }
            Self::PendingEffects {
                amendments,
                evictions,
                ..
            } => {
                for content in amendments {
                    content_references(content, &mut visit)?;
                }
                for (target, _) in evictions {
                    visit(Reference::Account(target))?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GovernancePhase, GovernanceTiming, Proposal};
    use burst_transactions::governance::GovernanceVote;
    fn account(n: u8) -> WalletAddress {
        WalletAddress::new(format!("brst_retention_{n}"))
    }
    fn proposal() -> Proposal {
        Proposal {
            hash: TxHash::new([1; 32]),
            proposer: account(2),
            phase: GovernancePhase::Activated,
            content: ProposalContent::RepresentativeEviction {
                target: account(3),
                evict: false,
                reason: "restore".into(),
            },
            endorsement_count: 2,
            total_eligible_voters: 3,
            exploration_started_at: Some(Timestamp::new(5)),
            exploration_votes_yea: 1,
            exploration_votes_nay: 0,
            exploration_votes_abstain: 0,
            cooldown_started_at: Some(Timestamp::new(6)),
            promotion_started_at: Some(Timestamp::new(7)),
            promotion_votes_yea: 0,
            promotion_votes_nay: 0,
            promotion_votes_abstain: 0,
            round: 0,
            created_at: Timestamp::new(4),
            activation_at: Some(Timestamp::new(8)),
        }
    }
    #[test]
    fn all_record_variants_preserve_references_and_callback_boundaries() {
        let p = proposal();
        let hash = p.hash;
        let proposal_ref = format!("{:?}", Reference::Proposal(hash));
        let mut records = vec![
            (
                GovernanceRecord::VoteBucket {
                    proposal: hash,
                    promotion: false,
                },
                vec![proposal_ref.clone()],
            ),
            (
                GovernanceRecord::Endorsement {
                    proposal: hash,
                    count: 3,
                    burned: 4,
                },
                vec![proposal_ref.clone()],
            ),
            (
                GovernanceRecord::Timing {
                    proposal: hash,
                    timing: GovernanceTiming {
                        proposal_secs: 1,
                        voting_secs: 2,
                        buffer_secs: 3,
                    },
                },
                vec![proposal_ref.clone()],
            ),
            (
                GovernanceRecord::KeyBallot {
                    proposal: hash,
                    owner: account(2),
                    grant: BlockHash::new([9; 32]),
                    vote: GovernanceVote::Yea,
                },
                vec![
                    proposal_ref.clone(),
                    format!("{:?}", Reference::Account(&account(2))),
                    format!("{:?}", Reference::Grant(BlockHash::new([9; 32]))),
                ],
            ),
            (
                GovernanceRecord::PendingEffects {
                    changes: vec![],
                    amendments: vec![p.content.clone()],
                    evictions: vec![(account(10), true), (account(11), false)],
                },
                [3, 10, 11]
                    .map(|n| format!("{:?}", Reference::Account(&account(n))))
                    .to_vec(),
            ),
        ];
        for promotion in [false, true] {
            records.push((
                GovernanceRecord::Vote {
                    proposal: hash,
                    promotion,
                    voter: account(2),
                    vote: GovernanceVote::Nay,
                },
                vec![
                    proposal_ref.clone(),
                    format!("{:?}", Reference::Account(&account(2))),
                ],
            ));
        }
        for phase in [
            GovernancePhase::Proposal,
            GovernancePhase::Exploration,
            GovernancePhase::Cooldown,
            GovernancePhase::Promotion,
            GovernancePhase::Activation,
            GovernancePhase::Activated,
            GovernancePhase::Rejected,
            GovernancePhase::Withdrawn,
            GovernancePhase::Conflicted,
        ] {
            let mut p = p.clone();
            p.phase = phase;
            let mut expected = vec![
                proposal_ref.clone(),
                format!("{:?}", Reference::Account(&account(2))),
            ];
            expected.extend(
                (4..=8).map(|time| format!("{:?}", Reference::Boundary(Timestamp::new(time)))),
            );
            expected.push(format!("{:?}", Reference::Account(&account(3))));
            records.push((GovernanceRecord::Proposal(Box::new(p)), expected));
        }
        for (record, expected) in records {
            let (key, bytes) = record.storage_entry().unwrap();
            let decoded = GovernanceRecord::from_storage_entry(&key, &bytes).unwrap();
            let mut actual = Vec::new();
            decoded
                .visit_retention_references(|r| {
                    actual.push(format!("{r:?}"));
                    Ok(())
                })
                .unwrap();
            assert_eq!(actual, expected);
            for stop in 1..=expected.len() {
                let mut calls = 0;
                assert!(decoded
                    .visit_retention_references(|_| {
                        calls += 1;
                        if calls == stop {
                            Err("stop".into())
                        } else {
                            Ok(())
                        }
                    })
                    .is_err());
                assert_eq!(calls, stop);
            }
        }
    }
}
