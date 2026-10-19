//! Test the shared transition without snapshots, timers or a database writer.
//! This adapter supplies no checkpoint authority or global event ordering.

use super::*;
use crate::{confirmed_history::ConfirmedHistory, governance_commit::CandidateGovernance};
use burst_brn::{BrnEngine, BrnWalletState};
use burst_governance::{DelegationEngine, GovernanceEngine};
use burst_trst::TrstEngine;
use burst_types::Timestamp;
use burst_verification::bootstrap::BootstrapPhase;
use std::collections::HashMap;

#[derive(Clone)]
pub(crate) struct ReplayedLedger {
    pub accounts: HashMap<WalletAddress, AccountInfo>,
    pub applied: HashMap<BlockHash, StateBlock>,
    pub metadata: HashMap<String, Vec<u8>>,
    pub contents: HashMap<BlockHash, Vec<u8>>,
    pub bootstrap: BootstrapPhase,
}

impl Ledger for ReplayedLedger {
    fn get_account(&self, address: &WalletAddress) -> Result<AccountInfo, StoreError> {
        self.accounts
            .get(address)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(address.to_string()))
    }
    fn put_account(&mut self, account: &AccountInfo) -> Result<(), StoreError> {
        self.accounts
            .insert(account.address.clone(), account.clone());
        Ok(())
    }
    fn get_block(&self, hash: &BlockHash) -> Result<Vec<u8>, StoreError> {
        bincode::serialize(
            self.applied
                .get(hash)
                .ok_or_else(|| StoreError::NotFound(hash.to_string()))?,
        )
        .map_err(|error| StoreError::Serialization(error.to_string()))
    }
    fn get_meta(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.metadata.get(key).cloned())
    }
    fn put_meta(&mut self, key: &str, bytes: &[u8]) -> Result<(), StoreError> {
        self.metadata.insert(key.into(), bytes.into());
        Ok(())
    }
    fn verified_accounts(&self) -> Result<Vec<WalletAddress>, StoreError> {
        Ok(self
            .accounts
            .values()
            .filter(|account| account.state.is_verified())
            .map(|account| account.address.clone())
            .collect())
    }
    fn verified_account_count(&self) -> Result<u64, StoreError> {
        Ok(self.verified_accounts()?.len() as u64)
    }
    fn full_content(&self, block: &StateBlock) -> Result<ProposalContent, String> {
        let bytes = self
            .contents
            .get(&block.hash)
            .ok_or("replayed proposal body missing")?;
        crate::governance_content::authenticate(block)?
            .decode_verified(bytes)
            .map_err(|error| error.to_string())
    }
    fn observe_bootstrap(&mut self, params: &ProtocolParams) -> Result<(), String> {
        self.bootstrap.observe(
            self.verified_account_count()
                .map_err(|error| error.to_string())?,
            params.bootstrap_exit_threshold,
        );
        Ok(())
    }
}

/// Replay the live test's single-account transcript, not an arbitrary DAG.
/// Multi-human voting cuts, delegation timing and cross-proposal activation
/// order require certified boundaries before this can become node replay.
pub(crate) fn genesis_transcript(
    history: &ConfirmedHistory,
    params: &ProtocolParams,
) -> CandidateGovernance {
    let genesis = crate::genesis_state::block(history.network(), params);
    assert_eq!(genesis.hash, history.genesis());
    let account =
        crate::genesis_state::account_to_write(None, genesis.account.clone(), genesis.hash)
            .unwrap();
    let mut ledger = ReplayedLedger {
        accounts: HashMap::from([(genesis.account.clone(), account)]),
        applied: HashMap::from([(genesis.hash, genesis.clone())]),
        metadata: HashMap::new(),
        contents: history
            .proposal_witnesses()
            .iter()
            .map(|witness| (witness.block, witness.bytes.clone()))
            .collect(),
        bootstrap: BootstrapPhase::at_genesis(params.bootstrap_exit_threshold),
    };
    let mut state = CandidateGovernance {
        network: history.network(),
        genesis: history.genesis(),
        governance: GovernanceEngine::new(),
        delegation: DelegationEngine::default(),
        constitution: burst_consti::ConstiEngine::bootstrap(),
        params: params.clone(),
    };
    let mut brn = BrnEngine::with_rate(params.brn_rate, Timestamp::EPOCH);
    brn.track_wallet(
        genesis.account.clone(),
        BrnWalletState::new(Timestamp::EPOCH),
    );
    let mut trst = TrstEngine::with_expiry(params.trst_expiry_secs);
    let mut prepared_ballots = HashMap::new();
    for block in history
        .replay_blocks()
        .iter()
        .filter(|block| block.hash != genesis.hash)
    {
        assert_eq!(
            block.account, genesis.account,
            "this fixture proves only one account's ordering"
        );
        assert!(CandidateGovernance::applies_to(&block.block_type));
        assert_eq!(block.payment_epoch, genesis.hash);
        assert_eq!(block.params_hash, state.params.params_hash());
        assert_eq!(block.hash, block.compute_hash());
        assert!(burst_crypto::verify_signature(
            block.hash.as_bytes(),
            &block.signature,
            &burst_crypto::keypair_from_seed(&[0; 32]).public
        ));
        let previous = ledger.applied.get(&block.previous).unwrap();
        let account = ledger.accounts.get(&block.account).unwrap();
        assert_eq!(account.head, block.previous);
        crate::block_processor::BlockProcessor::validate_balance_transition(
            block,
            previous.brn_balance,
            previous.trst_balance,
        )
        .unwrap();
        let economics = crate::economic_candidate::apply(
            block,
            crate::economic_candidate::EconomicContext {
                account: Some(account),
                previous_brn: previous.brn_balance,
                pending: None,
                params: &state.params,
            },
            &mut brn,
            &mut trst,
        )
        .unwrap();
        let updated =
            crate::ledger_updater::account_after_block(block, Some(account), previous.brn_balance)
                .unwrap();
        ledger.accounts.insert(block.account.clone(), updated);
        state
            .apply_to(&mut ledger, block, &economics.effect, &mut brn, &mut trst)
            .unwrap();
        ledger.applied.insert(block.hash, block.clone());
        if block.block_type == burst_ledger::BlockType::GovernanceVote {
            let id = block.link.into_tx_hash();
            let counting = state
                .governance
                .counting_time(state.governance.get_proposal(&id).unwrap(), &state.params)
                .unwrap();
            let head = crate::confirmed_history::HistoryHead {
                account: block.account.clone(),
                height: ledger.accounts[&block.account].block_count,
                hash: block.hash,
            };
            let cut = history.prefix(counting.as_secs(), &[head], &[]).unwrap();
            let prepared = state
                .governance
                .prepare_ballot_cut(
                    &id,
                    cut.commitment(),
                    counting,
                    &state.params,
                    &state.delegation,
                    &ledger.verified_accounts().unwrap(),
                )
                .unwrap();
            assert_eq!(
                state.governance.get_proposal(&id).unwrap().phase,
                burst_governance::GovernancePhase::Exploration
            );
            assert!(!prepared.commitment().unwrap().is_zero());
            prepared_ballots.insert(id, prepared);
        }
    }
    assert!(!prepared_ballots.is_empty());
    for (id, prepared) in prepared_ballots {
        let projected = state.governance.get_proposal(&id).unwrap();
        let counted = prepared.next_proposal();
        assert_eq!(
            projected.exploration_votes_yea,
            counted.exploration_votes_yea
        );
        assert_eq!(
            projected.exploration_votes_nay,
            counted.exploration_votes_nay
        );
        assert_eq!(
            projected.exploration_votes_abstain,
            counted.exploration_votes_abstain
        );
        assert_eq!(
            projected.total_eligible_voters,
            counted.total_eligible_voters
        );
    }
    state
}
