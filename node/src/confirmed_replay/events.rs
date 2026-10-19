use super::selections::source::SelectionSource;
use super::*;
use crate::checkpoint_store::{chain::CertifiedEvent, CertifiedCheckpoint};

pub(super) struct Boundary<'a> {
    certificate: &'a burst_consensus::checkpoint::CheckpointCertificate,
    roster: &'a burst_consensus::checkpoint::HumanRoster,
    heads: &'a [crate::confirmed_history::HistoryHead],
    selection: Option<&'a CertifiedCheckpoint>,
    outcome: Option<&'a WalletAddress>,
    activation: Option<&'a crate::checkpoint_activation::ActivationProof>,
    deadline: bool,
    governance: bool,
    proposals: Option<&'a [burst_types::TxHash]>,
}

impl<'a> From<&'a CertifiedEvent> for Boundary<'a> {
    fn from(event: &'a CertifiedEvent) -> Self {
        Self {
            certificate: event.certificate(),
            roster: event.next_roster(),
            heads: event.heads(),
            selection: event.selection(),
            outcome: match event {
                CertifiedEvent::Outcome(value) => Some(&value.manifest().target),
                _ => None,
            },
            activation: match event {
                CertifiedEvent::Activation(value) => Some(&value.manifest().activation),
                _ => None,
            },
            deadline: matches!(event, CertifiedEvent::Deadline(_)),
            governance: matches!(event, CertifiedEvent::Governance(_)),
            proposals: match event {
                CertifiedEvent::Governance(value) => Some(&value.manifest().proposals),
                _ => None,
            },
        }
    }
}

impl<'a> Boundary<'a> {
    pub(super) fn from_selection(selection: &'a CertifiedCheckpoint) -> Self {
        Self {
            certificate: selection.certificate(),
            roster: selection.replayed().roster(),
            heads: selection.heads(),
            selection: Some(selection),
            outcome: None,
            activation: None,
            deadline: false,
            governance: false,
            proposals: None,
        }
    }
    /// The caller must verify the old-roster quorum and replay the exact cut.
    /// Selection reconstruction still requires its independently replayed record.
    pub(super) fn from_manifest(
        certificate: &'a burst_consensus::checkpoint::CheckpointCertificate,
        roster: &'a burst_consensus::checkpoint::HumanRoster,
        manifest: &'a crate::checkpoint_agreement::ReplayManifest,
    ) -> Result<Self, ReplayError> {
        use crate::checkpoint_agreement::ReplayManifest;
        let (heads, outcome, activation, deadline, governance, proposals) = match manifest {
            ReplayManifest::Selection(_) => {
                return Err(invalid("selection manifests require replay_selection"));
            }
            ReplayManifest::ChallengeOutcome(value) => {
                (&value.heads, Some(&value.target), None, false, false, None)
            }
            ReplayManifest::Roster(value) => {
                let expected = if certificate.checkpoint.next_roster == roster.commitment() {
                    crate::checkpoint_recovery::commitment()
                } else {
                    crate::checkpoint_roster::commitment()
                };
                if certificate.checkpoint.selection_root != expected {
                    return Err(invalid("roster checkpoint has a different event domain"));
                }
                (&value.heads, None, None, false, false, None)
            }
            ReplayManifest::Activation(value) => {
                if value
                    .activation
                    .commitment()
                    .map_err(|error| invalid(error.to_string()))?
                    != certificate.checkpoint.selection_root
                {
                    return Err(invalid(
                        "activation proof differs from its certified commitment",
                    ));
                }
                (
                    &value.heads,
                    None,
                    Some(&value.activation),
                    false,
                    false,
                    None,
                )
            }
            ReplayManifest::Deadline(value) => (&value.heads, None, None, true, false, None),
            ReplayManifest::Governance(value) => (
                &value.heads,
                None,
                None,
                false,
                true,
                Some(value.proposals.as_slice()),
            ),
        };
        if manifest.checkpoint() != &certificate.checkpoint {
            return Err(invalid("manifest differs from its certificate"));
        }
        Ok(Self {
            certificate,
            roster,
            heads,
            selection: None,
            outcome,
            activation,
            deadline,
            governance,
            proposals,
        })
    }
}

pub(super) struct EpochInput<'a> {
    pub directory: Option<&'a std::path::Path>,
    pub network: NetworkId,
    pub cutoff: u64,
    pub heads: &'a [crate::confirmed_history::HistoryHead],
    pub blocks: &'a super::blocks::source::Source<'a>,
    pub contents: &'a [crate::confirmed_history::ProposalWitness],
    pub old: HashSet<BlockHash>,
}

pub(super) enum InputMode {
    Certified { deadline_candidate: bool },
    Applied,
}

impl ConfirmedReplay {
    /// Every certified cut closes the previous payment authority. Later blocks
    /// must sign its hash, including after a selection that changes the roster.
    pub(crate) fn replay_with_events(
        history: &ConfirmedHistory,
        network: NetworkId,
        params: &ProtocolParams,
        events: &[CertifiedEvent],
    ) -> Result<Self, ReplayError> {
        Self::replay_events(history, network, params, events, false, false)
    }

    pub(crate) fn replay_deadline_candidate(
        history: &ConfirmedHistory,
        network: NetworkId,
        params: &ProtocolParams,
        events: &[CertifiedEvent],
    ) -> Result<Self, ReplayError> {
        Self::replay_events(history, network, params, events, true, false)
    }

    pub(crate) fn replay_governance_candidate(
        history: &ConfirmedHistory,
        network: NetworkId,
        params: &ProtocolParams,
        events: &[CertifiedEvent],
    ) -> Result<Self, ReplayError> {
        Self::replay_events(history, network, params, events, true, true)
    }

    /// Find governance boundaries without letting an unrelated due lifecycle
    /// effect prevent discovery. This state cannot authorize an event; the
    /// selected exact cut must pass its normal replay before signing.
    pub(crate) fn replay_governance_discovery(
        history: &ConfirmedHistory,
        network: NetworkId,
        params: &ProtocolParams,
        events: &[CertifiedEvent],
    ) -> Result<Self, ReplayError> {
        Self::replay_events(history, network, params, events, true, true)
    }

    fn replay_events(
        history: &ConfirmedHistory,
        network: NetworkId,
        params: &ProtocolParams,
        events: &[CertifiedEvent],
        deadline_candidate: bool,
        governance_candidate: bool,
    ) -> Result<Self, ReplayError> {
        let boundaries: Vec<_> = events.iter().map(Boundary::from).collect();
        Self::replay_boundaries(
            history,
            network,
            params,
            &boundaries,
            deadline_candidate,
            governance_candidate,
        )
    }

    pub(super) fn replay_selection_boundaries(
        history: &ConfirmedHistory,
        network: NetworkId,
        params: &ProtocolParams,
        certificates: &[crate::checkpoint_store::CertifiedCheckpoint],
    ) -> Result<Self, ReplayError> {
        let boundaries: Vec<_> = certificates.iter().map(Boundary::from_selection).collect();
        Self::replay_boundaries(history, network, params, &boundaries, false, false)
    }

    fn replay_boundaries(
        history: &ConfirmedHistory,
        network: NetworkId,
        params: &ProtocolParams,
        events: &[Boundary<'_>],
        deadline_candidate: bool,
        governance_candidate: bool,
    ) -> Result<Self, ReplayError> {
        let mut anchor =
            crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(network, params)
                .map_err(|error| invalid(error.to_string()))?;
        for event in events {
            if event.certificate.checkpoint.cutoff > history.cutoff() {
                return Err(invalid("history predates a certified lifecycle event"));
            }
            anchor = anchor
                .advance(event.certificate, event.roster.clone())
                .map_err(|error| invalid(error.to_string()))?;
        }
        let Some(first) = events.first() else {
            let state = Self::replay_with_selection_refs(
                history,
                network,
                params,
                &[],
                deadline_candidate,
                true,
            )?;
            if !governance_candidate {
                state.require_closed_governance(Timestamp::new(history.cutoff()))?;
            }
            return Ok(state);
        };
        let mut selections = Vec::new();
        let mut cut = history
            .prefix(first.certificate.checkpoint.cutoff, first.heads, &[])
            .map_err(|error| invalid(error.to_string()))?;
        let mut state = Self::replay_with_selection_refs(
            &cut,
            network,
            params,
            &selections,
            first.deadline || first.governance,
            true,
        )?;
        let mut epoch = history.genesis();
        for (offset, event) in events.iter().enumerate() {
            if offset > 0 {
                let next = history
                    .prefix(
                        event.certificate.checkpoint.cutoff,
                        event.heads,
                        cut.heads(),
                    )
                    .map_err(|error| invalid(error.to_string()))?;
                state.append_epoch(
                    &cut,
                    &next,
                    epoch,
                    params,
                    &selections,
                    event.deadline || event.governance,
                )?;
                cut = next;
            }
            state.apply_boundary(&cut, event, &mut selections)?;
            epoch = event.certificate.checkpoint.hash();
        }
        state.append_epoch(
            &cut,
            history,
            epoch,
            params,
            &selections,
            deadline_candidate,
        )?;
        if !governance_candidate {
            state.require_closed_governance(Timestamp::new(history.cutoff()))?;
        }
        Ok(state)
    }

    pub(super) fn apply_boundary<'a>(
        &mut self,
        cut: &impl LifecycleHistory,
        event: &Boundary<'a>,
        selections: &mut Vec<&'a CertifiedCheckpoint>,
    ) -> Result<(), ReplayError> {
        self.apply_boundary_source(cut, event, selections)?;
        if let Some(selection) = event.selection {
            selections.push(selection);
        }
        Ok(())
    }

    pub(super) fn apply_boundary_source(
        &mut self,
        cut: &impl LifecycleHistory,
        event: &Boundary<'_>,
        selections: &(impl SelectionSource + ?Sized),
    ) -> Result<(), ReplayError> {
        if cut.commitment() != event.certificate.checkpoint.ledger_root {
            return Err(invalid("certified event differs from its history cut"));
        }
        if !event.governance {
            self.require_closed_governance(Timestamp::new(cut.cutoff()))?;
        }
        let params = &self.params.clone();
        if let Some(outcome) = event.outcome {
            let root = self.apply_upheld(cut, params, outcome)?;
            if root != event.certificate.checkpoint.selection_root {
                return Err(invalid("replayed outcome differs from its certified event"));
            }
        }
        if event.deadline {
            let root = self
                .apply_agreed_deadlines(cut, params)?
                .ok_or_else(|| invalid("certified deadline has no due effects"))?;
            if root != event.certificate.checkpoint.selection_root {
                return Err(invalid(
                    "deadline completion differs from its certified event",
                ));
            }
        }
        if event.governance {
            let (proposals, root) = self.apply_governance_cut(cut)?;
            if root != event.certificate.checkpoint.selection_root
                || event.proposals != Some(proposals.as_slice())
            {
                return Err(invalid(
                    "governance ballot cut differs from its certificate",
                ));
            }
        }
        if let Some(selection) = event.selection {
            let request = self.prepare_selection(selection.replayed().request().target())?;
            if &request != selection.replayed().request() {
                return Err(invalid(
                    "replayed request differs from its certified selection",
                ));
            }
            self.selection_checkpoints
                .push(selection.certificate().checkpoint.hash())?;
            self.selection_receipts
                .push(selection.replayed().receipt())?;
        }
        if let Some(proof) = event.activation {
            let selection = selections
                .find_selection(|selection| {
                    selection.certificate().checkpoint.hash() == proof.selection
                })?
                .ok_or_else(|| invalid("activation has no earlier certified selection"))?;
            self.apply_activation(&selection, proof, params)?;
            if proof
                .commitment()
                .map_err(|error| invalid(error.to_string()))?
                != event.certificate.checkpoint.selection_root
            {
                return Err(invalid("activation differs from its certified event"));
            }
        }
        if self.human_roster()?.commitment() != event.certificate.checkpoint.next_roster {
            return Err(invalid("replayed event changes certified human authority"));
        }
        if self.params.params_hash() != event.certificate.checkpoint.params_hash {
            return Err(invalid("replayed parameters differ from certified event"));
        }
        let epoch = event.certificate.checkpoint.hash();
        crate::verification_commit::receipts::certify_metadata(&mut self.lifecycle_metadata, epoch)
            .map_err(invalid)?;
        self.payment_epoch = epoch;
        // Any certified handoff containing a terminal proposal closes its
        // old-epoch promises, even when another lifecycle caused the cut.
        if let Some(governance) = &self.governance {
            self.closed_governance.extend(
                governance
                    .governance
                    .all_proposals()
                    .filter(|proposal| {
                        matches!(
                            proposal.phase,
                            burst_governance::GovernancePhase::Withdrawn
                                | burst_governance::GovernancePhase::Rejected
                        )
                    })
                    .map(|proposal| proposal.hash),
            );
        }
        if !event.governance {
            if let Some(governance) = &mut self.governance {
                governance
                    .governance
                    .close_proposal_windows(Timestamp::new(cut.cutoff()), &governance.params)
                    .map_err(|error| invalid(error.to_string()))?;
            }
        }
        Ok(())
    }

    pub(super) fn append_epoch(
        &mut self,
        previous: &ConfirmedHistory,
        history: &ConfirmedHistory,
        epoch: BlockHash,
        _initial_params: &ProtocolParams,
        certificates: &[&CertifiedCheckpoint],
        deadline_candidate: bool,
    ) -> Result<(), ReplayError> {
        if epoch.is_zero()
            || self.payment_epoch != epoch
            || history.network() != previous.network()
            || history.genesis() != previous.genesis()
        {
            return Err(invalid("invalid post-checkpoint replay context"));
        }
        history
            .validate_extension(previous.heads())
            .map_err(|error| invalid(error.to_string()))?;
        let old: HashSet<_> = previous
            .replay_blocks()
            .iter()
            .map(|block| block.hash)
            .collect();
        self.append_input(
            EpochInput {
                directory: None,
                network: history.network(),
                cutoff: history.cutoff(),
                heads: history.heads(),
                blocks: &super::blocks::source::Source::new(history.replay_blocks(), None)
                    .map_err(|error| invalid(error.to_string()))?,
                contents: history.proposal_witnesses(),
                old,
            },
            epoch,
            certificates,
            deadline_candidate,
        )
    }

    pub(super) fn append_input(
        &mut self,
        input: EpochInput<'_>,
        epoch: BlockHash,
        certificates: &(impl SelectionSource + ?Sized),
        deadline_candidate: bool,
    ) -> Result<(), ReplayError> {
        self.append_input_mode(
            input,
            epoch,
            certificates,
            InputMode::Certified { deadline_candidate },
        )
    }

    pub(super) fn append_input_mode(
        &mut self,
        input: EpochInput<'_>,
        epoch: BlockHash,
        certificates: &(impl SelectionSource + ?Sized),
        mode: InputMode,
    ) -> Result<(), ReplayError> {
        self.append_input_observed(input, epoch, certificates, mode, &[], &mut |_, _| Ok(()))
    }

    pub(super) fn append_input_observed(
        &mut self,
        input: EpochInput<'_>,
        epoch: BlockHash,
        certificates: &(impl SelectionSource + ?Sized),
        mode: InputMode,
        ordering: &[(BlockHash, BlockHash)],
        observe: &mut impl FnMut(Option<&StateBlock>, &Self) -> Result<(), ReplayError>,
    ) -> Result<(), ReplayError> {
        let params = &self.params.clone();
        let old = &input.old;
        if epoch.is_zero()
            || self.payment_epoch != epoch
            || input.cutoff < self.cutoff.as_secs()
            || certificates.len() != self.selection_checkpoints.len()
            || !self.replay_dependencies.is_subset(old)
        {
            return Err(invalid(
                "tail omits authenticated selection or ordering history",
            ));
        }
        for (certificate, expected) in certificates
            .read_all()
            .zip(self.selection_checkpoints.iter())
        {
            if certificate?.certificate().checkpoint.hash() != *expected? {
                return Err(invalid(
                    "tail selection sequence differs from authenticated history",
                ));
            }
        }
        let blocks = input.blocks;
        if let Some(directory) = input.directory {
            self.selection_checkpoints.stage(directory)?;
            self.selection_receipts.stage(directory)?;
        }
        let mut applied = super::blocks::AppliedHistory::new(input.directory)
            .map_err(|error| invalid(error.to_string()))?;
        for hash in blocks.hashes() {
            let hash = hash.map_err(|error| invalid(error.to_string()))?;
            if !old.contains(&hash) {
                continue;
            }
            let block = blocks
                .get(&hash)
                .map_err(|error| invalid(error.to_string()))?;
            applied
                .insert_cow(block)
                .map_err(|error| invalid(error.to_string()))?;
        }
        if applied.len() != old.len() {
            return Err(invalid("post-checkpoint history omits a certified block"));
        }
        let mut selections = selections::Selections::for_certified_epoch(certificates, blocks)?;
        selections.include_deadlines(&self.lifecycle_metadata)?;
        let genesis = crate::genesis_key::genesis_address(input.network);
        let mut bootstrap = HashMap::new();
        if !self.bootstrap_complete() {
            let mut verified = self
                .accounts
                .values()
                .filter(|account| account.state.is_verified())
                .count() as u64;
            for hash in blocks.hashes() {
                let hash = hash.map_err(|error| invalid(error.to_string()))?;
                if old.contains(&hash) {
                    continue;
                }
                let block = blocks
                    .get(&hash)
                    .map_err(|error| invalid(error.to_string()))?;
                if block.block_type != BlockType::Endorse
                    || block.account != genesis
                    || verified >= params.bootstrap_exit_threshold as u64
                {
                    continue;
                }
                let target = crate::ledger_bridge::extract_receiver_from_link(&block.link)
                    .ok_or_else(|| invalid("invalid bootstrap target"))?;
                if target == genesis
                    || bootstrap.insert(target.clone(), block.hash).is_some()
                    || self
                        .endorsement_blocks
                        .get(&target)
                        .is_some_and(|blocks| !blocks.is_empty())
                {
                    return Err(invalid("ambiguous or duplicate bootstrap verification"));
                }
                verified += 1;
            }
        }
        let mut prerequisites = HashMap::new();
        let governance_dependencies = if self.governance.is_some() {
            governance::streamed_dependencies(blocks)?
        } else {
            HashMap::new()
        };
        if self.governance.is_some() {
            self.proposal_contents.extend(
                input
                    .contents
                    .iter()
                    .map(|witness| (witness.block, witness.bytes.clone())),
            );
        }
        let mut budget = MAX_LIFECYCLE_DEPENDENCIES;
        let mut challenges = HashSet::new();
        for hash in blocks.hashes() {
            let hash = hash.map_err(|error| invalid(error.to_string()))?;
            if !old.contains(&hash) {
                continue;
            }
            let block = blocks
                .get(&hash)
                .map_err(|error| invalid(error.to_string()))?;
            if block.block_type == BlockType::Challenge
                && crate::ledger_bridge::extract_receiver_from_link(&block.link)
                    .is_some_and(|target| self.verification.has_active_challenge(&target))
            {
                challenges.insert(block.link);
            }
        }
        for hash in blocks.hashes() {
            let hash = hash.map_err(|error| invalid(error.to_string()))?;
            if old.contains(&hash) {
                continue;
            }
            let block = blocks
                .get(&hash)
                .map_err(|error| invalid(error.to_string()))?;
            if block.payment_epoch != epoch {
                return Err(invalid(
                    "post-checkpoint block payment epoch differs from its certified boundary",
                ));
            }
            if block.timestamp < self.cutoff {
                return Err(invalid("post-checkpoint block predates the certified cut"));
            }
            if !(crate::payment_transition::supports(&block.block_type)
                || (self.governance.is_some() && governance::supports(&block.block_type))
                || matches!(
                    block.block_type,
                    BlockType::Epoch
                        | BlockType::Endorse
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
            if block.block_type == BlockType::Challenge && !challenges.insert(block.link) {
                return Err(invalid(
                    "repeated challenge admissions require agreed lifecycle ordering",
                ));
            }
            if block.block_type == BlockType::Endorse {
                let target = crate::ledger_bridge::extract_receiver_from_link(&block.link)
                    .ok_or_else(|| invalid("invalid endorsement target"))?;
                if bootstrap
                    .get(&target)
                    .is_some_and(|hash| *hash != block.hash)
                {
                    return Err(invalid("competing bootstrap and ordinary endorsements"));
                }
            }
            let mut dependencies = selections.dependencies(&block, budget)?;
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
                    .ok_or_else(|| invalid("invalid challenge target"))?;
                dependencies.extend(bootstrap.get(&target).copied());
            }
            budget = budget
                .checked_sub(dependencies.len())
                .ok_or_else(|| invalid("lifecycle dependencies require incremental replay"))?;
            prerequisites.insert(block.hash, dependencies);
        }
        if !ordering.is_empty() && !matches!(mode, InputMode::Applied) {
            return Err(invalid("local ordering cannot change certified replay"));
        }
        if ordering.len() > budget {
            return Err(invalid("local replay ordering exceeds dependency budget"));
        }
        let known: HashSet<_> = blocks
            .hashes()
            .collect::<Result<_, _>>()
            .map_err(|error| invalid(error.to_string()))?;
        let mut unique = HashSet::new();
        for &(block, dependency) in ordering {
            if block == dependency
                || !known.contains(&dependency)
                || !unique.insert((block, dependency))
            {
                return Err(invalid("invalid local replay ordering edge"));
            }
            prerequisites
                .get_mut(&block)
                .filter(|_| !old.contains(&block))
                .ok_or_else(|| invalid("local ordering cannot change retained history"))?
                .push(dependency);
        }
        let ordered = crate::confirmed_history::causal_hash_order_after(
            known.iter().copied().filter(|hash| !old.contains(hash)),
            old,
            |hash| {
                let block = blocks.get(hash)?;
                Ok(block
                    .dependencies()
                    .chain(prerequisites.get(hash).into_iter().flatten().copied())
                    .collect())
            },
        )
        .map_err(|error| invalid(error.to_string()))?;
        observe(None, self)?;
        for hash in ordered {
            if let Some(mutations) = &mut self.mutations {
                *mutations = Default::default();
                self.brn.wallets.record_transition(true);
            }
            let block = blocks
                .get(&hash)
                .map_err(|error| invalid(error.to_string()))?;
            self.apply_authenticated(&block, &applied, params, &genesis, &mut selections)?;
            observe(Some(&block), self)?;
            applied
                .insert_cow(block)
                .map_err(|error| invalid(error.to_string()))?;
        }
        match mode {
            InputMode::Applied => self.check_head_list(input.heads),
            InputMode::Certified { deadline_candidate } => {
                self.confirm_head_list(input.heads)?;
                if deadline_candidate {
                    self.advance_deadline_candidate_cutoff(Timestamp::new(input.cutoff))
                } else {
                    self.advance_cutoff(Timestamp::new(input.cutoff), params)
                }
            }
        }
    }
}
