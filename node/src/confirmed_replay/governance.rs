use super::*;

pub(super) fn supports(kind: &BlockType) -> bool {
    matches!(
        kind,
        BlockType::GovernanceProposal
            | BlockType::GovernanceEndorse
            | BlockType::GovernanceVote
            | BlockType::GovernanceWithdraw
            | BlockType::Delegate
            | BlockType::RevokeDelegation
    )
}

/// Collect proposal support before opening its voting window. Withdrawal and
/// support on independent chains are accepted only when their signed timestamps
/// agree on support preceding withdrawal. On the proposer's own chain, signed
/// ancestry orders equal timestamps; a later endorsement creates a causal cycle.
/// Ambiguous cuts cannot prepare a ballot.
pub(super) fn dependencies(
    history: &ConfirmedHistory,
) -> Result<HashMap<BlockHash, Vec<BlockHash>>, ReplayError> {
    block_dependencies(history.replay_blocks())
}

pub(super) fn block_dependencies(
    blocks: &[StateBlock],
) -> Result<HashMap<BlockHash, Vec<BlockHash>>, ReplayError> {
    collect_dependencies(
        blocks
            .iter()
            .map(|block| Ok(std::borrow::Cow::Borrowed(block))),
    )
}

pub(super) fn streamed_dependencies(
    blocks: &super::blocks::source::Source<'_>,
) -> Result<HashMap<BlockHash, Vec<BlockHash>>, ReplayError> {
    collect_dependencies(blocks.iter())
}

fn collect_dependencies<'a>(
    blocks: impl IntoIterator<Item = Result<std::borrow::Cow<'a, StateBlock>, burst_store::StoreError>>,
) -> Result<HashMap<BlockHash, Vec<BlockHash>>, ReplayError> {
    struct Entry {
        hash: BlockHash,
        account: WalletAddress,
        timestamp: Timestamp,
    }
    let mut endorsements: HashMap<BlockHash, Vec<Entry>> = HashMap::new();
    let mut withdrawals = HashMap::new();
    let mut ballots = Vec::new();
    for block in blocks {
        let block = block.map_err(|error| invalid(error.to_string()))?;
        if block.block_type == BlockType::GovernanceEndorse {
            endorsements.entry(block.link).or_default().push(Entry {
                hash: block.hash,
                account: block.account.clone(),
                timestamp: block.timestamp,
            });
        }
        if block.block_type == BlockType::GovernanceWithdraw
            && withdrawals.insert(block.link, block.hash).is_some()
        {
            return Err(invalid("multiple governance withdrawals"));
        }
        if matches!(
            block.block_type,
            BlockType::GovernanceVote | BlockType::GovernanceWithdraw
        ) {
            ballots.push((
                block.link,
                block.block_type == BlockType::GovernanceWithdraw,
                Entry {
                    hash: block.hash,
                    account: block.account.clone(),
                    timestamp: block.timestamp,
                },
            ));
        }
    }
    let mut result = HashMap::new();
    let mut budget = MAX_LIFECYCLE_DEPENDENCIES;
    for (link, withdraw, block) in ballots {
        let support = endorsements
            .get(&link)
            .map_or(&[][..], |blocks| blocks.as_slice());
        budget = budget
            .checked_sub(support.len() + usize::from(withdrawals.contains_key(&link)))
            .ok_or_else(|| invalid("governance dependency budget exceeded"))?;
        if withdraw
            && support.iter().any(|support| {
                support.timestamp > block.timestamp
                    || (support.timestamp == block.timestamp && support.account != block.account)
            })
        {
            return Err(invalid(
                "ambiguous governance support and withdrawal ordering",
            ));
        }
        let mut required: Vec<_> = support.iter().map(|support| support.hash).collect();
        if !withdraw {
            required.extend(withdrawals.get(&link).copied());
        }
        required.sort_by_key(|hash| *hash.as_bytes());
        result.insert(block.hash, required);
    }
    Ok(result)
}

impl ConfirmedReplay {
    pub(crate) fn next_governance_cut(
        &self,
        through: Timestamp,
    ) -> Result<Option<u64>, ReplayError> {
        Ok(self
            .governance_counting_cuts(through)?
            .into_iter()
            .chain(self.governance_activation_cuts())
            .map(|(_, at)| at)
            .filter(|at| *at <= through.as_secs())
            .min())
    }

    /// Reconstruct a ballot candidate from a signed, closed genesis-epoch cut.
    /// This helper returns a ballot, not a certificate. Certified event replay
    /// separately validates and installs the batch of ballots due at that cut.
    pub fn prepare_governance_ballot(
        history: &ConfirmedHistory,
        network: NetworkId,
        initial_params: &ProtocolParams,
        proposal: TxHash,
    ) -> Result<burst_governance::ballot::PreparedBallot, ReplayError> {
        let replay =
            Self::replay_with_selection_refs(history, network, initial_params, &[], false, true)?;
        replay.ballot_at(history, proposal)
    }

    pub(crate) fn ballot_at(
        &self,
        history: &impl LifecycleHistory,
        proposal: TxHash,
    ) -> Result<burst_governance::ballot::PreparedBallot, ReplayError> {
        let state = self
            .governance
            .as_ref()
            .expect("requested governance candidate");
        let voters: Vec<_> = self
            .accounts
            .values()
            .filter(|account| account.state.is_verified())
            .map(|account| account.address.clone())
            .collect();
        let mut governance = state.governance.clone();
        let mut target = governance
            .get_proposal(&proposal)
            .cloned()
            .ok_or_else(|| invalid("proposal missing from ballot history"))?;
        if target.phase == burst_governance::GovernancePhase::Withdrawn
            || (target.phase == burst_governance::GovernancePhase::Proposal
                && target.endorsement_count < state.params.governance_proposal_endorsements)
        {
            return burst_governance::ballot::PreparedBallot::close_unvoted(
                &target,
                history.commitment(),
                Timestamp::new(history.cutoff()),
                &state.params,
                governance
                    .timing(&target, &state.params)
                    .map_err(|error| invalid(error.to_string()))?,
            )
            .map_err(|error| invalid(error.to_string()));
        }
        if target.phase == burst_governance::GovernancePhase::Proposal {
            governance
                .try_advance(
                    &mut target,
                    Timestamp::new(history.cutoff()),
                    &state.params,
                    None,
                )
                .map_err(|error| invalid(error.to_string()))?;
            *governance
                .get_proposal_mut(&proposal)
                .expect("existing proposal") = target;
        }
        let delegation = state
            .delegation_at_cut(proposal, &voters, &self.lifecycle_metadata)
            .map_err(invalid)?;
        governance
            .prepare_ballot_cut(
                &proposal,
                history.commitment(),
                Timestamp::new(history.cutoff()),
                &state.params,
                &delegation,
                &voters,
            )
            .map_err(|error| invalid(error.to_string()))
    }

    pub(crate) fn apply_governance_cut(
        &mut self,
        history: &impl LifecycleHistory,
    ) -> Result<(Vec<TxHash>, BlockHash), ReplayError> {
        let due = self.governance_counting_cuts(Timestamp::new(history.cutoff()))?;
        if due.iter().any(|(_, at)| *at < history.cutoff()) {
            return Err(invalid(
                "history skipped an unclosed governance counting cut",
            ));
        }
        let mut ids: Vec<_> = due
            .into_iter()
            .filter_map(|(id, at)| (at == history.cutoff()).then_some(id))
            .collect();
        ids.sort_by_key(|id| *id.as_bytes());
        let mut activations = self.governance_activation_cuts();
        if activations.iter().any(|(_, at)| *at < history.cutoff()) {
            return Err(invalid("history skipped an approved governance activation"));
        }
        activations.retain(|(_, at)| *at == history.cutoff());
        activations.sort_by_key(|(id, _)| *id.as_bytes());
        if ids.is_empty() && activations.is_empty() {
            return Err(invalid("governance cut has no due decisions"));
        }
        // Due panels determine the electorate before this cut's ballots. Expiry
        // at the cut uses the rules produced by its approved activations.
        let previous_params = self.params.clone();
        let before = Timestamp::new(history.cutoff().saturating_sub(1));
        self.require_completed_deadlines(before)?;
        if self
            .pending
            .values()
            .any(|pending| pending_expired(pending, &previous_params, before))
        {
            return Err(invalid("governance cut skips an earlier pending expiry"));
        }
        let panels = self.apply_panel_deadlines(history, &previous_params)?;
        let mut encoded = b"BURST/governance-cut/v5\0".to_vec();
        encoded.extend_from_slice(panels.unwrap_or(BlockHash::ZERO).as_bytes());
        encoded.extend_from_slice(&(ids.len() as u64).to_be_bytes());
        let prepared: Vec<_> = ids
            .iter()
            .map(|id| self.ballot_at(history, *id))
            .collect::<Result<_, _>>()?;
        for ballot in &prepared {
            encoded.extend_from_slice(
                ballot
                    .commitment()
                    .map_err(|error| invalid(error.to_string()))?
                    .as_bytes(),
            );
        }
        let state = self
            .governance
            .as_mut()
            .expect("validated governance replay");
        for ballot in prepared {
            let proposal = ballot.next_proposal();
            self.closed_governance.insert(proposal.hash);
            if proposal
                .activation_at
                .is_some_and(|at| at.as_secs() == history.cutoff())
            {
                activations.push((proposal.hash, history.cutoff()));
            }
            *state
                .governance
                .get_proposal_mut(&proposal.hash)
                .expect("prepared proposal") = proposal.clone();
        }
        state
            .governance
            .close_proposal_windows(Timestamp::new(history.cutoff()), &state.params)
            .map_err(|error| invalid(error.to_string()))?;
        activations.sort_by_key(|(id, _)| *id.as_bytes());
        encoded.extend_from_slice(&(activations.len() as u64).to_be_bytes());
        let applied = history.applied_blocks();
        for (id, _) in activations {
            let mut proposal = state
                .governance
                .get_proposal(&id)
                .cloned()
                .expect("due proposal");
            proposal.phase = burst_governance::GovernancePhase::Activation;
            *state
                .governance
                .get_proposal_mut(&id)
                .expect("due proposal") = proposal.clone();
            let expected = state
                .governance
                .preview_activation(&proposal, &state.params)
                .map_err(|error| invalid(error.to_string()))?
                .params_hash();
            state
                .activate_approved(
                    &mut crate::governance_commit::ledger::derived::DerivedLedger {
                        mutations: self.mutations.as_mut(),
                        accounts: &mut self.accounts,
                        metadata: &mut self.lifecycle_metadata,
                        applied: &applied,
                        contents: &self.proposal_contents,
                        bootstrap: &mut self.bootstrap_phase,
                    },
                    id,
                    &expected,
                    Timestamp::new(history.cutoff()),
                    &mut self.brn,
                    &mut self.trst,
                )
                .map_err(invalid)?;
            self.verification.apply_governed_rules(&state.params);
            encoded.extend_from_slice(id.as_bytes());
            encoded.extend_from_slice(expected.as_bytes());
            match state.constitution.activation_conflict(&id) {
                None => encoded.push(0),
                Some(burst_consti::AmendmentConflict::ArticleNotFound(number)) => {
                    encoded.push(1);
                    encoded.extend_from_slice(&number.to_be_bytes());
                }
                Some(burst_consti::AmendmentConflict::ArticleAlreadyRepealed(number)) => {
                    encoded.push(2);
                    encoded.extend_from_slice(&number.to_be_bytes());
                }
            }
            ids.push(id);
        }
        self.params = state.params.clone();
        let active_params = self.params.clone();
        let expiry = self.apply_agreed_deadlines(history, &active_params)?;
        encoded.extend_from_slice(expiry.unwrap_or(BlockHash::ZERO).as_bytes());
        self.advance_deadline_candidate_cutoff(Timestamp::new(history.cutoff()))?;
        ids.sort_by_key(|id| *id.as_bytes());
        ids.dedup();
        Ok((ids, BlockHash::new(burst_crypto::blake2b_256(&encoded))))
    }

    pub(super) fn require_closed_governance(&self, cutoff: Timestamp) -> Result<(), ReplayError> {
        if self
            .governance_counting_cuts(cutoff)?
            .iter()
            .any(|(_, at)| *at <= cutoff.as_secs())
        {
            return Err(invalid(
                "history requires a certified governance counting cut",
            ));
        }
        if self
            .governance_activation_cuts()
            .iter()
            .any(|(_, at)| *at <= cutoff.as_secs())
        {
            return Err(invalid(
                "history requires a certified governance activation",
            ));
        }
        Ok(())
    }

    fn governance_activation_cuts(&self) -> Vec<(TxHash, u64)> {
        self.governance
            .as_ref()
            .into_iter()
            .flat_map(|state| state.governance.all_proposals())
            .filter(|proposal| {
                matches!(
                    proposal.phase,
                    burst_governance::GovernancePhase::Cooldown
                        | burst_governance::GovernancePhase::Activation
                )
            })
            .filter_map(|proposal| {
                proposal
                    .activation_at
                    .map(|at| (proposal.hash, at.as_secs()))
            })
            .collect()
    }

    fn governance_counting_cuts(
        &self,
        _through: Timestamp,
    ) -> Result<Vec<(TxHash, u64)>, ReplayError> {
        let Some(state) = self.governance.as_ref() else {
            return Ok(Vec::new());
        };
        let mut cuts = Vec::new();
        for proposal in state.governance.all_proposals() {
            if self.closed_governance.contains(&proposal.hash) {
                continue;
            }
            let at = match proposal.phase {
                burst_governance::GovernancePhase::Proposal
                | burst_governance::GovernancePhase::Withdrawn
                    if proposal.endorsement_count > 0
                        || state.params.governance_proposal_endorsements == 0 =>
                {
                    state.governance.counting_time(proposal, &state.params)
                }
                burst_governance::GovernancePhase::Exploration => {
                    state.governance.counting_time(proposal, &state.params)
                }
                _ => continue,
            }
            .map_err(|error| invalid(error.to_string()))?
            .as_secs();
            cuts.push((proposal.hash, at));
        }
        Ok(cuts)
    }
}
