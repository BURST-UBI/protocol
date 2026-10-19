use crate::StoreError;
use burst_types::WalletAddress;

pub type BrnWalletEntries<'a> =
    Box<dyn Iterator<Item = Result<(WalletAddress, &'a [u8]), StoreError>> + 'a>;

/// Metadata and wallet rows from one immutable storage view.
pub trait BrnSnapshot {
    fn get_meta(&self, key: &[u8]) -> Result<Option<&[u8]>, StoreError>;
    fn wallet_entries(&self) -> Result<BrnWalletEntries<'_>, StoreError>;
}

/// Store trait for persisting BRN engine state to durable storage.
///
/// Uses opaque bytes so the store doesn't depend on the `burst-brn` crate
/// (which would create a circular dependency). The BRN engine serializes/deserializes
/// its own types.
pub trait BrnStore {
    fn read_snapshot(&self) -> Result<Box<dyn BrnSnapshot + '_>, StoreError>;
    fn get_wallet_state(&self, address: &WalletAddress) -> Result<Option<Vec<u8>>, StoreError>;
    fn put_wallet_state(&self, address: &WalletAddress, state: &[u8]) -> Result<(), StoreError>;
    fn delete_wallet_state(&self, address: &WalletAddress) -> Result<(), StoreError>;
    fn iter_wallet_states(&self) -> Result<Vec<(WalletAddress, Vec<u8>)>, StoreError> {
        self.read_snapshot()?
            .wallet_entries()?
            .map(|row| row.map(|(address, bytes)| (address, bytes.to_vec())))
            .collect()
    }

    fn get_meta(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError>;
    fn put_meta(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError>;
}
