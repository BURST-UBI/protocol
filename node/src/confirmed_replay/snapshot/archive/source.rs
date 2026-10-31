use super::*;
use serde::ser::{Error, SerializeSeq};
use super::super::installed::authority_history::Entry;
use burst_consensus::checkpoint::CheckpointCertificate;

pub(super) type AuthenticatedStep = (CheckpointCertificate, CheckpointAnchor);

/// Ordered authority steps. Raw steps and serialization do not authenticate
/// the chain. Authentication starts from a caller-trusted base anchor; consume
/// the entire iterator and propagate errors to validate the complete history.
pub(crate) trait AuthoritySource {
    fn count(&self) -> Result<usize, ReplayError>;
    fn steps(&self) -> Box<dyn Iterator<Item = Result<SnapshotAuthorityStep, ReplayError>> + '_>;
    fn authenticated_steps(&self, base: &CheckpointAnchor) -> Box<dyn Iterator<Item = Result<AuthenticatedStep, ReplayError>> + '_> {
        let mut parent = base.clone();
        Box::new(self.steps().map(move |step| {
            let step = step?;
            framed::frame_size(&step)?;
            let next = step.authenticate(&parent).map_err(invalid)?;
            parent = next.clone();
            Ok((step.certificate, next))
        }))
    }
    fn entries(&self, base: &CheckpointAnchor) -> Box<dyn Iterator<Item = Result<Entry, ReplayError>> + '_> {
        let mut parent = base.clone();
        Box::new(self.steps().map(move |step| {
            let step = step?;
            let next = step.authenticate(&parent).map_err(invalid)?;
            let entry = Entry::between(&parent, &step.certificate, &next).map_err(invalid)?;
            parent = next;
            Ok(entry)
        }))
    }

}

impl AuthoritySource for [SnapshotAuthorityStep] {
    fn count(&self) -> Result<usize, ReplayError> {
        Ok(self.len())
    }
    fn steps(&self) -> Box<dyn Iterator<Item = Result<SnapshotAuthorityStep, ReplayError>> + '_> {
        Box::new(self.iter().cloned().map(Ok))
    }
}

impl AuthoritySource for Vec<SnapshotAuthorityStep> {
    fn count(&self) -> Result<usize, ReplayError> {
        Ok(self.len())
    }
    fn steps(&self) -> Box<dyn Iterator<Item = Result<SnapshotAuthorityStep, ReplayError>> + '_> {
        self.as_slice().steps()
    }
}

impl AuthoritySource for super::super::installed::authority_history::AuthorityHistory {
    fn authenticated_steps(&self, base: &CheckpointAnchor) -> Box<dyn Iterator<Item = Result<AuthenticatedStep, ReplayError>> + '_> {
        Box::new(self.authenticated_steps(base).map(|step| step.map_err(invalid)))
    }
    fn entries(&self, base: &CheckpointAnchor) -> Box<dyn Iterator<Item = Result<Entry, ReplayError>> + '_> {
        Box::new(self.encoded_entries(base).map(|entry| entry.map_err(invalid)))
    }

    fn count(&self) -> Result<usize, ReplayError> {
        self.step_count().map_err(invalid)
    }
    fn steps(&self) -> Box<dyn Iterator<Item = Result<SnapshotAuthorityStep, ReplayError>> + '_> {
        Box::new(self.steps().map(|step| step.map_err(invalid)))
    }
}

pub(super) struct Sequence<'a, A: ?Sized>(pub &'a A);

impl<A: AuthoritySource + ?Sized> Serialize for Sequence<'_, A> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let count = self.0.count().map_err(S::Error::custom)?;
        let mut sequence = serializer.serialize_seq(Some(count))?;
        let mut actual = 0usize;
        for step in self.0.steps() {
            sequence.serialize_element(&step.map_err(S::Error::custom)?)?;
            actual += 1;
        }
        if actual != count {
            return Err(S::Error::custom("authority source length changed"));
        }
        sequence.end()
    }
}
