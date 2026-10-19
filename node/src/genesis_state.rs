use burst_store::account::AccountInfo;
use burst_types::{
    BlockHash, NetworkId, ProtocolParams, Signature, Timestamp, TxHash, WalletAddress, WalletState,
};

/// The running node's genesis contents, derived only from configured public inputs.
pub(crate) fn block(network: NetworkId, params: &ProtocolParams) -> burst_ledger::StateBlock {
    let address = crate::genesis_key::genesis_address(network);
    let mut block = burst_ledger::StateBlock {
        signing_grant: burst_types::BlockHash::ZERO,
        delegation_payload: Vec::new(),
        version: burst_ledger::CURRENT_BLOCK_VERSION,
        payment_epoch: BlockHash::ZERO,
        block_type: burst_ledger::BlockType::Open,
        account: address.clone(),
        representative: address,
        previous: BlockHash::ZERO,
        brn_balance: 0,
        trst_balance: 0,
        link: BlockHash::ZERO,
        origin: TxHash::ZERO,
        transaction: TxHash::ZERO,
        target_head: BlockHash::ZERO,
        merge_sources: Vec::new(),
        release_sources: Vec::new(),
        timestamp: Timestamp::EPOCH,
        params_hash: params.params_hash(),
        work: 0,
        signature: Signature([0; 64]),
        hash: BlockHash::ZERO,
    };
    block.hash = block.compute_hash();
    block
}

/// The configured genesis hash is the trust anchor, not an ORV election result.
/// This does not establish finality for any descendant or repair account state.
pub(crate) fn confirm_anchor(
    batch: &mut burst_store_lmdb::LmdbWriteTransaction<'_>,
    expected: &burst_ledger::StateBlock,
) -> Result<(), burst_store::StoreError> {
    use burst_store::StoreError;
    let stored: burst_ledger::StateBlock = bincode::deserialize(&batch.get_block(&expected.hash)?)
        .map_err(|error| StoreError::Serialization(error.to_string()))?;
    let account = batch.get_account(&expected.account)?;
    if stored.hash != expected.hash
        || stored.compute_hash() != expected.hash
        || account.address != expected.account
        || account.confirmation_height > account.block_count
        || batch.block_height(&expected.account, &expected.hash)? != 1
        || batch.block_height(&account.address, &account.head)? != account.block_count
    {
        return Err(StoreError::Corruption(
            "genesis anchor or account index mismatch".into(),
        ));
    }
    batch.advance_confirmation_height(&expected.account, 1)
}

/// Stage missing genesis lifecycle state; publish the copy only after commit.
pub(crate) fn initialize_verification(
    batch: &mut burst_store_lmdb::LmdbWriteTransaction<'_>,
    genesis: &burst_ledger::StateBlock,
    live: &burst_verification::VerificationOrchestrator,
) -> Result<Option<burst_verification::VerificationOrchestrator>, burst_store::StoreError> {
    use burst_store::StoreError;
    if live.get_state(&genesis.account).is_some() {
        return Ok(None);
    }
    let account = batch.get_account(&genesis.account)?;
    if account.state != WalletState::Verified || account.verified_at != Some(Timestamp::EPOCH) {
        return Err(StoreError::Corruption(
            "genesis lifecycle is missing after an identity transition".into(),
        ));
    }
    confirm_anchor(batch, genesis)?;
    let mut candidate = live.clone();
    candidate.initialize_genesis(&genesis.account);
    batch.put_meta(
        crate::endorsement_commit::SNAPSHOT_KEY,
        &candidate
            .snapshot()
            .encode()
            .map_err(|error| StoreError::Serialization(error.to_string()))?,
    )?;
    Ok(Some(candidate))
}

/// Initialize once. Repairing an old frontier must not undo an adjudication.
pub(crate) fn account_to_write(
    existing: Option<AccountInfo>,
    address: WalletAddress,
    frontier: BlockHash,
) -> Option<AccountInfo> {
    match existing {
        Some(mut account) if account.head.is_zero() && !frontier.is_zero() => {
            account.head = frontier;
            account.block_count = account.block_count.max(1);
            Some(account)
        }
        Some(_) => None,
        None => Some(AccountInfo {
            address: address.clone(),
            state: WalletState::Verified,
            verified_at: Some(Timestamp::new(0)),
            head: frontier,
            block_count: 1,
            confirmation_height: 0,
            representative: address,
            total_brn_burned: 0,
            total_brn_staked: 0,
            trst_balance: 0,
            expired_trst: 0,
            revoked_trst: 0,
            epoch: 0,
            verifier_opted_in_at: None,
            orv_evicted: false,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genesis_lifecycle_initialization_is_atomic_and_never_resurrects_an_adjudicated_wallet() {
        use burst_store::{account::AccountStore, meta::MetaStore};
        use burst_verification::{
            state::VerificationPhase, OrchestratorSnapshot, VerificationOrchestrator,
        };
        let dir = tempfile::tempdir().unwrap();
        let store = burst_store_lmdb::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let genesis = block(NetworkId::Dev, &ProtocolParams::default());
        let mut account = account_to_write(None, genesis.account.clone(), genesis.hash).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_block_with_account(
                &genesis.hash,
                &bincode::serialize(&genesis).unwrap(),
                &genesis.account,
                1,
            )
            .unwrap();
        batch.put_account_info(&account, false).unwrap();
        batch.commit().unwrap();
        let live = VerificationOrchestrator::new();
        let mut batch = store.tx_begin_write().unwrap();
        let candidate = initialize_verification(&mut batch, &genesis, &live)
            .unwrap()
            .unwrap();
        assert!(live.get_state(&genesis.account).is_none());
        assert_eq!(
            candidate.get_state(&genesis.account).unwrap().phase,
            VerificationPhase::Verified
        );
        drop(batch);
        assert!(matches!(
            store
                .meta_store()
                .get_meta(crate::endorsement_commit::SNAPSHOT_KEY),
            Err(burst_store::StoreError::NotFound(_))
        ));
        let mut batch = store.tx_begin_write().unwrap();
        initialize_verification(&mut batch, &genesis, &live)
            .unwrap()
            .unwrap();
        batch.commit().unwrap();
        let bytes = store
            .meta_store()
            .get_meta(crate::endorsement_commit::SNAPSHOT_KEY)
            .unwrap();
        let restored =
            VerificationOrchestrator::restore(OrchestratorSnapshot::decode(&bytes).unwrap());
        let mut snapshot = restored.snapshot();
        snapshot.states.get_mut(&genesis.account).unwrap().phase = VerificationPhase::Unverified;
        let mut revoked = VerificationOrchestrator::restore(snapshot);
        revoked.initialize_genesis(&genesis.account);
        assert_eq!(
            revoked.get_state(&genesis.account).unwrap().phase,
            VerificationPhase::Unverified
        );
        assert!(revoked.drain_events().is_empty());
        account.state = WalletState::Revoked;
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_account_info(&account, false).unwrap();
        batch
            .put_meta(
                crate::endorsement_commit::SNAPSHOT_KEY,
                &revoked.snapshot().encode().unwrap(),
            )
            .unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert!(initialize_verification(&mut batch, &genesis, &revoked)
            .unwrap()
            .is_none());
        assert!(initialize_verification(&mut batch, &genesis, &live).is_err());
        drop(batch);
        assert_eq!(
            store
                .account_store()
                .get_account(&genesis.account)
                .unwrap()
                .state,
            WalletState::Revoked
        );
    }

    #[test]
    fn confirming_the_pinned_anchor_preserves_descendants_and_adjudicated_state() {
        use burst_store::account::AccountStore;
        let dir = tempfile::tempdir().unwrap();
        let store = burst_store_lmdb::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let genesis = block(NetworkId::Dev, &ProtocolParams::default());
        let mut next = genesis.clone();
        next.block_type = burst_ledger::BlockType::ChangeRepresentative;
        next.previous = genesis.hash;
        next.timestamp = Timestamp::new(1);
        next.hash = next.compute_hash();
        let mut account = account_to_write(None, genesis.account.clone(), genesis.hash).unwrap();
        account.head = next.hash;
        account.block_count = 2;
        account.state = WalletState::Revoked;
        account.total_brn_burned = 100;
        account.total_brn_staked = 20;
        let mut batch = store.tx_begin_write().unwrap();
        for (block, height) in [(&genesis, 1), (&next, 2)] {
            batch
                .put_block_with_account(
                    &block.hash,
                    &bincode::serialize(block).unwrap(),
                    &block.account,
                    height,
                )
                .unwrap();
        }
        batch.put_account_info(&account, false).unwrap();
        batch.commit().unwrap();
        {
            let mut batch = store.tx_begin_write().unwrap();
            let mut corrupt = genesis.clone();
            corrupt.brn_balance = 1;
            batch
                .put_block(&genesis.hash, &bincode::serialize(&corrupt).unwrap())
                .unwrap();
            assert!(confirm_anchor(&mut batch, &genesis).is_err());
        }
        for commit in [false, true] {
            let mut batch = store.tx_begin_write().unwrap();
            confirm_anchor(&mut batch, &genesis).unwrap();
            let mut expected = account.clone();
            expected.confirmation_height = 1;
            assert_eq!(
                bincode::serialize(&batch.get_account(&account.address).unwrap()).unwrap(),
                bincode::serialize(&expected).unwrap()
            );
            if commit {
                batch.commit().unwrap();
            } else {
                drop(batch);
            }
            assert_eq!(
                store
                    .account_store()
                    .get_account(&account.address)
                    .unwrap()
                    .confirmation_height,
                u64::from(commit)
            );
        }
        let mut batch = store.tx_begin_write().unwrap();
        let foreign = block(NetworkId::Test, &ProtocolParams::default());
        assert!(confirm_anchor(&mut batch, &foreign).is_err());
        assert_eq!(
            batch
                .get_account(&account.address)
                .unwrap()
                .confirmation_height,
            1
        );
    }

    #[test]
    fn initialization_is_once_only_and_repairs_preserve_adjudication() {
        let address = WalletAddress::new("brst_genesis");
        let frontier = BlockHash::new([1; 32]);
        let fresh = account_to_write(None, address.clone(), frontier).unwrap();
        assert_eq!(fresh.state, WalletState::Verified);
        assert_eq!(fresh.head, frontier);
        for state in [
            WalletState::Verified,
            WalletState::Unverified,
            WalletState::Revoked,
            WalletState::Deactivated,
            WalletState::Challenged,
        ] {
            let mut account = fresh.clone();
            account.state = state;
            account.total_brn_burned = 100;
            account.verified_at = Some(Timestamp::new(123));
            assert!(account_to_write(Some(account.clone()), address.clone(), frontier).is_none());
            account.head = BlockHash::ZERO;
            let repaired = account_to_write(Some(account), address.clone(), frontier).unwrap();
            assert_eq!(repaired.state, state);
            assert_eq!(repaired.total_brn_burned, 100);
            assert_eq!(repaired.verified_at, Some(Timestamp::new(123)));
        }
    }
}
