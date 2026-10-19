use super::*;
use crate::confirmed_replay::snapshot::installed::authority_history::AuthorityHistory;
use serde::de::{DeserializeSeed, Error, SeqAccess, Visitor};

pub(in crate::confirmed_replay::snapshot) enum Authority {
    Memory(Vec<SnapshotAuthorityStep>),
    File(Box<AuthorityHistory>),
}

impl AuthoritySource for Authority {
    fn count(&self) -> Result<usize, ReplayError> {
        match self {
            Self::Memory(steps) => steps.count(),
            Self::File(history) => history.count(),
        }
    }

    fn steps(&self) -> Box<dyn Iterator<Item = Result<SnapshotAuthorityStep, ReplayError>> + '_> {
        match self {
            Self::Memory(steps) => steps.steps(),
            Self::File(history) => AuthoritySource::steps(history.as_ref()),
        }
    }
}

impl Authority {
    pub(in crate::confirmed_replay::snapshot) fn steps(
        &self,
    ) -> Box<dyn Iterator<Item = Result<SnapshotAuthorityStep, ReplayError>> + '_> {
        AuthoritySource::steps(self)
    }

    pub(in crate::confirmed_replay::snapshot) fn into_history(
        self,
        base: &CheckpointAnchor,
        directory: &std::path::Path,
    ) -> Result<AuthorityHistory, burst_store::StoreError> {
        match self {
            Self::File(history) => Ok(*history),
            Self::Memory(steps) => {
                let mut history = AuthorityHistory::new(base.clone(), directory)?;
                let mut parent = base.clone();
                for step in steps {
                    let next = step
                        .authenticate(&parent)
                        .map_err(|e| burst_store::StoreError::Corruption(e.to_string()))?;
                    history.append(&step.certificate, &next)?;
                    parent = next;
                }
                Ok(history)
            }
        }
    }
}

type StagedHeader = (
    Authority,
    SnapshotCertificate,
    Vec<(burst_types::BlockHash, burst_vrf::RandomOutput)>,
);

struct AuthoritySeed<'a> {
    base: &'a CheckpointAnchor,
    directory: &'a std::path::Path,
}

impl<'de> DeserializeSeed<'de> for AuthoritySeed<'_> {
    type Value = Authority;

    fn deserialize<D: serde::Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        de.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for AuthoritySeed<'_> {
    type Value = Authority;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("an ordered checkpoint certificate sequence")
    }

    fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
        let mut history =
            AuthorityHistory::new(self.base.clone(), self.directory).map_err(S::Error::custom)?;
        let mut parent = self.base.clone();
        while let Some(step) = seq.next_element::<SnapshotAuthorityStep>()? {
            let next = step.authenticate(&parent).map_err(S::Error::custom)?;
            history
                .append(&step.certificate, &next)
                .map_err(S::Error::custom)?;
            parent = next;
        }
        Ok(Authority::File(Box::new(history)))
    }
}

struct HeaderSeed<'a>(AuthoritySeed<'a>);

impl<'de> DeserializeSeed<'de> for HeaderSeed<'_> {
    type Value = StagedHeader;

    fn deserialize<D: serde::Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        de.deserialize_tuple(3, self)
    }
}

impl<'de> Visitor<'de> for HeaderSeed<'_> {
    type Value = StagedHeader;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a v2 snapshot header")
    }

    fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
        let authority = seq
            .next_element_seed(self.0)?
            .ok_or_else(|| S::Error::custom("missing snapshot authority"))?;
        let certificate = seq
            .next_element()?
            .ok_or_else(|| S::Error::custom("missing snapshot certificate"))?;
        let beacons = seq
            .next_element()?
            .ok_or_else(|| S::Error::custom("missing snapshot beacons"))?;
        Ok((authority, certificate, beacons))
    }
}

pub(super) fn read_header(
    base: &CheckpointAnchor,
    reader: &mut impl Read,
    length: u64,
    directory: &std::path::Path,
) -> Result<StagedHeader, ReplayError> {
    if length == 0 || length > MAX_HEADER_BYTES {
        return Err(invalid("snapshot archive header exceeds its limit"));
    }
    let mut framed = std::io::BufReader::with_capacity(HEADER_BUFFER_BYTES, reader.take(length));
    let header = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(length)
        .deserialize_from_seed(HeaderSeed(AuthoritySeed { base, directory }), &mut framed)
        .map_err(invalid)?;
    if !framed.buffer().is_empty() || framed.get_ref().limit() != 0 {
        return Err(invalid("trailing snapshot header data"));
    }
    Ok(header)
}
