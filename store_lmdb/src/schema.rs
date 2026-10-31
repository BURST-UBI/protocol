//! Schema compatibility checks. Open commits supported index rebuilds and their
//! markers together. Unsupported layouts are never relabelled as migrated.

use burst_store::MetaStore;

use crate::LmdbError;

/// Schema 8 adds shared historical head nodes; schema 7 added electorate membership.
pub const CURRENT_SCHEMA_VERSION: u32 = 8;
const LEGACY_SCHEMA_VERSION: u32 = 3;

/// Check an initialized store. Initialization requires the full database
/// view in `LmdbEnvironment::open`, not just access to metadata.
pub fn check_schema(meta_store: &impl MetaStore) -> Result<(), LmdbError> {
    let current = meta_store
        .get_schema_version()
        .map_err(|error| LmdbError::Heed(error.to_string()))?;

    require_supported(current)
}

fn require_supported(current: u32) -> Result<(), LmdbError> {
    if matches!(
        current,
        LEGACY_SCHEMA_VERSION | 4 | 5 | 6 | 7 | CURRENT_SCHEMA_VERSION
    ) {
        return Ok(());
    }
    Err(LmdbError::Heed(format!(
        "unsupported database schema {current}; supported schemas are {LEGACY_SCHEMA_VERSION}, 4, 5, 6, 7 and {CURRENT_SCHEMA_VERSION}; existing data was not migrated"
    )))
}

/// Missing metadata needs a layout check before initialization in the writer.
pub(crate) fn check_version(bytes: Option<&[u8]>) -> Result<bool, LmdbError> {
    let Some(bytes) = bytes else { return Ok(true) };
    let version = u32::from_le_bytes(bytes.try_into().map_err(|_| {
        LmdbError::Serialization("schema_version has unexpected byte length".into())
    })?);
    require_supported(version)?;
    Ok(false)
}
