use super::*;
use crate::confirmed_replay::snapshot::{
    CompactActivationCoverage, CompactDeadlineCoverage, CompactGovernanceCoverage,
    CompactOutcomeCoverage, CompactRosterCoverage, CompactSelectionCoverage,
};

#[derive(Clone, Copy)]
pub enum CompactReplayInput<'a> {
    Selection(&'a CompactSelectionCoverage),
    Deadline(&'a CompactDeadlineCoverage),
    Outcome(&'a CompactOutcomeCoverage),
    Roster(&'a CompactRosterCoverage),
    Governance(&'a CompactGovernanceCoverage),
    Activation(&'a CompactActivationCoverage),
}

pub enum CompactReplayCandidate {
    Selection(Box<CompactSelectionCoverage>),
    Deadline(Box<CompactDeadlineCoverage>),
    Outcome(Box<CompactOutcomeCoverage>),
    Roster(Box<CompactRosterCoverage>),
    Governance(Box<CompactGovernanceCoverage>),
    Activation(Box<CompactActivationCoverage>),
}

impl CompactReplayCandidate {
    pub fn input(&self) -> CompactReplayInput<'_> {
        match self {
            Self::Selection(value) => CompactReplayInput::Selection(value),
            Self::Deadline(value) => CompactReplayInput::Deadline(value),
            Self::Outcome(value) => CompactReplayInput::Outcome(value),
            Self::Roster(value) => CompactReplayInput::Roster(value),
            Self::Governance(value) => CompactReplayInput::Governance(value),
            Self::Activation(value) => CompactReplayInput::Activation(value),
        }
    }
}

impl CompactReplayInput<'_> {
    pub fn checkpoint(&self) -> &Checkpoint {
        match self {
            Self::Selection(value) => value.replay().checkpoint(),
            Self::Deadline(value) => &value.manifest().checkpoint,
            Self::Outcome(value) => &value.manifest().checkpoint,
            Self::Roster(value) => &value.manifest().checkpoint,
            Self::Governance(value) => &value.manifest().checkpoint,
            Self::Activation(value) => &value.manifest().checkpoint,
        }
    }
    pub(crate) fn manifest(&self) -> ReplayManifest {
        match self {
            Self::Selection(value) => ReplayManifest::Selection(value.replay().manifest()),
            Self::Deadline(value) => ReplayManifest::Deadline(value.manifest().clone()),
            Self::Outcome(value) => ReplayManifest::ChallengeOutcome(value.manifest().clone()),
            Self::Roster(value) => ReplayManifest::Roster(value.manifest().clone()),
            Self::Governance(value) => ReplayManifest::Governance(value.manifest().clone()),
            Self::Activation(value) => ReplayManifest::Activation(value.manifest().clone()),
        }
    }
    pub(crate) fn validate_parent(&self, anchor: &CheckpointAnchor) -> Result<(), StoreError> {
        match self {
            Self::Selection(value) => value.replay().validate_parent_commitments(anchor),
            Self::Deadline(value) => value.validate_parent(anchor),
            Self::Outcome(value) => value.validate_parent(anchor),
            Self::Roster(value) => value.validate_parent(anchor),
            Self::Governance(value) => value.validate_parent(anchor),
            Self::Activation(value) => value.validate_parent(anchor),
        }
    }
    pub(crate) fn check_signing_parent(
        &self,
        batch: &burst_store_lmdb::LmdbWriteTransaction<'_>,
        anchor: &CheckpointAnchor,
    ) -> Result<(), StoreError> {
        match self {
            Self::Selection(value) => value.check_signing_parent(batch, anchor),
            Self::Deadline(value) => value.check_signing_parent(batch, anchor),
            Self::Outcome(value) => value.check_signing_parent(batch, anchor),
            Self::Roster(value) => value.check_signing_parent(batch, anchor),
            Self::Governance(value) => value.check_signing_parent(batch, anchor),
            Self::Activation(value) => value.check_signing_parent(batch, anchor),
        }
    }
    pub(crate) fn covers_local_promises(
        &self, batch: &burst_store_lmdb::LmdbWriteTransaction<'_>, representative: &WalletAddress,
    ) -> Result<bool, StoreError> {
        match self {
            Self::Selection(value) => value.covers_local_promises(batch, representative),
            Self::Deadline(value) => value.covers_local_promises(batch, representative),
            Self::Outcome(value) => value.covers_local_promises(batch, representative),
            Self::Roster(value) => value.covers_local_promises(batch, representative),
            Self::Governance(value) => value.covers_local_promises(batch, representative),
            Self::Activation(value) => value.covers_local_promises(batch, representative),
        }
    }
    pub(super) fn seal_local_votes(
        &self,
        batch: &mut burst_store_lmdb::LmdbWriteTransaction<'_>,
        anchor: &CheckpointAnchor,
        representative: &WalletAddress,
    ) -> Result<(), StoreError> {
        match self {
            Self::Selection(value) => value.seal_local_votes(batch, anchor, representative),
            Self::Deadline(value) => value.seal_local_votes(batch, anchor, representative),
            Self::Outcome(value) => value.seal_local_votes(batch, anchor, representative),
            Self::Roster(value) => value.seal_local_votes(batch, anchor, representative),
            Self::Governance(value) => value.seal_local_votes(batch, anchor, representative),
            Self::Activation(value) => value.seal_local_votes(batch, anchor, representative),
        }
    }
    pub(crate) fn archive_rows(&self) -> crate::confirmed_replay::snapshot::tail::ArchiveRows<'_> {
        match self {
            Self::Selection(value) => value.archive_rows(),
            Self::Deadline(value) => value.archive_rows(),
            Self::Outcome(value) => value.archive_rows(),
            Self::Roster(value) => value.archive_rows(),
            Self::Governance(value) => value.archive_rows(),
            Self::Activation(value) => value.archive_rows(),
        }
    }
    pub(super) fn validate(
        &self,
        anchor: &CheckpointAnchor,
        input: Validation<'_>,
        clock: Option<SigningClock>,
    ) -> Result<(), StoreError> {
        if let Self::Selection(value) = self {
            return value.replay().validate_commitments(anchor, input, clock);
        }
        replay::validate_event_cut(anchor, self.checkpoint(), &input, clock, |clock| {
            if let Self::Activation(value) = self {
                value.validate_fresh_time(clock)?;
            }
            Ok(())
        })
    }
}
