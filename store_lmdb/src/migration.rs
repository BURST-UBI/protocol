//! Database schema migration engine.
//!
//! Tracks a monotonically increasing schema version in the meta store and
//! runs sequential migration functions to bring an older database up to date.

use burst_store::MetaStore;

use crate::LmdbError;

/// The schema version that the current code expects.
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

/// Runs database migrations to bring the schema up to date.
pub struct Migrator;

impl Migrator {
    /// Check the stored schema version and run any needed migrations.
    ///
    /// - Version 0 means a fresh database (no version stored yet).
    /// - If the stored version matches `CURRENT_SCHEMA_VERSION`, this is a no-op.
    /// - If the stored version is *higher* than what this code supports,
    ///   the database was written by a newer node and we refuse to open it.
    pub fn run(meta_store: &impl MetaStore) -> Result<(), LmdbError> {
        let current = meta_store.get_schema_version().unwrap_or(0);

        if current == CURRENT_SCHEMA_VERSION {
            tracing::info!(version = current, "database schema is up to date");
            return Ok(());
        }

        if current > CURRENT_SCHEMA_VERSION {
            return Err(LmdbError::Heed(format!(
                "database schema version {} is newer than supported version {}",
                current, CURRENT_SCHEMA_VERSION
            )));
        }

        for version in current..CURRENT_SCHEMA_VERSION {
            tracing::info!(from = version, to = version + 1, "running migration");
            run_migration(version, version + 1)?;
        }

        meta_store
            .set_schema_version(CURRENT_SCHEMA_VERSION)
            .map_err(|e| LmdbError::Heed(e.to_string()))?;

        tracing::info!(version = CURRENT_SCHEMA_VERSION, "migration complete");
        Ok(())
    }
}

fn run_migration(from: u32, to: u32) -> Result<(), LmdbError> {
    match (from, to) {
        (0, 1) => {
            // Initial schema — nothing to migrate from a blank slate.
            Ok(())
        }
        (1, 2) => {
            // Schema v2: composite binary keys for all indexes.
            // account_blocks_db removed (height_db is canonical),
            // trst_origin_db uses (origin, tx_hash) composite keys,
            // trst_expiry_db uses binary (expiry_be, tx_hash) keys,
            // pending_db uses binary (dest, source_hash) keys,
            // votes_db uses composite (proposal, voter) only (no length-prefixed),
            // verification stores use composite (target, actor) keys,
            // account_txs_db uses composite (account, tx_hash) keys,
            // meta_db tracks verified_count counter.
            // No data migration needed — no production data exists yet.
            Ok(())
        }
        _ => Err(LmdbError::Heed(format!(
            "unknown migration: {} -> {}",
            from, to
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_migration_is_error() {
        let result = run_migration(99, 100);
        assert!(result.is_err());
    }

    #[test]
    fn initial_migration_succeeds() {
        let result = run_migration(0, 1);
        assert!(result.is_ok());
    }
}
