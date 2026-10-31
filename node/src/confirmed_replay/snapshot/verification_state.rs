//! Checkpoint encoding borrows verification state; decoding owns restoration input.
use burst_verification::{orchestrator::SnapshotView, OrchestratorSnapshot, VerificationOrchestrator};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub(super) enum Verification<'a> {
    Borrowed(&'a VerificationOrchestrator),
    Owned(OrchestratorSnapshot),
}

impl Verification<'_> {
    pub(super) fn snapshot_view(&self) -> SnapshotView<'_> {
        match self {
            Self::Borrowed(state) => state.snapshot_view(),
            Self::Owned(state) => state.snapshot_view(),
        }
    }

    pub(super) fn into_owned(self) -> OrchestratorSnapshot {
        match self {
            Self::Borrowed(state) => state.snapshot(),
            Self::Owned(state) => state,
        }
    }
}

impl Serialize for Verification<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Borrowed(state) => state.snapshot_view().serialize(serializer),
            Self::Owned(state) => state.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Verification<'_> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        super::compact::Value::<OrchestratorSnapshot>::deserialize(deserializer)
            .map(|state| Self::Owned(state.0))
    }
}
