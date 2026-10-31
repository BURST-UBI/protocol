//! Database readability and data-directory checks.
//! Not called by node startup. These helpers do not validate ledger contents
//! or cross-table consistency; the named-table list is incomplete.

use std::path::Path;

use heed::Env;

use crate::LmdbError;

/// Database entry counts and read errors.
pub struct DatabaseCheckReport {
    pub databases_checked: u32,
    pub total_entries: u64,
    pub errors: Vec<String>,
}

impl DatabaseCheckReport {
    /// Returns `true` if no errors were detected.
    pub fn has_no_errors(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Partial historical list used by this optional readability check.
/// The current schema inventory is defined by LmdbEnvironment::open.
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

/// Check that existing databases can be opened and their entries counted.
///
/// Visits the partial list below and counts entries in tables that exist.
/// Missing tables are skipped; per-table read failures are collected. Failure
/// to open the read transaction returns an error immediately.
pub fn check_databases(env: &Env) -> Result<DatabaseCheckReport, LmdbError> {
    let mut report = DatabaseCheckReport {
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
/// if the directory exists but `data.mdb` is missing, even for an empty directory.
/// It does not inspect the file contents or establish database health.
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
        let report = DatabaseCheckReport {
            databases_checked: 5,
            total_entries: 100,
            errors: Vec::new(),
        };
        assert!(report.has_no_errors());
    }

    #[test]
    fn unhealthy_report() {
        let report = DatabaseCheckReport {
            databases_checked: 5,
            total_entries: 100,
            errors: vec!["corruption detected".to_string()],
        };
        assert!(!report.has_no_errors());
    }
}
