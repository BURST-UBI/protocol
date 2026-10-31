use super::*;
use std::io::Write;
mod records;

/// Export borrows engines. Record imports validate rows while building the
/// in-memory engine; legacy imports retain the whole byte string until decoding.
/// The caller bounds input size and authenticates the snapshot.
pub(super) enum Binary<'a> {
    Wallets(&'a TrstEngine),
    Graph(&'a MergerGraph),
    LegacyWallets(&'a TrstEngine),
    LegacyGraph(&'a MergerGraph),
    RestoredWallets(Box<TrstEngine>),
    RestoredGraph(Box<MergerGraph>),
    Decoded(Vec<u8>),
}

impl Binary<'_> {
    pub(super) fn write(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        match self {
            Self::LegacyWallets(engine) => engine
                .write_wallets(output)
                .map_err(|e| invalid(e.to_string())),
            Self::LegacyGraph(graph) => graph
                .write_snapshot(output)
                .map_err(|e| invalid(e.to_string())),
            Self::Decoded(bytes) => output.write_all(bytes).map_err(|e| invalid(e.to_string())),
            _ => Err(invalid("record snapshot is not a legacy binary string")),
        }
    }

    #[cfg(test)]
    pub(super) fn into_decoded(self) -> Result<Vec<u8>, ReplayError> {
        match self {
            Self::Decoded(bytes) => Ok(bytes),
            _ => Err(invalid("snapshot binary field was not decoded")),
        }
    }

    pub(super) fn into_wallets(self, expiry: u64, legacy: bool) -> Result<TrstEngine, ReplayError> {
        match self {
            Self::Decoded(bytes) if legacy => {
                TrstEngine::load_wallets(&bytes, expiry).map_err(|error| invalid(error.to_string()))
            }
            Self::RestoredWallets(engine) if !legacy && engine.expiry_secs == expiry => Ok(*engine),
            _ => Err(invalid(
                "snapshot holdings format or governed expiry mismatch",
            )),
        }
    }

    pub(super) fn into_graph(self, legacy: bool) -> Result<MergerGraph, ReplayError> {
        match self {
            Self::Decoded(bytes) if legacy => MergerGraph::from_bytes(&bytes).map_err(invalid),
            Self::RestoredGraph(graph) if !legacy => Ok(*graph),
            _ => Err(invalid("snapshot graph format mismatch")),
        }
    }
}

// Production encoding uses canonical::BinaryField. Serde provides the matching
// representation for record sequences and legacy byte strings.
impl Serialize for Binary<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.is_records() {
            return self.serialize_records(serializer);
        }
        if let Self::Decoded(bytes) = self {
            return serializer.serialize_bytes(bytes);
        }
        let mut bytes = Vec::new();
        self.write(&mut bytes).map_err(serde::ser::Error::custom)?;
        serializer.serialize_bytes(&bytes)
    }
}

impl<'de> Deserialize<'de> for Binary<'_> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        records::deserialize(deserializer)
    }
}

#[cfg(test)]
mod tests {
    use super::super::canonical::{BinaryField, Field};
    use super::*;

    #[test]
    fn binary_fields_preserve_bytes_and_propagate_write_failures() {
        let engine = TrstEngine::new();
        let graph = MergerGraph::new();
        let legacy_graph = [
            b"BRSTMG03".as_slice(),
            bincode::serialize(&graph).unwrap().as_slice(),
        ]
        .concat();
        for (field, expected) in [
            (Binary::LegacyWallets(&engine), engine.save_wallets()),
            (Binary::LegacyGraph(&graph), legacy_graph),
        ] {
            let mut bytes = Vec::new();
            BinaryField(&field).encode_field(&mut bytes).unwrap();
            assert_eq!(
                bytes,
                super::super::canonical::encode(&Blob(expected.clone().into())).unwrap()
            );
            let decoded: Binary = ciborium::from_reader(bytes.as_slice()).unwrap();
            assert_eq!(decoded.into_decoded().unwrap(), expected);
            let mut short = [0u8; 7];
            assert!(BinaryField(&field)
                .encode_field(&mut short.as_mut_slice())
                .is_err());
            assert!(field.into_decoded().is_err());
        }
    }
}
