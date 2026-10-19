//! Durable authenticated base, stored with the projected ledger. Reading a base
//! does not validate a newer tail, publish engines or grant local signing rights.

use super::archive::AuthenticatedSnapshot;
use burst_store::StoreError;
use burst_store_lmdb::{LmdbStore, LmdbWriteTransaction};
use burst_types::{NetworkId, ProtocolParams};

mod caches;
mod current;
pub(crate) use current::recover_current_tip_with_cache;
mod changes;
mod economics;
mod projection_cache;
pub(crate) use projection_cache::{CommittedProjection, ProjectionCache};
mod journals;
mod metadata;
mod order;
mod pages;
#[cfg(test)]
pub(crate) use pages::read as read_base_bytes;
pub(crate) use pages::Commitment as BaseCommitment;
pub(in crate::confirmed_replay::snapshot) mod authority_history;
pub(in crate::confirmed_replay) mod publication;
mod recovery;
mod selection_history;
pub use selection_history::SelectionHistory;
mod retained;
mod runtime;
pub use changes::ReplayedEconomicChange;
pub(crate) use metadata::{recover_metadata_retaining_progress, CompactPreparation};
pub use metadata::{
    recover_metadata_from_peers, PreparedCompactInstallation, RecoveredIndexProjection,
    RecoveredInstalledMetadata, RecoveredUndoProjection,
};
pub use recovery::RecoveredSnapshot;
pub(crate) use recovery::ReplayCache;
pub use runtime::RecoveredRuntime;

pub(crate) const BASE_KEY: &str = "checkpoint_snapshot/v1/base";

fn invalid(error: impl ToString) -> StoreError {
    StoreError::Corruption(error.to_string())
}

pub(super) fn persist(
    snapshot: &AuthenticatedSnapshot,
    batch: &mut LmdbWriteTransaction<'_>,
) -> Result<(), StoreError> {
    let body = snapshot
        .state
        .prepare_snapshot_in(snapshot.anchor(), batch.storage_directory())
        .map_err(invalid)?;
    let mut writer = pages::Writer::new(batch)?;
    body.write_archive_with_beacons(
        &snapshot.authority_base,
        &snapshot.authority,
        snapshot.certificate(),
        &snapshot.beacons,
        &mut writer,
    )
    .map_err(invalid)?;
    writer.finish()
}

/// Reauthenticate persisted state against independently configured genesis.
/// This is the base only; startup must also validate installed authority, its
/// sparse indexes, any certified successors and the provisional payment tail.
pub fn load_base(
    store: &LmdbStore,
    network: NetworkId,
    initial_params: &ProtocolParams,
) -> Result<Option<AuthenticatedSnapshot>, StoreError> {
    let txn = store.tx_begin_read()?;
    read_base(
        |key| store.meta_store().get_meta_txn(&txn, key),
        network,
        initial_params,
        store.env().path(),
    )
    .map(|loaded| loaded.map(|(snapshot, _)| snapshot))
}

#[cfg(test)]
pub(crate) fn read_stored_base_bytes(store: &LmdbStore) -> Result<Option<Vec<u8>>, StoreError> {
    let txn = store.tx_begin_read()?;
    read_base_bytes(|key| store.meta_store().get_meta_txn(&txn, key))
}

pub(crate) fn read_base(
    get: impl FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>,
    network: NetworkId,
    initial_params: &ProtocolParams,
    directory: &std::path::Path,
) -> Result<Option<(AuthenticatedSnapshot, BaseCommitment)>, StoreError> {
    let genesis =
        crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(network, initial_params)?;
    read_base_at(get, &genesis, directory)
}

pub(crate) fn read_base_at(
    get: impl FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>,
    genesis: &burst_consensus::checkpoint::CheckpointAnchor,
    directory: &std::path::Path,
) -> Result<Option<(AuthenticatedSnapshot, BaseCommitment)>, StoreError> {
    let Some(mut input) = pages::Reader::open(get)? else {
        return Ok(None);
    };
    let snapshot =
        AuthenticatedSnapshot::read_in(genesis, &mut input, directory).map_err(invalid)?;
    let commitment = input.commitment()?;
    snapshot.validate_selections().map_err(invalid)?;
    Ok(Some((snapshot, commitment)))
}
