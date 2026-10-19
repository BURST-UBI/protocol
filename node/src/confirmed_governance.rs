//! Voting boundaries derived from cemented governance, never provisional state.

use std::collections::BTreeMap;

use bincode::Options;
use burst_governance::{GovernancePhase, ProposalContent};
use burst_ledger::{BlockType, StateBlock};
use burst_store::StoreError;
use burst_store_lmdb::{LmdbStore, LmdbWriteTransaction};
use burst_types::{BlockHash, NetworkId, ProtocolParams, Timestamp, WalletAddress};
use serde::{Deserialize, Serialize};

pub(crate) const KEY: &str = "confirmed_governance/v2";
const MAX_BYTES: u64 = 16 << 20;
const MAX_PROPOSALS: usize = 65_536;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Schedule {
    proposer: WalletAddress,
    created_at: u64,
    timing: burst_governance::GovernanceTiming,
    endorsements: u32,
    last_endorsement: Option<u64>,
    withdrawn_at: Option<u64>,
    exploration_started_at: Option<u64>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ConfirmedGovernance {
    epoch: BlockHash,
    params: ProtocolParams,
    proposals: BTreeMap<[u8; 32], Schedule>,
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
            proposals: BTreeMap::new(),
        }
    }

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
        if batch
            .get_meta(burst_store::meta::CERTIFIED_PAYMENT_AUTHORITY_KEY)?
            .is_none()
        {
            return Ok(None);
        }
        let bytes = batch
            .get_meta(KEY)?
            .ok_or_else(|| invalid("confirmed governance schedule is missing"))?;
        let result: Self = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(MAX_BYTES)
            .reject_trailing_bytes()
            .deserialize(&bytes)
            .map_err(invalid)?;
        if result.epoch != batch.payment_epoch()? || result.proposals.len() > MAX_PROPOSALS {
            return Err(invalid(
                "confirmed governance schedule has an invalid epoch or size",
            ));
        }
        let expected = result.limit_bytes()?;
        if batch
            .get_meta(burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY)?
            .as_deref()
            != Some(expected.as_slice())
        {
            return Err(invalid(
                "confirmed governance counting limit differs from its schedule",
            ));
        }
        Ok(Some(result))
    }

    pub(crate) fn promote(
        &mut self,
        batch: &LmdbWriteTransaction<'_>,
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
                    crate::governance_content::load(batch, block).map_err(invalid)?
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
        let mut limit: Option<u64> = None;
        for proposal in self.proposals.values().filter(|proposal| {
            proposal.withdrawn_at.is_none()
                && (proposal.exploration_started_at.is_some()
                    || proposal.endorsements > 0
                    || self.params.governance_proposal_endorsements == 0)
        }) {
            let start = match proposal.exploration_started_at {
                Some(start) => start,
                None => proposal
                    .timing
                    .proposal_end(Timestamp::new(proposal.created_at))
                    .map_err(invalid)?
                    .as_secs(),
            };
            let at = proposal
                .timing
                .counting_at(Timestamp::new(start))
                .map_err(invalid)?
                .as_secs();
            limit = Some(limit.map_or(at, |previous| previous.min(at)));
        }
        Ok(burst_store::meta::encode_payment_vote_limit(
            self.epoch, limit,
        ))
    }

    pub(crate) fn persist(&self, batch: &mut LmdbWriteTransaction<'_>) -> Result<(), StoreError> {
        let bytes = bincode::serialize(self).map_err(invalid)?;
        if bytes.len() as u64 > MAX_BYTES || self.proposals.len() > MAX_PROPOSALS {
            return Err(invalid(
                "confirmed governance schedule exceeds storage bounds",
            ));
        }
        let limit = self.limit_bytes()?;
        batch.put_meta(KEY, &bytes)?;
        batch.put_meta(burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY, &limit)
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

    fn verify_blocks<'a>(
        mut self,
        batch: &LmdbWriteTransaction<'_>,
        blocks: impl IntoIterator<Item = Result<std::borrow::Cow<'a, StateBlock>, StoreError>>,
    ) -> Result<(), StoreError> {
        if batch.payment_epoch()? != self.epoch {
            return Err(invalid("governance epoch changed during startup"));
        }
        let epoch = self.epoch;
        for block in blocks {
            let block = block?;
            if block.payment_epoch == epoch {
                self.promote(batch, &block)?;
            }
        }
        if batch.get_meta(KEY)? != Some(bincode::serialize(&self).map_err(invalid)?)
            || batch.get_meta(burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY)?
                != Some(self.limit_bytes()?)
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
