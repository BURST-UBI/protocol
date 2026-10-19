//! Recompute supported payments and verification from a structurally closed cut
//! without account afterimages, economic snapshots or undo journals.
//!
//! Checkpoint validation uses this supported subset with certified parent
//! history. Complete certified normal panels and their revotes are supported.
//! Signed challenges and rejected challenges retain transaction rights. Upheld
//! outcomes, panel deadlines and pending-expiry returns require certified cuts.
//! Ordinary replay cannot invent those boundaries. Unsupported governance
//! transitions still fail closed.

use crate::{block_processor::BlockProcessor, confirmed_history::ConfirmedHistory, ledger_updater};
use burst_brn::{BrnEngine, BrnWalletState};
use burst_ledger::{BlockType, StateBlock};
use burst_store::{account::AccountInfo, pending::PendingInfo};
pub(crate) mod blocks;
use burst_trst::{MergerGraph, TrstEngine, WalletPortfolio};
use burst_types::{BlockHash, NetworkId, ProtocolParams, Timestamp, TxHash, WalletAddress};
use burst_verification::{
    selection_request::SelectionRequest, VerificationEvent, VerificationOrchestrator,
    VerificationResult, VerificationState,
};
use std::collections::{HashMap, HashSet};

mod activation;
mod deadline;
mod events;
mod governance;
pub mod installation;
pub(crate) mod mutations;
mod outcome;
mod receipts;
mod scratch_index;
mod scratch_records;
mod selections;
pub mod snapshot;

const MAX_LIFECYCLE_DEPENDENCIES: usize = 1_048_576;

pub(crate) trait LifecycleHistory {
    fn cutoff(&self) -> u64;
    fn commitment(&self) -> BlockHash;
    fn applied_blocks(&self) -> impl blocks::AppliedBlocks;
    fn first_block(&self) -> Result<std::borrow::Cow<'_, StateBlock>, ReplayError>;
}

impl LifecycleHistory for ConfirmedHistory {
    fn cutoff(&self) -> u64 {
        self.cutoff()
    }
    fn commitment(&self) -> BlockHash {
        self.commitment()
    }
    fn applied_blocks(&self) -> impl blocks::AppliedBlocks {
        self.replay_blocks()
            .iter()
            .map(|block| (block.hash, block))
            .collect::<HashMap<_, _>>()
    }
    fn first_block(&self) -> Result<std::borrow::Cow<'_, StateBlock>, ReplayError> {
        self.replay_blocks()
            .first()
            .map(std::borrow::Cow::Borrowed)
            .ok_or_else(|| invalid("empty lifecycle history"))
    }
}

#[derive(Clone)]
pub struct ConfirmedReplay {
    mutations: Option<mutations::Mutations>,
    network: NetworkId,
    accounts: HashMap<WalletAddress, AccountInfo>,
    pending: HashMap<(WalletAddress, TxHash), PendingInfo>,
    brn: BrnEngine,
    trst: TrstEngine,
    cutoff: Timestamp,
    payment_epoch: BlockHash,
    verification: VerificationOrchestrator,
    bootstrap_phase: burst_verification::bootstrap::BootstrapPhase,
    lifecycle_metadata: HashMap<String, Vec<u8>>,
    params: ProtocolParams,
    endorsement_blocks: HashMap<WalletAddress, Vec<(Timestamp, BlockHash)>>,
    completed_endorsements: HashSet<(WalletAddress, BlockHash)>,
    verification_triggers: HashMap<WalletAddress, (Timestamp, BlockHash)>,
    counterparties: HashMap<WalletAddress, HashSet<WalletAddress>>,
    governance: Option<crate::governance_commit::CandidateGovernance>,
    proposal_contents: HashMap<BlockHash, Vec<u8>>,
    closed_governance: HashSet<TxHash>,
    selection_checkpoints: receipts::CheckpointHistory,
    selection_receipts: receipts::ReceiptHistory,
    replay_dependencies: HashSet<BlockHash>,
    minted_burns: HashMap<TxHash, Timestamp>,
}

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("confirmed replay does not support {kind:?} at {hash}")]
    UnsupportedBlock { kind: BlockType, hash: BlockHash },
    #[error("pending expiry requires agreed lifecycle replay")]
    PendingExpiry,
    #[error("invalid confirmed history: {0}")]
    Invalid(String),
}

fn invalid(message: impl Into<String>) -> ReplayError {
    ReplayError::Invalid(message.into())
}

fn needs_verified_history(kind: &BlockType) -> bool {
    crate::economic_candidate::requires_verified(kind)
        || matches!(
            kind,
            BlockType::Delegate
                | BlockType::GovernanceProposal
                | BlockType::GovernanceEndorse
                | BlockType::GovernanceVote
        )
}

impl ConfirmedReplay {
    pub(crate) fn payment_epoch(&self) -> BlockHash {
        self.payment_epoch
    }

    /// `network` and `initial_params` must come from local genesis configuration,
    /// not a peer's manifest or the currently governed parameter snapshot.
    /// Old confirmed blocks are checked against the cut, never today's clock.
    pub fn replay(
        history: &ConfirmedHistory,
        network: NetworkId,
        initial_params: &ProtocolParams,
    ) -> Result<Self, ReplayError> {
        Self::replay_with_certificates(history, network, initial_params, &[])
    }

    pub(crate) fn replay_with_certificates(
        history: &ConfirmedHistory,
        network: NetworkId,
        initial_params: &ProtocolParams,
        certificates: &[crate::checkpoint_store::CertifiedCheckpoint],
    ) -> Result<Self, ReplayError> {
        Self::replay_selection_boundaries(history, network, initial_params, certificates)
    }

    fn replay_with_selection_refs(
        history: &ConfirmedHistory,
        network: NetworkId,
        initial_params: &ProtocolParams,
        certificates: &[&crate::checkpoint_store::CertifiedCheckpoint],
        deadline_candidate: bool,
        governance_candidate: bool,
    ) -> Result<Self, ReplayError> {
        let genesis = crate::genesis_state::block(network, initial_params);
        if history.network() != network || history.genesis() != genesis.hash {
            return Err(invalid(
                "history does not match configured network and genesis parameters",
            ));
        }
        let mut bootstrap = HashMap::new();
        let mut challenges = HashSet::new();
        let mut selections = selections::Selections::new(certificates, history.replay_blocks())?;
        let mut verified_count = 1u64;
        for block in history.replay_blocks() {
            if block.block_type == BlockType::Challenge {
                let target = crate::ledger_bridge::extract_receiver_from_link(&block.link)
                    .ok_or_else(|| invalid("challenge target is invalid"))?;
                if !challenges.insert(target) {
                    return Err(invalid(
                        "repeated challenge admissions require agreed lifecycle ordering",
                    ));
                }
            }
            if block.block_type == BlockType::Endorse {
                if block.account == genesis.account
                    && verified_count < initial_params.bootstrap_exit_threshold as u64
                {
                    let target = crate::ledger_bridge::extract_receiver_from_link(&block.link)
                        .ok_or_else(|| invalid("endorsement target is invalid"))?;
                    if target == genesis.account || bootstrap.insert(target, block.hash).is_some() {
                        return Err(invalid("duplicate or self bootstrap verification"));
                    }
                    verified_count += 1;
                }
            } else if !(crate::payment_transition::supports(&block.block_type)
                || (governance_candidate && governance::supports(&block.block_type))
                || matches!(
                    block.block_type,
                    BlockType::Epoch
                        | BlockType::VerifierOptIn
                        | BlockType::VerifierOptOut
                        | BlockType::VerificationVote
                        | BlockType::Challenge
                ))
            {
                return Err(ReplayError::UnsupportedBlock {
                    kind: block.block_type.clone(),
                    hash: block.hash,
                });
            }
        }
        // Ordinary endorsements and creator admission of the same target do not
        // commute. Until admission binds its endorsement history, do not invent
        // an order that could accept a burn the live node rejected (or vice versa).
        for block in history
            .replay_blocks()
            .iter()
            .filter(|block| block.block_type == BlockType::Endorse)
        {
            let target = crate::ledger_bridge::extract_receiver_from_link(&block.link)
                .ok_or_else(|| invalid("endorsement target is invalid"))?;
            if bootstrap
                .get(&target)
                .is_some_and(|hash| *hash != block.hash)
            {
                return Err(ReplayError::UnsupportedBlock {
                    kind: block.block_type.clone(),
                    hash: block.hash,
                });
            }
        }
        // Bootstrap spending requires its creator admission even when the
        // target's account chain arrived first. A target_head that already
        // includes such spending creates a cycle and is rejected. Certified
        // normal panels supply their separate dependencies above.
        // Pending challenges depend on target admission but retain its rights.
        // This inference must not be extended to challenge outcomes or re-verification.
        let mut prerequisites = HashMap::new();
        let governance_dependencies = if governance_candidate {
            governance::dependencies(history)?
        } else {
            HashMap::new()
        };
        let mut dependency_budget = MAX_LIFECYCLE_DEPENDENCIES;
        for block in history.replay_blocks() {
            let mut dependencies = selections.dependencies(block, dependency_budget)?;
            dependencies.extend(
                governance_dependencies
                    .get(&block.hash)
                    .into_iter()
                    .flatten()
                    .copied(),
            );
            if needs_verified_history(&block.block_type) {
                dependencies.extend(bootstrap.get(&block.account).copied());
            }
            if block.block_type == BlockType::Challenge {
                let target = crate::ledger_bridge::extract_receiver_from_link(&block.link)
                    .ok_or_else(|| invalid("challenge target is invalid"))?;
                dependencies.extend(bootstrap.get(&target).copied());
            }
            dependency_budget = dependency_budget
                .checked_sub(dependencies.len())
                .ok_or_else(|| invalid("lifecycle dependencies require incremental replay"))?;
            prerequisites.insert(block.hash, dependencies);
        }
        let ordered = crate::confirmed_history::causal_order(history.replay_blocks(), |block| {
            prerequisites[&block.hash].iter().copied()
        })
        .map_err(|error| invalid(error.to_string()))?;
        let account =
            crate::genesis_state::account_to_write(None, genesis.account.clone(), genesis.hash)
                .expect("fresh genesis account");
        let mut state = Self {
            mutations: None,
            network,
            accounts: HashMap::from([(genesis.account.clone(), account)]),
            pending: HashMap::new(),
            brn: BrnEngine::with_rate(initial_params.brn_rate, Timestamp::EPOCH),
            trst: TrstEngine::with_expiry(initial_params.trst_expiry_secs),
            cutoff: Timestamp::new(history.cutoff()),
            payment_epoch: history.genesis(),
            verification: VerificationOrchestrator::new(),
            bootstrap_phase: burst_verification::bootstrap::BootstrapPhase::at_genesis(
                initial_params.bootstrap_exit_threshold,
            ),
            lifecycle_metadata: HashMap::new(),
            params: initial_params.clone(),
            endorsement_blocks: HashMap::new(),
            completed_endorsements: HashSet::new(),
            closed_governance: HashSet::new(),
            selection_checkpoints: Default::default(),
            selection_receipts: receipts::ReceiptHistory::default(),
            replay_dependencies: HashSet::new(),
            minted_burns: HashMap::new(),
            verification_triggers: HashMap::new(),
            counterparties: HashMap::new(),
            governance: governance_candidate.then(|| {
                crate::governance_commit::CandidateGovernance::initial(
                    initial_params.clone(),
                    history.network(),
                    history.genesis(),
                )
            }),
            proposal_contents: if governance_candidate {
                history
                    .proposal_witnesses()
                    .iter()
                    .map(|witness| (witness.block, witness.bytes.clone()))
                    .collect()
            } else {
                HashMap::new()
            },
        };
        state.brn.track_wallet(
            genesis.account.clone(),
            BrnWalletState::new(Timestamp::EPOCH),
        );
        let mut applied = HashMap::from([(genesis.hash, &genesis)]);
        state.verification.apply_governed_rules(initial_params);
        state.verification.initialize_genesis(&genesis.account);
        for block in ordered {
            if block.hash == genesis.hash {
                continue;
            }
            if block.payment_epoch != genesis.hash {
                return Err(invalid(
                    "block payment epoch has no installed certified boundary",
                ));
            }
            state.apply_authenticated(
                block,
                &applied,
                initial_params,
                &genesis.account,
                &mut selections,
            )?;
            applied.insert(block.hash, block);
        }
        state.confirm_heads(history)?;
        if deadline_candidate {
            state.advance_deadline_candidate_cutoff(state.cutoff)?;
        } else {
            state.advance_cutoff(state.cutoff, initial_params)?;
        }
        Ok(state)
    }

    fn apply_authenticated(
        &mut self,
        block: &StateBlock,
        applied: &dyn blocks::AppliedBlocks,
        params: &ProtocolParams,
        genesis: &WalletAddress,
        selections: &mut selections::Selections<
            '_,
            impl selections::source::SelectionSource + ?Sized,
        >,
    ) -> Result<(), ReplayError> {
        let authenticated = if block.block_type == BlockType::Epoch {
            crate::payment_transition::authenticate_epoch_contents(block, params, genesis)
        } else if self.governance.is_some() && governance::supports(&block.block_type) {
            crate::payment_transition::authenticate_signed_contents(block, params)
        } else {
            crate::payment_transition::authenticate_contents(block, params)
        };
        authenticated.map_err(|error| invalid(format!("{}: {error}", block.hash)))?;
        selections.install_for(block, &mut self.verification, params)?;
        self.apply(block, applied, params)?;
        if block.block_type == BlockType::Burn {
            self.record_minted_burn(block.hash.into_tx_hash(), block.timestamp)?;
        }
        if block.block_type == BlockType::Endorse {
            self.endorse(block, applied, genesis, params)?;
        }
        if block.block_type == BlockType::VerificationVote {
            let target = crate::ledger_bridge::extract_receiver_from_link(&block.link)
                .ok_or_else(|| invalid("verification vote target is invalid"))?;
            let source = self
                .verification_triggers
                .entry(target)
                .or_insert((block.timestamp, block.hash));
            if (block.timestamp.as_secs(), block.hash.as_bytes())
                > (source.0.as_secs(), source.1.as_bytes())
            {
                *source = (block.timestamp, block.hash);
            }
        }
        if matches!(
            block.block_type,
            BlockType::VerificationVote
                | BlockType::Challenge
                | BlockType::GovernanceEndorse
                | BlockType::GovernanceWithdraw
        ) {
            self.replay_dependencies.insert(block.hash);
        }
        Ok(())
    }

    fn record_minted_burn(&mut self, token: TxHash, at: Timestamp) -> Result<(), ReplayError> {
        if let Some(mutations) = &mut self.mutations {
            mutations
                .record_mint(
                    token,
                    self.minted_burns.get(&token).copied(),
                    at,
                    self.params.trst_expiry_secs,
                )
                .map_err(|error| invalid(error.to_string()))?;
        }
        self.minted_burns.insert(token, at);
        Ok(())
    }

    fn confirm_heads(&mut self, history: &ConfirmedHistory) -> Result<(), ReplayError> {
        self.confirm_head_list(history.heads())
    }

    fn confirm_head_list(
        &mut self,
        heads: &[crate::confirmed_history::HistoryHead],
    ) -> Result<(), ReplayError> {
        self.check_head_list(heads)?;
        for head in heads {
            self.accounts
                .get_mut(&head.account)
                .expect("checked account")
                .confirmation_height = head.height;
        }
        Ok(())
    }

    fn check_head_list(
        &self,
        heads: &[crate::confirmed_history::HistoryHead],
    ) -> Result<(), ReplayError> {
        for head in heads {
            let account = self
                .accounts
                .get(&head.account)
                .ok_or_else(|| invalid("replayed account is missing"))?;
            if account.head != head.hash || account.block_count != head.height {
                return Err(invalid("replayed head does not match manifest"));
            }
        }
        Ok(())
    }

    fn advance_cutoff(
        &mut self,
        cutoff: Timestamp,
        initial_params: &ProtocolParams,
    ) -> Result<(), ReplayError> {
        self.require_completed_deadlines(cutoff)?;
        // Only a certified deadline boundary may return unclaimed transfers.
        if self
            .pending
            .values()
            .any(|pending| pending_expired(pending, initial_params, cutoff))
        {
            return Err(ReplayError::PendingExpiry);
        }
        self.advance_deadline_candidate_cutoff(cutoff)
    }

    fn advance_deadline_candidate_cutoff(&mut self, cutoff: Timestamp) -> Result<(), ReplayError> {
        if cutoff < self.cutoff {
            return Err(invalid("replay cutoff cannot move backwards"));
        }
        self.cutoff = cutoff;
        self.trst.flush_all_expired(self.cutoff);
        for account in self.accounts.values_mut() {
            if self.trst.get_portfolio(&account.address).is_some() {
                ledger_updater::update_token_counters(account, &self.trst, self.cutoff)
                    .map_err(invalid)?;
            }
        }
        Ok(())
    }

    fn endorse(
        &mut self,
        block: &StateBlock,
        applied: &dyn blocks::AppliedBlocks,
        genesis: &WalletAddress,
        params: &ProtocolParams,
    ) -> Result<(), ReplayError> {
        let target = crate::ledger_bridge::extract_receiver_from_link(&block.link)
            .ok_or_else(|| invalid("endorsement target is invalid"))?;
        if !block.target_head.is_zero() {
            let dependency = applied
                .get(&block.target_head)
                .map_err(|error| invalid(error.to_string()))?
                .ok_or_else(|| invalid("endorsement target head has not been replayed"))?;
            crate::endorsement_commit::validate_target_head(block, &target, &dependency)
                .map_err(invalid)?;
        }
        use burst_verification::bootstrap::BootstrapPhase;
        let count = if *genesis == block.account && self.bootstrap_phase == BootstrapPhase::Active {
            self.accounts
                .values()
                .filter(|account| account.state.is_verified())
                .count() as u64
        } else {
            0
        };
        self.bootstrap_phase
            .observe(count, params.bootstrap_exit_threshold);
        if self
            .accounts
            .get(&target)
            .is_some_and(|account| account.state.is_verified())
        {
            return Err(invalid("wallet is already verified"));
        }
        if block.account != *genesis || self.bootstrap_phase == BootstrapPhase::Complete {
            let previous = applied
                .get(&block.previous)
                .map_err(|error| invalid(error.to_string()))?
                .ok_or_else(|| invalid("endorser predecessor is missing"))?;
            let burned = block
                .brn_balance
                .checked_sub(previous.brn_balance)
                .ok_or_else(|| invalid("endorsement spend decreased"))?;
            let restarting = self
                .verification
                .get_state(&target)
                .is_some_and(|state| state.phase.can_reverify());
            self.verification
                .process_endorsement_at(&target, &block.account, burned, params, block.timestamp)
                .map_err(|error| invalid(error.to_string()))?;
            if self.verification.drain_events().iter().any(|event|
                !matches!(event, VerificationEvent::EndorsementComplete { wallet } if *wallet == target)) {
                return Err(invalid("unexpected normal endorsement effects"));
            }
            if restarting {
                self.completed_endorsements
                    .insert((target.clone(), self.endorsement_commitment(&target)?));
                self.endorsement_blocks.remove(&target);
            }
            self.endorsement_blocks
                .entry(target)
                .or_default()
                .push((block.timestamp, block.hash));
            return Ok(());
        }
        self.verification
            .genesis_verify_at(
                &target,
                genesis,
                count,
                params.bootstrap_exit_threshold as u64,
                block.timestamp,
            )
            .map_err(|error| invalid(error.to_string()))?;
        let events = self.verification.drain_events();
        if events.len() != 1
            || !matches!(&events[0],
            VerificationEvent::VerificationComplete { wallet, result: VerificationResult::Verified, outcomes }
                if *wallet == target && outcomes.verifiers.is_empty())
        {
            return Err(invalid("unexpected bootstrap verification effects"));
        }
        let mut ledger = crate::verification_commit::ledger::ReplayedLedger {
            mutations: self.mutations.as_mut(),
            accounts: &mut self.accounts,
            pending: &mut self.pending,
            metadata: &mut self.lifecycle_metadata,
            applied,
            current: block,
        };
        crate::verification_commit::verify_wallet(
            &mut ledger,
            &target,
            block.timestamp,
            &mut self.brn,
            &mut self.trst,
        )
        .map_err(invalid)?;
        crate::verification_commit::dependencies::record_identity(&mut ledger, &target, block.hash)
            .map_err(invalid)?;
        self.bootstrap_phase
            .observe(count + 1, params.bootstrap_exit_threshold);
        Ok(())
    }

    pub(crate) fn bootstrap_complete(&self) -> bool {
        self.bootstrap_phase == burst_verification::bootstrap::BootstrapPhase::Complete
    }

    pub fn verification(&self, address: &WalletAddress) -> Option<&VerificationState> {
        self.verification.get_state(address)
    }

    fn apply(
        &mut self,
        block: &StateBlock,
        applied: &dyn blocks::AppliedBlocks,
        params: &ProtocolParams,
    ) -> Result<(), ReplayError> {
        let account = self.accounts.get(&block.account);
        if (block.block_type == BlockType::Open) != block.previous.is_zero()
            || account.map_or(!block.previous.is_zero(), |account| {
                account.head != block.previous
            })
        {
            return Err(invalid("block does not extend replayed account"));
        }
        for hash in block.dependencies() {
            if !applied
                .contains(&hash)
                .map_err(|error| invalid(error.to_string()))?
            {
                return Err(invalid("causal dependency has not been replayed"));
            }
        }
        let previous = applied
            .get(&block.previous)
            .map_err(|error| invalid(error.to_string()))?;
        block
            .validate_signing_grant(previous.as_deref())
            .map_err(invalid)?;
        let previous_brn = previous.as_ref().map_or(0, |block| block.brn_balance);
        let previous_trst = previous.as_ref().map_or(0, |block| block.trst_balance);
        if previous.is_some() {
            BlockProcessor::validate_balance_transition(block, previous_brn, previous_trst)
                .map_err(invalid)?;
        }
        let claims = matches!(
            block.block_type,
            BlockType::Receive | BlockType::RejectReceive
        ) || (block.block_type == BlockType::Open && !block.link.is_zero());
        let source_key = (block.account.clone(), block.link.into_tx_hash());
        let pending = if claims {
            let pending = self
                .pending
                .get(&source_key)
                .ok_or_else(|| invalid("missing replayed pending transfer"))?;
            if pending_expired(pending, params, block.timestamp) {
                return Err(ReplayError::PendingExpiry);
            }
            if block.block_type != BlockType::RejectReceive {
                ledger_updater::validate_receive_amount(block, previous_trst, pending)
                    .map_err(invalid)?;
            }
            Some(pending)
        } else {
            None
        };
        BlockProcessor::validate_open_block(block, pending.map(|pending| pending.amount))
            .map_err(invalid)?;
        let economics = crate::economic_candidate::apply(
            block,
            crate::economic_candidate::EconomicContext {
                account,
                previous_brn,
                pending,
                params,
            },
            &mut self.brn,
            &mut self.trst,
        )
        .map_err(invalid)?;
        let updated =
            ledger_updater::account_after_block(block, account, previous_brn).map_err(invalid)?;
        use crate::verification_commit::ledger::Ledger;
        let mut ledger = crate::verification_commit::ledger::ReplayedLedger {
            mutations: self.mutations.as_mut(),
            accounts: &mut self.accounts,
            pending: &mut self.pending,
            metadata: &mut self.lifecycle_metadata,
            applied,
            current: block,
        };
        ledger
            .put_account(&updated)
            .map_err(|error| invalid(error.to_string()))?;
        crate::verification_commit::dependencies::bind_identity(&mut ledger, block)
            .map_err(|error| invalid(error.to_string()))?;
        if governance::supports(&block.block_type) {
            let governance = self
                .governance
                .as_mut()
                .ok_or_else(|| invalid("governance is not enabled for this replay"))?;
            governance
                .apply_to(
                    &mut crate::governance_commit::ledger::derived::DerivedLedger {
                        mutations: self.mutations.as_mut(),
                        accounts: &mut self.accounts,
                        metadata: &mut self.lifecycle_metadata,
                        applied,
                        contents: &self.proposal_contents,
                        bootstrap: &mut self.bootstrap_phase,
                    },
                    block,
                    &economics.effect,
                    &mut self.brn,
                    &mut self.trst,
                )
                .map_err(invalid)?;
        }
        if matches!(
            block.block_type,
            BlockType::VerificationVote | BlockType::Challenge
        ) {
            crate::verification_commit::collect(
                &mut crate::verification_commit::ledger::ReplayedLedger {
                    mutations: self.mutations.as_mut(),
                    accounts: &mut self.accounts,
                    pending: &mut self.pending,
                    metadata: &mut self.lifecycle_metadata,
                    applied,
                    current: block,
                },
                block,
                &economics.effect,
                params,
                &mut self.verification,
                (&mut self.brn, &mut self.trst),
                self.network,
            )
            .map_err(invalid)?;
        }
        let mut ledger = crate::verification_commit::ledger::ReplayedLedger {
            mutations: self.mutations.as_mut(),
            accounts: &mut self.accounts,
            pending: &mut self.pending,
            metadata: &mut self.lifecycle_metadata,
            applied,
            current: block,
        };
        if claims {
            let pending = ledger
                .take_pending(&source_key.0, &source_key.1)
                .map_err(|error| invalid(error.to_string()))?;
            if block.block_type == BlockType::RejectReceive {
                ledger
                    .put_pending(&pending.source, &block.hash.into_tx_hash(), &pending)
                    .map_err(|error| invalid(error.to_string()))?;
            } else if pending.source != block.account
                && applied
                    .get(&block.link)
                    .map_err(|error| invalid(error.to_string()))?
                    .is_some_and(|source| {
                        matches!(source.block_type, BlockType::Burn | BlockType::Send)
                    })
            {
                self.counterparties
                    .entry(block.account.clone())
                    .or_default()
                    .insert(pending.source.clone());
                self.counterparties
                    .entry(pending.source)
                    .or_default()
                    .insert(block.account.clone());
            }
        }
        if let Some((amount, destination, provenance)) = economics.pending {
            ledger
                .put_pending(
                    &destination,
                    &block.hash.into_tx_hash(),
                    &ledger_updater::pending_from_provenance(block, amount, provenance),
                )
                .map_err(|error| invalid(error.to_string()))?;
        }
        Ok(())
    }

    /// Eligibility is measured at this cut, never against a live account cache.
    /// Decision 15.5 requires minimum BRN and real TRST activity in addition to
    /// age and on-chain opt-in. The separate vote stake is checked at voting.
    pub fn eligible_verifiers(&self) -> Result<Vec<WalletAddress>, ReplayError> {
        let mut eligible = Vec::new();
        for account in self.accounts.values() {
            if !account.state.is_verified()
                || account.verifier_opted_in_at.is_none()
                || !account.verified_at.is_some_and(|verified| {
                    self.cutoff
                        .as_secs()
                        .checked_sub(verified.as_secs())
                        .is_some_and(|age| age >= self.params.min_verification_age_secs)
                })
                || !self
                    .counterparties
                    .get(&account.address)
                    .is_some_and(|peers| {
                        peers.iter().any(|peer| {
                            self.accounts
                                .get(peer)
                                .is_some_and(|peer| peer.state.is_verified())
                        })
                    })
            {
                continue;
            }
            if self.brn_balance(&account.address)? >= self.params.min_verifier_brn_balance {
                eligible.push(account.address.clone());
            }
        }
        eligible.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        Ok(eligible)
    }

    pub fn human_roster(&self) -> Result<burst_consensus::checkpoint::HumanRoster, ReplayError> {
        use burst_consensus::checkpoint::{HumanDelegation, HumanRoster};
        // Governance eviction has not been replayed yet. Never silently drop
        // these humans or reinterpret an evicted delegation as self-delegation.
        if self.accounts.values().any(|account| account.orv_evicted) {
            return Err(invalid("evicted representatives require governance replay"));
        }
        HumanRoster::new(
            self.accounts
                .values()
                .filter(|account| account.state.is_verified())
                .map(|account| HumanDelegation {
                    human: account.address.clone(),
                    representative: account.representative.clone(),
                })
                .collect(),
        )
        .map_err(|error| invalid(error.to_string()))
    }

    /// All inputs, including the signed admission trigger, come from replay.
    /// Within a cut, timestamp/hash order identifies the threshold-crossing
    /// endorsement independently of account traversal or packet arrival order.
    pub fn prepare_selection(
        &mut self,
        target: &WalletAddress,
    ) -> Result<SelectionRequest, ReplayError> {
        if !self.bootstrap_complete() {
            return Err(invalid("verifier selection requires bootstrap exit"));
        }
        let mut trigger = match self.selection_lifecycle(target)? {
            crate::checkpoint_replay::SelectionLifecycle::Verification(origin)
            | crate::checkpoint_replay::SelectionLifecycle::Challenge(origin) => origin,
        };
        if self
            .verification
            .get_state(target)
            .is_some_and(|state| state.revote_count > 0)
        {
            let signed = self.verification_triggers.get(target).copied();
            let mut completed = None;
            for (key, bytes) in &self.lifecycle_metadata {
                if !key.starts_with("verification_deadline/v1/") {
                    continue;
                }
                let value = crate::verification_deadline::decode(bytes).map_err(invalid)?;
                if &value.activation.target == target
                    && value.lifecycle == trigger
                    && completed.as_ref().is_none_or(
                        |previous: &crate::verification_deadline::Completion| {
                            value.completed_at > previous.completed_at
                        },
                    )
                {
                    completed = Some(value);
                }
            }
            trigger = match (signed, completed) {
                (Some((time, hash)), Some(value)) if time.as_secs() > value.completed_at => hash,
                (_, Some(value)) => {
                    crate::verification_deadline::commitment(&[value]).map_err(invalid)?
                }
                (Some((_, hash)), None) => hash,
                (None, None) => {
                    return Err(invalid("revote has no signed or certified completed panel"))
                }
            };
        }
        let eligible = self.eligible_verifiers()?;
        self.verification
            .prepare_selection(target, trigger, self.cutoff, &eligible, &self.params)
            .map_err(|error| invalid(error.to_string()))
    }

    pub(crate) fn selection_lifecycle(
        &self,
        target: &WalletAddress,
    ) -> Result<crate::checkpoint_replay::SelectionLifecycle, ReplayError> {
        use crate::checkpoint_replay::SelectionLifecycle;
        if let Some(challenge) = self
            .verification
            .active_challenge_context(target)
            .map_err(|error| invalid(error.to_string()))?
        {
            return Ok(SelectionLifecycle::Challenge(challenge));
        }
        let threshold = usize::try_from(self.params.endorsement_threshold)
            .map_err(|_| invalid("invalid endorsement threshold"))?;
        if threshold == 0 {
            return Err(invalid("invalid endorsement threshold"));
        }
        let mut endorsements = self
            .endorsement_blocks
            .get(target)
            .ok_or_else(|| invalid("selection target has no replayed endorsements"))?
            .clone();
        endorsements.sort_by_key(|(time, hash)| (time.as_secs(), *hash.as_bytes()));
        let trigger = endorsements
            .get(threshold - 1)
            .ok_or_else(|| invalid("endorsement threshold not reached"))?
            .1;
        Ok(SelectionLifecycle::Verification(trigger))
    }

    pub(crate) fn selection_targets(&self) -> Vec<WalletAddress> {
        if !self.bootstrap_complete() {
            return Vec::new();
        }
        let mut targets: Vec<_> = self
            .endorsement_blocks
            .keys()
            .chain(self.accounts.keys())
            .filter(|target| {
                use burst_verification::state::VerificationPhase;
                self.verification
                    .get_state(target)
                    .is_some_and(|state| match state.phase {
                        VerificationPhase::Challenged => true,
                        VerificationPhase::Endorsing => {
                            state.endorsements.len() >= self.params.endorsement_threshold as usize
                        }
                        VerificationPhase::Voting if state.selected_verifiers.is_empty() => {
                            self.verification.has_active_challenge(target)
                                || state.endorsements.len()
                                    >= self.params.endorsement_threshold as usize
                        }
                        _ => false,
                    })
            })
            .cloned()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        targets.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        targets
    }

    pub(crate) fn has_endorsement_commitment(
        &self,
        target: &WalletAddress,
        commitment: BlockHash,
    ) -> Result<bool, ReplayError> {
        Ok(self.endorsement_commitment(target)? == commitment
            || self
                .completed_endorsements
                .contains(&(target.clone(), commitment)))
    }

    pub(crate) fn endorsement_commitment(
        &self,
        target: &WalletAddress,
    ) -> Result<BlockHash, ReplayError> {
        let mut hashes: Vec<_> = self
            .endorsement_blocks
            .get(target)
            .into_iter()
            .flatten()
            .map(|(_, hash)| *hash.as_bytes())
            .collect();
        hashes.sort_unstable();
        let mut bytes = b"BURST/selection-endorsements/v1\0".to_vec();
        bytes.extend_from_slice(&(hashes.len() as u64).to_be_bytes());
        for hash in hashes {
            bytes.extend_from_slice(&hash);
        }
        Ok(BlockHash::new(burst_crypto::blake2b_256(&bytes)))
    }

    pub(crate) fn selection_history_commitment(
        &self,
        target: &WalletAddress,
    ) -> Result<BlockHash, ReplayError> {
        let defaults = crate::verification_deadline::history_defaults(
            &self.verification,
            target,
            self.selection_lifecycle(target)?,
            |voter| {
                let Some(context) = self
                    .lifecycle_metadata
                    .get(&crate::verification_deadline::voter_key(target, voter))
                else {
                    return Ok(None);
                };
                let context = BlockHash::new(
                    context
                        .as_slice()
                        .try_into()
                        .map_err(|_| "invalid deadline voter binding")?,
                );
                self.lifecycle_metadata
                    .get(&crate::verification_deadline::key(context))
                    .ok_or_else(|| "deadline voter has no completion".to_owned())
                    .and_then(|bytes| crate::verification_deadline::decode(bytes))
                    .map(Some)
            },
        )
        .map_err(invalid)?;
        self.verification
            .selection_history_with_defaults(target, &defaults)
            .map_err(|error| invalid(error.to_string()))
    }

    pub fn account(&self, address: &WalletAddress) -> Option<&AccountInfo> {
        self.accounts.get(address)
    }

    pub(crate) fn params(&self) -> &ProtocolParams {
        &self.params
    }

    pub fn brn_balance(&self, address: &WalletAddress) -> Result<u128, ReplayError> {
        self.brn.get_wallet(address).map_or(Ok(0), |wallet| {
            self.brn
                .compute_balance_checked(wallet, self.cutoff)
                .map_err(|error| invalid(error.to_string()))
        })
    }

    pub fn pending(&self, address: &WalletAddress, source: TxHash) -> Option<&PendingInfo> {
        self.pending.get(&(address.clone(), source))
    }

    pub fn portfolio(&self, address: &WalletAddress) -> Option<&WalletPortfolio> {
        self.trst.get_portfolio(address)
    }

    pub fn merger_graph(&self) -> &MergerGraph {
        &self.trst.merger_graph
    }
}

fn pending_expired(pending: &PendingInfo, params: &ProtocolParams, at: Timestamp) -> bool {
    !pending.return_credit
        && pending.provenance.iter().any(|part| {
            part.effective_origin_timestamp
                .has_expired(params.trst_expiry_secs, at)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confirmed_history::HistoryHead;
    use burst_store::{
        account::AccountStore, block::BlockStore, meta::MetaStore, pending::PendingStore,
    };
    use burst_store_lmdb::LmdbStore;
    use burst_types::{Signature, WalletState};

    mod applied_blocks;
    mod certified_votes;
    mod compact_heights;
    mod deadline_recovery;
    mod delayed_prepare;
    mod governance;
    mod historical_transfer;
    mod live_clock;
    mod live_selection;
    mod pending_expiry;
    mod protocol_epoch;
    mod roster;
    mod snapshot;
    mod snapshot_export;
    mod snapshot_history;

    struct Fixture {
        params: ProtocolParams,
        blocks: Vec<StateBlock>,
    }

    fn address(seed: u8) -> WalletAddress {
        burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[seed; 32]).public)
    }

    fn destination(seed: u8) -> BlockHash {
        BlockHash::new(burst_crypto::keypair_from_seed(&[seed; 32]).public.0)
    }

    fn sign(seed: u8, block: &mut StateBlock) {
        block.hash = block.compute_hash();
        block.signature = burst_crypto::sign_message(
            block.hash.as_bytes(),
            &burst_crypto::keypair_from_seed(&[seed; 32]).private,
        );
    }

    impl Fixture {
        fn new() -> Self {
            let params = ProtocolParams {
                brn_rate: 1,
                endorsement_burn_amount: 1,
                min_work_difficulty: 0,
                trst_expiry_secs: 1000,
                new_wallet_spending_limit: 0,
                ..ProtocolParams::default()
            };
            Self {
                blocks: vec![crate::genesis_state::block(NetworkId::Dev, &params)],
                params,
            }
        }

        fn push(
            &mut self,
            seed: u8,
            kind: BlockType,
            time: u64,
            edit: impl FnOnce(&mut StateBlock),
        ) -> BlockHash {
            let account = address(seed);
            let previous = self
                .blocks
                .iter()
                .rev()
                .find(|block| block.account == account);
            let mut block = self.blocks[0].clone();
            block.payment_epoch = self
                .blocks
                .last()
                .map(|block| block.payment_epoch)
                .filter(|epoch| !epoch.is_zero())
                .unwrap_or(self.blocks[0].hash);
            block.block_type = kind;
            block.account = account.clone();
            block.representative = previous.map_or(account, |block| block.representative.clone());
            block.previous = previous.map_or(BlockHash::ZERO, |block| block.hash);
            block.signing_grant =
                previous.map_or(BlockHash::ZERO, StateBlock::active_signing_grant);
            block.brn_balance = previous.map_or(0, |block| block.brn_balance);
            block.trst_balance = previous.map_or(0, |block| block.trst_balance);
            block.timestamp = Timestamp::new(time);
            edit(&mut block);
            sign(seed, &mut block);
            let hash = block.hash;
            self.blocks.push(block);
            hash
        }

        // Component fixtures supply final prefixes directly; live journeys use
        // the node's admission, election and cementation workers instead.
        fn append_confirmed_prefix(&self, store: &LmdbStore, cutoff: u64) {
            self.append_confirmed_history(store, &self.history(cutoff));
        }

        fn append_confirmed_history(&self, store: &LmdbStore, history: &ConfirmedHistory) {
            let events =
                crate::checkpoint_store::chain::load(store, NetworkId::Dev, &self.params).unwrap();
            let state =
                ConfirmedReplay::replay_with_events(history, NetworkId::Dev, &self.params, &events)
                    .unwrap();
            let mut heights = HashMap::new();
            let mut batch = store.tx_begin_write().unwrap();
            for block in &self.blocks {
                let height = heights.entry(block.account.clone()).or_insert(0u64);
                *height += 1;
                batch
                    .put_block_with_account(
                        &block.hash,
                        &bincode::serialize(block).unwrap(),
                        &block.account,
                        *height,
                    )
                    .unwrap();
            }
            for account in state.accounts.values() {
                batch.put_account_info(account, false).unwrap();
                if account.block_count > 0 {
                    batch.put_frontier(&account.address, &account.head).unwrap();
                }
            }
            batch.commit().unwrap();
        }

        // Only blocks, height indexes and finality markers are supplied. No
        // economic state or undo journal exists for replay to accidentally trust.
        fn store(&self) -> (tempfile::TempDir, LmdbStore, Vec<HistoryHead>) {
            self.store_with_map_size(32 << 20)
        }

        fn store_with_map_size(
            &self,
            map_size: usize,
        ) -> (tempfile::TempDir, LmdbStore, Vec<HistoryHead>) {
            let dir = tempfile::tempdir().unwrap();
            let store = LmdbStore::open(dir.path(), 32, map_size).unwrap();
            let mut heads: HashMap<WalletAddress, HistoryHead> = HashMap::new();
            let mut batch = store.tx_begin_write().unwrap();
            // This signed-history fixture supplies the context of its latest
            // block, not a live certified economic installation.
            let epoch = self
                .blocks
                .last()
                .map(|block| block.payment_epoch)
                .filter(|epoch| !epoch.is_zero())
                .unwrap_or(self.blocks[0].hash);
            crate::payment_epoch::initialize(&mut batch, epoch).unwrap();
            for block in &self.blocks {
                let height = heads.get(&block.account).map_or(1, |head| head.height + 1);
                batch
                    .put_block_with_account(
                        &block.hash,
                        &bincode::serialize(block).unwrap(),
                        &block.account,
                        height,
                    )
                    .unwrap();
                heads.insert(
                    block.account.clone(),
                    HistoryHead {
                        account: block.account.clone(),
                        height,
                        hash: block.hash,
                    },
                );
            }
            for head in heads.values() {
                let mut account =
                    crate::genesis_state::account_to_write(None, head.account.clone(), head.hash)
                        .unwrap();
                account.block_count = head.height;
                account.confirmation_height = head.height;
                // Deliberately lie about the economic and identity projection.
                account.state = WalletState::Revoked;
                account.total_brn_burned = u128::MAX;
                account.trst_balance = u128::MAX;
                account.orv_evicted = true;
                batch.put_account_info(&account, false).unwrap();
            }
            batch
                .put_meta(TrstEngine::meta_key(), b"invalid snapshot")
                .unwrap();
            batch.put_meta("merger_graph", b"invalid snapshot").unwrap();
            batch.commit().unwrap();
            (dir, store, heads.into_values().collect())
        }

        fn history(&self, cutoff: u64) -> ConfirmedHistory {
            let (_dir, store, heads) = self.store();
            ConfirmedHistory::read(
                &store,
                NetworkId::Dev,
                self.blocks[0].hash,
                cutoff,
                &heads,
                &[],
            )
            .unwrap()
        }

        fn replay(&self, cutoff: u64) -> Result<ConfirmedReplay, ReplayError> {
            ConfirmedReplay::replay(&self.history(cutoff), NetworkId::Dev, &self.params)
        }

        fn assert_rejected(&self, contains: &str) {
            match self.replay(200) {
                Ok(_) => panic!("invalid history accepted"),
                Err(error) => assert!(error.to_string().contains(contains), "{error}"),
            }
        }

        fn payments() -> Self {
            let mut fixture = Self::new();
            fixture.push(1, BlockType::Open, 90, |_| {});
            let first = fixture.push(0, BlockType::Burn, 100, |block| {
                block.brn_balance = 40;
                block.link = destination(0);
            });
            let first_receive = fixture.push(0, BlockType::Receive, 101, |block| {
                block.trst_balance = 40;
                block.link = first;
            });
            let second = fixture.push(0, BlockType::Burn, 102, |block| {
                block.brn_balance = 60;
                block.link = destination(0);
            });
            let second_receive = fixture.push(0, BlockType::Receive, 103, |block| {
                block.trst_balance = 60;
                block.link = second;
            });
            let merge = fixture.push(0, BlockType::Merge, 104, |block| {
                block.merge_sources =
                    vec![first_receive.into_tx_hash(), second_receive.into_tx_hash()];
            });
            let send = fixture.push(0, BlockType::Send, 105, |block| {
                block.trst_balance = 35;
                block.link = destination(1);
                block.origin = merge.into_tx_hash();
            });
            let reject = fixture.push(1, BlockType::RejectReceive, 106, |block| block.link = send);
            fixture.push(0, BlockType::Receive, 107, |block| {
                block.trst_balance = 60;
                block.link = reject;
            });
            let accepted = fixture.push(0, BlockType::Send, 108, |block| {
                block.trst_balance = 50;
                block.link = destination(1);
                block.origin = merge.into_tx_hash();
            });
            fixture.push(1, BlockType::Receive, 109, |block| {
                block.trst_balance = 10;
                block.link = accepted;
            });
            fixture.push(0, BlockType::Send, 110, |block| {
                block.trst_balance = 45;
                block.link = destination(1);
                block.origin = merge.into_tx_hash();
            });
            fixture.push(1, BlockType::ChangeRepresentative, 111, |block| {
                block.representative = address(0)
            });
            fixture
        }

        fn selection() -> Self {
            Self::selection_with(|_| {})
        }

        fn selection_with(configure: impl FnOnce(&mut ProtocolParams)) -> Self {
            let mut fixture = Self::new();
            fixture.params.brn_rate = burst_types::BRN_UNIT;
            fixture.params.min_verification_age_secs = 100;
            fixture.params.endorsement_threshold = 2;
            fixture.params.num_verifiers = 2;
            fixture.params.bootstrap_exit_threshold = 9;
            configure(&mut fixture.params);
            fixture.blocks[0] = crate::genesis_state::block(NetworkId::Dev, &fixture.params);
            let base = 1_595_431_100;
            for seed in 1..=8 {
                fixture.push(0, BlockType::Endorse, base, |block| {
                    block.brn_balance += 1;
                    block.link = destination(seed);
                });
                fixture.push(seed, BlockType::Open, base + 1, |_| {});
                fixture.push(seed, BlockType::VerifierOptIn, base + 2, |_| {});
            }
            // A claimed burn is a payment, not merely a mint or a self-transfer.
            for seed in 1..=8 {
                let burn = fixture.push(0, BlockType::Burn, base + 3, |block| {
                    block.brn_balance += 1;
                    block.link = destination(seed);
                });
                fixture.push(seed, BlockType::Receive, base + 4, |block| {
                    block.trst_balance = 1;
                    block.link = burn;
                });
            }
            for seed in [1, 2] {
                fixture.push(seed, BlockType::Endorse, base + 5, |block| {
                    block.brn_balance = 1;
                    block.link = destination(20);
                });
            }
            fixture
        }
    }

    #[test]
    fn neutral_account_arrival_order_changes_local_identity_bindings() {
        let mut fixture = Fixture::new();
        fixture.params.bootstrap_exit_threshold = 2;
        fixture.blocks[0] = crate::genesis_state::block(NetworkId::Dev, &fixture.params);
        let base = fixture.replay(99).unwrap();
        let source = fixture.push(0, BlockType::Endorse, 100, |block| {
            block.brn_balance = 1;
            block.link = destination(1);
        });
        let open = fixture.push(1, BlockType::Open, 101, |_| {});
        let successor = fixture.push(1, BlockType::ChangeRepresentative, 102, |block| {
            block.representative = address(0);
        });
        let history = fixture.history(103);
        let ordered = |edges: &[(BlockHash, BlockHash)], mode| {
            let mut state = base.clone();
            state
                .append_input_observed(
                    events::EpochInput {
                        directory: None,
                        network: NetworkId::Dev,
                        cutoff: 103,
                        heads: history.heads(),
                        blocks: &blocks::source::Source::new(&fixture.blocks, None).unwrap(),
                        contents: &[],
                        old: HashSet::from([fixture.blocks[0].hash]),
                    },
                    fixture.blocks[0].hash,
                    &[],
                    mode,
                    edges,
                    &mut |_, _| Ok(()),
                )
                .map(|_| state)
        };
        let opened_order = ordered(
            &[(source, open), (successor, source)],
            events::InputMode::Applied,
        )
        .unwrap();
        let endorsed_order = ordered(&[(open, source)], events::InputMode::Applied).unwrap();
        assert!(opened_order
            .lifecycle_metadata
            .contains_key(&format!("verification_identity_binding/v1/{successor}")));
        assert!(endorsed_order
            .lifecycle_metadata
            .contains_key(&format!("verification_identity_binding/v1/{open}")));
        for edges in [
            vec![(open, source), (source, open)],
            vec![(open, BlockHash::new([99; 32]))],
            vec![(fixture.blocks[0].hash, open)],
            vec![(open, source), (open, source)],
        ] {
            assert!(ordered(&edges, events::InputMode::Applied).is_err());
        }
        assert!(ordered(
            &[(open, source)],
            events::InputMode::Certified {
                deadline_candidate: false
            }
        )
        .is_err());
        let execute = |order: [usize; 3]| {
            let mut state = base.clone();
            let mut applied = HashMap::from([(fixture.blocks[0].hash, &fixture.blocks[0])]);
            let mut selections = selections::Selections::new(&[], &fixture.blocks).unwrap();
            for index in order {
                let block = &fixture.blocks[index];
                state
                    .apply_authenticated(
                        block,
                        &applied,
                        &fixture.params,
                        &address(0),
                        &mut selections,
                    )
                    .unwrap();
                applied.insert(block.hash, block);
            }
            state
        };
        let mut endorsed_first = execute([1, 2, 3]);
        let mut opened_first = execute([2, 1, 3]);
        let early_key = format!("verification_identity_binding/v1/{open}");
        let late_key = format!("verification_identity_binding/v1/{successor}");
        assert_eq!(
            endorsed_first.lifecycle_metadata.remove(&early_key),
            Some(source.as_bytes().to_vec())
        );
        assert_eq!(
            opened_first.lifecycle_metadata.remove(&late_key),
            Some(source.as_bytes().to_vec())
        );
        assert!(!endorsed_first.lifecycle_metadata.contains_key(&late_key));
        assert!(!opened_first.lifecycle_metadata.contains_key(&early_key));
        assert_eq!(endorsed_first.accounts.len(), opened_first.accounts.len());
        for (address, account) in &endorsed_first.accounts {
            assert_eq!(
                bincode::serialize(account).unwrap(),
                bincode::serialize(&opened_first.accounts[address]).unwrap()
            );
        }
        assert_eq!(
            endorsed_first.lifecycle_metadata,
            opened_first.lifecycle_metadata
        );
        assert_eq!(
            endorsed_first.trst.save_wallets(),
            opened_first.trst.save_wallets()
        );
        assert_eq!(
            endorsed_first.brn.wallets.len(),
            opened_first.brn.wallets.len()
        );
        for (address, wallet) in &endorsed_first.brn.wallets {
            assert_eq!(
                bincode::serialize(wallet).unwrap(),
                bincode::serialize(&opened_first.brn.wallets[address]).unwrap()
            );
        }
        assert_eq!(
            endorsed_first.snapshot_bytes().unwrap(),
            opened_first.snapshot_bytes().unwrap()
        );
    }

    #[test]
    fn replay_binds_identity_once_and_successors_inherit_the_dependency() {
        let mut fixture = Fixture::new();
        fixture.params.bootstrap_exit_threshold = 2;
        fixture.blocks[0] = crate::genesis_state::block(NetworkId::Dev, &fixture.params);
        let source = fixture.push(0, BlockType::Endorse, 100, |block| {
            block.brn_balance = 1;
            block.link = destination(1);
        });
        let burn = fixture.push(0, BlockType::Burn, 101, |block| {
            block.brn_balance += 1;
            block.link = destination(1);
        });
        let open = fixture.push(1, BlockType::Open, 102, |block| {
            block.link = burn;
            block.trst_balance = 1;
        });
        let successor = fixture.push(1, BlockType::ChangeRepresentative, 103, |block| {
            block.representative = address(0);
        });
        let state = fixture.replay(103).unwrap();
        for key in [
            format!("verification_identity_binding/v1/{open}"),
            format!("verification_identity_bound/v1/{}", address(1)),
        ] {
            assert_eq!(
                state.lifecycle_metadata.get(&key).map(Vec::as_slice),
                Some(source.as_bytes().as_slice())
            );
        }
        assert!(!state
            .lifecycle_metadata
            .contains_key(&format!("verification_identity_binding/v1/{successor}")));
    }

    #[test]
    fn replay_enforces_normal_endorsement_cost_after_creator_bootstrap() {
        for endorser in [0, 1] {
            let mut fixture = Fixture::new();
            fixture.params.endorsement_burn_amount = 2;
            fixture.params.bootstrap_exit_threshold = 2;
            fixture.blocks[0] = crate::genesis_state::block(NetworkId::Dev, &fixture.params);
            fixture.push(0, BlockType::Endorse, 100, |block| {
                block.brn_balance = 1;
                block.link = destination(1);
            });
            fixture.push(1, BlockType::Open, 101, |_| {});
            assert!(
                fixture.replay(101).is_ok(),
                "creator bootstrap remains a distinct admission path"
            );
            fixture.push(endorser, BlockType::Endorse, 103, |block| {
                block.brn_balance += 1;
                block.link = destination(2);
            });
            fixture.assert_rejected("insufficient endorsement burn");
        }
    }

    #[test]
    fn automatic_cut_removes_future_dependencies_and_their_descendants() {
        let mut fixture = Fixture::new();
        let burn = fixture.push(0, BlockType::Burn, 200, |block| {
            block.brn_balance = 10;
            block.link = destination(0);
        });
        let receive = fixture.push(0, BlockType::Receive, 201, |block| {
            block.trst_balance = 10;
            block.link = burn;
        });
        let send = fixture.push(0, BlockType::Send, 202, |block| {
            block.trst_balance = 5;
            block.link = destination(1);
            block.origin = receive.into_tx_hash();
        });
        fixture.push(1, BlockType::Open, 200, |block| {
            block.link = send;
            block.trst_balance = 5;
        });
        fixture.push(1, BlockType::ChangeRepresentative, 201, |_| {});
        let independent = fixture.push(2, BlockType::Open, 180, |_| {});
        let (_dir, store, _) = fixture.store();
        let heads = ConfirmedHistory::heads_at(&store, 201).unwrap();
        assert_eq!(heads.len(), 2);
        assert_eq!(
            heads
                .iter()
                .find(|head| head.account == address(0))
                .unwrap()
                .hash,
            receive
        );
        assert_eq!(
            heads
                .iter()
                .find(|head| head.account == address(2))
                .unwrap()
                .hash,
            independent
        );
        assert!(heads.iter().all(|head| head.account != address(1)));
        let history = ConfirmedHistory::read(
            &store,
            NetworkId::Dev,
            fixture.blocks[0].hash,
            201,
            &heads,
            &[],
        )
        .unwrap();
        let replay = ConfirmedReplay::replay(&history, NetworkId::Dev, &fixture.params).unwrap();
        assert!(replay.account(&address(1)).is_none());
        assert_eq!(replay.account(&address(0)).unwrap().trst_balance, 10);
    }

    #[test]
    fn bootstrap_must_finish_before_discovering_or_certifying_a_verifier_panel() {
        let fixture = Fixture::selection_with(|params| params.bootstrap_exit_threshold = 50);
        let mut replay = fixture.replay(1_595_431_210).unwrap();
        assert!(!replay.bootstrap_complete());
        assert!(!replay.eligible_verifiers().unwrap().is_empty());
        assert!(replay.selection_targets().is_empty());
        assert!(replay
            .prepare_selection(&address(20))
            .unwrap_err()
            .to_string()
            .contains("bootstrap exit"));
        let fixture = Fixture::selection();
        let mut replay = fixture.replay(1_595_431_210).unwrap();
        assert!(replay.bootstrap_complete());
        assert!(replay.selection_targets().contains(&address(20)));
        assert!(replay.prepare_selection(&address(20)).is_ok());
    }

    #[test]
    fn selection_inputs_are_derived_from_authenticated_history_not_account_snapshots() {
        let fixture = Fixture::selection();
        let cutoff = 1_595_431_210;
        let mut replay = fixture.replay(cutoff).unwrap();
        let mut eligible: Vec<_> = (1..=8).map(address).collect();
        eligible.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        assert_eq!(replay.eligible_verifiers().unwrap(), eligible);
        let request = replay.prepare_selection(&address(20)).unwrap();
        let trigger = fixture
            .blocks
            .iter()
            .filter(|block| block.block_type == BlockType::Endorse && block.link == destination(20))
            .max_by_key(|block| (block.timestamp.as_secs(), *block.hash.as_bytes()))
            .unwrap()
            .hash;
        let expected = SelectionRequest::new(
            address(20),
            trigger,
            0,
            cutoff,
            fixture.params.params_hash(),
            (3..=8).map(address).collect(),
            2,
        )
        .unwrap();
        assert_eq!(request, expected);
        assert_eq!(replay.prepare_selection(&address(20)).unwrap(), request);
        assert!(replay.prepare_selection(&address(21)).is_err());
        let roster = replay.human_roster().unwrap();
        assert_eq!(roster.total_humans(), 9);
        for seed in 0..=8 {
            assert_eq!(roster.weight(&address(seed)), 1);
        }
        assert_eq!(roster.weight(&address(20)), 0);
    }

    #[test]
    fn verifier_eligibility_enforces_age_balance_opt_out_and_completed_external_activity() {
        let mut fixture = Fixture::selection();
        let base = 1_595_431_100;
        assert!(fixture
            .replay(base + 99)
            .unwrap()
            .eligible_verifiers()
            .unwrap()
            .is_empty());
        assert_eq!(
            fixture
                .replay(base + 100)
                .unwrap()
                .eligible_verifiers()
                .unwrap()
                .len(),
            8
        );
        fixture.push(3, BlockType::VerifierOptOut, base + 101, |_| {});
        fixture.push(4, BlockType::Endorse, base + 101, |block| {
            block.brn_balance = 60 * burst_types::BRN_UNIT + 1;
            block.link = destination(21);
        });
        let eligible = fixture
            .replay(base + 110)
            .unwrap()
            .eligible_verifiers()
            .unwrap();
        assert!(!eligible.contains(&address(3)));
        assert!(!eligible.contains(&address(4)));
        assert!(fixture
            .replay(base + 111)
            .unwrap()
            .eligible_verifiers()
            .unwrap()
            .contains(&address(4)));

        let mut fixture = Fixture::new();
        fixture.params.brn_rate = burst_types::BRN_UNIT;
        fixture.params.min_verification_age_secs = 0;
        fixture.blocks[0] = crate::genesis_state::block(NetworkId::Dev, &fixture.params);
        fixture.push(0, BlockType::VerifierOptIn, 100, |_| {});
        let burn = fixture.push(0, BlockType::Burn, 101, |block| {
            block.brn_balance = 1;
            block.link = destination(0);
        });
        fixture.push(0, BlockType::Receive, 102, |block| {
            block.link = burn;
            block.trst_balance = 1;
        });
        let send = fixture.push(0, BlockType::Send, 103, |block| {
            block.link = destination(1);
            block.trst_balance = 0;
            block.origin = burn.into_tx_hash();
        });
        fixture.push(1, BlockType::Open, 103, |_| {});
        fixture.push(1, BlockType::RejectReceive, 104, |block| block.link = send);
        let rejected = fixture.blocks.last().unwrap().hash;
        fixture.push(0, BlockType::Receive, 105, |block| {
            block.link = rejected;
            block.trst_balance = 1;
        });
        assert!(fixture
            .replay(200)
            .unwrap()
            .eligible_verifiers()
            .unwrap()
            .is_empty());
        let external = fixture.push(0, BlockType::Send, 106, |block| {
            block.link = destination(1);
            block.trst_balance = 0;
            block.origin = burn.into_tx_hash();
        });
        assert!(fixture
            .replay(200)
            .unwrap()
            .eligible_verifiers()
            .unwrap()
            .is_empty());
        fixture.push(1, BlockType::Receive, 107, |block| {
            block.link = external;
            block.trst_balance = 1;
        });
        assert!(
            fixture
                .replay(200)
                .unwrap()
                .eligible_verifiers()
                .unwrap()
                .is_empty(),
            "unverified counterparty"
        );
        let target_head = fixture.blocks.last().unwrap().hash;
        fixture.push(0, BlockType::Endorse, 108, |block| {
            block.brn_balance = 2;
            block.link = destination(1);
            block.target_head = target_head;
        });
        assert_eq!(
            fixture.replay(200).unwrap().eligible_verifiers().unwrap(),
            vec![address(0)]
        );
    }

    #[test]
    fn governed_verifier_balance_is_inclusive_and_does_not_replace_other_requirements() {
        let fixture = Fixture::selection();
        let base = 1_595_431_100;
        let mut replay = fixture.replay(base + 100).unwrap();
        let candidate = address(4);
        let balance = replay.brn_balance(&candidate).unwrap();
        assert!(balance > 0 && balance < u128::MAX);
        replay.params.min_verifier_brn_balance = balance;
        assert!(replay.eligible_verifiers().unwrap().contains(&candidate));
        replay.params.min_verifier_brn_balance = balance + 1;
        assert!(!replay.eligible_verifiers().unwrap().contains(&candidate));
        replay.params.min_verifier_brn_balance = 0;
        assert_eq!(replay.eligible_verifiers().unwrap().len(), 8);
        // The verified genesis human did not opt in; zero balance is not a bypass.
        assert!(!replay.eligible_verifiers().unwrap().contains(&address(0)));
        let mut young = fixture.replay(base + 99).unwrap();
        young.params.min_verifier_brn_balance = 0;
        assert!(young.eligible_verifiers().unwrap().is_empty());
        let mut inactive = Fixture::new().replay(200).unwrap();
        inactive.params.min_verifier_brn_balance = 0;
        inactive.params.min_verification_age_secs = 0;
        assert!(inactive.eligible_verifiers().unwrap().is_empty());
    }

    #[test]
    fn pending_checkpoint_manifest_recovers_before_any_certificate_exists() {
        use crate::checkpoint_agreement::{self, Command};
        use crate::checkpoint_replay::{ReplayedCheckpoint, SigningClock};
        use crate::checkpoint_store::CertifiedCheckpoint;

        let fixture = Fixture::selection();
        let (dir, store, heads) = fixture.store();
        let cutoff = 1_595_434_050;
        let anchor = ReplayedCheckpoint::genesis_anchor(NetworkId::Dev, &fixture.params).unwrap();
        let replay = CertifiedCheckpoint::prepare(
            &store,
            NetworkId::Dev,
            &fixture.params,
            cutoff,
            &heads,
            &address(20),
        )
        .unwrap();
        let expected_manifest = replay.manifest();
        let key = burst_crypto::keypair_from_seed(&[0; 32]);
        let output = checkpoint_agreement::apply(
            &store,
            &anchor,
            &key.private,
            Command::Propose(replay.checkpoint().clone()),
            &[&replay],
            Some(SigningClock {
                now: cutoff,
                error_bound: 0,
            }),
        )
        .unwrap();
        drop(replay);
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        assert!(
            CertifiedCheckpoint::load(&store, NetworkId::Dev, &fixture.params)
                .unwrap()
                .is_none()
        );
        let restored =
            checkpoint_agreement::read_replays(&store, &anchor, &address(0), &fixture.params)
                .unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].manifest(), expected_manifest);
        // Recovery repeats an already reserved signature, not a late fresh choice.
        assert_eq!(
            checkpoint_agreement::apply(
                &store,
                &anchor,
                &key.private,
                Command::Resume,
                &restored.iter().collect::<Vec<_>>(),
                None,
            )
            .unwrap(),
            output
        );
        let different = CertifiedCheckpoint::prepare(
            &store,
            NetworkId::Dev,
            &fixture.params,
            cutoff + 1,
            &heads,
            &address(20),
        )
        .unwrap();
        assert!(checkpoint_agreement::apply(
            &store,
            &anchor,
            &key.private,
            Command::Resume,
            &[&different],
            None,
        )
        .is_err());
        assert!(checkpoint_agreement::apply(
            &store,
            &anchor,
            &key.private,
            Command::Resume,
            &[],
            None,
        )
        .is_err());
        let state = checkpoint_agreement::read_state(&store, &anchor, &address(0)).unwrap();
        let hash = fixture.blocks.last().unwrap().hash;
        store.block_store().delete_block(&hash).unwrap();
        assert!(
            checkpoint_agreement::read_replays(&store, &anchor, &address(0), &fixture.params,)
                .is_err(),
            "a persisted root does not replace its signed history"
        );
        assert_eq!(
            checkpoint_agreement::read_state(&store, &anchor, &address(0)).unwrap(),
            state
        );
    }

    #[tokio::test]
    async fn certified_beacon_worker_retries_the_same_round_and_recovers_from_disk() {
        beacon_worker_recovery(false, false).await;
    }

    #[tokio::test]
    async fn certified_beacon_worker_installs_only_the_exact_committee_and_recovers_from_disk() {
        beacon_worker_recovery(true, false).await;
    }

    #[tokio::test]
    async fn certified_beacon_worker_recovers_an_interrupted_mirror_fetch_without_changing_selection(
    ) {
        beacon_worker_recovery(true, true).await;
    }

    async fn beacon_worker_recovery(install_committee: bool, interrupt_fetch: bool) {
        use crate::checkpoint_store::CertifiedCheckpoint;
        use burst_consensus::checkpoint::{CheckpointCertificate, CheckpointSignature};
        use burst_consensus::checkpoint_round::Ballot;
        use std::sync::Arc;
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let fixture = Fixture::selection();
        let (_dir, store, heads) = fixture.store();
        let store = Arc::new(store);
        let live = Arc::new(tokio::sync::Mutex::new(
            fixture.replay(1_595_434_050).unwrap().verification,
        ));
        {
            let mut batch = store.tx_begin_write().unwrap();
            batch
                .put_brn_meta(
                    burst_verification::bootstrap::BootstrapPhase::KEY,
                    &burst_verification::bootstrap::BootstrapPhase::Complete.encoding(),
                )
                .unwrap();
            batch.commit().unwrap();
        }
        let replay = CertifiedCheckpoint::prepare(
            &store,
            NetworkId::Dev,
            &fixture.params,
            1_595_434_050,
            &heads,
            &address(20),
        )
        .unwrap();
        let checkpoint = replay.checkpoint().clone();
        assert_eq!(replay.request().round(), 102);
        let certificate = CheckpointCertificate {
            checkpoint: checkpoint.clone(),
            round: 0,
            signatures: vec![CheckpointSignature {
                representative: address(0),
                signature: burst_crypto::sign_message(
                    &Ballot::precommit(&checkpoint, 0).signing_payload(),
                    &burst_crypto::keypair_from_seed(&[0; 32]).private,
                ),
            }],
        };
        CertifiedCheckpoint::accept(
            &store,
            NetworkId::Dev,
            &fixture.params,
            certificate.clone(),
            heads,
            address(20),
        )
        .unwrap();
        if install_committee {
            let installed =
                installation::PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
                    .unwrap()
                    .unwrap()
                    .commit(&store)
                    .unwrap();
            *live.lock().await = installed.verification;
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let unavailable = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let unavailable_url = format!("http://{}", unavailable.local_addr().unwrap());
        let (stop_relay, mut relay_stopped) = tokio::sync::oneshot::channel();
        let unavailable_task = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    _ = &mut relay_stopped => return,
                    accepted = unavailable.accept() => accepted.unwrap(),
                };
                let (mut socket, _) = accepted;
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    if socket.read(&mut byte).await.unwrap() == 0 {
                        break;
                    }
                    request.push(byte[0]);
                    assert!(request.len() <= 4096);
                }
                let _ = socket.write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
            }
        });
        let mainnet = burst_vrf::DrandClient::mainnet().unwrap();
        let info = mainnet.chain_info().unwrap().clone();
        let relay_url = format!("http://{}", listener.local_addr().unwrap());
        let client = Arc::new(
            burst_vrf::DrandClient::with_chain(&relay_url, &info.chain_hash)
                .with_verifier(burst_vrf::DrandVerifier::mainnet().unwrap())
                .with_chain_info(info.clone())
                .with_relays(&[unavailable_url, relay_url])
                .unwrap(),
        );
        let (retry_started, retry_seen) = tokio::sync::oneshot::channel();
        let (allow_valid, wait_valid) = tokio::sync::oneshot::channel();
        let (check_restart, wait_restart) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let beacons: Vec<burst_vrf::DrandBeacon> =
                serde_json::from_str(include_str!("../tests/fixtures/selection-beacons.json"))
                    .unwrap();
            let mut gate = Some((retry_started, wait_valid));
            let responses = if interrupt_fetch {
                vec![1, 0, 0]
            } else {
                vec![1, 0]
            };
            for index in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut buffer = [0; 512];
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert!(count > 0 && request.len() + count <= 4096);
                    request.extend_from_slice(&buffer[..count]);
                }
                assert!(String::from_utf8(request)
                    .unwrap()
                    .starts_with(&format!("GET /{}/public/102 HTTP/1.1\r\n", info.chain_hash)));
                if index == 0 && gate.is_some() {
                    let (notify, gate) = gate.take().unwrap();
                    notify.send(()).unwrap();
                    gate.await.unwrap();
                }
                let body = serde_json::to_string(&beacons[index]).unwrap();
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let result = socket.write_all(response.as_bytes()).await;
                if !interrupt_fetch {
                    result.unwrap();
                }
            }
            wait_restart.await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(200), listener.accept())
                    .await
                    .is_err(),
                "a stored beacon must not be fetched again after restart"
            );
        });
        let service =
            snapshot_history::installed_recovery::Service::start(store.clone(), store.clone(), 0)
                .await;
        let startup = crate::confirmed_replay::snapshot::installed::recover_current_tip_with_cache(
            store.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
            None,
        )
        .await
        .unwrap();
        let worker_live = || crate::checkpoint_beacon::Live {
            recovery: Some(startup.fork(store.env().path()).unwrap()),
            registry: service.registry.clone(),
            orchestrator: live.clone(),
        };
        let mut shutdown = Arc::new(crate::shutdown::ShutdownController::new());
        let mut worker = tokio::spawn(crate::checkpoint_beacon::run(
            Arc::clone(&store),
            NetworkId::Dev,
            fixture.params.clone(),
            Arc::clone(&client),
            Arc::clone(&shutdown),
            Duration::from_millis(20),
            install_committee.then(worker_live),
        ));
        tokio::time::timeout(Duration::from_secs(5), retry_seen)
            .await
            .unwrap()
            .unwrap();
        let pending = CertifiedCheckpoint::load(&store, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap();
        assert_eq!(pending.certificate(), &certificate);
        assert!(
            pending.beacon().is_none(),
            "wrong-round relay response must leave the request pending"
        );
        {
            let orchestrator = live.lock().await;
            assert_eq!(
                orchestrator.selection_request(&address(20)).is_some(),
                install_committee
            );
            assert!(orchestrator.selection_context(&address(20)).is_none());
        }
        if interrupt_fetch {
            let frozen = live.lock().await.selection_request(&address(20)).cloned();
            shutdown.shutdown();
            tokio::time::timeout(Duration::from_secs(1), worker)
                .await
                .unwrap()
                .unwrap();
            let pending = CertifiedCheckpoint::load(&store, NetworkId::Dev, &fixture.params)
                .unwrap()
                .unwrap();
            assert_eq!(pending.certificate(), &certificate);
            assert!(pending.beacon().is_none());
            let bytes = store
                .meta_store()
                .get_meta(crate::endorsement_commit::SNAPSHOT_KEY)
                .unwrap();
            *live.lock().await = VerificationOrchestrator::restore(
                burst_verification::OrchestratorSnapshot::decode(&bytes).unwrap(),
            );
            assert_eq!(
                live.lock().await.selection_request(&address(20)),
                frozen.as_ref()
            );
            shutdown = Arc::new(crate::shutdown::ShutdownController::new());
            worker = tokio::spawn(crate::checkpoint_beacon::run(
                store.clone(),
                NetworkId::Dev,
                fixture.params.clone(),
                client.clone(),
                shutdown.clone(),
                Duration::from_millis(20),
                Some(worker_live()),
            ));
        }
        allow_valid.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if CertifiedCheckpoint::load(&store, NetworkId::Dev, &fixture.params)
                    .unwrap()
                    .unwrap()
                    .beacon()
                    .is_some()
                    && (!install_committee
                        || live.lock().await.selection_context(&address(20)).is_some())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        shutdown.shutdown();
        worker.await.unwrap();
        let context = live.lock().await.selection_context(&address(20));
        assert_eq!(context.is_some(), install_committee);
        if install_committee {
            let bytes = store
                .meta_store()
                .get_meta(crate::endorsement_commit::SNAPSHOT_KEY)
                .unwrap();
            let restored = burst_verification::OrchestratorSnapshot::decode(&bytes).unwrap();
            *live.lock().await = VerificationOrchestrator::restore(restored);
            assert_eq!(live.lock().await.selection_context(&address(20)), context);
        }
        let shutdown = Arc::new(crate::shutdown::ShutdownController::new());
        let worker = tokio::spawn(crate::checkpoint_beacon::run(
            Arc::clone(&store),
            NetworkId::Dev,
            fixture.params.clone(),
            client,
            Arc::clone(&shutdown),
            Duration::from_millis(20),
            install_committee.then(worker_live),
        ));
        check_restart.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        shutdown.shutdown();
        worker.await.unwrap();
        let restored = CertifiedCheckpoint::load(&store, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap();
        assert_eq!(restored.certificate(), &certificate);
        assert_eq!(restored.beacon().unwrap().round, 102);
        assert_eq!(live.lock().await.selection_context(&address(20)), context);
        stop_relay.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), unavailable_task)
            .await
            .unwrap()
            .unwrap();
        assert!(
            shutdown.failure().is_none(),
            "relay outages must not be fatal worker failures"
        );
        service.stop().await;
    }

    #[test]
    fn checkpoint_signing_requires_replayed_commitments_and_resumes_after_restart() {
        use crate::checkpoint_agreement::{self, Command, Outgoing};
        use crate::checkpoint_replay::{ReplayedCheckpoint, SigningClock};
        use crate::checkpoint_store::CertifiedCheckpoint;
        use burst_consensus::checkpoint::{
            CheckpointAnchor, CheckpointCertificate, HumanDelegation, HumanRoster,
        };
        use burst_consensus::checkpoint_round::Quorum;

        let fixture = Fixture::selection();
        let (dir, store, heads) = fixture.store();
        let cutoff = 1_595_434_050;
        let history = ConfirmedHistory::read(
            &store,
            NetworkId::Dev,
            fixture.blocks[0].hash,
            cutoff,
            &heads,
            &[],
        )
        .unwrap();
        let anchor = CheckpointAnchor::genesis(
            NetworkId::Dev,
            fixture.blocks[0].hash,
            0,
            HumanRoster::new(vec![HumanDelegation {
                human: address(0),
                representative: address(0),
            }])
            .unwrap(),
            6700,
        )
        .unwrap();
        let replay =
            ReplayedCheckpoint::replay(&history, &anchor, &fixture.params, &address(20)).unwrap();
        let checkpoint = replay.checkpoint().clone();
        let key = burst_crypto::keypair_from_seed(&[0; 32]);
        let clock = Some(SigningClock {
            now: cutoff,
            error_bound: 0,
        });
        let release = replay.request().beacon_time().unwrap();
        assert!(release > cutoff);
        let wrong_parent = CheckpointAnchor::genesis(
            NetworkId::Dev,
            anchor.genesis_hash(),
            0,
            anchor.roster().clone(),
            9000,
        )
        .unwrap();
        assert!(checkpoint_agreement::apply(
            &store,
            &wrong_parent,
            &key.private,
            Command::Propose(checkpoint.clone()),
            &[&replay],
            clock
        )
        .is_err());
        assert!(checkpoint_agreement::apply(
            &store,
            &wrong_parent,
            &key.private,
            Command::Resume,
            &[&replay],
            None
        )
        .is_err());
        assert!(
            checkpoint_agreement::read_state(&store, &anchor, &address(0))
                .unwrap()
                .is_none()
        );
        for field in 0..5 {
            let mut wrong = checkpoint.clone();
            match field {
                0 => wrong.ledger_root = BlockHash::new([91; 32]),
                1 => wrong.params_hash = BlockHash::new([92; 32]),
                2 => wrong.next_roster = anchor.roster().commitment(),
                3 => wrong.selection_root = BlockHash::new([93; 32]),
                _ => wrong.next_quorum_bps = 6667,
            }
            assert!(checkpoint_agreement::apply(
                &store,
                &anchor,
                &key.private,
                Command::Propose(wrong),
                &[&replay],
                clock
            )
            .is_err());
            assert!(
                checkpoint_agreement::read_state(&store, &anchor, &address(0))
                    .unwrap()
                    .is_none()
            );
        }
        for clock in [
            None,
            Some(SigningClock {
                now: release,
                error_bound: 0,
            }),
            Some(SigningClock {
                now: cutoff,
                error_bound: release - cutoff,
            }),
        ] {
            assert!(checkpoint_agreement::apply(
                &store,
                &anchor,
                &key.private,
                Command::Propose(checkpoint.clone()),
                &[&replay],
                clock
            )
            .is_err());
            assert!(
                checkpoint_agreement::read_state(&store, &anchor, &address(0))
                    .unwrap()
                    .is_none()
            );
        }
        let proposed = checkpoint_agreement::apply(
            &store,
            &anchor,
            &key.private,
            Command::Propose(checkpoint.clone()),
            &[&replay],
            clock,
        )
        .unwrap();
        let Outgoing::Proposal(proposal) = &proposed[0] else {
            panic!("expected proposal")
        };
        let output = checkpoint_agreement::apply(
            &store,
            &anchor,
            &key.private,
            Command::Proposal(proposal),
            &[&replay],
            clock,
        )
        .unwrap();
        let Outgoing::Vote(prevote) = &output[0] else {
            panic!("expected prevote")
        };
        let proof = Quorum {
            ballot: prevote.ballot.clone(),
            signatures: vec![prevote.signer.clone()],
        };
        // A prepared value may finish after beacon release, with exactly the
        // earlier pool. No late fresh proposal is admitted to obtain this proof.
        let output = checkpoint_agreement::apply(
            &store,
            &anchor,
            &key.private,
            Command::Prepared(proposal, &proof),
            &[&replay],
            None,
        )
        .unwrap();
        let Outgoing::Vote(precommit) = &output[0] else {
            panic!("expected precommit")
        };
        let certificate = CheckpointCertificate {
            checkpoint,
            round: 0,
            signatures: vec![precommit.signer.clone()],
        };
        let decision = checkpoint_agreement::apply(
            &store,
            &anchor,
            &key.private,
            Command::Decide(certificate.clone()),
            &[&replay],
            None,
        )
        .unwrap();
        anchor.verify_certificate(&certificate).unwrap();
        let successor = anchor
            .advance(&certificate, replay.roster().clone())
            .unwrap();
        assert_eq!(successor.roster().total_humans(), 9);
        assert_eq!(successor.required_humans(), 7);
        assert!(
            ReplayedCheckpoint::replay(&history, &successor, &fixture.params, &address(20))
                .is_err()
        );
        let mut accepted = CertifiedCheckpoint::accept(
            &store,
            NetworkId::Dev,
            &fixture.params,
            certificate.clone(),
            heads.clone(),
            address(20),
        )
        .unwrap();
        assert!(accepted.beacon().is_none());
        assert_eq!(accepted.replayed().request(), replay.request());
        let loaded = CertifiedCheckpoint::load(&store, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap();
        assert!(
            loaded.beacon().is_none(),
            "certificate persists while the relay is unavailable"
        );
        let mut forged = certificate.clone();
        forged.signatures[0].signature = Signature([0; 64]);
        assert!(CertifiedCheckpoint::accept(
            &store,
            NetworkId::Dev,
            &fixture.params,
            forged,
            heads.clone(),
            address(20)
        )
        .is_err());
        assert!(CertifiedCheckpoint::accept(
            &store,
            NetworkId::Dev,
            &fixture.params,
            certificate.clone(),
            heads[..heads.len() - 1].to_vec(),
            address(20)
        )
        .is_err());
        assert!(CertifiedCheckpoint::accept(
            &store,
            NetworkId::Dev,
            &fixture.params,
            certificate.clone(),
            heads.clone(),
            address(21)
        )
        .is_err());
        let beacons: Vec<burst_vrf::DrandBeacon> =
            serde_json::from_str(include_str!("../tests/fixtures/selection-beacons.json")).unwrap();
        let wrong = burst_vrf::DrandClient::beacon_to_random_output(&beacons[1]).unwrap();
        assert!(accepted
            .attach_beacon(&store, &fixture.params, wrong)
            .is_err());
        assert!(accepted.beacon().is_none());
        let beacon = burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap();
        assert_eq!(replay.request().round(), beacon.round);
        accepted
            .attach_beacon(&store, &fixture.params, beacon.clone())
            .unwrap();
        accepted
            .attach_beacon(&store, &fixture.params, beacon)
            .unwrap();
        let retried = CertifiedCheckpoint::accept(
            &store,
            NetworkId::Dev,
            &fixture.params,
            certificate.clone(),
            heads.clone(),
            address(20),
        )
        .unwrap();
        assert!(
            retried.beacon().is_some(),
            "idempotent import must not clear the beacon"
        );
        let (observer_dir, observer_store, _) = fixture.store();
        let wire_record =
            CertifiedCheckpoint::export_record(&store, NetworkId::Dev, &fixture.params, 1)
                .unwrap()
                .unwrap();
        let mut trailing = wire_record.clone();
        trailing.push(0);
        assert!(CertifiedCheckpoint::import_record(
            &observer_store,
            NetworkId::Dev,
            &fixture.params,
            1,
            &trailing,
        )
        .is_err());
        assert!(CertifiedCheckpoint::import_record(
            &observer_store,
            NetworkId::Dev,
            &fixture.params,
            2,
            &wire_record,
        )
        .is_err());
        assert!(CertifiedCheckpoint::import_record(
            &observer_store,
            NetworkId::Test,
            &fixture.params,
            1,
            &wire_record,
        )
        .is_err());
        assert!(
            CertifiedCheckpoint::load(&observer_store, NetworkId::Dev, &fixture.params)
                .unwrap()
                .is_none()
        );
        let mut imported = CertifiedCheckpoint::import_record(
            &observer_store,
            NetworkId::Dev,
            &fixture.params,
            1,
            &wire_record,
        )
        .unwrap();
        assert_eq!(imported.certificate(), &certificate);
        assert_eq!(imported.beacon().unwrap().round, 102);
        imported
            .attach_beacon(
                &observer_store,
                &fixture.params,
                accepted.beacon().unwrap().clone(),
            )
            .unwrap();
        assert_eq!(imported.replayed().request(), accepted.replayed().request());
        drop(observer_store);
        drop(observer_dir);
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let recovered =
            checkpoint_agreement::read_replays(&store, &anchor, &address(0), &fixture.params)
                .unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(
            checkpoint_agreement::apply(
                &store,
                &anchor,
                &key.private,
                Command::Resume,
                &[&recovered[0]],
                None
            )
            .unwrap(),
            decision
        );
        let restored =
            ReplayedCheckpoint::replay(&history, &anchor, &fixture.params, &address(20)).unwrap();
        assert_eq!(restored.request(), replay.request());
        let loaded = CertifiedCheckpoint::load(&store, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.replayed().request(), replay.request());
        assert_eq!(loaded.beacon().unwrap().round, 102);
        let record_key = format!("checkpoint_chain/v1/dev/{}/1", anchor.genesis_hash());
        let encoded = store.meta_store().get_meta(&record_key).unwrap();
        let mut wrong_version = encoded.clone();
        wrong_version[0] = 255;
        let mut trailing = encoded.clone();
        trailing.push(0);
        for damaged in [
            encoded[..encoded.len() - 1].to_vec(),
            wrong_version,
            trailing,
        ] {
            store.meta_store().put_meta(&record_key, &damaged).unwrap();
            assert!(CertifiedCheckpoint::load(&store, NetworkId::Dev, &fixture.params).is_err());
        }
        store.meta_store().put_meta(&record_key, &encoded).unwrap();
        let dependency = &fixture.blocks[1];
        store.block_store().delete_block(&dependency.hash).unwrap();
        assert!(
            CertifiedCheckpoint::load(&store, NetworkId::Dev, &fixture.params).is_ok(),
            "the authenticated archive retains the exact signed dependency"
        );
        store
            .meta_store()
            .delete_meta(&format!("checkpoint_history/v1/{}", dependency.hash))
            .unwrap();
        assert!(
            CertifiedCheckpoint::load(&store, NetworkId::Dev, &fixture.params).is_err(),
            "a durable certificate cannot replace missing history"
        );
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_block_with_account(
                &dependency.hash,
                &bincode::serialize(dependency).unwrap(),
                &dependency.account,
                2,
            )
            .unwrap();
        batch.commit().unwrap();
        assert!(
            CertifiedCheckpoint::load(&store, NetworkId::Dev, &fixture.params)
                .unwrap()
                .is_some()
        );
        assert!(
            CertifiedCheckpoint::load(&store, NetworkId::Test, &fixture.params)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store.meta_store().get_meta(TrstEngine::meta_key()).unwrap(),
            b"invalid snapshot"
        );
    }

    struct DriverFixture {
        fixture: Fixture,
        dirs: Vec<tempfile::TempDir>,
        stores: Vec<Option<std::sync::Arc<LmdbStore>>>,
        drivers: Vec<Option<crate::checkpoint_driver::CheckpointDriver>>,
        manifests: Vec<crate::checkpoint_replay::CheckpointManifest>,
        leader: usize,
        cutoff: u64,
    }

    fn checkpoint_drivers() -> DriverFixture {
        use crate::{checkpoint_driver::CheckpointDriver, checkpoint_store::CertifiedCheckpoint};
        use burst_consensus::{
            checkpoint::{CheckpointCertificate, CheckpointSignature},
            checkpoint_round::Ballot,
        };
        use std::sync::Arc;
        let mut fixture = Fixture::selection();
        for seed in [3, 4] {
            fixture.push(seed, BlockType::Endorse, 1_595_431_106, |block| {
                block.brn_balance += 1;
                block.link = destination(21);
            });
        }
        let cutoff = 1_595_434_050;
        let mut dirs = Vec::new();
        let mut stores = Vec::new();
        let mut drivers = Vec::new();
        let mut manifests = Vec::new();
        let mut leader = None;
        for seed in 0..=8 {
            let (dir, store, heads) = fixture.store();
            let store = Arc::new(store);
            let first = CertifiedCheckpoint::prepare(
                &store,
                NetworkId::Dev,
                &fixture.params,
                cutoff,
                &heads,
                &address(20),
            )
            .unwrap();
            let checkpoint = first.checkpoint().clone();
            let certificate = CheckpointCertificate {
                checkpoint: checkpoint.clone(),
                round: 0,
                signatures: vec![CheckpointSignature {
                    representative: address(0),
                    signature: burst_crypto::sign_message(
                        &Ballot::precommit(&checkpoint, 0).signing_payload(),
                        &burst_crypto::keypair_from_seed(&[0; 32]).private,
                    ),
                }],
            };
            let first = CertifiedCheckpoint::accept(
                &store,
                NetworkId::Dev,
                &fixture.params,
                certificate,
                heads.clone(),
                address(20),
            )
            .unwrap();
            let anchor = first
                .parent()
                .advance(first.certificate(), first.replayed().roster().clone())
                .unwrap();
            // These drivers test agreement with preloaded history, not live
            // economic installation. Supply the certified signing context.
            let mut batch = store.tx_begin_write().unwrap();
            batch
                .advance_payment_epoch(
                    &first.parent().hash(),
                    &first.parent().hash(),
                    &anchor.hash(),
                    first.replayed().history_hashes().unwrap(),
                )
                .unwrap();
            batch.commit().unwrap();
            assert_eq!(anchor.required_humans(), 7);
            if anchor.proposer(0) == &address(seed) {
                leader = Some(seed as usize);
            }
            let replay = CertifiedCheckpoint::prepare(
                &store,
                NetworkId::Dev,
                &fixture.params,
                cutoff + 30,
                &heads,
                &address(21),
            )
            .unwrap();
            manifests.push(replay.manifest());
            drivers.push(Some(
                CheckpointDriver::new(
                    Arc::clone(&store),
                    fixture.params.clone(),
                    burst_crypto::keypair_from_seed(&[seed; 32]).private,
                    replay,
                )
                .unwrap(),
            ));
            dirs.push(dir);
            stores.push(Some(store));
        }
        let leader = leader.unwrap();
        DriverFixture {
            fixture,
            dirs,
            stores,
            drivers,
            manifests,
            leader,
            cutoff,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn checkpoint_service_agrees_over_tcp_only_after_seven_of_nine_humans_join() {
        checkpoint_tcp_workers(false, false).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn checkpoint_runtime_builds_candidates_and_advances_late_nodes_over_tcp() {
        checkpoint_tcp_workers(true, false).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn checkpoint_service_recovers_expired_minority_selection_over_tcp() {
        checkpoint_tcp_workers(false, true).await;
    }

    async fn checkpoint_tcp_workers(runtime: bool, expired: bool) {
        let _permit = crate::verification_test_support::NETWORK_FIXTURE
            .acquire()
            .await
            .unwrap();
        use crate::{
            checkpoint_replay::SigningClock,
            checkpoint_service::{self, Clock, Session},
            checkpoint_store::CertifiedCheckpoint,
            checkpoint_transport::Incoming,
            connection_registry::ConnectionRegistry,
            shutdown::ShutdownController,
            wire_message::WireMessage,
        };
        use std::{
            sync::Arc,
            time::{Duration, Instant},
        };
        use tokio::{
            io::AsyncReadExt,
            net::{TcpListener, TcpStream},
            sync::{mpsc, RwLock},
        };

        let DriverFixture {
            fixture,
            dirs: _dirs,
            stores,
            mut drivers,
            manifests,
            leader,
            cutoff,
        } = checkpoint_drivers();
        let active: Vec<usize> = (0..9).map(|index| (leader + index) % 9).collect();
        let anchor = drivers[leader].as_ref().unwrap().anchor().clone();
        let registries: Vec<_> = (0..9)
            .map(|_| Arc::new(RwLock::new(ConnectionRegistry::new())))
            .collect();
        let stops: Vec<_> = (0..9)
            .map(|_| Arc::new(ShutdownController::new()))
            .collect();
        let (senders, mut receivers): (Vec<_>, Vec<_>) = (0..9)
            .map(|_| {
                let (sender, receiver) = mpsc::channel(checkpoint_service::INBOX_CAPACITY);
                (sender, Some(receiver))
            })
            .unzip();
        let mut readers = tokio::task::JoinSet::new();
        for left in 0..9 {
            registries[left]
                .write()
                .await
                .set_agreement_inbox(senders[left].clone());
            for right in left + 1..9 {
                // The fixture supplies preconnected TCP channels. Peer handshake
                // authentication is tested separately; payload votes are real.
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let connect = TcpStream::connect(listener.local_addr().unwrap());
                let (outgoing, incoming) = tokio::time::timeout(Duration::from_secs(10), async {
                    tokio::try_join!(connect, listener.accept())
                })
                .await
                .expect("checkpoint fixture TCP connection timed out")
                .expect("checkpoint fixture TCP connection failed");
                for (owner, peer, stream) in [(left, right, outgoing), (right, left, incoming.0)] {
                    let (mut reader, writer) = stream.into_split();
                    registries[owner]
                        .write()
                        .await
                        .insert(peer.to_string(), writer);
                    let sender = senders[owner].clone();
                    readers.spawn(async move {
                        while let Ok(size) = reader.read_u32().await {
                            assert!(size as usize <= burst_types::MAX_P2P_MESSAGE_BYTES);
                            let mut body = vec![0; size as usize];
                            if reader.read_exact(&mut body).await.is_err() {
                                break;
                            }
                            let WireMessage::CheckpointAgreement(packet) =
                                crate::wire_message::WireMessage::decode(&body).unwrap()
                            else {
                                panic!("unexpected agreement fixture frame")
                            };
                            let _ = sender.try_send(Incoming {
                                peer: peer.to_string(),
                                packet,
                            });
                        }
                    });
                }
            }
        }
        let mut workers = Vec::new();
        let signing_time = Arc::new(std::sync::atomic::AtomicU64::new(cutoff + 30));
        for stage in 0..3 {
            if expired && stage == 1 {
                signing_time.store(cutoff + 3601, std::sync::atomic::Ordering::SeqCst);
            }
            let range = match stage {
                0 => 0..6,
                1 => 6..7,
                _ => 7..9,
            };
            for slot in range {
                let index = active[slot];
                // Historical replay fixture, not evidence of a production clock.
                // The final two signers must catch up with no clock authority.
                let signing_time = Arc::clone(&signing_time);
                let clock: Clock = Arc::new(move || {
                    (stage != 2).then_some(SigningClock {
                        now: signing_time.load(std::sync::atomic::Ordering::SeqCst),
                        error_bound: 0,
                    })
                });
                if runtime {
                    drop(drivers[index].take());
                    workers.push(tokio::spawn(checkpoint_service::run_node(
                        checkpoint_service::Settings {
                            store: stores[index].as_ref().unwrap().clone(),
                            network: NetworkId::Dev,
                            initial_params: fixture.params.clone(),
                            key: Some(Arc::new(
                                burst_crypto::keypair_from_seed(&[index as u8; 32]).private,
                            )),
                        },
                        Arc::clone(&registries[index]),
                        receivers[index].take().unwrap(),
                        clock,
                        Arc::clone(&stops[index]),
                    )));
                } else {
                    let session = Session::new(
                        drivers[index].take().unwrap(),
                        Instant::now(),
                        Duration::from_secs(if expired { 2 } else { 60 }),
                    )
                    .unwrap();
                    workers.push(tokio::spawn(checkpoint_service::run(
                        session,
                        Arc::clone(&registries[index]),
                        receivers[index].take().unwrap(),
                        clock,
                        Arc::clone(&stops[index]),
                    )));
                }
            }
            if stage == 0 {
                tokio::time::timeout(Duration::from_secs(60), async {
                    loop {
                        if active[..6].iter().all(|&index| {
                            crate::checkpoint_agreement::read_state(
                                stores[index].as_ref().unwrap(),
                                &anchor,
                                &address(index as u8),
                            )
                            .unwrap()
                            .is_some_and(|state| state.prevote().is_some())
                        }) {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                })
                .await
                .expect("leader must have started agreement");
                for &index in &active[..6] {
                    assert_eq!(
                        CertifiedCheckpoint::stored_tip(
                            stores[index].as_ref().unwrap(),
                            NetworkId::Dev,
                            &fixture.params
                        )
                        .unwrap(),
                        1
                    );
                }
            } else {
                let participants = if stage == 1 {
                    &active[..7]
                } else {
                    &active[..]
                };
                let deadline = Instant::now() + Duration::from_secs(120);
                loop {
                    if participants.iter().all(|index| {
                        CertifiedCheckpoint::stored_tip(
                            stores[*index].as_ref().unwrap(),
                            NetworkId::Dev,
                            &fixture.params,
                        )
                        .unwrap()
                            == 2
                    }) {
                        break;
                    }
                    if Instant::now() >= deadline {
                        let states: Vec<_> = participants
                            .iter()
                            .map(|index| {
                                let state = crate::checkpoint_agreement::read_state(
                                    stores[*index].as_ref().unwrap(),
                                    &anchor,
                                    &address(*index as u8),
                                )
                                .unwrap()
                                .unwrap();
                                (
                                    *index,
                                    state.round(),
                                    state.step(),
                                    state.prevote().cloned(),
                                    state.precommit().cloned(),
                                    state.valid_proof().map(|proof| proof.ballot.round),
                                )
                            })
                            .collect();
                        panic!("agreement did not complete at stage {stage}: {states:?}");
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
        }
        for stop in &stops {
            stop.shutdown();
        }
        for worker in workers {
            worker.await.unwrap().unwrap();
        }
        readers.abort_all();
        while readers.join_next().await.is_some() {}
        for index in active {
            if runtime {
                let certified = CertifiedCheckpoint::load(
                    stores[index].as_ref().unwrap(),
                    NetworkId::Dev,
                    &fixture.params,
                )
                .unwrap()
                .unwrap();
                assert_eq!(certified.certificate().checkpoint, manifests[0].checkpoint);
                anchor.verify_certificate(certified.certificate()).unwrap();
                continue;
            }
            let state = crate::checkpoint_agreement::read_state(
                stores[index].as_ref().unwrap(),
                &anchor,
                &address(index as u8),
            )
            .unwrap()
            .unwrap();
            if expired {
                let decision = state.decision().unwrap();
                assert!(decision.round > 0);
                assert!(decision.checkpoint.cutoff > manifests[0].checkpoint.cutoff);
                assert_eq!(decision.checkpoint.parent, manifests[0].checkpoint.parent);
                let leader_state = crate::checkpoint_agreement::read_state(
                    stores[leader].as_ref().unwrap(),
                    &anchor,
                    &address(leader as u8),
                )
                .unwrap()
                .unwrap();
                assert_eq!(
                    decision.checkpoint,
                    leader_state.decision().unwrap().checkpoint
                );
            } else {
                assert_eq!(
                    state.decision().unwrap().checkpoint,
                    manifests[0].checkpoint
                );
            }
            anchor
                .verify_certificate(state.decision().unwrap())
                .unwrap();
        }
    }

    #[test]
    fn checkpoint_driver_collects_human_quorums_and_recovers_late_prepared_choices() {
        use crate::checkpoint_driver::{CheckpointDriver, Message};
        use crate::checkpoint_replay::SigningClock;
        use crate::checkpoint_store::CertifiedCheckpoint;
        use burst_consensus::checkpoint::CheckpointSignature;
        use burst_consensus::checkpoint_round::{Ballot, Phase};
        use std::collections::VecDeque;
        use std::sync::Arc;

        fn deliver(
            drivers: &mut [Option<CheckpointDriver>],
            active: &[usize],
            messages: Vec<Message>,
            clock: Option<SigningClock>,
        ) -> Vec<Message> {
            let mut pending: VecDeque<_> = messages.into();
            let mut transcript = Vec::new();
            while let Some(message) = pending.pop_front() {
                assert!(
                    transcript.len() < 2000,
                    "agreement must not produce a duplicate-message loop"
                );
                for &index in active {
                    let driver = drivers[index].as_mut().unwrap();
                    pending.extend(driver.receive(message.clone(), clock).unwrap());
                    pending.extend(driver.receive(message.clone(), clock).unwrap());
                }
                transcript.push(message);
            }
            transcript
        }

        let DriverFixture {
            fixture,
            dirs,
            mut stores,
            mut drivers,
            manifests,
            leader,
            cutoff,
        } = checkpoint_drivers();
        let active: Vec<usize> = (0..7).map(|i| (leader + i) % 9).collect();
        let late = (leader + 7) % 9;
        let restarted = (leader + 8) % 9;
        let clock = Some(SigningClock {
            now: cutoff + 30,
            error_bound: 0,
        });
        let initial = drivers[leader].as_mut().unwrap().propose(clock).unwrap();
        let proposal = initial
            .iter()
            .find(|message| matches!(message, Message::Proposal { .. }))
            .unwrap()
            .clone();
        deliver(&mut drivers, &active[..6], initial, clock);
        for &index in &active[..6] {
            assert!(
                drivers[index]
                    .as_mut()
                    .unwrap()
                    .timeout(burst_consensus::checkpoint_round::Step::Prevote)
                    .is_err(),
                "six humans cannot use a timeout to stand in for seven"
            );
            assert!(drivers[index]
                .as_ref()
                .unwrap()
                .state()
                .unwrap()
                .unwrap()
                .decision()
                .is_none());
            assert_eq!(
                CertifiedCheckpoint::load(
                    stores[index].as_ref().unwrap(),
                    NetworkId::Dev,
                    &fixture.params
                )
                .unwrap()
                .unwrap()
                .certificate()
                .checkpoint
                .height,
                1
            );
        }
        let mut retries = vec![proposal.clone()];
        for &index in &active[..6] {
            retries.extend(drivers[index].as_mut().unwrap().resume().unwrap());
        }
        let transcript = deliver(&mut drivers, &active, retries, clock);
        let prepared = transcript
            .iter()
            .find(|message| matches!(message, Message::Prepared { .. }))
            .unwrap()
            .clone();
        let decision = transcript
            .iter()
            .find(|message| matches!(message, Message::Decision { .. }))
            .unwrap()
            .clone();

        for index in [late, restarted] {
            assert!(
                drivers[index]
                    .as_mut()
                    .unwrap()
                    .receive(proposal.clone(), None)
                    .is_err(),
                "no fresh value signature without a pre-beacon clock bound"
            );
            assert!(drivers[index].as_ref().unwrap().state().unwrap().is_none());
            let replies = drivers[index]
                .as_mut()
                .unwrap()
                .receive(prepared.clone(), None)
                .unwrap();
            assert!(replies
                .iter()
                .any(|message| matches!(message, Message::Vote(vote)
                if vote.ballot.phase == Phase::Prevote && vote.ballot.value.is_none())));
            assert!(replies
                .iter()
                .any(|message| matches!(message, Message::Vote(vote)
                if vote.ballot.phase == Phase::Precommit && vote.ballot.value.is_some())));
        }
        drop(drivers[restarted].take());
        drop(stores[restarted].take());
        let reopened = Arc::new(LmdbStore::open(dirs[restarted].path(), 32, 32 << 20).unwrap());
        let replay =
            CertifiedCheckpoint::replay_manifest(&reopened, &fixture.params, &manifests[restarted])
                .unwrap();
        let mut restored = CheckpointDriver::new(
            Arc::clone(&reopened),
            fixture.params.clone(),
            burst_crypto::keypair_from_seed(&[restarted as u8; 32]).private,
            replay,
        )
        .unwrap();
        let resumed = restored.resume().unwrap();
        assert!(resumed.iter().any(|message| matches!(message, Message::Vote(vote)
            if vote.ballot.phase == Phase::Precommit && vote.ballot.value == Some(manifests[restarted].checkpoint.hash()))));
        drivers[restarted] = Some(restored);
        stores[restarted] = Some(reopened);
        let future: Vec<_> = active[..3]
            .iter()
            .map(|&seed| {
                let mut ballot = Ballot::precommit(&manifests[late].checkpoint, 5);
                ballot.phase = Phase::Prevote;
                ballot.value = None;
                let signer = CheckpointSignature {
                    representative: address(seed as u8),
                    signature: burst_crypto::sign_message(
                        &ballot.signing_payload(),
                        &burst_crypto::keypair_from_seed(&[seed as u8; 32]).private,
                    ),
                };
                burst_consensus::checkpoint_round::SignedBallot { ballot, signer }
            })
            .collect();
        let late_driver = drivers[late].as_mut().unwrap();
        let before = late_driver.state().unwrap();
        assert!(late_driver
            .receive(
                Message::CatchUp {
                    round: 5,
                    evidence: future[..2].to_vec()
                },
                None
            )
            .is_err());
        assert_eq!(late_driver.state().unwrap(), before);
        let mut duplicated = future[..2].to_vec();
        duplicated.push(future[0].clone());
        assert!(late_driver
            .receive(
                Message::CatchUp {
                    round: 5,
                    evidence: duplicated
                },
                None
            )
            .is_err());
        for vote in &future[..2] {
            assert!(late_driver
                .receive(Message::Vote(vote.clone()), None)
                .unwrap()
                .is_empty());
            assert!(late_driver
                .receive(Message::Vote(vote.clone()), None)
                .unwrap()
                .is_empty());
        }
        assert_eq!(late_driver.round(), 0);
        let recovered = late_driver
            .receive(Message::Vote(future[2].clone()), None)
            .unwrap();
        assert!(recovered
            .iter()
            .any(|message| matches!(message, Message::CatchUp { round: 5, .. })));
        assert_eq!(late_driver.round(), 5);
        assert_eq!(
            late_driver
                .state()
                .unwrap()
                .unwrap()
                .valid_proof()
                .unwrap()
                .ballot
                .round,
            0
        );
        for index in [late, restarted] {
            drivers[index]
                .as_mut()
                .unwrap()
                .receive(decision.clone(), None)
                .unwrap();
        }
        for store in stores.into_iter().flatten() {
            let accepted = CertifiedCheckpoint::load(&store, NetworkId::Dev, &fixture.params)
                .unwrap()
                .unwrap();
            assert_eq!(accepted.certificate().checkpoint, manifests[0].checkpoint);
            assert_eq!(accepted.replayed().roster().total_humans(), 9);
        }
    }

    #[test]
    fn checkpoint_chain_hands_off_human_authority_without_resampling_pending_requests() {
        use crate::checkpoint_store::CertifiedCheckpoint;
        use burst_consensus::checkpoint::{Checkpoint, CheckpointCertificate, CheckpointSignature};
        use burst_consensus::checkpoint_round::Ballot;

        let certify = |checkpoint: &Checkpoint, seeds: &[u8]| CheckpointCertificate {
            checkpoint: checkpoint.clone(),
            round: 0,
            signatures: seeds
                .iter()
                .map(|seed| CheckpointSignature {
                    representative: address(*seed),
                    signature: burst_crypto::sign_message(
                        &Ballot::precommit(checkpoint, 0).signing_payload(),
                        &burst_crypto::keypair_from_seed(&[*seed; 32]).private,
                    ),
                })
                .collect(),
        };
        let mut fixture = Fixture::selection();
        for seed in [3, 4] {
            fixture.push(seed, BlockType::Endorse, 1_595_431_106, |block| {
                block.brn_balance += 1;
                block.link = destination(21);
            });
        }
        let (dir, store, heads) = fixture.store();
        let cutoff = 1_595_434_050;
        let first_replay = CertifiedCheckpoint::prepare(
            &store,
            NetworkId::Dev,
            &fixture.params,
            cutoff,
            &heads,
            &address(20),
        )
        .unwrap();
        let first_certificate = certify(first_replay.checkpoint(), &[0]);
        let mut first = CertifiedCheckpoint::accept(
            &store,
            NetworkId::Dev,
            &fixture.params,
            first_certificate.clone(),
            heads.clone(),
            address(20),
        )
        .unwrap();
        assert!(
            CertifiedCheckpoint::prepare(
                &store,
                NetworkId::Dev,
                &fixture.params,
                cutoff + 30,
                &heads,
                &address(20)
            )
            .is_err(),
            "a later beacon cannot resample a pending attempt"
        );
        assert!(
            CertifiedCheckpoint::prepare(
                &store,
                NetworkId::Dev,
                &fixture.params,
                cutoff + 30,
                &heads[..heads.len() - 1],
                &address(21)
            )
            .is_err(),
            "parent accounts cannot disappear"
        );
        let second_replay = CertifiedCheckpoint::prepare(
            &store,
            NetworkId::Dev,
            &fixture.params,
            cutoff + 30,
            &heads,
            &address(21),
        )
        .unwrap();
        assert_eq!(
            second_replay.checkpoint().parent,
            first_certificate.checkpoint.hash()
        );
        assert_eq!(second_replay.checkpoint().height, 2);
        for signers in [&[0][..], &[0, 1, 2, 3, 4, 5][..]] {
            assert!(CertifiedCheckpoint::accept(
                &store,
                NetworkId::Dev,
                &fixture.params,
                certify(second_replay.checkpoint(), signers),
                heads.clone(),
                address(21)
            )
            .is_err());
        }
        let second_certificate = certify(second_replay.checkpoint(), &[0, 1, 2, 3, 4, 5, 6]);
        let second = CertifiedCheckpoint::accept(
            &store,
            NetworkId::Dev,
            &fixture.params,
            second_certificate.clone(),
            heads.clone(),
            address(21),
        )
        .unwrap();
        assert_eq!(second.parent().roster().total_humans(), 9);
        assert_eq!(second.parent().required_humans(), 7);
        assert!(second.beacon().is_none());
        let beacons: Vec<burst_vrf::DrandBeacon> =
            serde_json::from_str(include_str!("../tests/fixtures/selection-beacons.json")).unwrap();
        first
            .attach_beacon(
                &store,
                &fixture.params,
                burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap(),
            )
            .unwrap();
        let latest = CertifiedCheckpoint::load(&store, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap();
        assert_eq!(latest.certificate(), &second_certificate);
        assert!(
            latest.beacon().is_none(),
            "older beacon cannot populate a later request"
        );
        assert!(CertifiedCheckpoint::accept(
            &store,
            NetworkId::Dev,
            &fixture.params,
            first_certificate.clone(),
            heads.clone(),
            address(20)
        )
        .unwrap()
        .beacon()
        .is_some());

        let (_observer_dir, observer, _) = fixture.store();
        assert!(
            CertifiedCheckpoint::accept(
                &observer,
                NetworkId::Dev,
                &fixture.params,
                second_certificate.clone(),
                heads.clone(),
                address(21)
            )
            .is_err(),
            "cannot skip the trusted parent"
        );
        CertifiedCheckpoint::accept(
            &observer,
            NetworkId::Dev,
            &fixture.params,
            first_certificate.clone(),
            heads.clone(),
            address(20),
        )
        .unwrap();
        let imported = CertifiedCheckpoint::accept(
            &observer,
            NetworkId::Dev,
            &fixture.params,
            second_certificate.clone(),
            heads.clone(),
            address(21),
        )
        .unwrap();
        assert_eq!(imported.replayed().request(), second.replayed().request());

        fixture.push(5, BlockType::Endorse, cutoff + 1, |block| {
            block.payment_epoch = first_certificate.checkpoint.hash();
            block.brn_balance += 1;
            block.link = destination(20);
        });
        let (_changed_dir, changed_store, changed_heads) = fixture.store();
        CertifiedCheckpoint::accept(
            &changed_store,
            NetworkId::Dev,
            &fixture.params,
            first_certificate,
            heads,
            address(20),
        )
        .unwrap();
        let changed = CertifiedCheckpoint::prepare(
            &changed_store,
            NetworkId::Dev,
            &fixture.params,
            cutoff + 30,
            &changed_heads,
            &address(21),
        );
        assert!(changed
            .err()
            .unwrap()
            .to_string()
            .contains("frozen selection"));
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let reopened = CertifiedCheckpoint::load(&store, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap();
        assert_eq!(reopened.certificate(), &second_certificate);
        let tip_key = format!(
            "checkpoint_chain/v1/dev/{}/head",
            second_certificate.checkpoint.genesis
        );
        store
            .meta_store()
            .put_meta(&tip_key, &1u64.to_be_bytes())
            .unwrap();
        assert!(
            CertifiedCheckpoint::load(&store, NetworkId::Dev, &fixture.params).is_err(),
            "a regressed head cannot hide a later certificate"
        );
        store
            .meta_store()
            .put_meta(&tip_key, &u64::MAX.to_be_bytes())
            .unwrap();
        assert!(CertifiedCheckpoint::load(&store, NetworkId::Dev, &fixture.params).is_err());
        store
            .meta_store()
            .put_meta(&tip_key, &2u64.to_be_bytes())
            .unwrap();
        assert!(
            CertifiedCheckpoint::load(&store, NetworkId::Dev, &fixture.params)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn replays_burn_receive_merge_send_rejection_and_delegation_without_afterimages() {
        let fixture = Fixture::payments();
        let replay = fixture.replay(200).unwrap();
        let creator = replay.account(&address(0)).unwrap();
        assert_eq!(creator.trst_balance, 45);
        assert_eq!(creator.total_brn_burned, 60);
        assert_eq!(creator.state, WalletState::Verified);
        assert!(!creator.orv_evicted);
        assert_eq!(replay.brn_balance(&address(0)).unwrap(), 140);
        let recipient = replay.account(&address(1)).unwrap();
        assert_eq!(recipient.state, WalletState::Unverified);
        assert_eq!(recipient.trst_balance, 10);
        assert_eq!(recipient.representative, address(0));
        assert_eq!(replay.brn_balance(&address(1)).unwrap(), 0);
        assert_eq!(recipient.block_count, recipient.confirmation_height);
        let pending = replay
            .pending(&address(1), fixture.blocks[12].hash.into_tx_hash())
            .unwrap();
        assert_eq!(pending.amount, 5);
        assert_eq!(
            pending.provenance[0].effective_origin_timestamp,
            Timestamp::new(100)
        );
        let merge = replay
            .merger_graph()
            .get_merge(&fixture.blocks[6].hash.into_tx_hash())
            .unwrap();
        assert_eq!(merge.total_amount, 60);
        assert_eq!(merge.source_origins.len(), 2);
        assert_eq!(
            replay
                .portfolio(&address(0))
                .unwrap()
                .tokens
                .iter()
                .map(|token| token.amount)
                .sum::<u128>(),
            45
        );
        assert_eq!(replay.pending.len(), 1);
    }

    #[tokio::test]
    async fn genesis_verification_rules_match_live_and_replay() {
        let mut fixture = Fixture::new();
        fixture.params.neither_penalty_threshold_bps = 2500;
        fixture.params.neither_window_assignments = 17;
        fixture.blocks[0] = crate::genesis_state::block(NetworkId::Dev, &fixture.params);
        let replay = fixture.replay(1).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let node = crate::BurstNode::new(crate::NodeConfig {
            network: NetworkId::Dev,
            data_dir: directory.path().to_path_buf(),
            params: fixture.params,
            ..crate::NodeConfig::default()
        })
        .await
        .unwrap();
        node.initialize_genesis().unwrap();
        let batch = node.store.tx_begin_write().unwrap();
        let live = crate::endorsement_commit::restore(&batch).unwrap();
        assert_eq!(
            live.snapshot().encode().unwrap(),
            replay.verification.snapshot().encode().unwrap()
        );
    }

    #[test]
    fn replay_matches_the_persisted_payment_transition() {
        let fixture = Fixture::payments();
        let replay = fixture.replay(200).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let genesis = &fixture.blocks[0];
        let account =
            crate::genesis_state::account_to_write(None, genesis.account.clone(), genesis.hash)
                .unwrap();
        let mut brn = BrnEngine::with_rate(1, Timestamp::EPOCH);
        brn.track_wallet(
            genesis.account.clone(),
            BrnWalletState::new(Timestamp::EPOCH),
        );
        let mut batch = store.tx_begin_write().unwrap();
        crate::payment_epoch::initialize(&mut batch, genesis.hash).unwrap();
        batch
            .put_block_with_account(
                &genesis.hash,
                &bincode::serialize(genesis).unwrap(),
                &genesis.account,
                1,
            )
            .unwrap();
        batch.put_account_info(&account, false).unwrap();
        ledger_updater::persist_economics(
            &mut batch,
            &brn,
            &TrstEngine::with_expiry(fixture.params.trst_expiry_secs),
        )
        .unwrap();
        batch.commit().unwrap();
        for block in &fixture.blocks[1..] {
            let mut batch = store.tx_begin_write().unwrap();
            crate::payment_transition::apply(&mut batch, block, &fixture.params, block.timestamp)
                .unwrap();
            batch.commit().unwrap();
        }
        for seed in [0, 1] {
            let mut persisted = store.account_store().get_account(&address(seed)).unwrap();
            let replayed = replay.account(&address(seed)).unwrap();
            persisted.confirmation_height = replayed.confirmation_height;
            assert_eq!(
                bincode::serialize(&persisted).unwrap(),
                bincode::serialize(replayed).unwrap()
            );
        }
        assert_eq!(
            store.meta_store().get_meta(TrstEngine::meta_key()).unwrap(),
            replay.trst.save_wallets()
        );
        assert_eq!(
            store.meta_store().get_meta("merger_graph").unwrap(),
            replay.trst.merger_graph.to_bytes()
        );
        let pending = store.pending_store().get_all_pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            bincode::serialize(&pending[0].2).unwrap(),
            bincode::serialize(replay.pending(&pending[0].0, pending[0].1).unwrap()).unwrap()
        );
    }

    #[test]
    fn receive_open_preserves_origin_and_never_grants_identity_or_accrual() {
        let mut fixture = Fixture::new();
        let burn = fixture.push(0, BlockType::Burn, 100, |block| {
            block.brn_balance = 40;
            block.link = destination(2);
        });
        fixture.push(2, BlockType::Open, 101, |block| {
            block.link = burn;
            block.trst_balance = 40;
        });
        let replay = fixture.replay(200).unwrap();
        let token = &replay.portfolio(&address(2)).unwrap().tokens[0];
        assert_eq!(token.origin, burn.into_tx_hash());
        assert_eq!(token.origin_wallet, address(0));
        assert_eq!(token.amount, 40);
        assert_eq!(
            replay.account(&address(2)).unwrap().state,
            WalletState::Unverified
        );
        fixture.push(2, BlockType::Send, 102, |block| {
            block.trst_balance = 30;
            block.origin = burn.into_tx_hash();
            block.link = destination(0);
        });
        fixture.assert_rejected("spending requires a verified wallet");
    }

    #[test]
    fn replay_rejects_a_correctly_signed_block_from_another_payment_epoch() {
        for epoch in [BlockHash::ZERO, BlockHash::new([88; 32])] {
            let mut fixture = Fixture::new();
            fixture.push(0, BlockType::ChangeRepresentative, 100, |block| {
                block.payment_epoch = epoch;
            });
            fixture.assert_rejected("block payment epoch");
        }
    }

    #[test]
    fn authenticates_signatures_parameters_work_and_trusted_genesis() {
        let mut fixture = Fixture::new();
        fixture.push(0, BlockType::ChangeRepresentative, 100, |_| {});
        let history = fixture.history(200);
        assert!(ConfirmedReplay::replay(&history, NetworkId::Test, &fixture.params).is_err());
        let mut wrong_params = fixture.params.clone();
        wrong_params.brn_rate += 1;
        assert!(ConfirmedReplay::replay(&history, NetworkId::Dev, &wrong_params).is_err());
        fixture.blocks.last_mut().unwrap().signature = Signature([0; 64]);
        fixture.assert_rejected("signature");
        let last = fixture.blocks.last_mut().unwrap();
        last.params_hash = BlockHash::new([9; 32]);
        sign(0, last);
        fixture.assert_rejected("parameters");

        let mut fixture = Fixture::new();
        fixture.params.min_work_difficulty = u64::MAX;
        fixture.blocks[0] = crate::genesis_state::block(NetworkId::Dev, &fixture.params);
        fixture.push(0, BlockType::ChangeRepresentative, 100, |_| {});
        fixture.assert_rejected("proof of work");
    }

    #[test]
    fn rejects_signed_inflation_overspending_and_repeated_claims() {
        let mut fixture = Fixture::new();
        fixture.push(0, BlockType::Burn, 100, |block| block.brn_balance = 101);
        fixture.assert_rejected("BRN burn failed");

        let mut fixture = Fixture::new();
        let burn = fixture.push(0, BlockType::Burn, 100, |block| {
            block.brn_balance = 40;
            block.link = destination(0);
        });
        fixture.push(0, BlockType::Receive, 101, |block| {
            block.link = burn;
            block.trst_balance = 41;
        });
        fixture.assert_rejected("receive balance");
        let last = fixture.blocks.last_mut().unwrap();
        last.trst_balance = 40;
        sign(0, last);
        fixture.push(0, BlockType::Receive, 102, |block| {
            block.link = burn;
            block.trst_balance = 80;
        });
        fixture.assert_rejected("missing replayed pending");
    }

    #[test]
    fn refuses_every_unimplemented_lifecycle_instead_of_skipping_it() {
        for kind in [BlockType::GovernanceActivation] {
            let mut fixture = Fixture::new();
            let hash = fixture.push(0, kind.clone(), 100, |_| {});
            match fixture.replay(200) {
                Err(ReplayError::UnsupportedBlock {
                    kind: actual,
                    hash: actual_hash,
                }) => {
                    assert_eq!(actual, kind);
                    assert_eq!(actual_hash, hash);
                }
                _ => panic!("unsupported history must not be labelled invalid or accepted"),
            }
        }
    }

    #[test]
    fn refuses_pending_expiry_races_but_preserves_received_expired_reputation() {
        let mut fixture = Fixture::new();
        let burn = fixture.push(0, BlockType::Burn, 100, |block| {
            block.brn_balance = 40;
            block.link = destination(0);
        });
        assert!(fixture.replay(1099).is_ok());
        assert!(matches!(
            fixture.replay(1100),
            Err(ReplayError::PendingExpiry)
        ));
        fixture.push(0, BlockType::Receive, 1100, |block| {
            block.link = burn;
            block.trst_balance = 40;
        });
        assert!(matches!(
            fixture.replay(1100),
            Err(ReplayError::PendingExpiry)
        ));
        let last = fixture.blocks.last_mut().unwrap();
        last.timestamp = Timestamp::new(101);
        sign(0, last);
        let replay = fixture.replay(1100).unwrap();
        let account = replay.account(&address(0)).unwrap();
        assert_eq!((account.trst_balance, account.expired_trst), (40, 40));
        let portfolio = replay.portfolio(&address(0)).unwrap();
        assert_eq!(portfolio.cached_transferable, 0);
        assert_eq!(portfolio.tokens[0].state, burst_types::TrstState::Expired);
        fixture.push(0, BlockType::Send, 1100, |block| {
            block.trst_balance = 30;
            block.link = destination(1);
            block.origin = burn.into_tx_hash();
        });
        assert!(fixture.replay(1100).is_err());
    }

    #[test]
    fn corrupt_history_encoding_is_rejected_before_replay() {
        let fixture = Fixture::new();
        let (_dir, store, heads) = fixture.store();
        let genesis = &fixture.blocks[0];
        let mut bytes = bincode::serialize(genesis).unwrap();
        bytes.push(0);
        store
            .block_store()
            .put_block(&genesis.hash, &bytes)
            .unwrap();
        assert!(
            ConfirmedHistory::read(&store, NetworkId::Dev, genesis.hash, 200, &heads, &[]).is_err()
        );
    }

    #[test]
    fn ignores_provisional_successors_and_manifest_order_without_writing_the_source() {
        let fixture = Fixture::payments();
        let (_dir, store, mut heads) = fixture.store();
        let read = |heads: &[HistoryHead]| {
            ConfirmedHistory::read(
                &store,
                NetworkId::Dev,
                fixture.blocks[0].hash,
                200,
                heads,
                &[],
            )
            .unwrap()
        };
        let before = read(&heads);
        heads.reverse();
        let mut provisional = fixture.blocks[12].clone();
        provisional.previous = provisional.hash;
        provisional.block_type = BlockType::ChangeRepresentative;
        provisional.representative = address(9);
        provisional.timestamp = Timestamp::new(201);
        provisional.origin = TxHash::ZERO;
        provisional.link = BlockHash::ZERO;
        sign(0, &mut provisional);
        let mut account = store.account_store().get_account(&address(0)).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_block_with_account(
                &provisional.hash,
                &bincode::serialize(&provisional).unwrap(),
                &provisional.account,
                account.block_count + 1,
            )
            .unwrap();
        account.head = provisional.hash;
        account.block_count += 1;
        batch.put_account_info(&account, false).unwrap();
        batch.commit().unwrap();
        let after = read(&heads);
        assert_eq!(before.commitment(), after.commitment());
        let first = ConfirmedReplay::replay(&before, NetworkId::Dev, &fixture.params).unwrap();
        let second = ConfirmedReplay::replay(&after, NetworkId::Dev, &fixture.params).unwrap();
        assert_eq!(first.trst.save_wallets(), second.trst.save_wallets());
        for seed in [0, 1] {
            assert_eq!(
                bincode::serialize(first.account(&address(seed)).unwrap()).unwrap(),
                bincode::serialize(second.account(&address(seed)).unwrap()).unwrap()
            );
        }
        assert_eq!(
            store.account_store().get_account(&address(0)).unwrap().head,
            provisional.hash
        );
        assert_eq!(
            store.meta_store().get_meta(TrstEngine::meta_key()).unwrap(),
            b"invalid snapshot"
        );
    }

    #[test]
    fn address_valued_link_does_not_consume_a_pending_claim() {
        let mut fixture = Fixture::new();
        let burn = fixture.push(0, BlockType::Burn, 100, |block| {
            block.brn_balance = 40;
            block.link = destination(0);
        });
        fixture.push(0, BlockType::ChangeRepresentative, 101, |block| {
            block.link = burn
        });
        assert_eq!(
            fixture
                .replay(200)
                .unwrap()
                .pending(&address(0), burn.into_tx_hash())
                .unwrap()
                .amount,
            40
        );
    }

    #[test]
    fn wrong_origin_and_unheld_merge_inputs_cannot_create_provenance() {
        let mut fixture = Fixture::payments();
        fixture.push(0, BlockType::Send, 112, |block| {
            block.trst_balance = 40;
            block.link = destination(1);
            block.origin = TxHash::new([99; 32]);
        });
        fixture.assert_rejected("insufficient transferable origin balance");
        fixture.blocks.pop();
        fixture.push(0, BlockType::Merge, 112, |block| {
            block.merge_sources = vec![TxHash::new([99; 32])];
        });
        fixture.assert_rejected("is not held by this wallet");
    }

    #[test]
    fn creator_verification_accrues_from_the_endorsement_not_the_first_account_block() {
        for opened_at in [90, 110] {
            let mut fixture = Fixture::new();
            fixture.push(0, BlockType::Endorse, 100, |block| {
                block.brn_balance = 1;
                block.link = destination(1);
            });
            let unopened = fixture.replay(100).unwrap();
            let account = unopened.account(&address(1)).unwrap();
            assert_eq!(account.state, WalletState::Verified);
            assert_eq!((account.block_count, account.head), (0, BlockHash::ZERO));
            assert_eq!(unopened.brn_balance(&address(1)).unwrap(), 0);
            assert!(
                unopened.portfolio(&address(0)).is_none(),
                "no endorser reward"
            );
            fixture.push(1, BlockType::Open, opened_at, |_| {});
            let burn = fixture.push(1, BlockType::Burn, 120, |block| {
                block.brn_balance = 10;
                block.link = destination(2);
            });
            fixture.push(2, BlockType::Open, 121, |block| {
                block.trst_balance = 10;
                block.link = burn;
            });
            fixture.push(1, BlockType::VerifierOptIn, 122, |_| {});
            let opted_in = fixture.replay(150).unwrap();
            assert_eq!(
                opted_in.account(&address(1)).unwrap().verifier_opted_in_at,
                Some(Timestamp::new(122))
            );
            fixture.push(1, BlockType::VerifierOptOut, 123, |_| {});
            let replay = fixture.replay(150).unwrap();
            let account = replay.account(&address(1)).unwrap();
            assert_eq!(account.verified_at, Some(Timestamp::new(100)));
            assert_eq!(account.verifier_opted_in_at, None);
            assert_eq!(account.total_brn_burned, 10);
            assert_eq!(replay.brn_balance(&address(1)).unwrap(), 40);
            assert_eq!(replay.brn_balance(&address(0)).unwrap(), 149);
            assert_eq!(
                replay.account(&address(2)).unwrap().state,
                WalletState::Unverified
            );
            assert_eq!(replay.account(&address(2)).unwrap().trst_balance, 10);
            assert_eq!(
                replay.verification(&address(1)).unwrap().phase,
                burst_verification::state::VerificationPhase::Verified
            );
        }
    }

    #[test]
    fn bootstrap_spending_order_is_independent_of_hash_sorted_manifest_walk() {
        // Vary signed hashes so both possible relative positions are exercised.
        let mut observed_spend_first = false;
        let mut observed_endorse_first = false;
        for nonce in 0..16u8 {
            let mut fixture = Fixture::new();
            let head = fixture.push(1, BlockType::Open, 90, |block| {
                block.transaction = TxHash::new([nonce; 32])
            });
            let endorsement = fixture.push(0, BlockType::Endorse, 100, |block| {
                block.brn_balance = 1;
                block.link = destination(1);
                block.target_head = head;
            });
            let spent = fixture.push(1, BlockType::Burn, 120, |block| block.brn_balance = 10);
            let history = fixture.history(150);
            let index = |hash| {
                history
                    .replay_blocks()
                    .iter()
                    .position(|block| block.hash == hash)
                    .unwrap()
            };
            observed_spend_first |= index(spent) < index(endorsement);
            observed_endorse_first |= index(endorsement) < index(spent);
            let replay =
                ConfirmedReplay::replay(&history, NetworkId::Dev, &fixture.params).unwrap();
            assert_eq!(replay.brn_balance(&address(1)).unwrap(), 40);
        }
        assert!(observed_spend_first && observed_endorse_first);
    }

    #[test]
    fn rejects_bootstrap_cycles_wrong_target_and_duplicate_identity() {
        let mut fixture = Fixture::new();
        fixture.push(1, BlockType::Open, 90, |_| {});
        let spent = fixture.push(1, BlockType::Burn, 120, |block| block.brn_balance = 10);
        fixture.push(0, BlockType::Endorse, 100, |block| {
            block.brn_balance = 1;
            block.link = destination(1);
            block.target_head = spent;
        });
        fixture.assert_rejected("cyclic history dependency");

        let mut fixture = Fixture::new();
        let foreign = fixture.push(2, BlockType::Open, 90, |_| {});
        fixture.push(0, BlockType::Endorse, 100, |block| {
            block.brn_balance = 1;
            block.link = destination(1);
            block.target_head = foreign;
        });
        fixture.assert_rejected("target head does not match");
        let last = fixture.blocks.last_mut().unwrap();
        last.target_head = BlockHash::ZERO;
        sign(0, last);
        fixture.push(0, BlockType::Endorse, 101, |block| {
            block.brn_balance = 2;
            block.link = destination(1);
        });
        fixture.assert_rejected("duplicate or self bootstrap verification");
    }

    #[test]
    fn normal_endorsements_are_not_promoted_to_creator_verifications() {
        let mut fixture = Fixture::new();
        fixture.params.bootstrap_exit_threshold = 2;
        fixture.blocks[0] = crate::genesis_state::block(NetworkId::Dev, &fixture.params);
        fixture.push(0, BlockType::Endorse, 100, |block| {
            block.brn_balance = 1;
            block.link = destination(1);
        });
        assert_eq!(
            fixture
                .replay(150)
                .unwrap()
                .account(&address(1))
                .unwrap()
                .state,
            WalletState::Verified
        );
        fixture.push(0, BlockType::Endorse, 110, |block| {
            block.brn_balance = 2;
            block.link = destination(2);
        });
        let replay = fixture.replay(150).unwrap();
        assert!(replay.account(&address(2)).is_none());
        assert_eq!(replay.brn_balance(&address(2)).unwrap(), 0);
        assert_eq!(
            replay.verification(&address(2)).unwrap().endorsements.len(),
            1
        );
        fixture.push(1, BlockType::Open, 111, |_| {});
        fixture.push(1, BlockType::Endorse, 120, |block| {
            block.brn_balance = 1;
            block.link = destination(2);
        });
        let replay = fixture.replay(150).unwrap();
        let state = replay.verification(&address(2)).unwrap();
        assert_eq!(state.endorsements.len(), 2);
        assert_eq!(
            state.phase,
            burst_verification::state::VerificationPhase::Endorsing
        );
        assert!(state.selected_verifiers.is_empty());
        assert!(replay.account(&address(2)).is_none());
        assert_eq!(replay.brn_balance(&address(1)).unwrap(), 49);
        assert_eq!(replay.account(&address(1)).unwrap().total_brn_burned, 1);
        assert!(
            replay.portfolio(&address(1)).is_none(),
            "endorsement burns never mint rewards"
        );
        fixture.push(1, BlockType::Endorse, 121, |block| {
            block.brn_balance = 2;
            block.link = destination(2);
        });
        fixture.assert_rejected("already endorsed");
    }

    #[test]
    fn normal_endorsement_threshold_preserves_pending_identity_and_permanent_burns() {
        let mut fixture = Fixture::new();
        fixture.params.endorsement_threshold = 1;
        fixture.blocks[0] = crate::genesis_state::block(NetworkId::Dev, &fixture.params);
        fixture.push(0, BlockType::Endorse, 100, |block| {
            block.brn_balance = 1;
            block.link = destination(1);
        });
        fixture.push(1, BlockType::Open, 110, |_| {});
        fixture.push(1, BlockType::Endorse, 120, |block| {
            block.brn_balance = 5;
            block.link = destination(2);
        });
        let replay = fixture.replay(150).unwrap();
        let state = replay.verification(&address(2)).unwrap();
        assert_eq!(state.endorsements.len(), 1);
        assert_eq!(state.endorsements[0].burn_amount, 5);
        assert_eq!(state.endorsements[0].timestamp, Timestamp::new(120));
        assert!(state.votes.is_empty() && state.selected_verifiers.is_empty());
        assert_eq!(replay.brn_balance(&address(1)).unwrap(), 45);
        assert!(replay.account(&address(2)).is_none());
        fixture.push(2, BlockType::Open, 130, |_| {});
        fixture.push(2, BlockType::Burn, 140, |block| block.brn_balance = 1);
        fixture.assert_rejected("spending requires a verified wallet");
    }

    #[test]
    fn overlapping_creator_and_normal_admission_require_explicit_lifecycle_ordering() {
        let mut fixture = Fixture::new();
        fixture.push(0, BlockType::Endorse, 100, |block| {
            block.brn_balance = 1;
            block.link = destination(1);
        });
        fixture.push(1, BlockType::Open, 110, |_| {});
        fixture.push(1, BlockType::Endorse, 120, |block| {
            block.brn_balance = 1;
            block.link = destination(2);
        });
        fixture.push(0, BlockType::Endorse, 130, |block| {
            block.brn_balance = 2;
            block.link = destination(2);
        });
        assert!(matches!(
            fixture.replay(150),
            Err(ReplayError::UnsupportedBlock {
                kind: BlockType::Endorse,
                ..
            })
        ));
    }

    #[test]
    fn verification_does_not_waive_burns_signatures_or_opt_in_eligibility() {
        let mut fixture = Fixture::new();
        fixture.push(1, BlockType::Open, 90, |_| {});
        fixture.push(1, BlockType::VerifierOptIn, 91, |_| {});
        fixture.assert_rejected("spending requires a verified wallet");
        fixture.blocks.pop();
        fixture.push(0, BlockType::Endorse, 100, |block| {
            block.link = destination(1);
            block.brn_balance = 101;
        });
        fixture.assert_rejected("endorsement BRN burn failed");
        let last = fixture.blocks.last_mut().unwrap();
        last.brn_balance = 1;
        sign(0, last);
        last.signature = Signature([0; 64]);
        fixture.assert_rejected("signature");
    }
}
