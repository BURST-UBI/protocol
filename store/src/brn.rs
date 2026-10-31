use crate::StoreError;
use burst_types::WalletAddress;

pub type BrnWalletEntries<'a> =
    Box<dyn Iterator<Item = Result<(WalletAddress, std::borrow::Cow<'a, [u8]>), StoreError>> + 'a>;

pub type BrnWalletHeaders<'a> =
    Box<dyn Iterator<Item = Result<(WalletAddress, &'a [u8]), StoreError>> + 'a>;

pub type BrnMetaEntries<'a> =
    Box<dyn Iterator<Item = Result<(&'a [u8], &'a [u8]), StoreError>> + 'a>;

/// Metadata and wallet rows from one immutable storage view.
pub trait BrnSnapshot {
    fn get_meta(&self, key: &[u8]) -> Result<Option<&[u8]>, StoreError>;
    fn meta_entries(&self, prefix: &[u8]) -> Result<BrnMetaEntries<'_>, StoreError>;
    fn wallet_entries(&self) -> Result<BrnWalletEntries<'_>, StoreError>;
    /// Optional normalized wallet headers. The implementation must validate the
    /// selected format and reject orphan child records before returning Some.
    /// Child bytes are available through meta_entries in this same pinned view.
    /// None retains the canonical-byte adapter for legacy/other backends.
    fn wallet_record_headers(&self) -> Result<Option<BrnWalletHeaders<'_>>, StoreError> {
        Ok(None)
    }
}

/// Serialized BRN wallet state and engine metadata.
///
/// The BRN engine owns the encoding; the store treats it as opaque bytes.
/// Durability and write grouping depend on the backend.
pub trait BrnStore {
    fn read_snapshot(&self) -> Result<Box<dyn BrnSnapshot + '_>, StoreError>;
    fn get_wallet_state(&self, address: &WalletAddress) -> Result<Option<Vec<u8>>, StoreError>;
    fn put_wallet_state(&self, address: &WalletAddress, state: &[u8]) -> Result<(), StoreError>;
    fn delete_wallet_state(&self, address: &WalletAddress) -> Result<(), StoreError>;
    fn all_wallet_states(&self) -> Result<Vec<(WalletAddress, Vec<u8>)>, StoreError> {
        self.read_snapshot()?
            .wallet_entries()?
            .map(|row| row.map(|(address, bytes)| (address, bytes.to_vec())))
            .collect()
    }

    fn get_meta(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError>;
    fn put_meta(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError>;
}
