//! Signed economic context. Only the genesis epoch can be installed until the
//! certified authority and outcome handoff is implemented.

use burst_store::{meta::PAYMENT_EPOCH_KEY, StoreError};
use burst_store_lmdb::LmdbWriteTransaction;
use burst_types::BlockHash;

pub(crate) fn initialize(
    batch: &mut LmdbWriteTransaction<'_>,
    genesis: BlockHash,
) -> Result<(), StoreError> {
    if genesis.is_zero() || batch.get_meta(PAYMENT_EPOCH_KEY)?.is_some() {
        return Err(StoreError::Corruption(
            "payment epoch already initialized or invalid".into(),
        ));
    }
    batch.put_meta(PAYMENT_EPOCH_KEY, genesis.as_bytes())
}

pub(crate) fn require(
    batch: &LmdbWriteTransaction<'_>,
    expected: BlockHash,
) -> Result<(), StoreError> {
    if batch.payment_epoch()? != expected || expected.is_zero() {
        return Err(StoreError::Corruption(
            "block payment epoch is not installed".into(),
        ));
    }
    Ok(())
}

pub(crate) fn require_block(
    batch: &LmdbWriteTransaction<'_>,
    block: &burst_ledger::StateBlock,
) -> Result<(), StoreError> {
    require(batch, block.payment_epoch)?;
    let cutoff =
        match batch.get_meta(crate::confirmed_replay::installation::CUTOFF_KEY)? {
            Some(bytes) => u64::from_be_bytes(bytes.as_slice().try_into().map_err(|_| {
                StoreError::Corruption("invalid installed checkpoint cutoff".into())
            })?),
            None if batch
                .get_meta(crate::confirmed_replay::installation::INSTALLED_KEY)?
                .is_none() =>
            {
                0
            }
            None => {
                return Err(StoreError::Corruption(
                    "installed checkpoint cutoff is missing".into(),
                ))
            }
        };
    if block.timestamp.as_secs() < cutoff {
        return Err(StoreError::Corruption(
            "block predates its installed checkpoint cutoff".into(),
        ));
    }
    Ok(())
}
