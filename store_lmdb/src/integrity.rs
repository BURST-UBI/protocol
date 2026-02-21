//! LMDB database integrity checks.
//!
//! Run on startup to detect corruption early, before the node begins
//! processing blocks.

use std::path::Path;
use std::sync::Arc;

use heed::Env;

use crate::LmdbError;

/// Summary of an integrity check run.
pub struct IntegrityReport {
    pub databases_checked: u32,
    pub total_entries: u64,
    pub errors: Vec<String>,
}

impl IntegrityReport {
    /// Returns `true` if no errors were detected.
    pub fn is_healthy(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Database names that we expect to exist in a valid BURST LMDB environment.
const EXPECTED_DATABASES: &[&str] = &[
    "accounts",
    "blocks",
    "account_blocks",
    "transactions",
    "account_txs",
    "merger_origins",
    "merger_downstream",
    "merger_nodes",
    "endorsements",
    "verification_votes",
    "challenges",
    "proposals",
    "votes",
    "delegations",
    "constitution",
    "frontiers",
    "meta",
    "pending",
    "trst_origins",
    "trst_expiry",
];

/// Check LMDB database integrity on startup.
///
/// Opens each expected database and attempts to count entries. Any read
/// failures are recorded in the report rather than causing a hard error.
pub fn check_integrity(env: &Arc<Env>) -> Result<IntegrityReport, LmdbError> {
    let mut report = IntegrityReport {
        databases_checked: 0,
        total_entries: 0,
        errors: Vec::new(),
    };

    let rtxn = env.read_txn().map_err(LmdbError::from)?;

    for &db_name in EXPECTED_DATABASES {
        match env.open_database::<heed::types::Bytes, heed::types::Bytes>(&rtxn, Some(db_name)) {
            Ok(Some(db)) => {
                report.databases_checked += 1;
                match db.len(&rtxn) {
                    Ok(count) => {
                        report.total_entries += count;
                    }
                    Err(e) => {
                        report
                            .errors
                            .push(format!("failed to read database '{}': {}", db_name, e));
                    }
                }
            }
            Ok(None) => {
                // Database doesn't exist yet — acceptable for a fresh node
            }
            Err(e) => {
                report
                    .errors
                    .push(format!("failed to open database '{}': {}", db_name, e));
            }
        }
    }

    Ok(report)
}

/// Check if the LMDB data directory looks valid before opening.
///
/// Returns `Ok(())` for a fresh (nonexistent) directory. Returns an error
/// if the directory exists but `data.mdb` is missing, which suggests
/// corruption or misconfiguration.
pub fn check_data_dir(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(()); // Fresh start
    }
    let data_file = path.join("data.mdb");
    if !data_file.exists() {
        return Err(format!(
            "LMDB directory exists but data.mdb is missing at {}",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_data_dir_fresh_path() {
        let result = check_data_dir(Path::new("/tmp/burst_test_nonexistent_12345"));
        assert!(result.is_ok());
    }

    #[test]
    fn healthy_report() {
        let report = IntegrityReport {
            databases_checked: 5,
            total_entries: 100,
            errors: Vec::new(),
        };
        assert!(report.is_healthy());
    }

    #[test]
    fn unhealthy_report() {
        let report = IntegrityReport {
            databases_checked: 5,
            total_entries: 100,
            errors: vec!["corruption detected".to_string()],
        };
        assert!(!report.is_healthy());
    }
}
