//! Lossless record encodings for snapshot body version eight.
//! Legacy bodies retain their original encoding; ledger storage is unchanged.

use super::{canonical, invalid, Blob, ReplayError, WalletAddress};
use bincode::Options;
use canonical::{Field, MapSource};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{borrow::Cow, io::Write};

pub(super) struct WalletMap<'a, M> {
    pub map: &'a M,
    pub compact: bool,
}

impl<M: MapSource<Key = WalletAddress>> Field for WalletMap<'_, M> {
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        self.encode_field_at(output, None, 1)
    }
    fn encode_field_at(
        &self,
        output: &mut dyn Write,
        directory: Option<&std::path::Path>,
        depth: usize,
    ) -> Result<(), ReplayError> {
        if !self.compact {
            return canonical::Map(self.map).encode_field_at(output, directory, depth);
        }
        canonical::encode_map_with_keys(
            self.map,
            output,
            directory,
            depth,
            |key, output, depth| {
                canonical::encode_at_depth(&Address(Cow::Borrowed(key)), output, depth)
            },
            |value, output, _| write_value(value, output),
        )
    }
}

pub(super) struct Metadata<'a, 'b> {
    pub map: &'a std::collections::HashMap<String, Blob<'b>>,
    pub compact: bool,
}

impl Field for Metadata<'_, '_> {
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        self.encode_field_at(output, None, 1)
    }
    fn encode_field_at(
        &self,
        output: &mut dyn Write,
        directory: Option<&std::path::Path>,
        depth: usize,
    ) -> Result<(), ReplayError> {
        if !self.compact {
            return canonical::BlobMap(self.map).encode_field_at(output, directory, depth);
        }
        canonical::encode_map_with_keys(
            self.map,
            output,
            directory,
            depth,
            |key, output, depth| {
                canonical::encode_at_depth(&MetadataKey(Cow::Borrowed(key)), output, depth)
            },
            |value, output, depth| {
                canonical::Bytes(&value.0).encode_field_at(output, directory, depth)
            },
        )
    }
}

pub(super) struct Verification<'a, 'b> {
    pub state: &'a super::verification_state::Verification<'b>,
    pub compact: bool,
}

impl Field for Verification<'_, '_> {
    fn encode_field(&self, output: &mut dyn Write) -> Result<(), ReplayError> {
        if self.compact {
            write_value(&self.state.snapshot_view(), output)
        } else {
            canonical::VerificationField(self.state.snapshot_view(), &|| Ok(()))
                .encode_field(output)
        }
    }
    fn encode_field_in(
        &self,
        output: &mut dyn Write,
        directory: Option<&std::path::Path>,
    ) -> Result<(), ReplayError> {
        if self.compact {
            self.encode_field(output)
        } else {
            canonical::VerificationField(self.state.snapshot_view(), &|| Ok(()))
                .encode_field_in(output, directory)
        }
    }
}

/// Decode either a legacy struct map or a compact typed byte string. The caller's
/// version-specific canonical comparison rejects mixed or nonminimal encodings.
pub(super) struct Value<T>(pub T);

impl<'de, T: DeserializeOwned> Deserialize<'de> for Value<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor<T>(std::marker::PhantomData<T>);
        impl<'de, T: DeserializeOwned> serde::de::Visitor<'de> for Visitor<T> {
            type Value = T;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a legacy record map or bounded compact record bytes")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<T, A::Error> {
                T::deserialize(serde::de::value::MapAccessDeserializer::new(map))
            }
            fn visit_bytes<E: serde::de::Error>(self, bytes: &[u8]) -> Result<T, E> {
                read_value(bytes).map_err(E::custom)
            }
        }
        deserializer
            .deserialize_any(Visitor(std::marker::PhantomData))
            .map(Self)
    }
}

pub(super) fn read_wallet_map<'de, D: serde::Deserializer<'de>, T: DeserializeOwned>(
    deserializer: D,
) -> Result<std::collections::HashMap<WalletAddress, T>, D::Error> {
    struct Visitor<T>(std::marker::PhantomData<T>);
    impl<'de, T: DeserializeOwned> serde::de::Visitor<'de> for Visitor<T> {
        type Value = std::collections::HashMap<WalletAddress, T>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("snapshot wallet records")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            use serde::de::Error;
            let mut rows = std::collections::HashMap::new();
            while let Some(key) = map.next_key::<Address<'_>>()? {
                let value = map.next_value::<Value<T>>()?.0;
                if rows.insert(key.0.into_owned(), value).is_some() {
                    return Err(A::Error::custom("duplicate snapshot wallet key"));
                }
            }
            Ok(rows)
        }
    }
    deserializer.deserialize_map(Visitor(std::marker::PhantomData))
}

pub(super) fn read_accounts<'de, 'a, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<
    Cow<'a, std::collections::HashMap<WalletAddress, burst_store::account::AccountInfo>>,
    D::Error,
> {
    read_wallet_map(deserializer).map(Cow::Owned)
}

pub(super) fn read_metadata<'de, 'a, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<std::collections::HashMap<String, Blob<'a>>, D::Error> {
    struct Visitor<'a>(std::marker::PhantomData<&'a ()>);
    impl<'de, 'a> serde::de::Visitor<'de> for Visitor<'a> {
        type Value = std::collections::HashMap<String, Blob<'a>>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("snapshot lifecycle metadata")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            use serde::de::Error;
            let mut rows = std::collections::HashMap::new();
            while let Some(key) = map.next_key::<MetadataKey<'_>>()? {
                let value = map.next_value::<Blob<'a>>()?;
                if rows.insert(key.0.into_owned(), value).is_some() {
                    return Err(A::Error::custom("duplicate snapshot metadata key"));
                }
            }
            Ok(rows)
        }
    }
    deserializer.deserialize_map(Visitor(std::marker::PhantomData))
}

fn options() -> impl Options {
    bincode::DefaultOptions::new()
        .with_little_endian()
        .with_varint_encoding()
        .reject_trailing_bytes()
        .with_limit(burst_consensus::snapshot::MAX_SNAPSHOT_BYTES)
}

pub(super) fn write_value<T: Serialize + ?Sized>(
    value: &T,
    output: &mut dyn Write,
) -> Result<(), ReplayError> {
    let length = options()
        .serialized_size(value)
        .map_err(|error| invalid(error.to_string()))?;
    let length = usize::try_from(length).map_err(|error| invalid(error.to_string()))?;
    let mut header = Vec::new();
    canonical::collection_header(0x40, length, &mut header)?;
    if length
        .checked_add(header.len())
        .is_none_or(|bytes| bytes as u64 > burst_consensus::snapshot::MAX_SNAPSHOT_BYTES)
    {
        return Err(invalid("snapshot record exceeds byte limit"));
    }
    output
        .write_all(&header)
        .map_err(|error| invalid(error.to_string()))?;
    options()
        .serialize_into(output, value)
        .map_err(|error| invalid(error.to_string()))
}

const IDENTITY_PREFIXES: [&str; 3] = [
    "verification_identity_source/v1/",
    "verification_identity_bound/v1/",
    "verification_identity_binding/v1/",
];

/// Only the key representation changes. Every source/bound/binding value remains
/// present; unknown namespaces and noncanonical suffixes remain exact strings.
pub(super) struct MetadataKey<'a>(pub Cow<'a, str>);

impl Serialize for MetadataKey<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        for (index, prefix) in IDENTITY_PREFIXES.iter().enumerate() {
            let Some(suffix) = self.0.strip_prefix(prefix) else {
                continue;
            };
            if index < 2 {
                if let Some(bytes) = burst_crypto::decode_address(suffix) {
                    return ((index + 1) as u8, Blob(Cow::Borrowed(&bytes))).serialize(serializer);
                }
            } else if suffix.len() == 64 && suffix.is_ascii() {
                let mut bytes = [0; 32];
                let mut valid = true;
                for (offset, byte) in bytes.iter_mut().enumerate() {
                    match u8::from_str_radix(&suffix[offset * 2..offset * 2 + 2], 16) {
                        Ok(value) => *byte = value,
                        Err(_) => {
                            valid = false;
                            break;
                        }
                    }
                }
                if valid && burst_types::BlockHash::new(bytes).to_string() == suffix {
                    return (3u8, Blob(Cow::Borrowed(&bytes))).serialize(serializer);
                }
            }
        }
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for MetadataKey<'_> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = String;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an identity key record or an unchanged metadata key")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<String, E> {
                Ok(value.to_owned())
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<String, A::Error> {
                use serde::de::Error;
                let tag: u8 = seq
                    .next_element()?
                    .ok_or_else(|| A::Error::custom("missing identity key tag"))?;
                if !(1..=3).contains(&tag) {
                    return Err(A::Error::custom("unknown identity key tag"));
                }
                let blob: Blob<'_> = seq
                    .next_element()?
                    .ok_or_else(|| A::Error::custom("missing identity key suffix"))?;
                let bytes: [u8; 32] = blob
                    .0
                    .as_ref()
                    .try_into()
                    .map_err(|_| A::Error::custom("invalid identity key suffix length"))?;
                if seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
                    return Err(A::Error::custom("trailing identity key fields"));
                }
                let suffix = if tag == 3 {
                    burst_types::BlockHash::new(bytes).to_string()
                } else {
                    burst_crypto::derive_address(&burst_types::PublicKey(bytes)).to_string()
                };
                Ok(format!(
                    "{}{suffix}",
                    IDENTITY_PREFIXES[usize::from(tag - 1)]
                ))
            }
        }
        deserializer
            .deserialize_any(Visitor)
            .map(|value| Self(Cow::Owned(value)))
    }
}

pub(super) fn read_value<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ReplayError> {
    if bytes.len() as u64 > burst_consensus::snapshot::MAX_SNAPSHOT_BYTES {
        return Err(invalid("snapshot record exceeds byte limit"));
    }
    options()
        .with_limit(bytes.len() as u64)
        .deserialize(bytes)
        .map_err(|error| invalid(error.to_string()))
}

/// Full checksum-validated addresses have a reversible 32-byte representation.
/// Preserve other strings exactly; synthetic and legacy state may contain them.
pub(super) struct Address<'a>(pub Cow<'a, WalletAddress>);

impl Serialize for Address<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match burst_crypto::decode_address(self.0.as_str()) {
            Some(bytes) => serializer.serialize_bytes(&bytes),
            None => serializer.serialize_str(self.0.as_str()),
        }
    }
}

impl<'de> Deserialize<'de> for Address<'_> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = WalletAddress;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a 32-byte address key or an unchanged address string")
            }
            fn visit_bytes<E: serde::de::Error>(self, bytes: &[u8]) -> Result<Self::Value, E> {
                let key = bytes
                    .try_into()
                    .map_err(|_| E::custom("invalid address key length"))?;
                Ok(burst_crypto::derive_address(&burst_types::PublicKey(key)))
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                // WalletAddress's derived decoder preserves the original string,
                // including legacy values. Context validation remains separate.
                ciborium::Value::Text(value.to_owned())
                    .deserialized()
                    .map_err(E::custom)
            }
        }
        deserializer
            .deserialize_any(Visitor)
            .map(|value| Self(Cow::Owned(value)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::account::AccountInfo;
    use burst_types::{BlockHash, Timestamp, WalletState};

    fn roundtrip<T: Serialize + DeserializeOwned>(value: &T) -> T {
        let mut encoded = Vec::new();
        write_value(value, &mut encoded).unwrap();
        let blob: Blob<'_> = ciborium::from_reader(encoded.as_slice()).unwrap();
        let decoded = read_value(&blob.0).unwrap();
        let mut again = Vec::new();
        write_value(&decoded, &mut again).unwrap();
        assert_eq!(encoded, again);
        let mut trailing = blob.0.into_owned();
        trailing.push(0);
        assert!(read_value::<T>(&trailing).is_err());
        decoded
    }

    #[test]
    fn compact_records_preserve_economic_extremes_and_reject_trailing_bytes() {
        let wallet = burst_crypto::derive_address(&burst_types::PublicKey([17; 32]));
        let mut account = AccountInfo::unopened(wallet);
        account.state = WalletState::Challenged;
        account.verified_at = Some(Timestamp::new(u64::MAX));
        account.head = BlockHash::new([255; 32]);
        account.block_count = u64::MAX;
        account.confirmation_height = u64::MAX - 1;
        account.total_brn_burned = u128::MAX;
        account.total_brn_staked = u128::MAX - 1;
        account.trst_balance = u128::MAX - 2;
        account.expired_trst = u128::MAX - 3;
        account.revoked_trst = u128::MAX - 4;
        account.epoch = u8::MAX;
        account.verifier_opted_in_at = Some(Timestamp::new(u64::MAX - 1));
        account.orv_evicted = true;
        assert_eq!(roundtrip(&account), account);
        let mut brn = burst_brn::BrnWalletState::new(Timestamp::new(u64::MAX));
        brn.total_burned = u128::MAX;
        brn.total_staked = u128::MAX - 1;
        brn.accrual_active = false;
        brn.accrual_stopped_at = Some(Timestamp::new(u64::MAX));
        assert_eq!(roundtrip(&brn), brn);
    }

    #[test]
    fn compact_address_keys_are_lossless_and_reject_wrong_binary_lengths() {
        for wallet in [
            burst_crypto::derive_address(&burst_types::PublicKey([0; 32])),
            burst_crypto::derive_address(&burst_types::PublicKey([255; 32])),
            WalletAddress::new("brst_legacy_fixture"),
        ] {
            let mut bytes = Vec::new();
            ciborium::into_writer(&Address(Cow::Borrowed(&wallet)), &mut bytes).unwrap();
            let decoded: Address<'_> = ciborium::from_reader(bytes.as_slice()).unwrap();
            assert_eq!(decoded.0.as_ref(), &wallet);
            if burst_crypto::decode_address(wallet.as_str()).is_some() {
                assert_eq!(bytes.len(), 34);
            }
        }
        for length in [0, 31, 33] {
            let mut bytes = Vec::new();
            ciborium::into_writer(&Blob(vec![0; length].into()), &mut bytes).unwrap();
            assert!(ciborium::from_reader::<Address<'_>, _>(bytes.as_slice()).is_err());
        }
    }

    #[test]
    fn compact_identity_keys_keep_all_namespaces_and_exact_fallbacks() {
        let wallet = burst_crypto::derive_address(&burst_types::PublicKey([71; 32]));
        let hash = BlockHash::new([171; 32]);
        for key in [
            format!("{}{wallet}", IDENTITY_PREFIXES[0]),
            format!("{}{wallet}", IDENTITY_PREFIXES[1]),
            format!("{}{hash}", IDENTITY_PREFIXES[2]),
            format!(
                "{}{}",
                IDENTITY_PREFIXES[2],
                hash.to_string().to_uppercase()
            ),
            format!("{}é", IDENTITY_PREFIXES[2]),
            "verification_identity_source/v1/brst_fixture".into(),
            "unrecognized/v99/identity".into(),
        ] {
            let mut bytes = Vec::new();
            ciborium::into_writer(&MetadataKey(Cow::Borrowed(&key)), &mut bytes).unwrap();
            let decoded: MetadataKey<'_> = ciborium::from_reader(bytes.as_slice()).unwrap();
            assert_eq!(decoded.0, key);
        }
        for tag in [0u8, 4] {
            let bytes = canonical::encode(&(tag, Blob(vec![0; 32].into()))).unwrap();
            assert!(ciborium::from_reader::<MetadataKey<'_>, _>(bytes.as_slice()).is_err());
        }
        let bytes = canonical::encode(&(1u8, Blob(vec![0; 32].into()), 0u8)).unwrap();
        assert!(ciborium::from_reader::<MetadataKey<'_>, _>(bytes.as_slice()).is_err());
    }

    #[test]
    fn compact_bootstrap_record_size_probe() {
        let wallet = burst_crypto::derive_address(&burst_types::PublicKey([17; 32]));
        let mut account = AccountInfo::unopened(wallet.clone());
        account.state = WalletState::Verified;
        account.verified_at = Some(Timestamp::new(300_000));
        account.head = BlockHash::new([255; 32]);
        account.block_count = 1;
        account.confirmation_height = 1;
        let brn = burst_brn::BrnWalletState::new(Timestamp::new(300_000));
        let mut verification = burst_verification::VerificationOrchestrator::new();
        verification.initialize_genesis(&wallet);
        for (name, old, new) in [
            (
                "account",
                canonical::encode(&account).unwrap().len(),
                options().serialized_size(&account).unwrap() as usize,
            ),
            (
                "brn_wallet",
                canonical::encode(&brn).unwrap().len(),
                options().serialized_size(&brn).unwrap() as usize,
            ),
            (
                "verification_one_wallet",
                canonical::encode(&verification.snapshot()).unwrap().len(),
                options()
                    .serialized_size(&verification.snapshot_view())
                    .unwrap() as usize,
            ),
        ] {
            eprintln!("compact size field={name} legacy_cbor={old} varint_payload={new}");
            assert!(new < old);
        }
        let mut bytes = Vec::new();
        write_value(&verification.snapshot_view(), &mut bytes).unwrap();
        let blob: Blob<'_> = ciborium::from_reader(bytes.as_slice()).unwrap();
        let restored: burst_verification::OrchestratorSnapshot = read_value(&blob.0).unwrap();
        assert_eq!(
            canonical::encode(&restored).unwrap(),
            canonical::encode(&verification.snapshot()).unwrap()
        );
    }

    #[test]
    fn compact_wallet_maps_preserve_legacy_and_spilled_records() {
        #[derive(Deserialize)]
        struct Rows(
            #[serde(deserialize_with = "read_wallet_map")]
            std::collections::HashMap<WalletAddress, AccountInfo>,
        );
        for count in [0, 2, 4097] {
            let rows: std::collections::HashMap<_, _> = (0u64..count)
                .map(|index| {
                    let mut key = [73; 32];
                    key[..8].copy_from_slice(&index.to_le_bytes());
                    let address = burst_crypto::derive_address(&burst_types::PublicKey(key));
                    (address.clone(), AccountInfo::unopened(address))
                })
                .collect();
            for compact in [false, true] {
                let mut bytes = Vec::new();
                WalletMap {
                    map: &rows,
                    compact,
                }
                .encode_field(&mut bytes)
                .unwrap();
                let decoded: Rows = ciborium::from_reader(bytes.as_slice()).unwrap();
                assert_eq!(decoded.0, rows);
                let mut encoded = Vec::new();
                WalletMap {
                    map: &decoded.0,
                    compact,
                }
                .encode_field(&mut encoded)
                .unwrap();
                assert_eq!(encoded, bytes);
            }
        }
    }

    #[test]
    #[ignore = "explicit capped-host 100000-row body sizing; not signed population qualification"]
    fn compact_hundred_thousand_wallet_body_fits_and_roundtrips() {
        let params = burst_types::ProtocolParams::default();
        let mut state =
            super::super::SnapshotContinuation::from_genesis(burst_types::NetworkId::Dev, &params)
                .unwrap()
                .into_state();
        let representative = state.accounts.keys().next().unwrap().clone();
        for index in 1u64..100_000 {
            let mut key = [83; 32];
            key[..8].copy_from_slice(&index.to_le_bytes());
            let address = burst_crypto::derive_address(&burst_types::PublicKey(key));
            let mut account = AccountInfo::unopened(address.clone());
            account.state = WalletState::Verified;
            account.verified_at = Some(Timestamp::new(300_000 + index));
            account.head = BlockHash::new(key);
            account.block_count = 1;
            account.confirmation_height = 1;
            account.representative = representative.clone();
            state.accounts.insert(address.clone(), account);
            state.brn.wallets.insert(
                address.clone(),
                burst_brn::BrnWalletState::new(Timestamp::new(300_000 + index)),
            );
            state.verification.initialize_genesis(&address);
            for prefix in &IDENTITY_PREFIXES[..2] {
                state
                    .lifecycle_metadata
                    .insert(format!("{prefix}{address}"), key.to_vec());
            }
            state.lifecycle_metadata.insert(
                format!("{}{}", IDENTITY_PREFIXES[2], BlockHash::new(key)),
                key.to_vec(),
            );
        }
        let bytes = state.snapshot_bytes().unwrap();
        eprintln!(
            "compact body wallets=100000 bytes={} limit={}",
            bytes.len(),
            burst_consensus::snapshot::MAX_SNAPSHOT_BYTES
        );
        assert!((bytes.len() as u64) <= burst_consensus::snapshot::MAX_SNAPSHOT_BYTES);
        let decoded: super::super::State<'_> = ciborium::from_reader(bytes.as_slice()).unwrap();
        assert_eq!(decoded.accounts.as_ref(), &state.accounts);
        assert_eq!(
            decoded.lifecycle_metadata.len(),
            state.lifecycle_metadata.len()
        );
        for (key, value) in &decoded.lifecycle_metadata {
            assert_eq!(value.0.as_ref(), state.lifecycle_metadata[key]);
        }
        let mut encoded = Vec::new();
        decoded.encode_into(&mut encoded).unwrap();
        assert_eq!(encoded, bytes);
    }
}
