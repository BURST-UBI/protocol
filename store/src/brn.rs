use burst_types::WalletAddress;
use crate::StoreError;

/// Store trait for persisting BRN engine state to durable storage.
///
/// Uses opaque `Vec<u8>` so the store doesn't depend on the `burst-brn` crate
/// (which would create a circular dependency). The BRN engine serializes/deserializes
/// its own types.
pub trait BrnStore {
    fn get_wallet_state(&self, address: &WalletAddress) -> Result<Option<Vec<u8>>, StoreError>;
    fn put_wallet_state(&self, address: &WalletAddress, state: &[u8]) -> Result<(), StoreError>;
    fn delete_wallet_state(&self, address: &WalletAddress) -> Result<(), StoreError>;
    fn iter_wallet_states(&self) -> Result<Vec<(WalletAddress, Vec<u8>)>, StoreError>;

    fn get_meta(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError>;
    fn put_meta(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError>;
}
