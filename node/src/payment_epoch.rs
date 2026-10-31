//! Initialize the genesis payment epoch and check blocks against the installed
//! epoch and cutoff. Later epochs are installed by checkpoint replay.

use burst_store::{meta::PAYMENT_EPOCH_KEY, StoreError};
use burst_store_lmdb::LmdbWriteTransaction;
use burst_types::BlockHash;

/// Stage a nonzero genesis epoch only when no epoch record exists.
/// The caller validates genesis and commits the transaction.
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
    batch: &impl crate::verification_commit::dependencies::view::DependencyView,
    expected: BlockHash,
) -> Result<(), StoreError> {
    if batch.payment_epoch()? != expected || expected.is_zero() {
        return Err(StoreError::Corruption(
            "block payment epoch is not installed".into(),
        ));
    }
    Ok(())
}

/// Require the installed epoch and a timestamp at or after its cutoff.
/// Missing cutoff is allowed only without an installed-authority record.
/// This does not authenticate the block or check its age against the local clock.
pub(crate) fn require_block(
    batch: &impl crate::verification_commit::dependencies::view::DependencyView,
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
