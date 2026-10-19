use super::*;
use crate::{
    checkpoint_activation::{ActivationProof, PreparedActivation},
    checkpoint_agreement::CompactReplayCandidate,
    checkpoint_deadline::PreparedDeadline,
    checkpoint_governance::PreparedGovernance,
    checkpoint_outcome::PreparedChallengeOutcome,
    checkpoint_replay::SigningClock,
    checkpoint_roster::PreparedRoster,
};
use burst_types::Timestamp;

impl SnapshotContinuation {
    /// Follow the archival runtime's priority order using only a checked local
    /// suffix. The caller must recheck its store observations before signing.
    pub fn prepare_next_cemented_candidate(
        &self,
        store: &LmdbStore,
        cemented: &super::super::tail::cemented::CementedTail,
        selections: &(impl SelectionSource + ?Sized),
        clock: SigningClock,
    ) -> Result<Option<CompactReplayCandidate>, ReplayError> {
        self.check_selection_receipts(selections)?;
        let Some(safe_cut) = clock
            .now
            .checked_sub(clock.error_bound)
            .and_then(|time| time.checked_sub(1))
        else {
            return Ok(None);
        };
        if safe_cut < self.anchor.cutoff() {
            return Ok(None);
        }
        let tail_at = |cutoff| {
            let heads = self.cemented_heads_at(cemented, cutoff)?;
            self.read_tail_retaining(store, cutoff, &heads, cemented.tail())
        };
        let safe_tail = tail_at(safe_cut)?;
        let (discovery, _) = self.discovery_state(&safe_tail, selections)?;
        let governance_cut = discovery
            .state
            .next_governance_cut(Timestamp::new(safe_cut))?;
        if governance_cut.is_some_and(|cut| cut <= self.anchor.cutoff()) {
            return Err(super::super::invalid(
                "governance boundary does not extend its certified parent",
            ));
        }
        let lifecycle_cut = governance_cut
            .and_then(|cut| cut.checked_sub(1))
            .unwrap_or(safe_cut);
        let mut result = None;
        if lifecycle_cut > self.anchor.cutoff() {
            let tail = tail_at(lifecycle_cut)?;
            result = self.discover_lifecycle(&tail, selections)?;
        }
        if result.is_none() {
            if let Some(cutoff) = governance_cut {
                let tail = tail_at(cutoff)?;
                let (mut replay, blocks) = self.discovery_state(&tail, selections)?;
                let cut = CompactCut {
                    cutoff,
                    commitment: tail.commitment(),
                    blocks: &blocks,
                };
                let (proposals, root) = replay.state.apply_governance_cut(&cut)?;
                let (manifest, _) = PreparedGovernance::manifest_for_cut(
                    &self.anchor,
                    cutoff,
                    tail.commitment(),
                    tail.heads(),
                    proposals,
                    root,
                    &replay.state,
                )
                .map_err(|error| super::super::invalid(error.to_string()))?;
                result = Some(CompactReplayCandidate::Governance(Box::new(
                    CompactGovernanceCoverage {
                        manifest,
                        cut: self.cut_coverage(&tail)?,
                    },
                )));
            } else if let Some(selection) =
                self.prepare_next_selection(tail_at(clock.now)?, selections)?
            {
                selection
                    .replay()
                    .request()
                    .validate_before_signing(
                        selection.replay().checkpoint(),
                        clock.now,
                        clock.error_bound,
                    )
                    .map_err(|error| super::super::invalid(error.to_string()))?;
                result = Some(CompactReplayCandidate::Selection(Box::new(selection)));
            } else if discovery.state.human_roster()? != *self.anchor.roster()
                || (crate::checkpoint_recovery::valid_cut(self.anchor.cutoff(), safe_cut)
                    && crate::checkpoint_recovery::requested(store, self.anchor.hash(), safe_cut)
                        .map_err(|error| super::super::invalid(error.to_string()))?)
            {
                let (manifest, _) = PreparedRoster::manifest_for_cut(
                    &self.anchor,
                    safe_cut,
                    safe_tail.commitment(),
                    safe_tail.heads(),
                    &discovery.state,
                )
                .map_err(|error| super::super::invalid(error.to_string()))?;
                result = Some(CompactReplayCandidate::Roster(Box::new(
                    CompactRosterCoverage {
                        manifest,
                        cut: self.cut_coverage(&safe_tail)?,
                    },
                )));
            }
        }
        if let Some(candidate) = &result {
            if bincode::serialized_size(&candidate.input().manifest())
                .map_err(|error| super::super::invalid(error.to_string()))?
                > crate::checkpoint_store::MAX_RECORD_BYTES
            {
                return Err(super::super::invalid(
                    "discovered compact manifest exceeds byte limit",
                ));
            }
            if let CompactReplayCandidate::Activation(activation) = candidate {
                activation
                    .validate_fresh_time(clock)
                    .map_err(|error| super::super::invalid(error.to_string()))?;
            }
        }
        Ok(result)
    }

    fn discovery_state(
        &self,
        tail: &SnapshotTail,
        selections: &(impl SelectionSource + ?Sized),
    ) -> Result<(Self, Source<'static>), ReplayError> {
        let mut candidate = Self {
            anchor: self.anchor.clone(),
            context: self.context,
            state: self.state.clone(),
        };
        let blocks = candidate.execute(tail, selections, true)?;
        Ok((candidate, blocks))
    }

    fn discover_lifecycle(
        &self,
        tail: &SnapshotTail,
        selections: &(impl SelectionSource + ?Sized),
    ) -> Result<Option<CompactReplayCandidate>, ReplayError> {
        let (mut candidate, blocks) = self.discovery_state(tail, selections)?;
        candidate
            .state
            .require_closed_governance(Timestamp::new(tail.cutoff()))?;
        let cut = CompactCut {
            cutoff: tail.cutoff(),
            commitment: tail.commitment(),
            blocks: &blocks,
        };
        let params = candidate.state.params().clone();
        if let Some(root) = candidate.state.apply_agreed_deadlines(&cut, &params)? {
            let (manifest, _) = PreparedDeadline::manifest_for_cut(
                &self.anchor,
                tail.cutoff(),
                tail.commitment(),
                tail.heads(),
                &candidate.state,
                root,
            )
            .map_err(|error| super::super::invalid(error.to_string()))?;
            return Ok(Some(CompactReplayCandidate::Deadline(Box::new(
                CompactDeadlineCoverage {
                    manifest,
                    cut: self.cut_coverage(tail)?,
                },
            ))));
        }
        candidate
            .state
            .advance_cutoff(Timestamp::new(tail.cutoff()), &params)?;
        if let Some(target) = candidate.state.upheld_targets().into_iter().next() {
            let root = candidate.state.apply_upheld(&cut, &params, &target)?;
            let (manifest, _) = PreparedChallengeOutcome::manifest_for_cut(
                &self.anchor,
                tail.cutoff(),
                tail.commitment(),
                tail.heads(),
                &target,
                &candidate.state,
                root,
            )
            .map_err(|error| super::super::invalid(error.to_string()))?;
            return Ok(Some(CompactReplayCandidate::Outcome(Box::new(
                CompactOutcomeCoverage {
                    manifest,
                    cut: self.cut_coverage(tail)?,
                },
            ))));
        }
        for selection in selections.read_all() {
            let selection = selection?;
            let Some(proof) = ActivationProof::from_selection(&selection) else {
                continue;
            };
            if tail.cutoff()
                < selection
                    .replayed()
                    .request()
                    .beacon_time()
                    .map_err(|error| super::super::invalid(error.to_string()))?
                || !candidate.state.can_activate(&selection)?
            {
                continue;
            }
            candidate
                .state
                .apply_activation(&selection, &proof, &params)?;
            let (manifest, _, signing_deadline) = PreparedActivation::manifest_for_cut(
                &self.anchor,
                tail.cutoff(),
                tail.commitment(),
                tail.heads(),
                &proof,
                &selection,
                &candidate.state,
            )
            .map_err(|error| super::super::invalid(error.to_string()))?;
            return Ok(Some(CompactReplayCandidate::Activation(Box::new(
                CompactActivationCoverage {
                    manifest,
                    cut: self.cut_coverage(tail)?,
                    signing_deadline,
                },
            ))));
        }
        Ok(None)
    }
}
