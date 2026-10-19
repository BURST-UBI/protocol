use super::{attempt_path, read_draft, COMPLETED_RECEIPTS_PER_ACCOUNT};
use crate::WalletError;
use burst_types::{BlockHash, WalletAddress};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

fn failure(error: std::io::Error) -> WalletError {
    WalletError::Other(format!("receipt retention: {error}"))
}

// Callers serialize signing and recovery for the private journal directory.
// Legacy flat receipts remain untouched: their names alone do not distinguish
// them from active drafts accepted by the public storage API.
pub(super) fn directory(path: &Path, account: &WalletAddress) -> Result<PathBuf, WalletError> {
    let parent = path
        .parent()
        .ok_or_else(|| WalletError::Other("Missing journal directory".into()))?;
    let mut directory = parent.to_path_buf();
    for component in ["completed", account.as_str()] {
        let next = directory.join(component);
        match std::fs::create_dir(&next) {
            Ok(()) => {
                #[cfg(unix)]
                std::fs::File::open(&directory)
                    .and_then(|file| file.sync_all())
                    .map_err(failure)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(failure(error)),
        }
        if !std::fs::symlink_metadata(&next).map_err(failure)?.is_dir() {
            return Err(WalletError::Other(
                "Completed receipts must use a directory, not a link".into(),
            ));
        }
        directory = next;
    }
    Ok(directory)
}

pub(super) fn maintain(
    path: &Path,
    newest: &Path,
    account: &WalletAddress,
    hash: BlockHash,
) -> Result<(), WalletError> {
    maintain_inner(path, newest, account, hash, true)
}

pub(super) fn migrated(
    path: &Path,
    receipt: &Path,
    account: &WalletAddress,
    hash: BlockHash,
) -> Result<(), WalletError> {
    maintain_inner(path, receipt, account, hash, false)
}

fn maintain_inner(
    path: &Path,
    newest: &Path,
    account: &WalletAddress,
    hash: BlockHash,
    pin_newest: bool,
) -> Result<(), WalletError> {
    let directory = newest
        .parent()
        .ok_or_else(|| WalletError::Other("Missing receipt directory".into()))?;
    clear_resolved_marker(path, hash)?;
    let mut retained = BTreeSet::new();
    for entry in std::fs::read_dir(directory).map_err(failure)? {
        let entry = entry.map_err(failure)?;
        if (pin_newest && entry.path() == newest) || !is_receipt(&entry.path(), account)? {
            continue;
        }
        // Retry maintenance interrupted after durable archive publication. Clear
        // only markers backed by a validated completed receipt, before pruning it.
        clear_resolved_marker(path, read_draft(&entry.path())?.block.hash)?;
        retained.insert((
            entry
                .metadata()
                .map_err(failure)?
                .modified()
                .map_err(failure)?,
            entry.path(),
        ));
        if retained.len() > COMPLETED_RECEIPTS_PER_ACCOUNT - usize::from(pin_newest) {
            retained.pop_first();
        }
    }
    let retained: BTreeSet<_> = retained.into_iter().map(|(_, path)| path).collect();
    for entry in std::fs::read_dir(directory).map_err(failure)? {
        let entry = entry.map_err(failure)?;
        let candidate = entry.path();
        if (!pin_newest || candidate != newest)
            && !retained.contains(&candidate)
            && is_receipt(&candidate, account)?
        {
            std::fs::remove_file(candidate).map_err(failure)?;
        }
    }
    #[cfg(unix)]
    std::fs::File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(failure)?;
    Ok(())
}

fn clear_resolved_marker(path: &Path, hash: BlockHash) -> Result<(), WalletError> {
    let marker = attempt_path(path, &hash);
    match std::fs::symlink_metadata(&marker) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return Err(WalletError::Other(
                "Invalid submission marker; preserving it".into(),
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(failure(error)),
    }
    // A second active path can contain the same signed draft. Its submission
    // marker must survive resolution of the first path. Unknown files fail safe.
    let parent = path
        .parent()
        .ok_or_else(|| WalletError::Other("Missing journal directory".into()))?;
    for entry in std::fs::read_dir(parent).map_err(failure)? {
        let candidate = entry.map_err(failure)?.path();
        if candidate
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let draft = read_draft(&candidate)?;
            if draft.block.hash == hash || draft.ancestry.iter().any(|block| block.hash == hash) {
                return Ok(());
            }
        }
    }
    std::fs::remove_file(marker).map_err(failure)?;
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|file| file.sync_all())
        .map_err(failure)?;
    Ok(())
}

fn is_receipt(path: &Path, account: &WalletAddress) -> Result<bool, WalletError> {
    if !std::fs::symlink_metadata(path).map_err(failure)?.is_file()
        || path.extension().is_none_or(|extension| extension != "json")
    {
        return Ok(false);
    }
    let draft = read_draft(path)?;
    Ok(draft.block.account == *account
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(&format!("-{}.json", draft.block.hash))))
}
