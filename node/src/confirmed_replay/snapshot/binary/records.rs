use super::*;
use burst_trst::{
    engine::{WalletRecord, WalletRecordReader},
    merger_graph::{GraphRecord, GraphRecordReader},
};
use serde::{de::SeqAccess, ser::SerializeSeq};

impl Binary<'_> {
    pub(in crate::confirmed_replay::snapshot) fn is_records(&self) -> bool {
        matches!(
            self,
            Self::Wallets(_) | Self::Graph(_) | Self::RestoredWallets(_) | Self::RestoredGraph(_)
        )
    }

    fn header(&self) -> Result<(u8, Option<u64>), String> {
        match self {
            Self::Wallets(engine) => Ok((0, Some(engine.expiry_secs))),
            Self::RestoredWallets(engine) => Ok((0, Some(engine.expiry_secs))),
            Self::Graph(_) | Self::RestoredGraph(_) => Ok((1, None)),
            _ => Err("legacy snapshot has no record header".into()),
        }
    }

    fn visit_rows(
        &self,
        mut visit: impl FnMut((String, Vec<u8>)) -> Result<(), String>,
    ) -> Result<(), String> {
        match self {
            Self::Wallets(engine) => engine.visit_wallet_records(|row| visit(row.storage_entry()?)),
            Self::RestoredWallets(engine) => {
                engine.visit_wallet_records(|row| visit(row.storage_entry()?))
            }
            Self::Graph(graph) => graph.visit_records(|row| visit(row.storage_entry()?)),
            Self::RestoredGraph(graph) => graph.visit_records(|row| visit(row.storage_entry()?)),
            _ => Err("legacy snapshot has no records".into()),
        }
    }

    fn count(&self) -> Result<usize, String> {
        let (_, expiry) = self.header()?;
        let rows = match self {
            Self::Wallets(engine) => engine.wallet_record_count(),
            Self::RestoredWallets(engine) => engine.wallet_record_count(),
            Self::Graph(graph) => graph.record_count(),
            Self::RestoredGraph(graph) => graph.record_count(),
            _ => return Err("legacy snapshot has no records".into()),
        };
        rows.and_then(|rows| rows.checked_add(if expiry.is_some() { 2 } else { 1 }))
            .ok_or_else(|| "snapshot record count overflow".into())
    }

    pub(in crate::confirmed_replay::snapshot) fn write_records(
        &self,
        mut output: &mut dyn Write,
    ) -> Result<(), ReplayError> {
        let count = self.count().map_err(invalid)?;
        let (tag, expiry) = self.header().map_err(invalid)?;
        super::super::canonical::collection_header(0x80, count, &mut output)?;
        output
            .write_all(&super::super::canonical::encode(&tag)?)
            .map_err(|error| invalid(error.to_string()))?;
        if let Some(expiry) = expiry {
            output
                .write_all(&super::super::canonical::encode(&expiry)?)
                .map_err(|error| invalid(error.to_string()))?;
        }
        self.visit_rows(|(key, bytes)| {
            let row = super::super::canonical::encode(&(key, Blob(bytes.into())))
                .map_err(|error| error.to_string())?;
            output.write_all(&row).map_err(|error| error.to_string())
        })
        .map_err(invalid)
    }

    pub(super) fn serialize_records<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let count = self.count().map_err(serde::ser::Error::custom)?;
        let (tag, expiry) = self.header().map_err(serde::ser::Error::custom)?;
        let mut sequence = serializer.serialize_seq(Some(count))?;
        sequence.serialize_element(&tag)?;
        if let Some(expiry) = expiry {
            sequence.serialize_element(&expiry)?;
        }
        self.visit_rows(|(key, bytes)| {
            sequence
                .serialize_element(&(key, Blob(bytes.into())))
                .map_err(|error| error.to_string())
        })
        .map_err(serde::ser::Error::custom)?;
        sequence.end()
    }
}

pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Binary<'static>, D::Error> {
    struct Records;
    impl<'de> serde::de::Visitor<'de> for Records {
        type Value = Binary<'static>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("economic snapshot records or a legacy byte string")
        }

        fn visit_bytes<E: serde::de::Error>(self, bytes: &[u8]) -> Result<Self::Value, E> {
            Ok(Binary::Decoded(bytes.to_vec()))
        }

        fn visit_byte_buf<E: serde::de::Error>(self, bytes: Vec<u8>) -> Result<Self::Value, E> {
            Ok(Binary::Decoded(bytes))
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
            let tag: u8 = sequence
                .next_element()?
                .ok_or_else(|| serde::de::Error::custom("missing economic record kind"))?;
            match tag {
                0 => {
                    let expiry = sequence
                        .next_element()?
                        .ok_or_else(|| serde::de::Error::custom("missing governed expiry"))?;
                    let mut reader = WalletRecordReader::new(expiry);
                    while let Some((key, bytes)) = sequence.next_element::<(String, Blob)>()? {
                        let record = WalletRecord::from_storage_entry(&key, &bytes.0)
                            .map_err(serde::de::Error::custom)?;
                        reader.push(record).map_err(serde::de::Error::custom)?;
                    }
                    reader
                        .finish()
                        .map(|engine| Binary::RestoredWallets(Box::new(engine)))
                        .map_err(serde::de::Error::custom)
                }
                1 => {
                    let mut reader = GraphRecordReader::default();
                    while let Some((key, bytes)) = sequence.next_element::<(String, Blob)>()? {
                        let record = GraphRecord::from_storage_entry(&key, &bytes.0)
                            .map_err(serde::de::Error::custom)?;
                        reader.push(record).map_err(serde::de::Error::custom)?;
                    }
                    reader
                        .finish()
                        .map(|graph| Binary::RestoredGraph(Box::new(graph)))
                        .map_err(serde::de::Error::custom)
                }
                _ => Err(serde::de::Error::custom("unsupported economic record kind")),
            }
        }
    }
    deserializer.deserialize_any(Records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confirmed_replay::snapshot::canonical::{BinaryField, Field};

    #[test]
    fn economic_records_roundtrip_without_aggregate_binary_strings() {
        let mut engine = TrstEngine::with_expiry(100);
        let owner = WalletAddress::new("brst_record_snapshot");
        let origin = TxHash::new([1; 32]);
        let amount = 1u128 << 80;
        let token = engine
            .mint(origin, owner.clone(), amount, owner, Timestamp::new(10))
            .unwrap();
        engine.receive_token(token, Timestamp::new(10));
        let mut graph = MergerGraph::new();
        graph.record_merge(burst_trst::MergeNode {
            merge_tx: TxHash::new([2; 32]),
            source_origins: vec![burst_trst::MergeSource { origin, amount }],
            total_amount: amount,
            revoked_contribs: HashMap::new(),
        });
        for (index, field) in [Binary::Wallets(&engine), Binary::Graph(&graph)]
            .into_iter()
            .enumerate()
        {
            let mut bytes = Vec::new();
            BinaryField(&field).encode_field(&mut bytes).unwrap();
            assert_eq!(
                bytes,
                super::super::super::canonical::encode(&field).unwrap()
            );
            assert_eq!(
                bytes[0] >> 5,
                4,
                "records are a CBOR array, not a byte string"
            );
            let decoded: Binary = ciborium::from_reader(bytes.as_slice()).unwrap();
            if index == 0 {
                assert!(decoded
                    .into_wallets(100, false)
                    .unwrap()
                    .wallet_snapshot_eq(&engine));
                let decoded: Binary = ciborium::from_reader(bytes.as_slice()).unwrap();
                assert!(decoded.into_wallets(101, false).is_err());
            } else {
                assert!(decoded.into_graph(false).unwrap() == graph);
            }
            let decoded: Binary = ciborium::from_reader(bytes.as_slice()).unwrap();
            assert!(decoded.into_graph(true).is_err());
            let mut value: ciborium::value::Value =
                ciborium::from_reader(bytes.as_slice()).unwrap();
            let ciborium::value::Value::Array(rows) = &mut value else {
                panic!("record array")
            };
            rows.push(rows.last().unwrap().clone());
            let mut duplicate = Vec::new();
            ciborium::into_writer(&value, &mut duplicate).unwrap();
            assert!(ciborium::from_reader::<Binary, _>(duplicate.as_slice()).is_err());
            let mut short = [0; 2];
            assert!(BinaryField(&field)
                .encode_field(&mut short.as_mut_slice())
                .is_err());
        }
    }
}
