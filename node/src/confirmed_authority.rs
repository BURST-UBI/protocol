//! Local confirmed authority projection. This is not an agreed checkpoint roster.
//! Promotion uses the block's committed journal, never its later live account state.

use burst_store::{account::AccountInfo, StoreError};
use burst_store_lmdb::{block_undo::UndoTable, LmdbStore, LmdbWriteTransaction};
use burst_types::BlockHash;
#[cfg(test)]
use burst_types::{WalletAddress, WalletState};
use serde::{Deserialize, Serialize};
use burst_utils::SharedMap;
pub(crate) use burst_store_lmdb::confirmed_authority::Authority;
#[cfg(test)]
const KEY: &str = burst_store_lmdb::confirmed_authority::KEY;

#[derive(Serialize, Deserialize)]
pub(crate) struct ConfirmedAuthority {
    genesis: BlockHash,
    pub accounts: SharedMap<String, Authority>,
}

fn invalid(message: &str) -> StoreError {
    StoreError::Corruption(message.into())
}

impl ConfirmedAuthority {
    /// Build from caller-validated checkpoint accounts; does not verify a certificate.
    pub(crate) fn from_checkpoint(genesis: BlockHash, accounts: &[AccountInfo]) -> Self {
        Self {
            genesis,
            accounts: accounts
                .iter()
                .map(|account| (account.address.to_string(), Authority::from(account)))
                .collect(),
        }
    }

    pub fn rebuild_weights(&self, weights: &mut burst_consensus::RepWeightCache) {
        weights.rebuild_from_accounts(
            self.accounts
                .values()
                .map(|a| (a.state.is_verified(), a.representative.clone())),
        );
        weights.set_evicted(
            self.accounts
                .values()
                .filter(|a| a.evicted)
                .map(|a| a.address.clone())
                .collect(),
        );
    }

    pub fn initialize(
        batch: &mut LmdbWriteTransaction<'_>,
        genesis: &AccountInfo,
    ) -> Result<(), StoreError> {
        if batch.has_confirmed_authority_storage()?
            || genesis.block_count != 1 || genesis.head.is_zero() {
            return Err(invalid("authority initialization requires a fresh genesis"));
        }
        Self {
            genesis: genesis.head,
            accounts: [(genesis.address.to_string(), Authority::from(genesis))].into_iter().collect(),
        }
        .persist(batch)
    }

    pub fn read(store: &LmdbStore) -> Result<Self, StoreError> {
        let snapshot = store.read_confirmed_authority_snapshot()?;
        Ok(Self { genesis: snapshot.genesis, accounts: snapshot.accounts })
    }

    pub fn load(batch: &LmdbWriteTransaction<'_>) -> Result<Self, StoreError> {
        let snapshot = batch.read_confirmed_authority()?;
        Ok(Self { genesis: snapshot.genesis, accounts: snapshot.accounts })
    }

    /// Apply account authority changes from one committed journal in memory.
    /// Validate all prerequisites before mutating; the caller establishes finality
    /// and persists this projection in the cementation transaction.
    pub fn promote(
        &mut self,
        batch: &LmdbWriteTransaction<'_>,
        hash: BlockHash,
    ) -> Result<(), StoreError> {
        if hash == self.genesis {
            return Ok(());
        }
        let journal = batch
            .block_undo(&hash)?
            .ok_or_else(|| invalid("missing authority transition journal"))?;
        let mut staged = Vec::new();
        for change in journal
            .changes
            .iter()
            .filter(|change| change.table == UndoTable::Accounts)
        {
            let decode = |bytes: Option<&[u8]>| -> Result<Option<Authority>, StoreError> {
                bytes
                    .map(|bytes| {
                        bincode::deserialize::<AccountInfo>(bytes)
                            .map(|a| Authority::from(&a))
                            .map_err(|e| StoreError::Serialization(e.to_string()))
                    })
                    .transpose()
            };
            let before = decode(change.before.as_deref())?;
            let after = decode(change.after.as_deref())?
                .ok_or_else(|| invalid("authority account deletion is unsupported"))?;
            let key = after.address.to_string();
            if change.key != key.as_bytes()
                || before.as_ref().is_some_and(|a| a.address != after.address)
                || self.accounts.get(&key) != before.as_ref()
            {
                return Err(invalid(
                    "authority transition has an unconfirmed or conflicting prerequisite",
                ));
            }
            staged.push((key, after));
        }
        if !staged.iter().any(|(_, a)| a.address == journal.account) {
            return Err(invalid("authority journal omits the block account"));
        }
        for (key, authority) in staged {
            if self.accounts.get(&key) != Some(&authority) { self.accounts.insert(key, authority); }
        }
        Ok(())
    }

    /// Stage the projection in the supplied transaction; the caller commits it.
    pub fn persist(&self, batch: &mut LmdbWriteTransaction<'_>) -> Result<(), StoreError> {
        batch.write_confirmed_authority(burst_store_lmdb::confirmed_authority::Snapshot {
            genesis: self.genesis, accounts: self.accounts.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::account::AccountStore;

    fn fixture(store: &LmdbStore) -> AccountInfo {
        let address =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[0; 32]).public);
        let account =
            crate::genesis_state::account_to_write(None, address, BlockHash::new([1; 32])).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_account_info(&account, false).unwrap();
        ConfirmedAuthority::initialize(&mut batch, &account).unwrap();
        batch.commit().unwrap();
        account
    }

    fn journal(store: &LmdbStore, account: &AccountInfo, tag: u8) -> BlockHash {
        let hash = BlockHash::new([tag; 32]);
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .begin_block_undo(hash, account.address.clone(), account.block_count)
            .unwrap();
        batch.put_account_info(account, false).unwrap();
        batch.commit().unwrap();
        hash
    }

    #[test]
    fn provisional_changes_and_later_live_state_never_supply_confirmed_authority() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let mut account = fixture(&store);
        let original = Authority::from(&account);
        account.representative = WalletAddress::new("brst_new_rep");
        account.block_count = 2;
        account.head = BlockHash::new([2; 32]);
        let first = journal(&store, &account, 2);
        let first_authority = Authority::from(&account);
        account.state = WalletState::Revoked;
        account.orv_evicted = true;
        account.block_count = 3;
        account.head = BlockHash::new([3; 32]);
        let second = journal(&store, &account, 3);
        assert_eq!(
            ConfirmedAuthority::read(&store).unwrap().accounts[&account.address.to_string()],
            original
        );

        let mut batch = store.tx_begin_write().unwrap();
        let mut confirmed = ConfirmedAuthority::load(&batch).unwrap();
        assert!(confirmed.promote(&batch, second).is_err());
        assert_eq!(confirmed.accounts[&account.address.to_string()], original);
        confirmed.promote(&batch, first).unwrap();
        confirmed.persist(&mut batch).unwrap();
        batch.commit().unwrap();
        assert_eq!(
            ConfirmedAuthority::read(&store).unwrap().accounts[&account.address.to_string()],
            first_authority
        );
        assert_eq!(
            store
                .account_store()
                .get_account(&account.address)
                .unwrap()
                .state,
            WalletState::Revoked
        );
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        assert_eq!(
            ConfirmedAuthority::read(&store).unwrap().accounts[&account.address.to_string()],
            first_authority
        );
        let mut batch = store.tx_begin_write().unwrap();
        let mut confirmed = ConfirmedAuthority::load(&batch).unwrap();
        confirmed.promote(&batch, second).unwrap();
        confirmed.persist(&mut batch).unwrap();
        drop(batch);
        assert_eq!(
            ConfirmedAuthority::read(&store).unwrap().accounts[&account.address.to_string()],
            first_authority
        );
    }

    #[test]
    fn missing_or_corrupt_history_cannot_be_rebuilt_from_provisional_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        assert!(ConfirmedAuthority::read(&store).is_err());
        let account = fixture(&store);
        let mut batch = store.tx_begin_write().unwrap();
        assert!(ConfirmedAuthority::initialize(&mut batch, &account).is_err());
        batch.put_meta(KEY, &[1, 2, 3]).unwrap();
        batch.commit().unwrap();
        assert!(ConfirmedAuthority::read(&store).is_err());
    }

    #[test]
    fn provisional_block_journals_cannot_modify_the_confirmed_projection() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let account = fixture(&store);
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .begin_block_undo(BlockHash::new([2; 32]), account.address, 2)
            .unwrap();
        assert!(batch.put_meta(KEY, &[0]).is_err());
        assert!(batch.commit().is_err());
        assert!(ConfirmedAuthority::read(&store).is_ok());
    }
    #[test]
    fn payment_only_journal_preserves_shared_authority_projection() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut account = fixture(&store);
        account.block_count = 2;
        account.head = BlockHash::new([2; 32]);
        let hash = journal(&store, &account, 2);
        let mut batch = store.tx_begin_write().unwrap();
        let mut confirmed = ConfirmedAuthority::load(&batch).unwrap();
        let before = confirmed.accounts.clone();
        confirmed.promote(&batch, hash).unwrap();
        assert!(confirmed.accounts.shares_storage_with(&before));
        confirmed.persist(&mut batch).unwrap();
        batch.commit().unwrap();
        let batch = store.tx_begin_write().unwrap();
        assert!(ConfirmedAuthority::load(&batch).unwrap().accounts.shares_storage_with(&before));
    }

}
