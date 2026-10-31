use super::validate_signed_payment;
use crate::WalletError;
use burst_ledger::StateBlock;
use std::io::{Read, Write};
use std::path::Path;

mod migration;
mod retention;
pub use migration::migrate_account_named_journal;
pub const COMPLETED_RECEIPTS_PER_ACCOUNT: usize = 100;

// JSON byte arrays need more room than the wire encoding. Include the largest
// legal merge and returned-stake proofs, not just ordinary payment blocks.
const MAX_DRAFT_BYTES: u64 = 128
    + super::MAX_ANCESTRY_BYTES as u64
    + 8 * (burst_ledger::state_block::MAX_MERGE_BLOCK_BYTES
        + 32 * burst_ledger::state_block::MAX_RELEASE_SOURCES as u64)
    + 4 * burst_governance::content::MAX_CONTENT_BYTES as u64;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Draft {
    version: u8,
    block: StateBlock,
    #[serde(default)]
    ancestry: Vec<StateBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    proposal_content: Option<Vec<u8>>,
}

/// Save an immutable signed payment before attempting submission. Existing files
/// are never overwritten. A failure after publication can leave a complete draft:
/// retain the path and reconcile it rather than signing a replacement.
///
/// The file contains payment details and a signature, never a seed/private key.
/// Applications must use their private data directory, not a shared directory.
pub fn save_payment(path: &Path, block: &StateBlock) -> Result<(), WalletError> {
    save_payment_with_ancestry(path, block, &[])
}

pub fn save_payment_with_ancestry(
    path: &Path,
    block: &StateBlock,
    ancestry: &[StateBlock],
) -> Result<(), WalletError> {
    validate_signed_payment(block)?;
    save_draft(path, block, ancestry, None, 3)
}

/// Save a signed action and any proposal content bound by its commitment.
/// The caller owns user review; this function checks the signature and content binding.
pub fn save_signed_action(
    path: &Path,
    block: &StateBlock,
    ancestry: &[StateBlock],
    content: Option<&[u8]>,
) -> Result<(), WalletError> {
    validate_action_content(block, content)?;
    save_draft(path, block, ancestry, content, 4)
}

fn validate_action_content(block: &StateBlock, content: Option<&[u8]>) -> Result<(), WalletError> {
    super::validate_signed_action(block)?;
    let manifest = if block.block_type == burst_ledger::BlockType::GovernanceProposal {
        burst_governance::content::ContentManifest::from_block_fields(
            block.link,
            block.origin,
            block.transaction,
        )
        .map_err(|error| WalletError::Signing(error.to_string()))?
    } else {
        None
    };
    match (manifest, content) {
        (Some(manifest), Some(bytes)) => {
            manifest
                .decode_verified(bytes)
                .map_err(|error| WalletError::Signing(error.to_string()))?;
        }
        (None, None) => {}
        _ => {
            return Err(WalletError::Signing(
                "saved action content does not match its signed commitment".into(),
            ))
        }
    }
    Ok(())
}

fn save_draft(
    path: &Path,
    block: &StateBlock,
    ancestry: &[StateBlock],
    content: Option<&[u8]>,
    version: u8,
) -> Result<(), WalletError> {
    super::validate_ancestry(&block.account, block.previous, ancestry)?;
    let bytes = serde_json::to_vec(&Draft {
        version,
        block: block.clone(),
        ancestry: ancestry.to_vec(),
        proposal_content: content.map(<[u8]>::to_vec),
    })
    .map_err(|error| WalletError::Other(error.to_string()))?;
    if bytes.len() as u64 > MAX_DRAFT_BYTES {
        return Err(WalletError::Other(
            "offline payment exceeds draft size limit".into(),
        ));
    }
    publish(path, &bytes).map_err(|error| WalletError::Other(format!(
        "could not durably save offline payment {}; retain the draft path and check it before retrying: {error}", block.hash
    )))
}

fn publish(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut staging = tempfile::NamedTempFile::new_in(parent)?;
    staging.write_all(bytes)?;
    staging.as_file().sync_all()?;
    let published = staging
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    published.sync_all()?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn attempt_path(path: &Path, hash: &burst_types::BlockHash) -> std::path::PathBuf {
    path.with_file_name(format!("attempted-{hash}.marker"))
}

/// Archive a resolved draft before removing its active file.
/// The caller establishes the outcome; `prefix` labels it but is not proof of it.
/// Retention runs after archival and logs failures without undoing resolution.
pub fn archive_payment(path: &Path, prefix: &str) -> Result<(), WalletError> {
    if prefix.is_empty()
        || !prefix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(WalletError::Other("invalid payment receipt prefix".into()));
    }
    let io = |error: std::io::Error| WalletError::Other(error.to_string());
    let draft = read_draft(path)?;
    let block = &draft.block;
    let legacy = path.with_file_name(format!("{prefix}-{}.json", block.hash));
    match std::fs::symlink_metadata(&legacy) {
        Ok(_) => ensure_same_receipt(&legacy, &draft)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io(error)),
    }
    let directory = retention::directory(path, &block.account)?;
    let archive = directory.join(format!("{prefix}-{}.json", block.hash));
    match std::fs::symlink_metadata(&archive) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            save_draft(
                &archive,
                block,
                &draft.ancestry,
                draft.proposal_content.as_deref(),
                draft.version,
            )?;
        }
        Err(error) => return Err(io(error)),
        Ok(_) => {
            ensure_same_receipt(&archive, &draft)?;
        }
    }
    std::fs::remove_file(path).map_err(io)?;
    #[cfg(unix)]
    std::fs::File::open(
        path.parent()
            .ok_or_else(|| WalletError::Other("Missing payment directory".into()))?,
    )
    .and_then(|directory| directory.sync_all())
    .map_err(io)?;
    // Resolution is already durable. Maintenance failure must not report the
    // confirmed transaction as failed or encourage signing a replacement.
    if let Err(error) = retention::maintain(path, &archive, &block.account, block.hash) {
        eprintln!("BURST completed-receipt maintenance deferred: {error}");
    }
    Ok(())
}

fn ensure_same_receipt(path: &Path, draft: &Draft) -> Result<(), WalletError> {
    let existing = read_draft(path)?;
    if serde_json::to_vec(&existing).map_err(|error| WalletError::Other(error.to_string()))?
        != serde_json::to_vec(draft).map_err(|error| WalletError::Other(error.to_string()))?
    {
        return Err(WalletError::Other(
            "Conflicting saved payment receipt".into(),
        ));
    }
    Ok(())
}

/// True when submission was attempted or an older draft has no attempt history.
/// The marker does not prove network delivery or acceptance.
pub fn payment_may_have_been_submitted(
    path: &Path,
    hash: &burst_types::BlockHash,
) -> Result<bool, WalletError> {
    match std::fs::symlink_metadata(attempt_path(path, hash)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(read_draft(path)?.version < 2)
        }
        Err(error) => Err(WalletError::Other(format!(
            "cannot check submission marker: {error}"
        ))),
    }
}

pub(crate) fn record_submission_attempt(
    path: &Path,
    hash: &burst_types::BlockHash,
) -> Result<(), WalletError> {
    let marker = attempt_path(path, hash);
    let persist = || -> std::io::Result<()> {
        let exists = match std::fs::symlink_metadata(&marker) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error),
        };
        if exists {
            if !std::fs::symlink_metadata(&marker)?.is_file() {
                return Err(std::io::Error::other("invalid submission marker"));
            }
            std::fs::File::open(&marker)?.sync_all()?;
            #[cfg(unix)]
            std::fs::File::open(
                marker
                    .parent()
                    .ok_or_else(|| std::io::Error::other("missing marker directory"))?,
            )?
            .sync_all()?;
            Ok(())
        } else {
            publish(&marker, b"1\n")
        }
    };
    persist().map_err(|error| {
        WalletError::Other(format!(
            "payment was not submitted: cannot persist attempt marker: {error}"
        ))
    })
}

/// Read a bounded, versioned payment or action draft and verify its signature.
/// Loading does not establish ledger acceptance or confirmation.
pub fn load_payment(path: &Path) -> Result<StateBlock, WalletError> {
    Ok(read_draft(path)?.block)
}

pub fn load_payment_with_ancestry(
    path: &Path,
) -> Result<(StateBlock, Vec<StateBlock>), WalletError> {
    let draft = read_draft(path)?;
    Ok((draft.block, draft.ancestry))
}

pub fn load_action_with_content(path: &Path) -> Result<(StateBlock, Option<Vec<u8>>), WalletError> {
    let draft = read_draft(path)?;
    Ok((draft.block, draft.proposal_content))
}

fn read_draft(path: &Path) -> Result<Draft, WalletError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| WalletError::Other(format!("cannot inspect offline payment: {error}")))?;
    if !metadata.is_file() {
        return Err(WalletError::Other(
            "offline payment must be a regular file, not a link or device".into(),
        ));
    }
    let file = std::fs::File::open(path)
        .map_err(|error| WalletError::Other(format!("cannot open offline payment: {error}")))?;
    if !file
        .metadata()
        .map_err(|error| WalletError::Other(error.to_string()))?
        .is_file()
    {
        return Err(WalletError::Other(
            "offline payment must be a regular file".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_DRAFT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| WalletError::Other(format!("cannot read offline payment: {error}")))?;
    if bytes.len() as u64 > MAX_DRAFT_BYTES {
        return Err(WalletError::Other(
            "offline payment exceeds draft size limit".into(),
        ));
    }
    let draft: Draft = serde_json::from_slice(&bytes)
        .map_err(|error| WalletError::Other(format!("invalid offline payment file: {error}")))?;
    if !matches!(draft.version, 1..=4) {
        return Err(WalletError::Other(
            "unsupported offline payment file version".into(),
        ));
    }
    if draft.version < 4 {
        validate_signed_payment(&draft.block)?;
        if draft.proposal_content.is_some() {
            return Err(WalletError::Signing(
                "legacy draft cannot contain proposal content".into(),
            ));
        }
    } else {
        validate_action_content(&draft.block, draft.proposal_content.as_deref())?;
    }
    super::validate_ancestry(&draft.block.account, draft.block.previous, &draft.ancestry)?;
    Ok(draft)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payment(seed: u8, amount: u128) -> StateBlock {
        let keys = burst_crypto::keypair_from_seed(&[seed; 32]);
        let account = burst_crypto::derive_address(&keys.public);
        let state = crate::transaction_builder::AccountState {
            signing_grant: burst_types::BlockHash::ZERO,
            head: burst_types::BlockHash::new([1; 32]),
            block_count: 1,
            payment_epoch: burst_types::BlockHash::new([2; 32]),
            representative: account.clone(),
            brn_spent: 1000,
            trst_balance: 0,
            release_sources: vec![],
        };
        let tx = burst_transactions::Transaction::Burn(
            crate::transaction_builder::build_burn_tx(
                &account,
                &account,
                amount,
                burst_types::Timestamp::new(42),
            )
            .unwrap(),
        );
        crate::transaction_builder::build_and_sign_state_block(
            &state,
            &tx,
            &keys.private,
            burst_types::TxHash::ZERO,
            burst_types::BlockHash::new([3; 32]),
        )
        .unwrap()
    }

    #[test]
    fn legacy_reopen_finishes_retention_after_all_sources_were_removed() {
        let directory = tempfile::tempdir().unwrap();
        let active_path = directory.path().join("pending.json");
        let first = payment(94, 1);
        let completed = retention::directory(&active_path, &first.account).unwrap();
        for amount in 1..=105 {
            let block = payment(94, amount);
            let path = completed.join(format!("confirmed-{}.json", block.hash));
            save_payment(&path, &block).unwrap();
            record_submission_attempt(&active_path, &block.hash).unwrap();
        }
        assert_eq!(migrate_account_named_journal(directory.path()).unwrap(), 0);
        assert_eq!(std::fs::read_dir(&completed).unwrap().count(), 100);
        for amount in 1..=105 {
            assert!(!attempt_path(&active_path, &payment(94, amount).hash).exists());
        }
    }

    #[test]
    fn legacy_migration_preserves_active_drafts_and_retains_latest_receipts() {
        let directory = tempfile::tempdir().unwrap();
        let active = payment(92, 999);
        let active_path = directory.path().join(format!("{}.json", active.account));
        save_payment(&active_path, &active).unwrap();
        record_submission_attempt(&active_path, &active.hash).unwrap();
        let original = std::fs::read(&active_path).unwrap();
        let unknown = directory.path().join("unrecognized-receipt.json");
        save_payment(&unknown, &active).unwrap();
        for amount in 1..=105 {
            let block = payment(92, amount);
            let path = directory
                .path()
                .join(format!("confirmed-{}.json", block.hash));
            save_payment(&path, &block).unwrap();
            std::fs::File::open(&path)
                .unwrap()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(amount as u64))
                .unwrap();
            record_submission_attempt(&path, &block.hash).unwrap();
        }
        assert_eq!(
            migrate_account_named_journal(directory.path()).unwrap(),
            105
        );
        assert_eq!(migrate_account_named_journal(directory.path()).unwrap(), 0);
        assert_eq!(std::fs::read(&active_path).unwrap(), original);
        assert!(payment_may_have_been_submitted(&active_path, &active.hash).unwrap());
        assert!(unknown.exists());
        let completed = retention::directory(&active_path, &active.account).unwrap();
        assert_eq!(std::fs::read_dir(&completed).unwrap().count(), 100);
        for amount in 1..=105 {
            let block = payment(92, amount);
            let receipt = completed.join(format!("confirmed-{}.json", block.hash));
            assert_eq!(receipt.exists(), amount > 5);
            assert!(!attempt_path(&active_path, &block.hash).exists());
        }
    }

    #[test]
    fn legacy_migration_retries_publication_and_refuses_conflicting_receipts() {
        let directory = tempfile::tempdir().unwrap();
        let block = payment(93, 1);
        let path = directory
            .path()
            .join(format!("confirmed-{}.json", block.hash));
        save_payment(&path, &block).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let target = retention::directory(&path, &block.account)
            .unwrap()
            .join(path.file_name().unwrap());
        std::fs::hard_link(&path, &target).unwrap();
        assert_eq!(migrate_account_named_journal(directory.path()).unwrap(), 1);
        assert_eq!(std::fs::read(&target).unwrap(), bytes);
        assert!(!path.exists());

        save_payment(&path, &block).unwrap();
        std::fs::remove_file(&target).unwrap();
        let other = payment(93, 2);
        save_payment(&target, &other).unwrap();
        assert!(migrate_account_named_journal(directory.path()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(load_payment(&target).unwrap().hash, other.hash);
    }

    #[test]
    fn completed_retention_bounds_new_receipts_and_preserves_unresolved_drafts() {
        let directory = tempfile::tempdir().unwrap();
        let pending = directory.path().join("pending.json");
        let protected = directory.path().join("unresolved.json");
        let unresolved = payment(84, 999);
        save_payment(&protected, &unresolved).unwrap();
        record_submission_attempt(&protected, &unresolved.hash).unwrap();
        let original = std::fs::read(&protected).unwrap();
        let other = payment(85, 1);
        save_payment(&pending, &other).unwrap();
        archive_payment(&pending, "confirmed").unwrap();
        for amount in 1..=COMPLETED_RECEIPTS_PER_ACCOUNT + 5 {
            let block = payment(84, amount as u128);
            save_payment(&pending, &block).unwrap();
            record_submission_attempt(&pending, &block.hash).unwrap();
            archive_payment(&pending, "confirmed").unwrap();
            assert!(!attempt_path(&pending, &block.hash).exists());
            let receipt = retention::directory(&pending, &block.account)
                .unwrap()
                .join(format!("confirmed-{}.json", block.hash));
            assert_eq!(load_payment(&receipt).unwrap().hash, block.hash);
        }
        assert_eq!(
            std::fs::read_dir(retention::directory(&pending, &unresolved.account).unwrap())
                .unwrap()
                .count(),
            COMPLETED_RECEIPTS_PER_ACCOUNT
        );
        assert_eq!(
            std::fs::read_dir(retention::directory(&pending, &other.account).unwrap())
                .unwrap()
                .count(),
            1
        );
        assert_eq!(std::fs::read(&protected).unwrap(), original);
        assert!(payment_may_have_been_submitted(&protected, &unresolved.hash).unwrap());
    }

    #[test]
    fn resolving_one_path_preserves_the_marker_for_another_copy() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.json");
        let second = directory.path().join("second.json");
        let block = payment(86, 1);
        save_payment(&first, &block).unwrap();
        save_payment(&second, &block).unwrap();
        record_submission_attempt(&first, &block.hash).unwrap();
        archive_payment(&first, "confirmed").unwrap();
        assert!(payment_may_have_been_submitted(&second, &block.hash).unwrap());
        let later = payment(86, 2);
        save_payment(&first, &later).unwrap();
        archive_payment(&first, "confirmed").unwrap();
        assert!(payment_may_have_been_submitted(&second, &block.hash).unwrap());
        archive_payment(&second, "confirmed").unwrap();
        assert!(!attempt_path(&second, &block.hash).exists());
    }

    #[test]
    fn later_completion_retries_interrupted_marker_cleanup() {
        let directory = tempfile::tempdir().unwrap();
        let pending = directory.path().join("pending.json");
        let unknown = directory.path().join("unknown.json");
        let first = payment(89, 1);
        save_payment(&pending, &first).unwrap();
        record_submission_attempt(&pending, &first.hash).unwrap();
        std::fs::write(&unknown, b"unreadable record").unwrap();
        archive_payment(&pending, "confirmed").unwrap();
        assert!(attempt_path(&pending, &first.hash).exists());
        std::fs::remove_file(&unknown).unwrap();

        // Reopen through the public storage API with a different transaction.
        let second = payment(89, 2);
        save_payment(&pending, &second).unwrap();
        record_submission_attempt(&pending, &second.hash).unwrap();
        archive_payment(&pending, "confirmed").unwrap();
        assert!(!attempt_path(&pending, &first.hash).exists());
        assert!(!attempt_path(&pending, &second.hash).exists());
        let receipts = retention::directory(&pending, &first.account).unwrap();
        assert_eq!(std::fs::read_dir(receipts).unwrap().count(), 2);
    }

    #[test]
    fn maintenance_failure_does_not_undo_resolution_or_drop_uncertain_markers() {
        let directory = tempfile::tempdir().unwrap();
        let pending = directory.path().join("pending.json");
        let unknown = directory.path().join("unknown.json");
        let block = payment(88, 1);
        save_payment(&pending, &block).unwrap();
        record_submission_attempt(&pending, &block.hash).unwrap();
        std::fs::write(&unknown, b"unreadable legacy record").unwrap();
        archive_payment(&pending, "confirmed").unwrap();
        assert!(!pending.exists());
        assert!(attempt_path(&pending, &block.hash).exists());
        assert_eq!(
            std::fs::read(&unknown).unwrap(),
            b"unreadable legacy record"
        );
        let archive = retention::directory(&pending, &block.account)
            .unwrap()
            .join(format!("confirmed-{}.json", block.hash));
        assert_eq!(load_payment(&archive).unwrap().hash, block.hash);
    }

    #[cfg(unix)]
    #[test]
    fn receipt_directory_symlink_cannot_redirect_or_release_a_draft() {
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let pending = directory.path().join("pending.json");
        let block = payment(87, 1);
        save_payment(&pending, &block).unwrap();
        std::os::unix::fs::symlink(outside.path(), directory.path().join("completed")).unwrap();
        assert!(archive_payment(&pending, "confirmed").is_err());
        assert_eq!(load_payment(&pending).unwrap().hash, block.hash);
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[test]
    fn action_journal_requires_exact_content_and_archives_the_complete_record() {
        let keys = burst_crypto::keypair_from_seed(&[83; 32]);
        let state = crate::transaction_builder::AccountState {
            signing_grant: burst_types::BlockHash::ZERO,
            head: burst_types::BlockHash::new([1; 32]),
            block_count: 1,
            payment_epoch: burst_types::BlockHash::new([2; 32]),
            representative: burst_crypto::derive_address(&keys.public),
            brn_spent: u64::MAX as u128 + 1,
            trst_balance: 0,
            release_sources: vec![],
        };
        let content = burst_governance::ProposalContent::ParameterChange {
            param: burst_governance::GovernableParam::BrnRate,
            new_value: 1,
        };
        let (block, _) = crate::governance_content::build_signed_proposal(
            &state,
            &keys,
            &content,
            burst_types::BlockHash::new([3; 32]),
            burst_types::Timestamp::new(42),
        )
        .unwrap();
        let bytes = content.encode_content().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pending.json");
        assert!(
            save_payment(&path, &block).is_err(),
            "offline payment context must stay restricted"
        );
        assert!(save_signed_action(&path, &block, &[], None).is_err());
        assert!(save_signed_action(&path, &block, &[], Some(b"wrong content")).is_err());
        assert!(!path.exists());
        save_signed_action(&path, &block, &[], Some(&bytes)).unwrap();
        record_submission_attempt(&path, &block.hash).unwrap();
        let original = std::fs::read(&path).unwrap();
        let recovered = read_draft(&path).unwrap();
        assert_eq!(recovered.version, 4);
        assert_eq!(recovered.block.hash, block.hash);
        assert_eq!(recovered.block.brn_spent, state.brn_spent);
        assert_eq!(
            recovered.proposal_content.as_deref(),
            Some(bytes.as_slice())
        );
        let archive = retention::directory(&path, &block.account)
            .unwrap()
            .join(format!("confirmed-{}.json", block.hash));
        save_signed_action(&archive, &block, &[], Some(&bytes)).unwrap();
        archive_payment(&path, "confirmed").unwrap();
        assert!(!path.exists());
        assert_eq!(std::fs::read(&archive).unwrap(), original);
        let mut damaged = read_draft(&archive).unwrap();
        damaged.proposal_content.as_mut().unwrap()[0] ^= 1;
        std::fs::write(&path, serde_json::to_vec(&damaged).unwrap()).unwrap();
        assert!(load_payment(&path).is_err());
        assert!(archive_payment(&path, "confirmed").is_err());
        assert_eq!(std::fs::read(&archive).unwrap(), original);
    }
}
