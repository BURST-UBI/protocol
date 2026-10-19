use super::invalid;
use crate::confirmed_replay::ReplayError;
use burst_consensus::snapshot::MAX_SNAPSHOT_BYTES;
use ciborium::value::Value;
use serde::Serialize;
use std::io::Write;
mod sorted;

pub(super) trait Field {
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError>;

    fn encode_field_in(
        &self,
        output: &mut dyn Write,
        _directory: Option<&std::path::Path>,
    ) -> Result<(), ReplayError> {
        self.encode_field(output)
    }
}

impl<T: Serialize> Field for T {
    fn encode_field(&self, mut output: &mut dyn Write) -> Result<(), ReplayError> {
        encode_at_depth(self, &mut output, 1)
    }
}

pub(super) struct Map<'a, K, V>(pub &'a std::collections::HashMap<K, V>);

impl<K: Serialize + Eq + std::hash::Hash, V: Serialize> Field for Map<'_, K, V> {
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        self.encode_field_in(output, None)
    }

    fn encode_field_in(
        &self,
        output: &mut dyn Write,
        directory: Option<&std::path::Path>,
    ) -> Result<(), ReplayError> {
        encode_map(self.0, output, directory, |value, mut output| {
            encode_at_depth(value, &mut output, 2)
        })
    }
}

pub(super) struct BlobMap<'a, 'b, K>(pub &'a std::collections::HashMap<K, super::Blob<'b>>);

impl<K: Serialize + Eq + std::hash::Hash> Field for BlobMap<'_, '_, K> {
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        self.encode_field_in(output, None)
    }

    fn encode_field_in(
        &self,
        output: &mut dyn Write,
        directory: Option<&std::path::Path>,
    ) -> Result<(), ReplayError> {
        encode_map(self.0, output, directory, |value, output| {
            Bytes(&value.0).encode_field(output)
        })
    }
}

pub(super) struct Bytes<'a>(pub &'a [u8]);

pub(super) struct OrderedSequence<'a, T>(pub &'a crate::confirmed_replay::receipts::OrderedHistory<T>);

impl<T: Clone + Serialize + serde::de::DeserializeOwned> Field for OrderedSequence<'_, T> {
    fn encode_field(&self, mut output: &mut dyn Write) -> Result<(), ReplayError> {
        let mut output = Bounded {
            output: &mut output,
            written: 0,
            limit: MAX_SNAPSHOT_BYTES as usize,
        };
        collection_header(0x80, self.0.len(), &mut output)?;
        for receipt in self.0.iter() {
            encode_at_depth(receipt?.as_ref(), &mut output, 2)?;
        }
        Ok(())
    }
}

pub(super) struct BinaryField<'a, 'b>(pub &'a super::binary::Binary<'b>);

impl Field for BinaryField<'_, '_> {
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        if self.0.is_records() {
            return self.0.write_records(output);
        }
        let mut counter = Bounded {
            output: std::io::sink(),
            written: 0,
            limit: MAX_SNAPSHOT_BYTES as usize,
        };
        self.0.write(&mut counter)?;
        let length = counter.written;
        let mut output = Bounded {
            output,
            written: 0,
            limit: MAX_SNAPSHOT_BYTES as usize,
        };
        collection_header(0x40, length, &mut output)?;
        let header = output.written;
        self.0.write(&mut output)?;
        if output.written - header != length {
            return Err(invalid("snapshot binary length changed during encoding"));
        }
        Ok(())
    }
}

impl Field for Bytes<'_> {
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        let mut output = Bounded {
            output,
            written: 0,
            limit: MAX_SNAPSHOT_BYTES as usize,
        };
        collection_header(0x40, self.0.len(), &mut output)?;
        output.write_all(self.0).map_err(|e| invalid(e.to_string()))
    }
}

fn encode_map<K: Serialize + Eq + std::hash::Hash, V: Serialize>(
    map: &std::collections::HashMap<K, V>,
    mut output: &mut dyn Write,
    directory: Option<&std::path::Path>,
    encode_value: impl Fn(&V, &mut dyn Write) -> Result<(), ReplayError>,
) -> Result<(), ReplayError> {
    if map.len() > 4096 {
        return sorted::encode(map, output, directory, encode_value);
    }
    let mut key_bytes = map.len();
    let mut entries = map
        .iter()
        .map(|(key, value)| {
            let mut encoded = Vec::new();
            encode_at_depth(key, &mut encoded, 2)?;
            key_bytes = key_bytes
                .checked_add(encoded.len())
                .filter(|bytes| *bytes <= MAX_SNAPSHOT_BYTES as usize)
                .ok_or_else(|| invalid("snapshot exceeds byte limit"))?;
            Ok((encoded, value))
        })
        .collect::<Result<Vec<_>, ReplayError>>()?;
    sort_entries(&mut entries)?;
    let mut output = Bounded {
        output: &mut output,
        written: 0,
        limit: MAX_SNAPSHOT_BYTES as usize,
    };
    collection_header(0xa0, entries.len(), &mut output)?;
    for (key, value) in entries {
        output.write_all(&key).map_err(|e| invalid(e.to_string()))?;
        encode_value(value, &mut output)?;
    }
    Ok(())
}

pub(super) struct Sequence<'a, T>(pub &'a [T]);

impl<T: Serialize> Field for Sequence<'_, T> {
    fn encode_field(&self, mut output: &mut dyn Write) -> Result<(), ReplayError> {
        let mut output = Bounded {
            output: &mut output,
            written: 0,
            limit: MAX_SNAPSHOT_BYTES as usize,
        };
        collection_header(0x80, self.0.len(), &mut output)?;
        for value in self.0 {
            encode_at_depth(value, &mut output, 2)?;
        }
        Ok(())
    }
}

fn sort_entries<T>(entries: &mut [(Vec<u8>, T)]) -> Result<(), ReplayError> {
    entries.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then_with(|| a.0.cmp(&b.0)));
    if entries.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(invalid("duplicate snapshot map key"));
    }
    Ok(())
}

pub(super) fn collection_header(
    major: u8,
    count: usize,
    output: &mut impl Write,
) -> Result<(), ReplayError> {
    // Collection headers use the canonical unsigned integer's length argument.
    let mut header = encode(&(count as u64))?;
    header[0] |= major;
    output
        .write_all(&header)
        .map_err(|e| invalid(e.to_string()))
}

/// Normalize one field at a time; the schema supplies both serde and these fields.
#[cfg(test)]
pub(super) fn encode_record(
    fields: &[(&str, &dyn Field)],
    output: &mut impl Write,
) -> Result<(), ReplayError> {
    encode_record_in(fields, output, None)
}

pub(super) fn encode_record_in(
    fields: &[(&str, &dyn Field)],
    output: &mut impl Write,
    directory: Option<&std::path::Path>,
) -> Result<(), ReplayError> {
    let mut fields = fields
        .iter()
        .map(|(name, field)| Ok((encode(name)?, *field)))
        .collect::<Result<Vec<_>, ReplayError>>()?;
    sort_entries(&mut fields)?;
    let write_fields = |output: &mut dyn Write| {
        let mut output = Bounded {
            output,
            written: 0,
            limit: MAX_SNAPSHOT_BYTES as usize,
        };
        collection_header(0xa0, fields.len(), &mut output)?;
        for (key, field) in &fields {
            output.write_all(key).map_err(|e| invalid(e.to_string()))?;
            field.encode_field_in(&mut output, directory)?;
        }
        Ok(())
    };
    // Preflight the streaming representation, including its complete size and
    // canonical validation, without invoking byte-buffer serde adapters.
    write_fields(&mut std::io::sink())?;
    write_fields(output)
}

struct Bounded<W> {
    output: W,
    written: usize,
    limit: usize,
}

impl<W: Write> Write for Bounded<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.written) {
            return Err(std::io::Error::other("snapshot exceeds byte limit"));
        }
        self.output.write_all(bytes)?;
        self.written += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.output.flush()
    }
}

/// Map keys use length-first encoded ordering; sequence order remains semantic.
/// Set fields must serialize in sorted order at their type boundary.
pub(super) fn encode<T: Serialize>(state: &T) -> Result<Vec<u8>, ReplayError> {
    let mut output = Vec::new();
    encode_into(state, &mut output)?;
    Ok(output)
}

pub(super) fn encode_into<T: Serialize>(
    state: &T,
    output: &mut impl std::io::Write,
) -> Result<(), ReplayError> {
    encode_at_depth(state, output, 0)
}

fn encode_at_depth<T: Serialize>(
    state: &T,
    output: &mut impl Write,
    depth: usize,
) -> Result<(), ReplayError> {
    // Bound the encoded source before expanding it into a sortable value tree.
    write_bounded(state, &mut std::io::sink(), MAX_SNAPSHOT_BYTES as usize)?;
    let mut value = Value::serialized(state).map_err(|e| invalid(e.to_string()))?;
    normalize(&mut value, depth)?;
    write_bounded(&value, output, MAX_SNAPSHOT_BYTES as usize)
}

fn bytes(value: &Value) -> Result<Vec<u8>, ReplayError> {
    write(value, MAX_SNAPSHOT_BYTES as usize)
}

fn write<T: Serialize>(value: &T, limit: usize) -> Result<Vec<u8>, ReplayError> {
    let mut bytes = Vec::new();
    write_bounded(value, &mut bytes, limit)?;
    Ok(bytes)
}

fn write_bounded<T: Serialize>(
    value: &T,
    output: &mut impl std::io::Write,
    limit: usize,
) -> Result<(), ReplayError> {
    let mut writer = Bounded {
        output,
        written: 0,
        limit,
    };
    ciborium::into_writer(value, &mut writer).map_err(|e| invalid(e.to_string()))?;
    Ok(())
}

fn normalize(value: &mut Value, depth: usize) -> Result<(), ReplayError> {
    if depth > 64 {
        return Err(invalid("snapshot nesting exceeds limit"));
    }
    match value {
        Value::Map(entries) => {
            let mut sorted = Vec::with_capacity(entries.len());
            for (mut key, mut value) in std::mem::take(entries) {
                normalize(&mut key, depth + 1)?;
                normalize(&mut value, depth + 1)?;
                sorted.push((bytes(&key)?, key, value));
            }
            sorted.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then_with(|| a.0.cmp(&b.0)));
            if sorted.windows(2).any(|pair| pair[0].0 == pair[1].0) {
                return Err(invalid("duplicate snapshot map key"));
            }
            *entries = sorted.into_iter().map(|(_, k, v)| (k, v)).collect();
        }
        Value::Array(values) => {
            for value in values {
                normalize(value, depth + 1)?;
            }
        }
        Value::Tag(_, value) => normalize(value, depth + 1)?,
        Value::Float(_) => return Err(invalid("floating point snapshot state is unsupported")),
        _ => (),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_blob_fields_and_maps_keep_exact_byte_string_encoding() {
        use super::super::Blob;
        use std::borrow::Cow;
        #[derive(serde::Serialize, serde::Deserialize)]
        struct Record<'a> {
            bytes: Blob<'a>,
            metadata: std::collections::HashMap<u64, Blob<'a>>,
        }
        for size in [0, 23, 24, 255, 256, 65535, 65536] {
            let payload = vec![0xab; size];
            let record = Record {
                bytes: Blob(Cow::Borrowed(&payload)),
                metadata: std::collections::HashMap::from([
                    (24, Blob(Cow::Borrowed(&payload))),
                    (1, Blob(Cow::Borrowed(&payload))),
                ]),
            };
            let mut output = Vec::new();
            encode_record(
                &[
                    ("bytes", &Bytes(&record.bytes.0)),
                    ("metadata", &BlobMap(&record.metadata)),
                ],
                &mut output,
            )
            .unwrap();
            assert_eq!(output, encode(&record).unwrap());
            let decoded: Record = ciborium::from_reader(output.as_slice()).unwrap();
            assert!(matches!(decoded.bytes.0, Cow::Owned(_)));
            assert_eq!(decoded.bytes.0.as_ref(), payload.as_slice());
            assert_eq!(decoded.metadata[&24].0.as_ref(), payload.as_slice());
        }
    }

    #[test]
    fn fieldwise_records_preserve_bytes_depth_and_invalid_value_rejection() {
        #[derive(Serialize)]
        struct Record {
            z: std::collections::HashMap<u64, u128>,
            a: Value,
        }
        let mut record = Record {
            z: std::collections::HashMap::from([(24, u128::MAX), (1, 7)]),
            a: Value::Null,
        };
        for _ in 0..63 {
            record.a = Value::Array(vec![record.a]);
        }
        let render = |record: &Record| {
            let mut bytes = Vec::new();
            encode_record(&[("z", &Map(&record.z)), ("a", &record.a)], &mut bytes)?;
            Ok::<_, ReplayError>(bytes)
        };
        assert_eq!(render(&record).unwrap(), encode(&record).unwrap());
        record.a = Value::Array(vec![record.a]);
        assert!(render(&record).is_err());
        assert!(encode(&record).is_err());
        record.a = Value::Float(1.0);
        assert!(render(&record).is_err());
        assert!(encode(&record).is_err());
        record.a = Value::Map(vec![(Value::Null, Value::Null), (Value::Null, Value::Null)]);
        assert!(render(&record).is_err());
        assert!(encode(&record).is_err());
        record.a = Value::Null;
        assert!(encode_record(&[("a", &record.a), ("a", &record.a)], &mut Vec::new()).is_err());
    }

    #[test]
    fn streamed_collections_preserve_headers_order_and_nested_depth() {
        #[derive(Serialize)]
        struct Record {
            map: std::collections::HashMap<(u64, u64), Value>,
            seq: Vec<Value>,
        }
        let render = |record: &Record| {
            let mut bytes = Vec::new();
            encode_record(
                &[("map", &Map(&record.map)), ("seq", &Sequence(&record.seq))],
                &mut bytes,
            )?;
            Ok::<_, ReplayError>(bytes)
        };
        for count in [0, 1, 23, 24, 255, 256] {
            let record = Record {
                map: (0..count)
                    .rev()
                    .map(|i| ((i, count - i), Value::serialized(&u128::MAX).unwrap()))
                    .collect(),
                seq: (0..count).rev().map(Value::from).collect(),
            };
            assert_eq!(render(&record).unwrap(), encode(&record).unwrap());
        }
        let mut nested = Value::Null;
        for _ in 0..62 {
            nested = Value::Array(vec![nested]);
        }
        let mut record = Record {
            map: std::collections::HashMap::from([((1, 2), nested.clone())]),
            seq: vec![nested.clone()],
        };
        assert_eq!(render(&record).unwrap(), encode(&record).unwrap());
        record.seq = vec![Value::Array(vec![nested.clone()])];
        assert!(render(&record).is_err());
        assert!(encode(&record).is_err());
        record.seq.clear();
        record.map.insert((1, 2), Value::Array(vec![nested]));
        assert!(render(&record).is_err());
        assert!(encode(&record).is_err());
    }

    #[test]
    fn streamed_maps_reject_distinct_keys_with_identical_encodings() {
        #[derive(Eq, PartialEq, Hash)]
        struct Key(u8);
        impl Serialize for Key {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_u8(7)
            }
        }
        let map = std::collections::HashMap::from([(Key(1), 1u8), (Key(2), 2u8)]);
        let mut output = Vec::new();
        assert!(Map(&map).encode_field(&mut output).is_err());
        assert!(output.is_empty());
        assert!(encode(&map).is_err());
    }

    #[test]
    fn fieldwise_record_output_propagates_io_errors_and_caps_aggregate_bytes() {
        struct TooLarge;
        impl Field for TooLarge {
            fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
                use std::io::Read;
                std::io::copy(&mut std::io::repeat(0).take(MAX_SNAPSHOT_BYTES + 1), output)
                    .map_err(|e| invalid(e.to_string()))?;
                Ok(())
            }
        }
        assert!(encode_record(&[("a", &TooLarge)], &mut std::io::sink())
            .unwrap_err()
            .to_string()
            .contains("snapshot exceeds byte limit"));
        struct FailedWriter;
        impl Write for FailedWriter {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("injected output failure"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert!(encode_record(&[], &mut FailedWriter)
            .unwrap_err()
            .to_string()
            .contains("injected output failure"));
        assert!(Map(&std::collections::HashMap::from([(1u8, 2u8)]))
            .encode_field(&mut FailedWriter)
            .is_err());
        assert!(Sequence(&[1u8, 2u8])
            .encode_field(&mut FailedWriter)
            .is_err());
        assert!(Bytes(&[1, 2]).encode_field(&mut FailedWriter).is_err());
    }

    #[test]
    fn map_order_is_irrelevant_but_sequence_order_and_full_u128_are_preserved() {
        let a = std::collections::HashMap::from([(1u64, u128::MAX), (2, 1)]);
        let b = std::collections::HashMap::from([(2u64, 1u128), (1, u128::MAX)]);
        let bytes = encode(&a).unwrap();
        let mut legacy: Value = ciborium::from_reader(write(&a, 4096).unwrap().as_slice()).unwrap();
        normalize(&mut legacy, 0).unwrap();
        assert_eq!(bytes, super::bytes(&legacy).unwrap());
        assert_eq!(bytes, encode(&b).unwrap());
        let decoded: std::collections::HashMap<u64, u128> =
            ciborium::from_reader(bytes.as_slice()).unwrap();
        assert_eq!(decoded, a);
        assert_ne!(
            encode(&vec![1u64, 2]).unwrap(),
            encode(&vec![2u64, 1]).unwrap()
        );
        let blob = super::super::Blob(vec![0xab; 4096].into());
        assert_eq!(encode(&blob).unwrap().len(), 4099);
        assert!(write(&blob, 4098).is_err());
        assert!(write(&blob, 4099).is_ok());
        assert!(write_bounded(&blob, &mut std::io::sink(), 4098).is_err());
        assert!(write_bounded(&blob, &mut std::io::sink(), 4099).is_ok());
    }
}
