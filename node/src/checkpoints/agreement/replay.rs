use super::*;
use crate::checkpoint_activation::{ActivationManifest, PreparedActivation};
use crate::checkpoint_deadline::{DeadlineManifest, PreparedDeadline};
use crate::checkpoint_governance::{GovernanceManifest, PreparedGovernance};
use crate::checkpoint_outcome::{ChallengeOutcomeManifest, PreparedChallengeOutcome};
use crate::checkpoint_roster::{PreparedRoster, RosterManifest};
use burst_consensus::checkpoint_round::Phase;

/// Untrusted typed input. The candidate kind, target and history must agree
/// with semantic replay; a valid proposal signature alone does not establish it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplayManifest {
    Selection(CheckpointManifest),
    ChallengeOutcome(ChallengeOutcomeManifest),
    Roster(RosterManifest),
    Activation(ActivationManifest),
    Deadline(DeadlineManifest),
    Governance(GovernanceManifest),
}

impl ReplayManifest {
    pub fn heads(&self) -> &[crate::confirmed_history::HistoryHead] {
        match self {
            Self::Selection(value) => &value.heads,
            Self::ChallengeOutcome(value) => &value.heads,
            Self::Roster(value) => &value.heads,
            Self::Activation(value) => &value.heads,
            Self::Deadline(value) => &value.heads,
            Self::Governance(value) => &value.heads,
        }
    }

    pub fn checkpoint(&self) -> &Checkpoint {
        match self {
            Self::Selection(value) => &value.checkpoint,
            Self::ChallengeOutcome(value) => &value.checkpoint,
            Self::Roster(value) => &value.checkpoint,
            Self::Activation(value) => &value.checkpoint,
            Self::Deadline(value) => &value.checkpoint,
            Self::Governance(value) => &value.checkpoint,
        }
    }

    pub(crate) fn replay(
        &self,
        store: &LmdbStore,
        params: &ProtocolParams,
    ) -> Result<ReplayedCandidate, StoreError> {
        match self {
            Self::Selection(value) => {
                crate::checkpoint_store::CertifiedCheckpoint::replay_manifest(store, params, value)
                    .map(|value| ReplayedCandidate::Selection(Box::new(value)))
            }
            Self::ChallengeOutcome(value) => {
                PreparedChallengeOutcome::replay_manifest(store, params, value)
                    .map(|value| ReplayedCandidate::ChallengeOutcome(Box::new(value)))
            }
            Self::Roster(value) => PreparedRoster::replay_manifest(store, params, value)
                .map(|value| ReplayedCandidate::Roster(Box::new(value))),
            Self::Activation(value) => PreparedActivation::replay_manifest(store, params, value)
                .map(|value| ReplayedCandidate::Activation(Box::new(value))),
            Self::Deadline(value) => PreparedDeadline::replay_manifest(store, params, value)
                .map(|value| ReplayedCandidate::Deadline(Box::new(value))),
            Self::Governance(value) => PreparedGovernance::replay_manifest(store, params, value)
                .map(|value| ReplayedCandidate::Governance(Box::new(value))),
        }
    }
}

/// Every value is constructed by signed-history replay. There is no variant
/// for a peer-supplied state snapshot or a caller-supplied validation callback.
pub enum ReplayedCandidate {
    Selection(Box<ReplayedCheckpoint>),
    ChallengeOutcome(Box<PreparedChallengeOutcome>),
    Roster(Box<PreparedRoster>),
    Activation(Box<PreparedActivation>),
    Deadline(Box<PreparedDeadline>),
    Governance(Box<PreparedGovernance>),
}

pub enum ReplayInput<'a> {
    Selection(&'a ReplayedCheckpoint),
    ChallengeOutcome(&'a PreparedChallengeOutcome),
    Roster(&'a PreparedRoster),
    Activation(&'a PreparedActivation),
    Deadline(&'a PreparedDeadline),
    Governance(&'a PreparedGovernance),
}

impl ReplayedCandidate {
    pub fn checkpoint(&self) -> &Checkpoint {
        match self {
            Self::Selection(value) => value.checkpoint(),
            Self::ChallengeOutcome(value) => &value.manifest().checkpoint,
            Self::Roster(value) => &value.manifest().checkpoint,
            Self::Activation(value) => &value.manifest().checkpoint,
            Self::Deadline(value) => &value.manifest().checkpoint,
            Self::Governance(value) => &value.manifest().checkpoint,
        }
    }

    pub fn manifest(&self) -> ReplayManifest {
        self.input().manifest()
    }

    pub fn input(&self) -> ReplayInput<'_> {
        match self {
            Self::Selection(value) => ReplayInput::Selection(value),
            Self::ChallengeOutcome(value) => ReplayInput::ChallengeOutcome(value),
            Self::Roster(value) => ReplayInput::Roster(value),
            Self::Activation(value) => ReplayInput::Activation(value),
            Self::Deadline(value) => ReplayInput::Deadline(value),
            Self::Governance(value) => ReplayInput::Governance(value),
        }
    }
}

impl ReplayInput<'_> {
    pub fn checkpoint(&self) -> &Checkpoint {
        match self {
            Self::Selection(value) => value.checkpoint(),
            Self::ChallengeOutcome(value) => &value.manifest().checkpoint,
            Self::Roster(value) => &value.manifest().checkpoint,
            Self::Activation(value) => &value.manifest().checkpoint,
            Self::Deadline(value) => &value.manifest().checkpoint,
            Self::Governance(value) => &value.manifest().checkpoint,
        }
    }

    pub(super) fn manifest(&self) -> ReplayManifest {
        match self {
            Self::Selection(value) => ReplayManifest::Selection(value.manifest()),
            Self::ChallengeOutcome(value) => {
                ReplayManifest::ChallengeOutcome(value.manifest().clone())
            }
            Self::Roster(value) => ReplayManifest::Roster(value.manifest().clone()),
            Self::Activation(value) => ReplayManifest::Activation(value.manifest().clone()),
            Self::Deadline(value) => ReplayManifest::Deadline(value.manifest().clone()),
            Self::Governance(value) => ReplayManifest::Governance(value.manifest().clone()),
        }
    }

    pub(crate) fn validate_parent(&self, anchor: &CheckpointAnchor) -> Result<(), StoreError> {
        let parent = match self {
            Self::Selection(value) => return value.validate_parent(anchor),
            Self::ChallengeOutcome(value) => value.parent(),
            Self::Roster(value) => value.parent(),
            Self::Activation(value) => value.parent(),
            Self::Deadline(value) => value.parent(),
            Self::Governance(value) => value.parent(),
        };
        if parent.roster() != anchor.roster()
            || parent.quorum_bps() != anchor.quorum_bps()
            || parent.hash() != anchor.hash()
            || parent.height() != anchor.height()
            || parent.cutoff() != anchor.cutoff()
            || parent.genesis_hash() != anchor.genesis_hash()
            || parent.network() != anchor.network()
        {
            return Err(invalid(
                "event signing authority differs from replayed parent",
            ));
        }
        anchor
            .validate_successor(self.checkpoint())
            .map_err(invalid)
    }

    pub(super) fn validate(
        &self,
        anchor: &CheckpointAnchor,
        input: Validation<'_>,
        clock: Option<SigningClock>,
    ) -> Result<(), StoreError> {
        let checkpoint = match self {
            Self::Selection(value) => return value.validate(anchor, input, clock),
            Self::ChallengeOutcome(value) => &value.manifest().checkpoint,
            Self::Roster(value) => &value.manifest().checkpoint,
            Self::Activation(value) => &value.manifest().checkpoint,
            Self::Deadline(value) => &value.manifest().checkpoint,
            Self::Governance(value) => &value.manifest().checkpoint,
        };
        validate_event_cut(anchor, checkpoint, &input, clock, |clock| {
            if let Self::Activation(value) = self {
                value.validate_fresh_time(clock)?;
            }
            Ok(())
        })
    }
}

pub(super) fn validate_event_cut(
    anchor: &CheckpointAnchor,
    checkpoint: &Checkpoint,
    input: &Validation<'_>,
    clock: Option<SigningClock>,
    fresh: impl FnOnce(SigningClock) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    if input.checkpoint != checkpoint {
        return Err(invalid("event signing commitments differ from replay"));
    }
    if input.decided {
        return Ok(());
    }
    if let Some(proof) = input.proof {
        proof.verify(anchor).map_err(invalid)?;
        if proof.ballot.phase != Phase::Prevote || proof.ballot.value != Some(checkpoint.hash()) {
            return Err(invalid("prepare proof does not bind this event"));
        }
        return Ok(());
    }
    let clock =
        clock.ok_or_else(|| invalid("fresh event signing requires a trusted clock bound"))?;
    fresh(clock)?;
    if clock
        .now
        .checked_sub(clock.error_bound)
        .is_none_or(|earliest| checkpoint.cutoff > earliest)
    {
        return Err(invalid("event cutoff is not known to have passed"));
    }
    Ok(())
}
