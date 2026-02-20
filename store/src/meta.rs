//! Metadata storage trait.

use crate::StoreError;

/// Trait for storing database metadata (schema version, configuration, etc.).
///
/// This is a generic key-value store for internal bookkeeping that doesn't
/// belong in any domain-specific store.
pub trait MetaStore {
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
