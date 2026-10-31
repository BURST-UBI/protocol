use super::{canonical::MapSource, BrnWalletState, HashMap, WalletAddress};
use burst_brn::wallet_states::WalletStates;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub(super) enum Wallets<'a> {
    Borrowed(&'a WalletStates),
    Owned(HashMap<WalletAddress, BrnWalletState>),
}

impl Wallets<'_> {
    pub(super) fn into_owned(self) -> HashMap<WalletAddress, BrnWalletState> {
        match self {
            Self::Owned(wallets) => wallets,
            Self::Borrowed(wallets) => wallets
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        }
    }
}

impl MapSource for Wallets<'_> {
    type Key = WalletAddress;
    type Value = BrnWalletState;
    fn len(&self) -> usize {
        match self {
            Self::Borrowed(wallets) => wallets.len(),
            Self::Owned(wallets) => wallets.len(),
        }
    }
    fn entries(&self) -> impl Iterator<Item = (&WalletAddress, &BrnWalletState)> {
        let (borrowed, owned) = match self {
            Self::Borrowed(wallets) => (Some(*wallets), None),
            Self::Owned(wallets) => (None, Some(wallets)),
        };
        borrowed
            .into_iter()
            .flat_map(WalletStates::iter)
            .chain(owned.into_iter().flat_map(HashMap::iter))
    }
}

impl Serialize for Wallets<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.entries())
    }
}

impl<'de> Deserialize<'de> for Wallets<'_> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        super::compact::read_wallet_map(deserializer).map(Self::Owned)
    }
}

#[cfg(test)]
mod tests {
    use super::super::canonical::{Field, Map};
    use super::*;
    use burst_types::Timestamp;

    #[test]
    fn persistent_wallet_export_matches_legacy_map_at_sort_boundary() {
        for count in [0, 1, 4096, 4097] {
            let rows: HashMap<_, _> = (0..count)
                .map(|n| {
                    (
                        WalletAddress::new(format!("brst_wallet_{n}")),
                        BrnWalletState::new(Timestamp::new(n as u64)),
                    )
                })
                .collect();
            let persistent = WalletStates::from(rows.clone());
            let borrowed = Wallets::Borrowed(&persistent);
            let mut old = Vec::new();
            Map(&rows).encode_field(&mut old).unwrap();
            let mut new = Vec::new();
            Map(&borrowed).encode_field(&mut new).unwrap();
            assert_eq!(new, old);
            let decoded: Wallets<'_> = ciborium::from_reader(new.as_slice()).unwrap();
            assert!(matches!(decoded, Wallets::Owned(_)));
            assert_eq!(decoded.into_owned(), rows);
        }
    }
}
