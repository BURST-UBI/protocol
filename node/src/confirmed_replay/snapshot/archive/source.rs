use super::*;
use serde::ser::{Error, SerializeSeq};

pub(crate) trait AuthoritySource {
    fn count(&self) -> Result<usize, ReplayError>;
    fn steps(&self) -> Box<dyn Iterator<Item = Result<SnapshotAuthorityStep, ReplayError>> + '_>;
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
