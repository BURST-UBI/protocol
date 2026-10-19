use burst_store::{BrnStore, StoreError};
use burst_store_lmdb::{block_undo::UndoTable, LmdbStore, LmdbWriteTransaction};
use burst_types::ProtocolParams;
use burst_verification::bootstrap::BootstrapPhase;

pub(crate) fn require(store: &LmdbStore) -> Result<BootstrapPhase, StoreError> {
    let bytes = store.brn_store().get_meta(BootstrapPhase::KEY)?;
    BootstrapPhase::decode(bytes.as_deref())
        .map_err(|error| StoreError::Corruption(error.to_string()))
}

pub(crate) fn initialize(
    batch: &mut LmdbWriteTransaction<'_>,
    params: &ProtocolParams,
) -> Result<(), StoreError> {
    if batch
        .undo_value(UndoTable::BrnMeta, BootstrapPhase::KEY)?
        .is_some()
    {
        return Err(StoreError::Corruption(
            "bootstrap phase is already initialized".into(),
        ));
    }
    batch.put_brn_meta(
        BootstrapPhase::KEY,
        &BootstrapPhase::at_genesis(params.bootstrap_exit_threshold).encoding(),
    )
}

pub(crate) fn read(batch: &LmdbWriteTransaction<'_>) -> Result<BootstrapPhase, String> {
    let bytes = batch
        .undo_value(UndoTable::BrnMeta, BootstrapPhase::KEY)
        .map_err(|error| error.to_string())?;
    BootstrapPhase::decode(bytes.as_deref()).map_err(|error| error.to_string())
}

pub(crate) fn observe(
    batch: &mut LmdbWriteTransaction<'_>,
    params: &ProtocolParams,
) -> Result<BootstrapPhase, String> {
    let mut phase = read(batch)?;
    let before = phase;
    phase.observe(
        batch
            .verified_account_count()
            .map_err(|error| error.to_string())?,
        params.bootstrap_exit_threshold,
    );
    if phase != before {
        batch
            .put_brn_meta(BootstrapPhase::KEY, &phase.encoding())
            .map_err(|error| error.to_string())?;
    }
    Ok(phase)
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::account::AccountInfo;
    use burst_types::{BlockHash, WalletState};

    fn account(seed: u8) -> AccountInfo {
        let address =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[seed; 32]).public);
        let mut account = AccountInfo::unopened(address);
        account.state = WalletState::Verified;
        account
    }

    #[test]
    fn exit_commits_with_verification_and_cannot_reopen_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut params = ProtocolParams {
            bootstrap_exit_threshold: 2,
            ..ProtocolParams::default()
        };
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_account_info(&account(0), false).unwrap();
        initialize(&mut batch, &params).unwrap();
        batch.commit().unwrap();
        let hash = BlockHash::new([5; 32]);
        for commit in [false, true] {
            let mut batch = store.tx_begin_write().unwrap();
            batch.begin_block_undo(hash, account(1).address, 1).unwrap();
            batch.put_account_info(&account(1), false).unwrap();
            assert_eq!(
                observe(&mut batch, &params).unwrap(),
                BootstrapPhase::Complete
            );
            if commit {
                batch.commit().unwrap();
            } else {
                drop(batch);
            }
            assert_eq!(
                require(&store).unwrap(),
                if commit {
                    BootstrapPhase::Complete
                } else {
                    BootstrapPhase::Active
                }
            );
        }
        let txn = store.tx_begin_write().unwrap();
        let undo = txn.block_undo(&hash).unwrap().unwrap();
        assert!(undo
            .changes
            .iter()
            .any(|change| change.table == UndoTable::BrnMeta && change.key == BootstrapPhase::KEY));
        drop(txn);
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let mut revoked = account(1);
        revoked.state = WalletState::Revoked;
        batch.put_account_info(&revoked, false).unwrap();
        params.bootstrap_exit_threshold = 1000;
        assert_eq!(
            observe(&mut batch, &params).unwrap(),
            BootstrapPhase::Complete
        );
        batch.commit().unwrap();
        assert_eq!(require(&store).unwrap(), BootstrapPhase::Complete);
    }

    #[test]
    fn missing_or_corrupt_phase_cannot_grant_creator_authority() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        assert!(require(&store).is_err());
        let mut batch = store.tx_begin_write().unwrap();
        assert!(observe(&mut batch, &ProtocolParams::default()).is_err());
        batch.put_brn_meta(BootstrapPhase::KEY, &[1, 1, 0]).unwrap();
        assert!(observe(&mut batch, &ProtocolParams::default()).is_err());
        assert!(initialize(&mut batch, &ProtocolParams::default()).is_err());
        batch.commit().unwrap();
        assert!(require(&store).is_err());
    }
}
