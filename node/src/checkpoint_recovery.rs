//! Local demand for a certified epoch closure. A request grants no authority.
use burst_ledger::{BlockType, StateBlock};
use burst_store::{account::AccountStore, meta::MetaStore, StoreError};
use burst_store_lmdb::LmdbStore;
use burst_types::BlockHash;

const KEY: &str = "checkpoint_recovery_demand_v1";
pub(crate) const MIN_CUT_INTERVAL: u64 = 60;

pub(crate) fn commitment() -> BlockHash {
    BlockHash::new(burst_crypto::blake2b_256(b"BURST/recovery-checkpoint/v1\0"))
}

fn demand(store: &LmdbStore) -> Result<Option<Vec<u8>>, StoreError> {
    let txn = store.tx_begin_read()?;
    store.meta_store().get_meta_txn(&txn, KEY)
}

/// Called only after ordinary signature/work authentication and a stale-time
/// rejection in the current epoch. Store one bounded hint, not the block body.
pub(crate) fn request(store: &LmdbStore, block: &StateBlock) -> Result<(), StoreError> {
    let Some(authority) = crate::payment_authority::PaymentAuthority::read(store)? else {
        return Ok(());
    };
    if authority.epoch != block.payment_epoch {
        return Ok(());
    }
    let eligible = match store.account_store().get_account(&block.account) {
        Ok(account) => account.head == block.previous
            && account.block_count == account.confirmation_height,
        Err(StoreError::NotFound(_)) => block.previous.is_zero() && block.block_type == BlockType::Open,
        Err(error) => return Err(error),
    };
    if !eligible {
        return Ok(());
    }
    if let Some(saved) = demand(store)? {
        if saved.len() != 40 {
            return Err(StoreError::Corruption("invalid checkpoint recovery demand".into()));
        }
        if saved[..32] == *block.payment_epoch.as_bytes() {
            // A later request cannot postpone an already requested closure.
            return Ok(());
        }
    }
    let mut value = block.payment_epoch.as_bytes().to_vec();
    value.extend_from_slice(&block.timestamp.as_secs().to_be_bytes());
    store.meta_store().put_meta(KEY, &value)?;
    Ok(())
}

pub(crate) fn requested(store: &LmdbStore, epoch: BlockHash, cutoff: u64) -> Result<bool, StoreError> {
    let Some(value) = demand(store)? else { return Ok(false) };
    if value.len() != 40 {
        return Err(StoreError::Corruption("invalid checkpoint recovery demand".into()));
    }
    let timestamp = u64::from_be_bytes(value[32..].try_into().unwrap());
    Ok(value[..32] == *epoch.as_bytes()
        && timestamp.checked_add(MIN_CUT_INTERVAL).is_some_and(|expired| cutoff > expired))
}

pub(crate) fn valid_cut(parent_cutoff: u64, cutoff: u64) -> bool {
    parent_cutoff.checked_add(MIN_CUT_INTERVAL).is_some_and(|earliest| cutoff >= earliest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demand_is_epoch_scoped_bounded_and_never_authority() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let epoch = BlockHash::new([1; 32]);
        assert!(!requested(&store, epoch, 500).unwrap());
        let mut demand = epoch.as_bytes().to_vec();
        demand.extend_from_slice(&100u64.to_be_bytes());
        store.meta_store().put_meta(KEY, &demand).unwrap();
        assert!(!requested(&store, epoch, 160).unwrap());
        assert!(requested(&store, epoch, 161).unwrap());
        assert!(!requested(&store, BlockHash::new([2; 32]), 500).unwrap());
        assert!(!valid_cut(100, 159));
        assert!(valid_cut(100, 160));
        assert!(!valid_cut(u64::MAX - 1, u64::MAX));
        assert!(crate::payment_authority::PaymentAuthority::read(&store).unwrap().is_none());
        store.meta_store().put_meta(KEY, &[0; 39]).unwrap();
        assert!(requested(&store, epoch, 500).is_err());
    }
}
