use super::{ensure_same_receipt, read_draft, retention};
use crate::WalletError;
use std::path::Path;

/// Migrate the desktop app's legacy receipt layout, not a generic draft folder.
/// The caller must own the profile signing lease and guarantee that active drafts
/// use `<account>.json` names. Unknown names and active drafts are never migrated.
/// Interrupted publication is retried by checking the complete existing receipt.
pub fn migrate_account_named_journal(directory: &Path) -> Result<usize, WalletError> {
    let io = |error: std::io::Error| WalletError::Other(format!("receipt migration: {error}"));
    let mut migrated = 0;
    for entry in std::fs::read_dir(directory).map_err(io)? {
        let entry = entry.map_err(io)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(stem) = name.strip_suffix(".json") else { continue };
        let Some((prefix, hash)) = stem.rsplit_once('-') else { continue };
        if !hex_hash(hash) || !receipt_prefix(prefix) { continue; }
        let source = entry.path();
        let draft = read_draft(&source)?;
        if draft.block.hash.to_string() != hash {
            return Err(WalletError::Other("Legacy receipt name disagrees with signed block; preserving it".into()));
        }
        let completed = retention::directory(&source, &draft.block.account)?;
        let target = completed.join(name);
        match std::fs::hard_link(&source, &target) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => ensure_same_receipt(&target, &draft)?,
            Err(error) => return Err(io(error)),
        }
        // Linking preserves bytes and modification time, so migration does not
        // make an old receipt newer than an existing completed transaction.
        std::fs::File::open(&target).and_then(|file| file.sync_all()).map_err(io)?;
        #[cfg(unix)]
        std::fs::File::open(&completed).and_then(|file| file.sync_all()).map_err(io)?;
        std::fs::remove_file(&source).map_err(io)?;
        #[cfg(unix)]
        std::fs::File::open(directory).and_then(|file| file.sync_all()).map_err(io)?;
        retention::migrated(&source, &target, &draft.block.account, draft.block.hash)?;
        migrated += 1;
    }
    // A crash can occur after the last legacy name is removed but before its
    // maintenance pass. Reopen must retry even when no legacy files remain.
    let completed = directory.join("completed");
    let accounts = match std::fs::read_dir(&completed) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(migrated),
        Err(error) => return Err(io(error)),
    };
    if !std::fs::symlink_metadata(&completed).map_err(io)?.is_dir() {
        return Err(WalletError::Other("Completed receipts must use a directory, not a link".into()));
    }
    for account in accounts {
        let account = account.map_err(io)?;
        if !account.file_type().map_err(io)?.is_dir() { continue; }
        for entry in std::fs::read_dir(account.path()).map_err(io)? {
            let entry = entry.map_err(io)?;
            if entry.path().extension().is_none_or(|extension| extension != "json") { continue; }
            let draft = read_draft(&entry.path())?;
            if account.file_name().to_str() != Some(draft.block.account.as_str()) {
                return Err(WalletError::Other("Receipt account directory mismatch; preserving it".into()));
            }
            retention::migrated(&directory.join(format!("{}.json", draft.block.account)), &entry.path(), &draft.block.account, draft.block.hash)?;
            break;
        }
    }
    Ok(migrated)
}

fn hex_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn receipt_prefix(prefix: &str) -> bool {
    matches!(prefix, "confirmed" | "discarded") ||
        ["confirmed-by-", "superseded-by-", "excluded-by-epoch-"]
            .iter().any(|start| prefix.strip_prefix(start).is_some_and(hex_hash))
}
