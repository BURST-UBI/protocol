use super::*;
use crate::checkpoint_replay::SelectionLifecycle;
use crate::checkpoint_store::CertifiedCheckpoint;

fn target_key(target: &WalletAddress) -> [u8; 32] {
    burst_crypto::blake2b_256_multi(&[b"burst/replay-target/v1", target.as_str().as_bytes()])
}

fn lifecycle_key(target: &WalletAddress, lifecycle: SelectionLifecycle) -> [u8; 32] {
    let (kind, origin) = match lifecycle {
        SelectionLifecycle::Verification(origin) => (0u8, origin),
        SelectionLifecycle::Challenge(origin) => (1u8, origin),
    };
    burst_crypto::blake2b_256_multi(&[
        b"burst/replay-lifecycle/v1",
        &target_key(target),
        &[kind],
        origin.as_bytes(),
    ])
}

mod panels;
#[cfg(test)]
pub(super) use panels::assert_scoped_reuse;
pub(crate) mod source;
use source::SelectionSource;

/// Certificates here have already been reconstructed from configured genesis.
/// A peer's manifest or standalone signature list cannot construct this view.
pub(super) struct Selections<'a, S: SelectionSource + ?Sized> {
    panels: panels::Panels<'a, S>,
    sources: super::scratch_index::ReplayIndex,
    targets: super::scratch_index::ReplayIndex,
    installed: HashSet<BlockHash>,
    completions: Vec<crate::verification_deadline::Completion>,
}

impl<'a, S: SelectionSource + ?Sized> Selections<'a, S> {
    pub(super) fn new(certificates: &'a S, blocks: &[StateBlock]) -> Result<Self, ReplayError> {
        Self::build(
            certificates,
            |visit| blocks.iter().try_for_each(visit),
            false,
            None,
        )
    }

    /// Earlier epochs have already applied their certified challenge outcomes.
    /// Only new blocks may use the latest lifecycle's verification dependencies.
    pub(super) fn for_certified_epoch(
        certificates: &'a S,
        blocks: &super::blocks::source::Source<'_>,
    ) -> Result<Self, ReplayError> {
        Self::build(
            certificates,
            |visit| blocks.visit_unordered(visit),
            true,
            blocks.directory(),
        )
    }

    fn build(
        certificates: &'a S,
        scan: impl FnOnce(
            &mut dyn FnMut(&StateBlock) -> Result<(), ReplayError>,
        ) -> Result<(), ReplayError>,
        ordered_lifecycles: bool,
        directory: Option<&std::path::Path>,
    ) -> Result<Self, ReplayError> {
        let mut state = Self {
            panels: panels::Panels::new(certificates, directory)?,
            sources: super::scratch_index::ReplayIndex::new(directory)
                .map_err(|error| invalid(error.to_string()))?,
            targets: super::scratch_index::ReplayIndex::new(directory)
                .map_err(|error| invalid(error.to_string()))?,
            installed: HashSet::new(),
            completions: Vec::new(),
        };
        let mut lifecycles = super::scratch_index::ReplayIndex::new(directory)
            .map_err(|error| invalid(error.to_string()))?;
        for (position, certificate) in certificates.iter_selections().enumerate() {
            let certificate = certificate?;
            let request = certificate.replayed().request();
            let context = request
                .vote_context(&certificate.certificate().checkpoint)
                .map_err(|error| invalid(error.to_string()))?;
            let selected = certificate
                .beacon()
                .map(|beacon| {
                    request
                        .select(certificate.parent(), certificate.certificate(), beacon)
                        .map(|selected| selected.into_iter().collect())
                        .map_err(|error| invalid(error.to_string()))
                })
                .transpose()?;
            let lifecycle = certificate.replayed().lifecycle();
            let key = lifecycle_key(request.target(), lifecycle);
            let previous_attempt = lifecycles
                .get(&key)
                .map_err(|error| invalid(error.to_string()))?
                .map(|previous| {
                    if previous[4..] != [0; 28] {
                        return Err(invalid("invalid staged lifecycle attempt"));
                    }
                    Ok(u32::from_be_bytes(previous[..4].try_into().map_err(
                        |_| invalid("invalid staged lifecycle attempt"),
                    )?))
                })
                .transpose()?;
            let expected_attempt =
                previous_attempt.map_or(Some(0), |attempt| attempt.checked_add(1));
            if Some(request.attempt()) != expected_attempt {
                return Err(invalid(
                    "certified verification attempts are missing or out of order",
                ));
            }
            let mut stored_attempt = [0; 32];
            stored_attempt[..4].copy_from_slice(&request.attempt().to_be_bytes());
            lifecycles
                .insert(key, stored_attempt)
                .map_err(|error| invalid(error.to_string()))?;
            if matches!(lifecycle, SelectionLifecycle::Verification(_)) {
                if !ordered_lifecycles
                    && state
                        .targets
                        .get(&target_key(request.target()))
                        .map_err(|error| invalid(error.to_string()))?
                        .map(|previous| {
                            state
                                .panels
                                .require(&BlockHash::new(previous))
                                .map(|panel| panel.certificate.replayed().lifecycle() != lifecycle)
                        })
                        .transpose()?
                        .unwrap_or(false)
                {
                    return Err(invalid("re-verification requires agreed lifecycle replay"));
                }
                state
                    .targets
                    .insert(target_key(request.target()), *context.as_bytes())
                    .map_err(|error| invalid(error.to_string()))?;
            }
            if state.panels.insert(
                context,
                position,
                certificate.certificate().checkpoint.hash(),
                selected,
            )? {
                return Err(invalid("duplicate certified selection context"));
            }
        }
        scan(&mut |block| {
            if block.block_type != BlockType::VerificationVote {
                return Ok(());
            }
            let context = BlockHash::new(*block.origin.as_bytes());
            let mut panel = state
                .panels
                .get(&context)?
                .ok_or_else(|| invalid("verification vote has no certified ancestry"))?;
            let selected = panel
                .selected
                .as_ref()
                .ok_or_else(|| invalid("verification vote has no exact certified beacon"))?;
            if !panel.certificate.replayed().bootstrap_complete() {
                return Err(invalid("certified verification requires bootstrap exit"));
            }
            if !selected.contains(&block.account)
                || crate::block_economics::wallet_from_link(&block.link).as_ref()
                    != Some(panel.certificate.replayed().request().target())
                || panel
                    .votes
                    .insert(block.account.clone(), block.hash)
                    .is_some()
            {
                return Err(invalid("duplicate or unrelated certified panel vote"));
            }
            state
                .sources
                .insert(*block.hash.as_bytes(), *context.as_bytes())
                .map_err(|error| invalid(error.to_string()))?;
            state.panels.save(context, panel)?;
            Ok(())
        })?;
        Ok(state)
    }

    /// Supply only completions reconstructed at earlier certified boundaries.
    /// This checks panel membership and coverage, not the completion boundary.
    /// Discard this selection view if an entry fails after earlier entries applied.
    pub(super) fn include_deadlines(
        &mut self,
        metadata: &HashMap<String, Vec<u8>>,
    ) -> Result<(), ReplayError> {
        for (key, bytes) in metadata {
            if !key.starts_with("verification_deadline/v1/") {
                continue;
            }
            let completion = crate::verification_deadline::decode(bytes).map_err(invalid)?;
            let mut panel = self
                .panels
                .get(&completion.activation.context)?
                .ok_or_else(|| invalid("deadline has no certified panel"))?;
            let selected = panel
                .selected
                .as_ref()
                .ok_or_else(|| invalid("deadline panel has no beacon"))?;
            if panel.certificate.certificate().checkpoint.hash() != completion.activation.selection
                || panel.certificate.replayed().request().target() != &completion.activation.target
                || completion
                    .absent
                    .iter()
                    .any(|voter| !selected.contains(voter) || panel.votes.contains_key(voter))
                || selected.len() != panel.votes.len() + completion.absent.len()
            {
                return Err(invalid(
                    "deadline defaults differ from their certified panel",
                ));
            }
            panel.defaults.extend(completion.absent.iter().cloned());
            self.panels.save(completion.activation.context, panel)?;
            self.completions.push(completion);
        }
        Ok(())
    }

    /// Return prerequisite hashes within the supplied count budget. Hashes may
    /// repeat across dependency groups; the budget counts those repeats.
    pub(super) fn dependencies(
        &self,
        block: &StateBlock,
        budget: usize,
    ) -> Result<Vec<BlockHash>, ReplayError> {
        let vote_panel = if block.block_type == BlockType::VerificationVote {
            Some(
                self.panels
                    .require(&BlockHash::new(*block.origin.as_bytes()))?,
            )
        } else {
            None
        };
        let heads = vote_panel
            .as_ref()
            .map_or(&[][..], |panel| panel.certificate.heads());
        let context = if matches!(
            block.block_type,
            BlockType::Open | BlockType::Receive | BlockType::RejectReceive
        ) {
            self.sources
                .get(block.link.as_bytes())
                .map_err(|error| invalid(error.to_string()))?
                .map(BlockHash::new)
        } else if needs_verified_history(&block.block_type) {
            self.targets
                .get(&target_key(&block.account))
                .map_err(|error| invalid(error.to_string()))?
                .map(BlockHash::new)
        } else {
            None
        };
        let panel = if let Some(context) = context {
            let panel = self.panels.require(&context)?;
            if !panel.complete() {
                return Err(invalid(
                    "spending or reward claim requires the complete certified panel",
                ));
            }
            Some(panel)
        } else {
            None
        };
        let required = heads
            .len()
            .checked_add(panel.as_ref().map_or(0, |panel| panel.votes.len()))
            .filter(|required| *required <= budget)
            .ok_or_else(|| invalid("lifecycle dependencies require incremental replay"))?;
        let mut dependencies = Vec::with_capacity(required);
        dependencies.extend(heads.iter().map(|head| head.hash));
        if let Some(panel) = panel {
            dependencies.extend(panel.vote_hashes());
        }
        if block.block_type == BlockType::Challenge {
            let target = crate::block_economics::wallet_from_link(&block.link)
                .ok_or_else(|| invalid("challenge target is invalid"))?;
            if let Some(context) = self
                .targets
                .get(&target_key(&target))
                .map_err(|error| invalid(error.to_string()))?
            {
                let panel = self.panels.require(&BlockHash::new(context))?;
                if !panel.complete() {
                    return Err(invalid(
                        "challenge target requires its complete verification panel",
                    ));
                }
                if dependencies
                    .len()
                    .checked_add(panel.votes.len())
                    .is_none_or(|size| size > budget)
                {
                    return Err(invalid("lifecycle dependencies require incremental replay"));
                }
                dependencies.extend(panel.vote_hashes());
            }
        }
        Ok(dependencies)
    }

    pub(super) fn install_for(
        &mut self,
        block: &StateBlock,
        orchestrator: &mut VerificationOrchestrator,
        params: &ProtocolParams,
    ) -> Result<(), ReplayError> {
        if block.block_type != BlockType::VerificationVote {
            return Ok(());
        }
        let context = BlockHash::new(*block.origin.as_bytes());
        if self.installed.contains(&context) {
            return Ok(());
        }
        let panel = self.panels.require(&context)?;
        let record = panel.certificate.as_ref();
        let request = record.replayed().request();
        if orchestrator.selection_context(request.target()) == Some(context) {
            let state = orchestrator
                .get_state(request.target())
                .ok_or_else(|| invalid("installed certified panel has no state"))?;
            if panel.selected.as_ref() != Some(&state.selected_verifiers.iter().cloned().collect())
            {
                return Err(invalid("installed certified panel differs from its beacon"));
            }
            self.installed.insert(context);
            return Ok(());
        }
        let lifecycle = match record.replayed().lifecycle() {
            SelectionLifecycle::Verification(origin) | SelectionLifecycle::Challenge(origin) => {
                origin
            }
        };
        let defaults = crate::verification_deadline::history_defaults_from_completions(
            orchestrator,
            request.target(),
            lifecycle,
            &self.completions,
        )
        .map_err(invalid)?;
        if orchestrator
            .selection_history_with_defaults(request.target(), &defaults)
            .map_err(|error| invalid(error.to_string()))?
            != record.replayed().selection_history_commitment()
        {
            return Err(invalid(
                "replayed selection history differs from the certified cut",
            ));
        }
        orchestrator
            .freeze_replayed_selection(request, params)
            .map_err(|error| invalid(error.to_string()))?;
        orchestrator
            .install_selection(
                request.target(),
                record.parent(),
                record.certificate(),
                record
                    .beacon()
                    .ok_or_else(|| invalid("missing certified beacon"))?,
            )
            .map_err(|error| invalid(error.to_string()))?;
        let events = orchestrator.drain_events();
        if events.len() != 1
            || !matches!(&events[0],
            VerificationEvent::VerifiersSelected { wallet, .. } if wallet == request.target())
        {
            return Err(invalid(
                "unexpected certified committee installation effects",
            ));
        }
        self.installed.insert(context);
        Ok(())
    }
}
