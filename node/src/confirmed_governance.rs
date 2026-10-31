//! Voting boundaries derived from cemented governance, never provisional state.

use burst_utils::SharedMap;

use burst_governance::{GovernancePhase, ProposalContent};
use burst_ledger::{BlockType, StateBlock};
use burst_store::StoreError;
use burst_store_lmdb::{LmdbStore, LmdbWriteTransaction};
use burst_types::{BlockHash, NetworkId, ProtocolParams, Timestamp};
use serde::{Deserialize, Serialize};

pub(crate) use burst_store_lmdb::confirmed_governance::{KEY, HEADER};
use burst_store_lmdb::confirmed_governance::{Schedule, Snapshot, MAX_PROPOSALS};

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ConfirmedGovernance {
    epoch: BlockHash,
    params: ProtocolParams,
    proposals: SharedMap<[u8; 32], Schedule>,
}

fn invalid(message: impl ToString) -> StoreError {
    StoreError::Corruption(message.to_string())
}

impl ConfirmedGovernance {
    pub(crate) fn applies_to(kind: &BlockType) -> bool {
        matches!(
            kind,
            BlockType::GovernanceProposal
                | BlockType::GovernanceEndorse
                | BlockType::GovernanceWithdraw
        )
    }
    pub(crate) fn initial(epoch: BlockHash, params: ProtocolParams) -> Self {
        Self {
            epoch,
            params,
            proposals: SharedMap::default(),
        }
    }

    /// Build from caller-authenticated checkpoint state. Only proposal and
    /// exploration phases contribute schedules.
    pub(crate) fn from_checkpoint(
        epoch: BlockHash,
        params: ProtocolParams,
        governance: Option<&crate::governance_commit::CandidateGovernance>,
    ) -> Result<Self, StoreError> {
        let mut result = Self::initial(epoch, params);
        if let Some(governance) = governance {
            for proposal in governance.governance.all_proposals().filter(|proposal| {
                matches!(
                    proposal.phase,
                    GovernancePhase::Proposal | GovernancePhase::Exploration
                )
            }) {
                result.proposals.insert(
                    *proposal.hash.as_bytes(),
                    Schedule {
                        proposer: proposal.proposer.clone(),
                        created_at: proposal.created_at.as_secs(),
                        timing: governance
                            .governance
                            .timing(proposal, &governance.params)
                            .map_err(invalid)?,
                        endorsements: proposal.endorsement_count,
                        last_endorsement: None,
                        withdrawn_at: None,
                        exploration_started_at: proposal
                            .exploration_started_at
                            .map(|at| at.as_secs()),
                    },
                );
            }
        }
        Ok(result)
    }

    pub(crate) fn load(batch: &LmdbWriteTransaction<'_>) -> Result<Option<Self>, StoreError> {
        if !batch.contains_meta(burst_store::meta::CERTIFIED_PAYMENT_AUTHORITY_KEY)? {
            return Ok(None);
        }
        let snapshot = batch.read_confirmed_governance()?;
        Ok(Some(Self { epoch: snapshot.epoch, params: snapshot.params, proposals: snapshot.proposals }))
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot { epoch: self.epoch, params: self.params.clone(), proposals: self.proposals.clone() }
    }

    pub(crate) fn promote(
        &mut self,
        batch: &LmdbWriteTransaction<'_>,
        block: &StateBlock,
    ) -> Result<(), StoreError> {
        self.promote_view(&batch.read_view(), block)
    }

    /// Apply one caller-confirmed block to the in-memory schedule. Endorsements
    /// are not deduplicated here; callers must promote each block exactly once.
    pub(crate) fn promote_view(
        &mut self,
        batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
        block: &StateBlock,
    ) -> Result<(), StoreError> {
        if !Self::applies_to(&block.block_type) {
            return Ok(());
        }
        if block.payment_epoch != self.epoch {
            return Err(invalid("governance confirmation crosses its epoch"));
        }
        if block.block_type == BlockType::GovernanceProposal {
            let content =
                if block.origin.as_bytes() == &burst_governance::content::FULL_CONTENT_MARKER {
                    crate::governance_content::load_view(batch, block).map_err(invalid)?
                } else {
                    ProposalContent::decode_compact(block.link.as_bytes()).map_err(invalid)?
                };
            if self.proposals.len() >= MAX_PROPOSALS
                || self.proposals.contains_key(block.hash.as_bytes())
            {
                return Err(invalid(
                    "duplicate or oversized confirmed governance schedule",
                ));
            }
            self.proposals.insert(
                *block.hash.as_bytes(),
                Schedule {
                    proposer: block.account.clone(),
                    created_at: block.timestamp.as_secs(),
                    timing: burst_governance::GovernanceTiming::new(&self.params, &content),
                    endorsements: 0,
                    last_endorsement: None,
                    withdrawn_at: None,
                    exploration_started_at: matches!(content, ProposalContent::Emergency { .. })
                        .then_some(block.timestamp.as_secs()),
                },
            );
            return Ok(());
        }
        let proposal = self
            .proposals
            .get_mut(block.link.as_bytes())
            .ok_or_else(|| invalid("confirmed governance support has no active proposal"))?;
        let end = proposal
            .timing
            .proposal_end(Timestamp::new(proposal.created_at))
            .map_err(invalid)?
            .as_secs();
        let at = block.timestamp.as_secs();
        if at < proposal.created_at || at >= end || proposal.exploration_started_at.is_some() {
            return Err(invalid(
                "confirmed governance support is outside its proposal window",
            ));
        }
        if block.block_type == BlockType::GovernanceWithdraw {
            if block.account != proposal.proposer
                || proposal.withdrawn_at.is_some()
                || proposal
                    .last_endorsement
                    .is_some_and(|support| support >= at)
            {
                return Err(invalid("ambiguous confirmed governance withdrawal"));
            }
            proposal.withdrawn_at = Some(at);
        } else {
            if proposal
                .withdrawn_at
                .is_some_and(|withdrawal| at >= withdrawal)
            {
                return Err(invalid("confirmed support does not precede withdrawal"));
            }
            proposal.endorsements = proposal
                .endorsements
                .checked_add(1)
                .ok_or_else(|| invalid("confirmed endorsement count overflows"))?;
            proposal.last_endorsement = Some(
                proposal
                    .last_endorsement
                    .map_or(at, |previous| previous.max(at)),
            );
        }
        Ok(())
    }

    /// Return the counting boundary for a proposal or endorsement that needs
    /// a voting limit. None means no additional limit, not that the block is valid.
    pub(crate) fn promise_limit(
        &self,
        batch: &LmdbWriteTransaction<'_>,
        block: &StateBlock,
    ) -> Result<Option<u64>, StoreError> {
        let proposal = match block.block_type {
            BlockType::GovernanceEndorse => self
                .proposals
                .get(block.link.as_bytes())
                .ok_or_else(|| invalid("governance promise has no confirmed proposal"))?
                .clone(),
            BlockType::GovernanceProposal => {
                let content =
                    if block.origin.as_bytes() == &burst_governance::content::FULL_CONTENT_MARKER {
                        crate::governance_content::load(batch, block).map_err(invalid)?
                    } else {
                        ProposalContent::decode_compact(block.link.as_bytes()).map_err(invalid)?
                    };
                let emergency = matches!(content, ProposalContent::Emergency { .. });
                if !emergency && self.params.governance_proposal_endorsements != 0 {
                    return Ok(None);
                }
                Schedule {
                    proposer: block.account.clone(),
                    created_at: block.timestamp.as_secs(),
                    timing: burst_governance::GovernanceTiming::new(&self.params, &content),
                    endorsements: 0,
                    last_endorsement: None,
                    withdrawn_at: None,
                    exploration_started_at: emergency.then_some(block.timestamp.as_secs()),
                }
            }
            _ => return Ok(None),
        };
        let start = match proposal.exploration_started_at {
            Some(at) => Timestamp::new(at),
            None => proposal
                .timing
                .proposal_end(Timestamp::new(proposal.created_at))
                .map_err(invalid)?,
        };
        proposal
            .timing
            .counting_at(start)
            .map(|at| Some(at.as_secs()))
            .map_err(invalid)
    }

    fn limit_bytes(&self) -> Result<Vec<u8>, StoreError> {
        self.snapshot().limit_bytes()
    }

    /// Stage the schedule and its voting limit; the caller commits or aborts.
    pub(crate) fn persist(&self, batch: &mut LmdbWriteTransaction<'_>) -> Result<(), StoreError> {
        batch.write_confirmed_governance(self.snapshot())
    }

    /// Startup verifies the projection against the certified baseline and the
    /// canonical cemented tail. It never repairs a missing or altered cache.
    pub(crate) fn verify_tail(
        self,
        store: &LmdbStore,
        network: NetworkId,
        genesis: BlockHash,
    ) -> Result<(), StoreError> {
        let heads = crate::confirmed_history::ConfirmedHistory::heads_at(store, u64::MAX)?;
        let history = if heads.is_empty() {
            None
        } else {
            Some(crate::confirmed_history::ConfirmedHistory::read(
                store,
                network,
                genesis,
                u64::MAX,
                &heads,
                &[],
            )?)
        };
        let batch = store.tx_begin_write()?;
        self.verify_blocks(
            &batch,
            history
                .as_ref()
                .map_or(&[][..], |history| history.replay_blocks())
                .iter()
                .map(|block| Ok(std::borrow::Cow::Borrowed(block))),
        )
    }

    pub(crate) fn verify_compact_tail(
        self,
        batch: &LmdbWriteTransaction<'_>,
        tail: &crate::confirmed_replay::snapshot::tail::SnapshotTail,
    ) -> Result<(), StoreError> {
        if tail.context().epoch != self.epoch {
            return Err(invalid("governance tail belongs to another epoch"));
        }
        self.verify_blocks(
            batch,
            tail.blocks()
                .map(|block| block.map(std::borrow::Cow::Owned)),
        )
    }

    pub(crate) fn verify_compact_tail_view_with_poll(
        self,
        batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
        tail: &crate::confirmed_replay::snapshot::tail::SnapshotTail,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        if tail.context().epoch != self.epoch {
            return Err(invalid("governance tail belongs to another epoch"));
        }
        self.verify_blocks_read(
            batch,
            tail.blocks()
                .map(|block| block.map(std::borrow::Cow::Owned)),
            || batch.read_confirmed_governance(), poll,
        )
    }

    fn verify_blocks<'a>(
        self,
        batch: &LmdbWriteTransaction<'_>,
        blocks: impl IntoIterator<Item = Result<std::borrow::Cow<'a, StateBlock>, StoreError>>,
    ) -> Result<(), StoreError> {
        self.verify_blocks_read(&batch.read_view(), blocks, || batch.read_confirmed_governance(), &|_| Ok(()))
    }

    fn verify_blocks_read<'a>(
        mut self,
        batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
        blocks: impl IntoIterator<Item = Result<std::borrow::Cow<'a, StateBlock>, StoreError>>,
        snapshot: impl FnOnce() -> Result<Snapshot, StoreError>,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        if batch.payment_epoch()? != self.epoch {
            return Err(invalid("governance epoch changed during startup"));
        }
        let epoch = self.epoch;
        for block in blocks {
            let block = block?;
            poll("metadata:governance-tail-block")?;
            if block.payment_epoch == epoch {
                self.promote_view(batch, &block)?;
            }
        }
        poll("metadata:governance-tail-snapshot")?;
        if snapshot()? != self.snapshot()
            || match batch.get_meta(KEY)? { Some(bytes) => bytes != bincode::serialize(&self.snapshot()).map_err(invalid)?, None => false }
            || batch.get_meta(burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY)? != Some(self.limit_bytes()?)
        {
            return Err(invalid(
                "confirmed governance schedule differs from cemented history",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
