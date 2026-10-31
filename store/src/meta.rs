//! Metadata storage trait.

use crate::StoreError;
use burst_types::BlockHash;

pub const PAYMENT_EPOCH_KEY: &str = "payment_epoch/v1";
pub const CERTIFIED_PAYMENT_AUTHORITY_KEY: &str = "checkpoint_installed/v1";
pub const CERTIFIED_PAYMENT_AUTHORITY_PAGE_PREFIX: &str = "checkpoint_installed/pages/v1/";
pub const PAYMENT_VOTE_LIMIT_KEY: &str = "payment_epoch/v1/vote_limit";
pub const GOVERNANCE_COUNTING_LIMIT_KEY: &str = "payment_epoch/v1/counting_limit";

/// Encode an epoch-bound ordinary-vote timestamp limit. This only encodes bytes;
/// the caller must authenticate the projection authorizing a changed limit.
pub fn encode_payment_vote_limit(epoch: BlockHash, limit: Option<u64>) -> Vec<u8> {
    let mut bytes = epoch.as_bytes().to_vec();
    bytes.push(u8::from(limit.is_some()));
    if let Some(limit) = limit {
        bytes.extend_from_slice(&limit.to_be_bytes());
    }
    bytes
}

pub fn decode_payment_vote_limit(
    bytes: &[u8],
    epoch: BlockHash,
) -> Result<Option<u64>, StoreError> {
    if epoch.is_zero() || bytes.get(..32) != Some(epoch.as_bytes().as_slice()) {
        return Err(StoreError::Corruption(
            "payment vote limit has a different epoch".into(),
        ));
    }
    match (bytes.len(), bytes.get(32)) {
        (33, Some(0)) => Ok(None),
        (41, Some(1)) => Ok(Some(u64::from_be_bytes(bytes[33..].try_into().unwrap()))),
        _ => Err(StoreError::Corruption(
            "invalid payment vote limit encoding".into(),
        )),
    }
}

pub fn closed_payment_epoch_key(epoch: &BlockHash) -> String {
    format!("{PAYMENT_EPOCH_KEY}/closed/{epoch}")
}

/// Resolve the installed context and reject a closed epoch. The caller must
/// make both reads through the same pinned metadata view.
pub fn read_open_payment_epoch(
    mut read: impl FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>,
) -> Result<BlockHash, StoreError> {
    let bytes = read(PAYMENT_EPOCH_KEY)?.ok_or_else(|| StoreError::Corruption("payment epoch is not initialized".into()))?;
    let epoch = decode_payment_epoch(&bytes)?;
    if read(&closed_payment_epoch_key(&epoch))?.is_some() {
        return Err(StoreError::Corruption("installed payment epoch is already closed".into()));
    }
    Ok(epoch)
}

pub fn decode_payment_epoch(bytes: &[u8]) -> Result<BlockHash, StoreError> {
    let hash = BlockHash::new(
        bytes
            .try_into()
            .map_err(|_| StoreError::Corruption("invalid payment epoch encoding".into()))?,
    );
    if hash.is_zero() {
        return Err(StoreError::Corruption(
            "payment epoch is missing its certified boundary".into(),
        ));
    }
    Ok(hash)
}

/// Trait for storing database metadata (schema version, configuration, etc.).
///
/// This is a generic key-value store for internal bookkeeping that doesn't
/// belong in any domain-specific store.
pub trait MetaStore {
    /// Read the current epoch and its closed marker. This default makes separate
    /// get_meta calls; use a pinned backend view when consistency is required.
    fn payment_epoch(&self) -> Result<BlockHash, StoreError> {
        let epoch = decode_payment_epoch(&self.get_meta(PAYMENT_EPOCH_KEY)?)?;
        match self.get_meta(&closed_payment_epoch_key(&epoch)) {
            Err(StoreError::NotFound(_)) => Ok(epoch),
            Err(error) => Err(error),
            Ok(_) => Err(StoreError::Corruption(
                "installed payment epoch is already closed".into(),
            )),
        }
    }

    /// Store a metadata value.
    fn put_meta(&self, key: &str, value: &[u8]) -> Result<(), StoreError>;

    /// Retrieve a metadata value.
    fn get_meta(&self, key: &str) -> Result<Vec<u8>, StoreError>;

    /// Delete a metadata entry.
    fn delete_meta(&self, key: &str) -> Result<(), StoreError>;

    /// Get the current database schema version (convenience wrapper).
    fn get_schema_version(&self) -> Result<u32, StoreError>;

    /// Set the database schema version (convenience wrapper).
    fn set_schema_version(&self, version: u32) -> Result<(), StoreError>;
}
