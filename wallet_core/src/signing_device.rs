//! Local device binding, separate from portable root-key backups.
use crate::WalletError;
use burst_ledger::{BlockType, StateBlock};
use burst_types::{BlockHash, WalletAddress};
use std::{
    io::{Read, Write},
    path::Path,
};

/// Missing binding denotes an unenrolled profile, not ownership of the network's
/// current grant. Corrupt or mismatched state must not become an unenrolled one.
pub fn load(path: &Path, account: &WalletAddress) -> Result<BlockHash, WalletError> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BlockHash::ZERO),
        Err(error) => return Err(WalletError::Other(error.to_string())),
    };
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(16 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() > 16 * 1024 {
        return Err(WalletError::Other("Device binding is oversized".into()));
    }
    let block: StateBlock =
        serde_json::from_slice(&bytes).map_err(|e| WalletError::Other(e.to_string()))?;
    validate(&block, account)?;
    Ok(block.hash)
}

/// Caller has verified this exact handoff is confirmed. Publish the binding
/// before releasing the pending action, so restart can finish either step.
pub fn activate(
    path: &Path,
    account: &WalletAddress,
    block: &StateBlock,
) -> Result<(), WalletError> {
    validate(block, account)?;
    let directory = path
        .parent()
        .ok_or_else(|| WalletError::Other("Missing device directory".into()))?;
    let bytes = serde_json::to_vec(block).map_err(|e| WalletError::Other(e.to_string()))?;
    let mut staging = tempfile::NamedTempFile::new_in(directory).map_err(io)?;
    staging.write_all(&bytes).map_err(io)?;
    staging.as_file().sync_all().map_err(io)?;
    staging
        .persist(path)
        .map_err(|e| io(e.error))?
        .sync_all()
        .map_err(io)?;
    #[cfg(unix)]
    std::fs::File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(io)?;
    Ok(())
}

fn validate(block: &StateBlock, account: &WalletAddress) -> Result<(), WalletError> {
    crate::offline::validate_signed_action(block)?;
    if block.block_type != BlockType::ChangeSigner || &block.account != account {
        return Err(WalletError::Signing(
            "Device binding belongs to another account or action".into(),
        ));
    }
    Ok(())
}

fn io(error: std::io::Error) -> WalletError {
    WalletError::Other(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction_builder::{build_signing_handoff, sign_state_block, AccountState};
    #[test]
    fn signing_device_binding_survives_restart_but_not_mnemonic_restore() {
        let keys = burst_crypto::keypair_from_seed(&[17; 32]);
        let account = burst_crypto::derive_address(&keys.public);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("device.json");
        let other_device = directory.path().join("restored-device.json");
        assert_eq!(load(&path, &account).unwrap(), BlockHash::ZERO);
        let state = AccountState {
            signing_grant: BlockHash::ZERO,
            payment_epoch: BlockHash::new([1; 32]),
            head: BlockHash::new([2; 32]),
            block_count: 1,
            representative: account.clone(),
            brn_balance: 3,
            trst_balance: 4,
            release_sources: vec![],
        };
        let block = sign_state_block(
            build_signing_handoff(
                &state,
                &account,
                BlockHash::new([5; 32]),
                BlockHash::new([6; 32]),
                burst_types::Timestamp::new(100),
            )
            .unwrap(),
            &keys.private,
        );
        activate(&path, &account, &block).unwrap();
        assert_eq!(load(&path, &account).unwrap(), block.hash);
        // Identical keys on another device do not import a local role.
        assert_eq!(load(&other_device, &account).unwrap(), BlockHash::ZERO);
        activate(&path, &account, &block).unwrap();
        assert_eq!(load(&path, &account).unwrap(), block.hash);
        let other =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[18; 32]).public);
        assert!(load(&path, &other).is_err());
        let mut tampered = block.clone();
        tampered.signing_grant = BlockHash::new([8; 32]);
        assert!(activate(&path, &account, &tampered).is_err());
        assert_eq!(load(&path, &account).unwrap(), block.hash);
        std::fs::write(&path, b"incomplete").unwrap();
        assert!(load(&path, &account).is_err());
    }
}
