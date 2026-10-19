//! Quarantined checkpoint history. Only a certificate reconstructed from the
//! configured genesis, plus complete semantic replay, can admit an old hash.

use super::*;
use burst_ledger::StateBlock;
use burst_types::BlockHash;
use std::collections::HashSet;
use std::sync::RwLock;

pub(crate) const MAX_TRANSFER_BYTES: u64 = 64 * 1024 * 1024;

pub(crate) mod snapshot_transfer;

pub(super) fn persist_history(
    batch: &mut burst_store_lmdb::LmdbWriteTransaction<'_>,
    history: &ConfirmedHistory,
) -> Result<(), StoreError> {
    for block in history.replay_blocks() {
        let key = format!("checkpoint_history/v1/{}", block.hash);
        let bytes = bincode::serialize(block).map_err(invalid)?;
        match batch.get_meta(&key)? {
            Some(existing) if existing != bytes => {
                return Err(invalid("certified history bytes changed"))
            }
            Some(_) => (),
            None => batch.put_meta(&key, &bytes)?,
        }
    }
    for witness in history.proposal_witnesses() {
        let key = crate::confirmed_history::proposal_archive_key(witness.block);
        match batch.get_meta(&key)? {
            Some(existing) if existing != witness.bytes => {
                return Err(invalid("certified proposal content changed"))
            }
            Some(_) => (),
            None => batch.put_meta(&key, &witness.bytes)?,
        }
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct Transfer {
    version: u8,
    record: Vec<u8>,
    blocks: Vec<StateBlock>,
    contents: Vec<crate::confirmed_history::ProposalWitness>,
}

struct Prefix {
    height: u64,
    checkpoint: BlockHash,
    hashes: HashSet<BlockHash>,
}

#[derive(Default)]
pub(crate) struct HistoricalAdmissions {
    prefix: RwLock<Option<Prefix>>,
}

impl HistoricalAdmissions {
    pub(crate) fn contains(&self, hash: &BlockHash) -> bool {
        self.prefix.read().is_ok_and(|prefix| {
            prefix
                .as_ref()
                .is_some_and(|prefix| prefix.hashes.contains(hash))
        })
    }

    fn authorize(
        &self,
        certificate: &chain::CertifiedEvent,
        history: &ConfirmedHistory,
        ancestors: &[chain::CertifiedEvent],
    ) -> Result<(), StoreError> {
        let checkpoint = &certificate.certificate().checkpoint;
        let mut prefix = self.prefix.write().map_err(invalid)?;
        if let Some(old) = prefix.as_ref() {
            if old.height > checkpoint.height {
                return Ok(());
            }
            if old.height == checkpoint.height {
                if old.checkpoint != checkpoint.hash() {
                    return Err(invalid("conflicting historical authorization"));
                }
                return Ok(());
            }
            if ancestors
                .get(old.height as usize - 1)
                .is_none_or(|ancestor| ancestor.certificate().checkpoint.hash() != old.checkpoint)
            {
                return Err(invalid(
                    "historical authorization does not extend the authenticated prefix",
                ));
            }
        }
        *prefix = Some(Prefix {
            height: checkpoint.height,
            checkpoint: checkpoint.hash(),
            hashes: history
                .replay_blocks()
                .iter()
                .map(|block| block.hash)
                .collect(),
        });
        Ok(())
    }
}

impl CertifiedCheckpoint {
    pub(crate) fn export_transfer(
        store: &LmdbStore,
        network: NetworkId,
        params: &ProtocolParams,
        height: u64,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        Self::export_transfer_for_request(store, network, params, height, false)
    }

    pub(crate) fn export_transfer_for_request(
        store: &LmdbStore,
        network: NetworkId,
        params: &ProtocolParams,
        height: u64,
        record_only: bool,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        Self::export_transfer_with_recovery(store, network, params, height, record_only, &mut ExportRecovery::default())
    }

    pub(crate) fn export_transfer_with_recovery(
        store: &LmdbStore,
        network: NetworkId,
        params: &ProtocolParams,
        height: u64,
        record_only: bool,
        recovery: &mut ExportRecovery,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let Some(bytes) = Self::export_record_with_recovery(store, network, params, height, recovery)? else {
            return Ok(None);
        };
        if record_only {
            return snapshot_transfer::encode(bytes).map(Some);
        }
        let transaction = store.tx_begin_read()?;
        let compact = store
            .meta_store()
            .get_meta_txn(
                &transaction,
                crate::confirmed_replay::snapshot::installed::BASE_KEY,
            )?
            .is_some();
        drop(transaction);
        if compact {
            return snapshot_transfer::encode(bytes).map(Some);
        }
        let record = chain::decode_record(&bytes)?;
        let genesis = ReplayedCheckpoint::genesis_anchor(network, params)?.genesis_hash();
        let history = ConfirmedHistory::read_certified(
            store,
            network,
            genesis,
            record.certificate().checkpoint.cutoff,
            record.heads(),
            &[],
        )?;
        let transfer = Transfer {
            version: 2,
            record: bytes,
            blocks: history.replay_blocks().to_vec(),
            contents: history.proposal_witnesses().to_vec(),
        };
        if bincode::serialized_size(&transfer).map_err(invalid)? > MAX_TRANSFER_BYTES {
            return Err(invalid("checkpoint history transfer exceeds byte limit"));
        }
        bincode::serialize(&transfer).map(Some).map_err(invalid)
    }

    /// Authenticate and replay before retaining signed history outside the live
    /// ledger. The coordinated installer supplies economics and finality; this
    /// import never applies peer account snapshots or reopens ordinary voting.
    pub(crate) fn import_transfer(
        store: &LmdbStore,
        network: NetworkId,
        params: &ProtocolParams,
        height: u64,
        bytes: &[u8],
        admissions: &HistoricalAdmissions,
    ) -> Result<(), StoreError> {
        if bytes.len() as u64 > MAX_TRANSFER_BYTES {
            return Err(invalid("checkpoint history transfer exceeds byte limit"));
        }
        let transfer: Transfer = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .with_limit(MAX_TRANSFER_BYTES)
            .deserialize(bytes)
            .map_err(invalid)?;
        if transfer.version != 2 {
            return Err(invalid("unsupported checkpoint history transfer version"));
        }
        let record = chain::decode_record(&transfer.record)?;
        if record.certificate().checkpoint.network != network
            || record.certificate().checkpoint.height != height
        {
            return Err(invalid(
                "checkpoint history response has a different network or height",
            ));
        }
        let mut chain = chain::load(store, network, params)?;
        let parent_height = height as usize - 1;
        if parent_height > chain.len() {
            return Err(invalid("checkpoint history skips certified ancestry"));
        }
        if let Some(old) = chain.get(parent_height) {
            if old.manifest() != record.manifest() {
                return Err(invalid(
                    "checkpoint history conflicts with durable certificate",
                ));
            }
        }
        chain.truncate(parent_height);
        let anchor = chain::next_anchor(&chain, network, params)?;
        anchor
            .verify_certificate(record.certificate())
            .map_err(invalid)?;
        let history = ConfirmedHistory::from_parts(
            network,
            anchor.genesis_hash(),
            record.certificate().checkpoint.cutoff,
            record.heads(),
            chain.last().map_or(&[][..], |parent| parent.heads()),
            transfer.blocks,
            transfer.contents,
        )?;
        let validated = chain::replay_record(&history, &anchor, params, record, &chain)?;
        let mut batch = store.tx_begin_write()?;
        persist_history(&mut batch, &history)?;
        batch.commit()?;
        admissions.authorize(&validated, &history, &chain)?;
        chain::import_record(store, network, params, height, &transfer.record)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
