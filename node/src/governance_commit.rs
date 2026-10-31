//! Stage governance effects alongside the block that authorizes them.

use burst_brn::BrnEngine;
use burst_consti::ConstiEngine;
use burst_governance::{
    DelegationEngine, GovernableParam, GovernanceEngine, GovernancePhase, ProposalContent,
};
use burst_ledger::{BlockType, StateBlock};
use burst_store_lmdb::LmdbWriteTransaction;
use burst_trst::TrstEngine;
use burst_types::ProtocolParams;

use crate::block_economics::EconomicResult;

mod activation;
pub(crate) mod key_delegation;
pub(crate) mod ledger;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct CandidateGovernance {
    pub network: burst_types::NetworkId,
    pub genesis: burst_types::BlockHash,
    pub governance: GovernanceEngine,
    pub delegation: DelegationEngine,
    pub constitution: ConstiEngine,
    pub params: ProtocolParams,
}

impl CandidateGovernance {
    pub(crate) fn restore(
        batch: &LmdbWriteTransaction<'_>,
        params: ProtocolParams,
        network: burst_types::NetworkId,
        genesis: burst_types::BlockHash,
    ) -> Result<Self, String> {
        Ok(Self {
            network,
            genesis,
            params,
            governance: crate::governance_storage::load(batch)
                .map_err(|e| e.to_string())?
                .ok_or("missing persisted governance_engine; authenticated replay required")?,
            delegation: batch
                .read_delegation_engine()
                .map_err(|error| format!("invalid persisted delegation: {error}"))?
                .ok_or(
                    "missing persisted delegation_engine_state; authenticated replay required",
                )?,
            constitution: batch
                .read_constitution_engine()
                .map_err(|error| format!("invalid persisted consti_engine: {error}"))?
                .ok_or("missing persisted consti_engine; authenticated replay required")?,
        })
    }

    pub(crate) fn initial(
        params: ProtocolParams,
        network: burst_types::NetworkId,
        genesis: burst_types::BlockHash,
    ) -> Self {
        Self {
            network,
            genesis,
            governance: GovernanceEngine::new(),
            delegation: DelegationEngine::default(),
            constitution: ConstiEngine::bootstrap(),
            params,
        }
    }

    pub(crate) fn persist(&self, batch: &mut LmdbWriteTransaction<'_>) -> Result<(), String> {
        self.persist_auxiliary(batch)?;
        batch
            .put_brn_meta(
                b"governance_engine",
                &bincode::serialize(&self.governance).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())
    }

    pub(crate) fn persist_checkpoint(
        &self,
        batch: &mut LmdbWriteTransaction<'_>,
    ) -> Result<(), String> {
        if batch.uses_governance_records().map_err(|e| e.to_string())? {
            batch
                .replace_checkpoint_governance_records(&self.governance)
                .map_err(|e| e.to_string())?;
            self.persist_auxiliary(batch)
        } else {
            self.persist(batch)
        }
    }

    fn persist_auxiliary(&self, batch: &mut LmdbWriteTransaction<'_>) -> Result<(), String> {
        batch
            .put_meta(DelegationEngine::meta_key(), &self.delegation.save_state())
            .map_err(|e| e.to_string())?;
        batch
            .put_meta(
                "consti_engine",
                &bincode::serialize(&self.constitution).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())
    }

    pub(crate) fn applies_to(kind: &BlockType) -> bool {
        matches!(
            kind,
            BlockType::GovernanceProposal
                | BlockType::GovernanceVote
                | BlockType::GovernanceActivation
                | BlockType::GovernanceEndorse
                | BlockType::GovernanceWithdraw
                | BlockType::GovernanceVeto
                | BlockType::Delegate
                | BlockType::RevokeDelegation
        )
    }

    #[cfg(test)]
    pub(crate) fn apply(
        &mut self,
        batch: &mut LmdbWriteTransaction<'_>,
        block: &StateBlock,
        economic: &EconomicResult,
        brn: &mut BrnEngine,
        trst: &mut TrstEngine,
    ) -> Result<(), String> {
        self.apply_staged(batch, block, economic, &mut (brn, trst))
    }

    pub(crate) fn apply_staged(
        &mut self,
        batch: &mut LmdbWriteTransaction<'_>,
        block: &StateBlock,
        economic: &EconomicResult,
        _engines: &mut impl crate::economic_candidate::Engines,
    ) -> Result<(), String> {
        let before = batch
            .uses_governance_records()
            .map_err(|e| e.to_string())?
            .then(|| self.governance.clone());
        self.apply_to_engines(batch, block, economic)?;
        if let Some(before) = before {
            batch
                .apply_governance_record_delta(&before, &self.governance)
                .map_err(|e| e.to_string())?;
            // Key grants/ballots persist their scoped records in the ledger.
            // Only legacy edge operations mutate the aggregate delegation engine.
            if block.delegation_payload.is_empty()
                && matches!(
                    block.block_type,
                    BlockType::Delegate | BlockType::RevokeDelegation
                )
            {
                batch
                    .put_meta(DelegationEngine::meta_key(), &self.delegation.save_state())
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        } else {
            self.persist(batch)
        }
    }

    /// The caller must discard the ledger and engine candidates on any error.
    /// This transition neither confirms a block nor publishes its candidate.
    pub(crate) fn apply_to(
        &mut self,
        batch: &mut impl ledger::Ledger,
        block: &StateBlock,
        economic: &EconomicResult,
        _brn: &mut BrnEngine,
        _trst: &mut TrstEngine,
    ) -> Result<(), String> {
        self.apply_to_engines(batch, block, economic)
    }

    fn apply_to_engines(
        &mut self,
        batch: &mut impl ledger::Ledger,
        block: &StateBlock,
        economic: &EconomicResult,
    ) -> Result<(), String> {
        if !block.delegation_payload.is_empty() {
            return self.apply_key_delegation(batch, block);
        }
        match block.block_type {
            BlockType::GovernanceVeto => {
                let bootstrap = batch.bootstrap_phase()?;
                if block.account != crate::genesis_key::genesis_address(self.network)
                    || !batch.get_account(&block.account).map_err(|e| e.to_string())?.state.is_verified()
                    || bootstrap == burst_verification::bootstrap::BootstrapPhase::Complete
                    || !bootstrap.permits_creator(
                        batch.verified_account_count().map_err(|e| e.to_string())?,
                        self.params.bootstrap_exit_threshold)
                {
                    return Err("creator veto authority has ended or signer is not the verified creator".into());
                }
                self.governance.veto_approved(&block.link.into_tx_hash(), block.timestamp)
                    .map_err(|e| e.to_string())?;
            }

            BlockType::GovernanceEndorse | BlockType::GovernanceWithdraw => {
                let proposal_hash = block.link.into_tx_hash();
                let proposal = self
                    .governance
                    .get_proposal(&proposal_hash)
                    .ok_or("governance proposal not found")?;
                if proposal.phase != GovernancePhase::Proposal
                    || block.timestamp < proposal.created_at
                    || block.timestamp
                        >= self
                            .governance
                            .timing(proposal, &self.params)
                            .and_then(|timing| timing.proposal_end(proposal.created_at))
                            .map_err(|error| error.to_string())?
                {
                    return Err("proposal support window is closed".into());
                }
                if block.block_type == BlockType::GovernanceWithdraw {
                    self.governance
                        .withdraw(&proposal_hash, &block.account)
                        .map_err(|e| e.to_string())?;
                } else {
                    let account = batch
                        .get_account(&block.account)
                        .map_err(|e| e.to_string())?;
                    if !account.state.is_verified() {
                        return Err("governance endorsement requires a verified wallet".into());
                    }
                    let key = format!("governance_endorsement/{proposal_hash}/{}", block.account);
                    if batch.get_meta(&key).map_err(|e| e.to_string())?.is_some() {
                        return Err("wallet already endorsed this proposal".into());
                    }
                    let previous: StateBlock = bincode::deserialize(
                        &batch
                            .get_block(&block.previous)
                            .map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?;
                    let amount = block
                        .brn_spent
                        .checked_sub(previous.brn_spent)
                        .ok_or("governance endorsement spend decreased")?;
                    if amount == 0 || amount != self.params.governance_proposal_cost {
                        return Err("incorrect governance endorsement burn amount".into());
                    }
                    if !matches!(economic, EconomicResult::GovernanceEndorse { burn_amount }
                        if *burn_amount == amount)
                    {
                        return Err("governance endorsement economic burn mismatch".into());
                    }
                    self.governance
                        .endorse_proposal(&proposal_hash, amount)
                        .map_err(|e| e.to_string())?;
                    batch
                        .put_meta(&key, block.hash.as_bytes())
                        .map_err(|e| e.to_string())?;
                }
            }
            BlockType::Delegate => {
                if !batch
                    .get_account(&block.account)
                    .map_err(|e| e.to_string())?
                    .state
                    .is_verified()
                {
                    return Err("delegation requires a verified wallet".into());
                }
                let target = crate::block_economics::wallet_from_link(&block.link)
                    .ok_or("delegation target is invalid")?;
                self.delegation
                    .delegate(&block.account, &target)
                    .map_err(|e| e.to_string())?;
            }
            BlockType::RevokeDelegation => self.delegation.undelegate(&block.account),
            BlockType::GovernanceProposal => {
                let EconomicResult::GovernanceProposal {
                    proposer,
                    proposal_hash,
                    content,
                } = economic
                else {
                    return Err("governance proposal payload missing".into());
                };
                let content =
                    if block.origin.as_bytes() == &burst_governance::content::FULL_CONTENT_MARKER {
                        batch.full_content(block)?
                    } else {
                        content
                            .clone()
                            .ok_or("governance proposal content is invalid or unsupported")?
                    };
                if matches!(content, ProposalContent::Emergency { .. })
                    && !crate::governance_content::enabled(self.network)
                {
                    return Err("emergency governance is behind the release gate".into());
                }
                if *proposal_hash != block.hash.into_tx_hash()
                    || self.governance.get_proposal(proposal_hash).is_some()
                {
                    return Err(
                        "governance proposal id does not match its block or already exists".into(),
                    );
                }
                let account = batch.get_account(proposer).map_err(|e| e.to_string())?;
                let total_eligible_voters =
                    u32::try_from(batch.verified_account_count().map_err(|e| e.to_string())?)
                        .map_err(|_| "governance electorate exceeds supported size")?;
                let proposal = burst_governance::proposal::Proposal {
                    hash: *proposal_hash,
                    proposer: proposer.clone(),
                    content,
                    phase: GovernancePhase::Proposal,
                    created_at: block.timestamp,
                    endorsement_count: 0,
                    exploration_votes_yea: 0,
                    exploration_votes_nay: 0,
                    exploration_votes_abstain: 0,
                    promotion_votes_yea: 0,
                    promotion_votes_nay: 0,
                    promotion_votes_abstain: 0,
                    exploration_started_at: None,
                    cooldown_started_at: None,
                    promotion_started_at: None,
                    activation_at: None,
                    total_eligible_voters,
                    round: 0,
                };
                if let Some(amendment) =
                    activation::constitutional_amendment(&proposal, block.timestamp)
                {
                    self.constitution
                        .apply_amendment_to_document(
                            &amendment,
                            &mut self.constitution.get_constitution().clone(),
                        )
                        .map_err(|error| error.to_string())?;
                }
                self.governance
                    .submit_proposal(proposal, 0, account.state.is_verified(), &self.params)
                    .map_err(|e| e.to_string())?;
            }
            BlockType::GovernanceVote => {
                let EconomicResult::GovernanceVote {
                    voter,
                    proposal_hash,
                    vote,
                } = economic
                else {
                    return Err("governance vote encoding is invalid".into());
                };
                if !batch
                    .get_account(voter)
                    .map_err(|e| e.to_string())?
                    .state
                    .can_vote()
                {
                    return Err("governance requires a verified voter".into());
                }
                // A scheduler tick is not a prerequisite for replaying a vote.
                if let Some(proposal) = self.governance.get_proposal(proposal_hash) {
                    if proposal.phase == GovernancePhase::Proposal {
                        let mut next = proposal.clone();
                        self.governance
                            .try_advance(&mut next, block.timestamp, &self.params, None)
                            .map_err(|e| e.to_string())?;
                        *self
                            .governance
                            .get_proposal_mut(proposal_hash)
                            .ok_or("proposal missing")? = next;
                    }
                }
                self.governance
                    .cast_exploration_vote(
                        proposal_hash,
                        voter,
                        *vote,
                        block.timestamp,
                        &self.params,
                    )
                    .map_err(|e| e.to_string())?;
            }
            BlockType::GovernanceActivation => {
                return Err("governance activation requires its certified boundary".into());
            }
            _ => return Err("block has no governance transition".into()),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::BrnStore;
    use burst_store::{account::AccountStore, block::BlockStore, meta::MetaStore};
    use burst_store_lmdb::LmdbStore;
    use burst_types::{BlockHash, NetworkId, Signature, Timestamp, TxHash};

    mod bootstrap_exit;
    mod key_fork_dependencies;
    mod scoped_undo;
    mod pagination;

    fn candidate() -> CandidateGovernance {
        CandidateGovernance {
            network: NetworkId::Dev,
            genesis: crate::genesis_state::block(NetworkId::Dev, &ProtocolParams::default()).hash,
            governance: GovernanceEngine::new(),
            delegation: DelegationEngine::default(),
            constitution: ConstiEngine::new(),
            params: ProtocolParams::default(),
        }
    }

    fn block(kind: BlockType) -> StateBlock {
        let account = crate::genesis_key::genesis_address(NetworkId::Dev);
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: burst_ledger::CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: kind,
            account: account.clone(),
            previous: BlockHash::ZERO,
            representative: account,
            brn_spent: 0,
            trst_balance: 0,
            link: BlockHash::new(burst_crypto::keypair_from_seed(&[41; 32]).public.0),
            origin: TxHash::ZERO,
            transaction: TxHash::new([7; 32]),
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: vec![],
            release_sources: Vec::new(),
            timestamp: Timestamp::new(100),
            params_hash: BlockHash::ZERO,
            work: 0,
            signature: Signature([0; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block
    }

    #[test]
    fn governance_restore_reads_the_writer_and_rejects_incomplete_snapshots() {
        use burst_store_lmdb::block_undo::UndoTable;
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut state = candidate();
        let restore = |batch: &LmdbWriteTransaction<'_>| {
            CandidateGovernance::restore(
                batch,
                ProtocolParams::default(),
                NetworkId::Dev,
                state.genesis,
            )
        };
        assert!(restore(&store.tx_begin_write().unwrap()).is_err());
        let mut batch = store.tx_begin_write().unwrap();
        state.persist(&mut batch).unwrap();
        batch.commit().unwrap();
        let from = crate::genesis_key::genesis_address(NetworkId::Dev);
        let to = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[41; 32]).public);
        state.delegation.delegate(&from, &to).unwrap();
        {
            let mut batch = store.tx_begin_write().unwrap();
            state.persist(&mut batch).unwrap();
            assert_eq!(
                restore(&batch).unwrap().delegation.get_delegate(&from),
                Some(&to)
            );
        }
        assert!(restore(&store.tx_begin_write().unwrap())
            .unwrap()
            .delegation
            .get_delegate(&from)
            .is_none());
        for (table, key) in [
            (UndoTable::BrnMeta, b"governance_engine".as_slice()),
            (UndoTable::Meta, DelegationEngine::meta_key().as_bytes()),
            (UndoTable::Meta, b"consti_engine".as_slice()),
        ] {
            let mut batch = store.tx_begin_write().unwrap();
            let mut bytes = batch.undo_value(table, key).unwrap().unwrap();
            bytes.push(0);
            if table == UndoTable::BrnMeta {
                batch.put_brn_meta(key, &bytes).unwrap();
            } else {
                batch
                    .put_meta(std::str::from_utf8(key).unwrap(), &bytes)
                    .unwrap();
            }
            assert!(restore(&batch).is_err());
        }
        let mut batch = store.tx_begin_write().unwrap();
        batch.delete_meta("consti_engine").unwrap();
        assert!(restore(&batch).is_err());
    }

    #[test]
    fn delegation_and_block_share_commit_and_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let block = block(BlockType::Delegate);
        let account =
            crate::genesis_state::account_to_write(None, block.account.clone(), block.hash)
                .unwrap();
        for commit in [false, true] {
            let mut state = candidate();
            let brn = BrnEngine::new();
            let trst = TrstEngine::new();
            let mut batch = store.tx_begin_write().unwrap();
            crate::ledger_updater::stage_block(&mut batch, &block, Some(&account), 0, None, None)
                .unwrap();
            let mut engines = crate::economic_candidate::StagedEngines::new(&brn, &trst);
            state
                .apply_staged(
                    &mut batch,
                    &block,
                    &EconomicResult::NoEconomicEffect,
                    &mut engines,
                )
                .unwrap();
            let (brn_candidate, trst_candidate) = engines.into_staged();
            assert!(brn_candidate.is_none() && trst_candidate.is_none());
            if commit {
                batch.commit().unwrap();
            } else {
                drop(batch);
            }
            assert_eq!(store.block_store().get_block(&block.hash).is_ok(), commit);
            assert_eq!(
                store
                    .meta_store()
                    .get_meta(DelegationEngine::meta_key())
                    .is_ok(),
                commit
            );
        }
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let restored = DelegationEngine::load_state(
            &store
                .meta_store()
                .get_meta(DelegationEngine::meta_key())
                .unwrap(),
        )
        .unwrap();
        let target = crate::block_economics::wallet_from_link(&block.link).unwrap();
        assert_eq!(restored.voting_power(&target), 2);
    }

    #[test]
    fn proposal_submission_does_not_require_a_computable_brn_balance() {
        for rate in [0, u128::MAX] {
            let dir = tempfile::tempdir().unwrap();
            let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
            let mut state = candidate();
            let proposal = block(BlockType::GovernanceProposal);
            let account = crate::genesis_state::account_to_write(
                None,
                proposal.account.clone(),
                proposal.hash,
            )
            .unwrap();
            let mut brn = BrnEngine::with_rate(rate, Timestamp::EPOCH);
            brn.track_wallet(
                proposal.account.clone(),
                burst_brn::BrnWalletState::new(Timestamp::EPOCH),
            );
            let before = bincode::serialize(&(
                brn.get_wallet(&proposal.account),
                &brn.rate_history,
                brn.next_stake_id(),
            ))
            .unwrap();
            let trst = TrstEngine::new();
            let mut batch = store.tx_begin_write().unwrap();
            crate::ledger_updater::stage_block(
                &mut batch,
                &proposal,
                Some(&account),
                0,
                None,
                None,
            )
            .unwrap();
            let mut engines = crate::economic_candidate::StagedEngines::new(&brn, &trst);
            state
                .apply_staged(
                    &mut batch,
                    &proposal,
                    &EconomicResult::GovernanceProposal {
                        proposer: proposal.account.clone(),
                        proposal_hash: proposal.hash.into_tx_hash(),
                        content: Some(ProposalContent::ParameterChange {
                            param: GovernableParam::BrnRate,
                            new_value: 2,
                        }),
                    },
                    &mut engines,
                )
                .unwrap();
            let mut withdrawal = block(BlockType::GovernanceWithdraw);
            withdrawal.link = proposal.hash;
            let mut withdrawn = state.clone();
            withdrawn
                .apply_staged(
                    &mut batch,
                    &withdrawal,
                    &EconomicResult::NoEconomicEffect,
                    &mut engines,
                )
                .unwrap();
            assert_eq!(
                withdrawn
                    .governance
                    .get_proposal(&proposal.hash.into_tx_hash())
                    .unwrap()
                    .phase,
                GovernancePhase::Withdrawn,
            );
            let saved = state
                .governance
                .get_proposal_mut(&proposal.hash.into_tx_hash())
                .unwrap();
            saved.phase = GovernancePhase::Exploration;
            saved.exploration_started_at = Some(proposal.timestamp);
            let vote = block(BlockType::GovernanceVote);
            state
                .apply_staged(
                    &mut batch,
                    &vote,
                    &EconomicResult::GovernanceVote {
                        voter: proposal.account.clone(),
                        proposal_hash: proposal.hash.into_tx_hash(),
                        vote: burst_transactions::governance::GovernanceVote::Yea,
                    },
                    &mut engines,
                )
                .unwrap();
            let (brn_candidate, trst_candidate) = engines.into_staged();
            assert!(brn_candidate.is_none() && trst_candidate.is_none());
            assert!(state
                .governance
                .get_proposal(&proposal.hash.into_tx_hash())
                .is_some());
            assert_eq!(
                bincode::serialize(&(
                    brn.get_wallet(&proposal.account),
                    &brn.rate_history,
                    brn.next_stake_id(),
                ))
                .unwrap(),
                before
            );
        }
    }

    #[test]
    fn malformed_proposal_and_vote_do_not_publish_a_block() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        for kind in [BlockType::GovernanceProposal, BlockType::GovernanceVote] {
            let block = block(kind);
            let mut state = candidate();
            let mut brn = BrnEngine::new();
            let mut trst = TrstEngine::new();
            let mut batch = store.tx_begin_write().unwrap();
            crate::ledger_updater::stage_block(&mut batch, &block, None, 0, None, None).unwrap();
            let result = EconomicResult::GovernanceProposal {
                proposer: block.account.clone(),
                proposal_hash: block.transaction,
                content: None,
            };
            assert!(state
                .apply(&mut batch, &block, &result, &mut brn, &mut trst)
                .is_err());
            assert_eq!(state.governance.all_proposals().count(), 0);
            drop(batch);
            assert!(store.block_store().get_block(&block.hash).is_err());
        }
    }

    #[test]
    fn activation_requires_approved_phase_and_cannot_repeat() {
        activation_persistence_case(false, false);
    }

    #[test]
    fn keyed_parameter_activation_preserves_auxiliary_snapshots() {
        activation_persistence_case(true, false);
    }

    #[test]
    fn keyed_constitution_activation_persists_only_changed_auxiliary_state() {
        activation_persistence_case(true, true);
    }

    fn activation_persistence_case(keyed: bool, constitutional: bool) {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let mut state = candidate();
        let block = block(BlockType::GovernanceActivation);
        let id = TxHash::new([6; 32]);
        state
            .governance
            .submit_proposal(
                burst_governance::proposal::Proposal {
                    hash: id,
                    proposer: block.account.clone(),
                    content: if constitutional {
                        ProposalContent::ConstitutionalAmendment {
                            title: "Fixture amendment".into(),
                            text: "Preserve the approved constitutional change.".into(),
                        }
                    } else {
                        ProposalContent::ParameterChange {
                            param: GovernableParam::BrnRate,
                            new_value: 123,
                        }
                    },
                    phase: GovernancePhase::Proposal,
                    created_at: Timestamp::new(1),
                    endorsement_count: 0,
                    exploration_votes_yea: 0,
                    exploration_votes_nay: 0,
                    exploration_votes_abstain: 0,
                    promotion_votes_yea: 0,
                    promotion_votes_nay: 0,
                    promotion_votes_abstain: 0,
                    exploration_started_at: None,
                    cooldown_started_at: None,
                    promotion_started_at: None,
                    activation_at: Some(Timestamp::new(block.timestamp.as_secs() - 10)),
                    total_eligible_voters: 1,
                    round: 0,
                },
                u128::MAX,
                true,
                &state.params,
            )
            .unwrap();
        let proposed = state.governance.get_proposal(&id).unwrap();
        let new_params_hash = state
            .governance
            .preview_activation(proposed, &state.params)
            .unwrap()
            .params_hash();
        let economic = EconomicResult::GovernanceActivation {
            proposal_hash: id,
            new_params_hash,
        };
        let mut brn = BrnEngine::new();
        let mut trst = TrstEngine::new();
        let mut batch = store.tx_begin_write().unwrap();
        assert!(state
            .apply(&mut batch, &block, &economic, &mut brn, &mut trst)
            .is_err());
        crate::bootstrap_phase::initialize(&mut batch, &state.params).unwrap();
        state.governance.get_proposal_mut(&id).unwrap().phase = GovernancePhase::Activation;
        if keyed {
            batch
                .initialize_governance_records(&state.governance)
                .unwrap();
            state.persist(&mut batch).unwrap();
        }
        let prior_delegation = batch.get_meta(DelegationEngine::meta_key()).unwrap();
        let prior_constitution = batch.get_meta("consti_engine").unwrap();
        let prior_params = state.params.clone();
        assert_eq!(
            state.governance.schedule_activations(
                block.timestamp,
                &state.params,
                &state.delegation,
                std::slice::from_ref(&block.account)
            ),
            vec![id]
        );
        assert!(state.governance.drain_pending_changes().is_empty());
        assert_eq!(state.params.params_hash(), prior_params.params_hash());
        state.activate_approved(&mut batch, id, &new_params_hash, block.timestamp,
            &mut brn, &mut trst).unwrap();
        state.persist_checkpoint(&mut batch).unwrap();
        assert_eq!(state.params.params_hash(), new_params_hash);
        if !constitutional {
            assert_eq!(brn.rate_history.current_rate(), 123);
            assert_eq!(
                brn.rate_history.segments().last().unwrap().start,
                Timestamp::new(block.timestamp.as_secs() - 10)
            );
        }
        if keyed {
            assert_eq!(
                batch.get_meta(DelegationEngine::meta_key()).unwrap(),
                prior_delegation
            );
            let persisted = batch.get_meta("consti_engine").unwrap();
            if constitutional {
                assert_ne!(persisted, prior_constitution);
                assert_eq!(
                    bincode::deserialize::<ConstiEngine>(persisted.as_ref().unwrap()).unwrap(),
                    state.constitution
                );
            } else {
                assert_eq!(persisted, prior_constitution);
            }
        }
        assert!(state
            .apply(&mut batch, &block, &economic, &mut brn, &mut trst)
            .is_err());
        batch.commit().unwrap();
        if keyed {
            drop(store);
            let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
            let restored = CandidateGovernance::restore(
                &store.tx_begin_write().unwrap(),
                state.params.clone(),
                state.network,
                state.genesis,
            )
            .unwrap();
            assert_eq!(restored.constitution, state.constitution);
            assert_eq!(restored.governance, state.governance);
        }
    }

    #[test]
    fn governance_endorsement_burn_and_unique_signer_commit_together() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let mut state = candidate();
        state.params.governance_proposal_cost = 10;
        state.params.min_work_difficulty = 0;
        let key = burst_crypto::keypair_from_seed(&[41; 32]);
        let sign = |mut block: StateBlock| {
            block.hash = block.compute_hash();
            burst_wallet_core::transaction_builder::sign_state_block(block, &key.private)
        };
        let mut opening = block(BlockType::Open);
        opening.account = burst_crypto::derive_address(&key.public);
        opening.representative = opening.account.clone();
        opening.link = BlockHash::ZERO;
        opening.transaction = TxHash::ZERO;
        opening.timestamp = Timestamp::EPOCH;
        opening.params_hash = state.params.params_hash();
        let opening = sign(opening);
        let mut proposal = block(BlockType::GovernanceProposal);
        proposal.account = opening.account.clone();
        proposal.representative = opening.representative.clone();
        proposal.previous = opening.hash;
        proposal.params_hash = state.params.params_hash();
        let proposal = sign(proposal);
        let account =
            crate::genesis_state::account_to_write(None, opening.account.clone(), opening.hash)
                .unwrap();
        let mut brn = BrnEngine::with_rate(1, Timestamp::EPOCH);
        brn.wallets.insert(
            proposal.account.clone(),
            burst_brn::BrnWalletState::new(Timestamp::EPOCH),
        );
        let mut trst = TrstEngine::new();
        let mut batch = store.tx_begin_write().unwrap();
        crate::payment_epoch::initialize(&mut batch, opening.payment_epoch).unwrap();
        batch.put_account_info(&account, false).unwrap();
        batch.put_block_with_account(&opening.hash, &bincode::serialize(&opening).unwrap(),
            &opening.account, 1).unwrap();
        batch.put_frontier(&opening.account, &opening.hash).unwrap();
        crate::ledger_updater::persist_economics(&mut batch, &brn, &trst).unwrap();
        crate::graph_storage::stage_replayed(&mut batch, &trst.merger_graph).unwrap();
        crate::trst_storage::stage_replayed_stable(&mut batch, &trst).unwrap();
        state.persist(&mut batch).unwrap();
        assert!(batch.migrate_governance_records().unwrap());
        CandidateGovernance::restore(&batch, state.params.clone(), state.network, state.genesis).unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        crate::ledger_updater::stage_block(&mut batch, &proposal, Some(&account), 0, None, None)
            .unwrap();
        state
            .apply(
                &mut batch,
                &proposal,
                &EconomicResult::GovernanceProposal {
                    proposer: proposal.account.clone(),
                    proposal_hash: proposal.hash.into_tx_hash(),
                    content: Some(ProposalContent::ParameterChange {
                        param: GovernableParam::BrnRate,
                        new_value: 2,
                    }),
                },
                &mut brn,
                &mut trst,
            )
            .unwrap();
        crate::ledger_updater::persist_economics(&mut batch, &brn, &trst).unwrap();
        batch.commit().unwrap();

        let mut support = block(BlockType::GovernanceEndorse);
        support.account = proposal.account.clone();
        support.representative = proposal.representative.clone();
        support.params_hash = state.params.params_hash();
        support.previous = proposal.hash;
        support.link = proposal.hash;
        support.brn_spent = 10;
        let support = sign(support);
        crate::payment_transition::authenticate_signed_contents(&support, &state.params).unwrap();
        let marker = format!(
            "governance_endorsement/{}/{}",
            proposal.hash.into_tx_hash(),
            proposal.account
        );
        use crate::economic_candidate::{prepared::CapturedPayment, Engines, StagedEngines};
        let observe = |batch: &LmdbWriteTransaction<'_>| {
            let economics = crate::payment_transition::restore(batch, &state.params).unwrap();
            (bincode::serialize(economics.brn.get_wallet(&support.account).unwrap()).unwrap(),
                bincode::serialize(&batch.get_account(&support.account).unwrap()).unwrap(),
                bincode::serialize(&batch.read_governance_records().unwrap()).unwrap(),
                batch.get_meta(&marker).unwrap())
        };
        let baseline = observe(&store.tx_begin_write().unwrap());
        // The proposal can close while actor-local work is being prepared.
        let batch = store.tx_begin_write().unwrap();
        let captured = CapturedPayment::capture(&batch, &support, &state.params).unwrap().unwrap();
        let before = crate::payment_transition::restore(&batch, &state.params).unwrap();
        batch.commit().unwrap();
        let prepared = captured.prepare().unwrap();
        let mut withdrawn = state.clone();
        withdrawn.governance.withdraw(&proposal.hash.into_tx_hash(), &proposal.account).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.apply_governance_record_delta(&state.governance, &withdrawn.governance).unwrap();
        let closed = observe(&batch);
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let prepared = prepared.stage_with_economics(&mut batch, &state.params).unwrap().unwrap();
        let mut engines = StagedEngines::new(&before.brn, &before.trst);
        engines.adopt_persisted(prepared.state.brn, prepared.state.trst);
        let mut current = CandidateGovernance::restore(&batch, state.params.clone(), state.network, state.genesis).unwrap();
        assert!(current.apply_staged(&mut batch, &support, &prepared.economics.effect,
            &mut engines).unwrap_err().contains("support window is closed"));
        drop(batch);
        assert_eq!(observe(&store.tx_begin_write().unwrap()), closed);
        assert!(store.block_store().get_block(&support.hash).is_err());
        let mut batch = store.tx_begin_write().unwrap();
        batch.apply_governance_record_delta(&withdrawn.governance, &state.governance).unwrap();
        batch.commit().unwrap();
        assert_eq!(observe(&store.tx_begin_write().unwrap()), baseline);
        let mut expected = None;
        for (prepared, commit) in [(false, false), (true, false), (true, true)] {
            let mut staged = state.clone();
            let batch = store.tx_begin_write().unwrap();
            let before = crate::payment_transition::restore(&batch, &state.params).unwrap();
            let captured = prepared.then(|| CapturedPayment::capture(&batch, &support,
                &state.params).unwrap().unwrap());
            batch.commit().unwrap();
            let candidate = captured.map(|capture| capture.prepare().unwrap());
            let mut batch = store.tx_begin_write().unwrap();
            let mut engines = StagedEngines::new(&before.brn, &before.trst);
            let economic = if let Some(candidate) = candidate {
                let prepared = candidate.stage_with_economics(&mut batch, &state.params).unwrap().unwrap();
                engines.adopt_persisted(prepared.state.brn, prepared.state.trst);
                prepared.economics
            } else {
                crate::economic_candidate::stage(&mut batch, &support, &state.params,
                    &mut engines).unwrap()
            };
            for invalid in [EconomicResult::NoEconomicEffect,
                EconomicResult::GovernanceEndorse { burn_amount: 9 }] {
                assert!(staged.clone().apply_staged(&mut batch, &support, &invalid,
                    &mut engines).unwrap_err().contains("economic burn mismatch"));
                assert!(batch.get_meta(&marker).unwrap().is_none());
            }
            staged.apply_staged(&mut batch, &support, &economic.effect, &mut engines).unwrap();
            engines.persist(&mut batch).unwrap();
            assert_eq!(engines.brn().get_wallet(&support.account).unwrap().total_burned, 10);
            let observed = observe(&batch);
            if let Some(expected) = &expected { assert_eq!(&observed, expected); }
            else { expected = Some(observed); }
            if commit {
                batch.commit().unwrap();
            } else {
                drop(batch);
            }
            assert_eq!(observe(&store.tx_begin_write().unwrap()),
                if commit { expected.clone().unwrap() } else { baseline.clone() });
            assert_eq!(store.block_store().get_block(&support.hash).is_ok(), commit);
            assert_eq!(store.meta_store().get_meta(&marker).is_ok(), commit);
            assert_eq!(
                store
                    .account_store()
                    .get_account(&proposal.account)
                    .unwrap()
                    .total_brn_burned,
                if commit { 10 } else { 0 }
            );
            let persisted = BrnEngine::load_from_store(&store.brn_store()).unwrap();
            assert_eq!(
                persisted.wallets[&proposal.account].total_burned,
                if commit { 10 } else { 0 }
            );
        }
        assert_eq!(observe(&store.tx_begin_write().unwrap()), expected.clone().unwrap());
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        state.governance = store.tx_begin_write().unwrap().read_governance_records().unwrap();
        assert_eq!(state.governance.get_endorsements(&proposal.hash.into_tx_hash()), Some((1, 10)));
        let mut repeated = support.clone();
        repeated.previous = support.hash;
        repeated.brn_spent = 20;
        repeated.hash = repeated.compute_hash();
        let mut batch = store.tx_begin_write().unwrap();
        assert!(state
            .apply(
                &mut batch,
                &repeated,
                &EconomicResult::NoEconomicEffect,
                &mut brn,
                &mut trst
            )
            .unwrap_err()
            .contains("already endorsed"));
        assert_eq!(brn.wallets[&proposal.account].total_burned, 0);
        drop(batch);
        let mut batch = store.tx_begin_write().unwrap();
        batch.rewind_block(&support.hash).unwrap();
        let restored = crate::payment_transition::restore(&batch, &state.params).unwrap();
        assert_eq!(bincode::serialize(restored.brn.get_wallet(&support.account).unwrap()).unwrap(), baseline.0);
        assert_eq!(bincode::serialize(&batch.get_account(&support.account).unwrap()).unwrap(), baseline.1);
        assert_eq!(bincode::serialize(&batch.read_governance_records().unwrap()).unwrap(), baseline.2);
        assert_eq!(batch.get_meta(&marker).unwrap(), baseline.3);
        batch.commit().unwrap();
    }
}
