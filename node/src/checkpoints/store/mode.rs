use super::*;

/// Select a replay implementation, not an authority source. Recovery still
/// authenticates the installed epoch and any snapshot before either is used.
pub(crate) fn uses_incremental_replay(
    store: &LmdbStore,
    network: NetworkId,
    params: &ProtocolParams,
) -> Result<bool, StoreError> {
    let transaction = store.tx_begin_read()?;
    let meta = store.meta_store();
    if meta.get_meta_txn(
            &transaction,
            crate::confirmed_replay::snapshot::installed::BASE_KEY,
        )?
        .is_some()
    {
        return Ok(true);
    }
    let Some(epoch) = meta.get_meta_txn(&transaction, burst_store::meta::PAYMENT_EPOCH_KEY)? else {
        return Ok(false);
    };
    let epoch = burst_types::BlockHash::new(epoch.as_slice().try_into().map_err(invalid)?);
    if epoch.is_zero() {
        return Err(invalid("installed payment epoch is zero"));
    }
    Ok(epoch != crate::genesis_state::block(network, params).hash)
}
