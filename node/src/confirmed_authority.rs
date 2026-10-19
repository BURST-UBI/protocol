//! Local confirmed authority projection. This is not an agreed checkpoint roster.
//! Promotion uses the block's committed journal, never its later live account state.

use std::collections::BTreeMap;

use bincode::Options;
use burst_store::{account::AccountInfo, StoreError};
use burst_store_lmdb::{block_undo::UndoTable, LmdbStore, LmdbWriteTransaction};
use burst_types::{BlockHash, Timestamp, WalletAddress, WalletState};
use serde::{Deserialize, Serialize};

const KEY: &str = "confirmed_authority/v1";
const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ACCOUNTS: usize = 1_000_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Authority {
    pub address: WalletAddress,
    pub state: WalletState,
    pub representative: WalletAddress,
    pub verified_at: Option<Timestamp>,
    pub opted_in_at: Option<Timestamp>,
    pub evicted: bool,
}

impl From<&AccountInfo> for Authority {
    fn from(account: &AccountInfo) -> Self {
        Self {
            address: account.address.clone(),
            state: account.state,
            representative: account.representative.clone(),
            verified_at: account.verified_at,
            opted_in_at: account.verifier_opted_in_at,
            evicted: account.orv_evicted,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ConfirmedAuthority {
    genesis: BlockHash,
    pub accounts: BTreeMap<String, Authority>,
}

fn invalid(message: &str) -> StoreError {
    StoreError::Corruption(message.into())
}

impl ConfirmedAuthority {
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
        if batch.get_meta(KEY)?.is_some() || genesis.block_count != 1 || genesis.head.is_zero() {
            return Err(invalid("authority initialization requires a fresh genesis"));
        }
        Self {
            genesis: genesis.head,
            accounts: BTreeMap::from([(genesis.address.to_string(), Authority::from(genesis))]),
        }
        .persist(batch)
    }

    fn decode(bytes: &[u8]) -> Result<Self, StoreError> {
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("confirmed authority exceeds byte limit"));
        }
        let state: Self = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(MAX_BYTES)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
        if state.genesis.is_zero()
            || state.accounts.is_empty()
            || state.accounts.len() > MAX_ACCOUNTS
            || state
                .accounts
                .iter()
                .any(|(key, account)| key != account.address.as_str())
        {
            return Err(invalid("invalid confirmed authority projection"));
        }
        Ok(state)
    }

    pub fn read(store: &LmdbStore) -> Result<Self, StoreError> {
        use burst_store::meta::MetaStore;
        Self::decode(&store.meta_store().get_meta(KEY)?)
    }

    pub fn load(batch: &LmdbWriteTransaction<'_>) -> Result<Self, StoreError> {
        Self::decode(&batch.get_meta(KEY)?.ok_or_else(|| {
            invalid(
                "ledger lacks confirmed authority history; use a separate fresh development ledger",
            )
        })?)
    }

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
            let key = after.address.as_str();
            if change.key != key.as_bytes()
                || before.as_ref().is_some_and(|a| a.address != after.address)
                || self.accounts.get(key) != before.as_ref()
            {
                return Err(invalid(
                    "authority transition has an unconfirmed or conflicting prerequisite",
                ));
            }
            staged.push((key.to_owned(), after));
        }
        if !staged.iter().any(|(_, a)| a.address == journal.account) {
            return Err(invalid("authority journal omits the block account"));
        }
        for (key, authority) in staged {
            self.accounts.insert(key, authority);
        }
        Ok(())
    }

    pub fn persist(&self, batch: &mut LmdbWriteTransaction<'_>) -> Result<(), StoreError> {
        let bytes =
            bincode::serialize(self).map_err(|e| StoreError::Serialization(e.to_string()))?;
        Self::decode(&bytes)?;
        batch.put_meta(KEY, &bytes)
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
            ConfirmedAuthority::read(&store).unwrap().accounts[account.address.as_str()],
            original
        );

        let mut batch = store.tx_begin_write().unwrap();
        let mut confirmed = ConfirmedAuthority::load(&batch).unwrap();
        assert!(confirmed.promote(&batch, second).is_err());
        assert_eq!(confirmed.accounts[account.address.as_str()], original);
        confirmed.promote(&batch, first).unwrap();
        confirmed.persist(&mut batch).unwrap();
        batch.commit().unwrap();
        assert_eq!(
            ConfirmedAuthority::read(&store).unwrap().accounts[account.address.as_str()],
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
            ConfirmedAuthority::read(&store).unwrap().accounts[account.address.as_str()],
            first_authority
        );
        let mut batch = store.tx_begin_write().unwrap();
        let mut confirmed = ConfirmedAuthority::load(&batch).unwrap();
        confirmed.promote(&batch, second).unwrap();
        confirmed.persist(&mut batch).unwrap();
        drop(batch);
        assert_eq!(
            ConfirmedAuthority::read(&store).unwrap().accounts[account.address.as_str()],
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
}
