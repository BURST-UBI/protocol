use super::*;
use std::collections::BTreeSet;

/// Walk one archived record per scheduling turn, retaining only its key.
/// This cursor is a hint, not authenticated state, and wraps after the last key.
#[derive(Default)]
pub(crate) struct ArchivedRefresh {
    cursor: Option<String>,
}

impl ArchivedRefresh {
    pub(crate) fn next(
        &mut self,
        store: &LmdbStore,
        network: NetworkId,
        params: &ProtocolParams,
    ) -> Result<Option<u64>, StoreError> {
        let genesis = ReplayedCheckpoint::genesis_anchor(network, params)?;
        let prefix = format!(
            "checkpoint_record_archive/v1/{}/{}/",
            network.as_str(),
            genesis.genesis_hash(),
        );
        let txn = store.tx_begin_read()?;
        let mut page = store.meta_store().prefix_page_bounded_txn(
            &txn,
            &prefix,
            self.cursor.as_deref(),
            1,
            MAX_RECORD_BYTES as usize + 256,
        )?;
        let Some((key, bytes)) = page.pop() else {
            self.cursor = None;
            return Ok(None);
        };
        let record = chain::decode_record(&bytes)?;
        let checkpoint = &record.certificate().checkpoint;
        if checkpoint.network != network
            || checkpoint.genesis != genesis.genesis_hash()
            || checkpoint.height == 0
            || key != archived_record_key(&genesis, checkpoint.height)
        {
            return Err(invalid("archived refresh record has a different identity"));
        }
        let height = checkpoint.height;
        let missing = match &record {
            chain::ParsedRecord::Selection(selection) => {
                selection.beacon.is_none()
                    && store
                        .meta_store()
                        .get_meta_txn(&txn, &archive_selection::beacon_key(checkpoint.hash()))?
                        .is_none()
            }
            _ => false,
        };
        self.cursor = Some(key);
        Ok(missing.then_some(height))
    }
}

/// Scheduling hints only. Imported records still require ordinary authentication.
pub(crate) fn selection_refresh_heights(
    store: &LmdbStore,
    network: NetworkId,
    params: &ProtocolParams,
) -> Result<Vec<u64>, StoreError> {
    let genesis = ReplayedCheckpoint::genesis_anchor(network, params)?;
    let prefix = prefix(&genesis);
    let txn = store.tx_begin_read()?;
    let tip = decode_tip(
        store
            .meta_store()
            .get_meta_txn(&txn, &format!("{prefix}/head"))?
            .as_deref(),
    )?;
    let records = inventory::read(store, &txn, &prefix, tip)?;
    drop(txn);
    let mut heights = BTreeSet::new();
    for (height, bytes) in records {
        let record = chain::decode_record(&bytes)?;
        if record.certificate().checkpoint.height != height {
            return Err(invalid("refresh record is stored at a different height"));
        }
        if let chain::ParsedRecord::Selection(selection) = record {
            if selection.beacon.is_none()
                && missing_stored_beacon(store, selection.certificate.checkpoint.hash())?
            {
                heights.insert(height);
            }
        }
    }
    if let Some(base) =
        crate::confirmed_replay::snapshot::installed::load_base(store, network, params)?
    {
        for selection in base.recover_selections().map_err(invalid)? {
            if selection.beacon().is_none()
                && missing_stored_beacon(store, selection.certificate().checkpoint.hash())?
            {
                heights.insert(selection.certificate().checkpoint.height);
            }
        }
    }
    Ok(heights.into_iter().collect())
}

// This only suppresses redundant downloads. Publication and replay still
// authenticate persisted proofs; their presence is not authority to install.
fn missing_stored_beacon(
    store: &LmdbStore,
    epoch: burst_types::BlockHash,
) -> Result<bool, StoreError> {
    let txn = store.tx_begin_read()?;
    Ok(store
        .meta_store()
        .get_meta_txn(&txn, &archive_selection::beacon_key(epoch))?
        .is_none())
}
