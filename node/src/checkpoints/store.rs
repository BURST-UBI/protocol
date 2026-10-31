//! A bounded certificate chain reconstructed from configured genesis and signed
//! ledger history. Beacon availability does not change an accepted selection.

use bincode::Options;
use burst_consensus::checkpoint::{CheckpointAnchor, CheckpointCertificate};
use burst_store::StoreError;
use burst_store_lmdb::LmdbStore;
use burst_types::{NetworkId, ProtocolParams, WalletAddress};
use burst_vrf::RandomOutput;
use serde::{Deserialize, Serialize};

use crate::checkpoint_replay::{CheckpointManifest, ReplayedCheckpoint};
use crate::confirmed_history::{ConfirmedHistory, HistoryHead};
use crate::confirmed_replay::snapshot::SelectionSource;

pub(crate) mod archive_selection;
pub(crate) mod chain;
pub(crate) mod compact;
mod export_recovery;
pub(crate) mod historical;
pub(crate) use export_recovery::ExportRecovery;
mod inventory;
pub(crate) mod record_storage;
mod mode;
mod publication;
pub(crate) use mode::uses_incremental_replay;
mod refresh;
pub(crate) use refresh::{selection_refresh_heights, ArchivedRefresh};
mod snapshot_path;
pub(crate) use snapshot_path::{reconcile_snapshot_path, stage_snapshot_head};

pub(crate) const MAX_RECORD_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CHAIN_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_CHECKPOINTS: u64 = 128;
const MAX_BEACON_BYTES: usize = 4096;

fn archived_record_key(anchor: &CheckpointAnchor, height: u64) -> String {
    format!(
        "checkpoint_record_archive/v1/{}/{}/{height}",
        anchor.network().as_str(),
        anchor.genesis_hash()
    )
}

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    version: u8,
    certificate: CheckpointCertificate,
    heads: Vec<HistoryHead>,
    target: WalletAddress,
    beacon: Option<RandomOutput>,
}

#[derive(Clone)]
pub struct CertifiedCheckpoint {
    record: Record,
    replay: ReplayedCheckpoint,
    parent: CheckpointAnchor,
}

fn invalid(error: impl ToString) -> StoreError {
    StoreError::Corruption(error.to_string())
}

fn prefix(anchor: &CheckpointAnchor) -> String {
    format!(
        "checkpoint_chain/v1/{}/{}",
        anchor.network().as_str(),
        anchor.genesis_hash()
    )
}

fn decode_tip(bytes: Option<&[u8]>) -> Result<u64, StoreError> {
    match bytes {
        None => Ok(0),
        Some(bytes) => {
            let height = u64::from_be_bytes(bytes.try_into().map_err(invalid)?);
            if height == 0 {
                return Err(invalid("invalid checkpoint chain height"));
            }
            Ok(height)
        }
    }
}

fn decode(bytes: &[u8]) -> Result<Record, StoreError> {
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(invalid("checkpoint record exceeds byte limit"));
    }
    let record: Record = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .reject_trailing_bytes()
        .with_limit(MAX_RECORD_BYTES)
        .deserialize(bytes)
        .map_err(invalid)?;
    if record.version != 1 || record.certificate.checkpoint.height == 0 {
        return Err(invalid("unsupported checkpoint record version or height"));
    }
    Ok(record)
}

fn encode(record: &Record) -> Result<Vec<u8>, StoreError> {
    if bincode::serialized_size(record).map_err(invalid)? > MAX_RECORD_BYTES {
        return Err(invalid("checkpoint record exceeds byte limit"));
    }
    bincode::serialize(record).map_err(invalid)
}

impl CertifiedCheckpoint {
    /// Recovery-only capability. Missing full-prefix inclusion evidence remains
    /// explicit in `replay`; agreement/signing cannot use its history inventory.
    pub(crate) fn from_compact(
        parent: CheckpointAnchor,
        certificate: CheckpointCertificate,
        replay: ReplayedCheckpoint,
    ) -> Result<Self, StoreError> {
        if !replay.is_compact() || replay.checkpoint() != &certificate.checkpoint {
            return Err(invalid("compact selection differs from its certificate"));
        }
        parent.verify_certificate(&certificate).map_err(invalid)?;
        let manifest = replay.manifest();
        Ok(Self {
            parent,
            record: Record {
                version: 1,
                certificate,
                heads: manifest.heads,
                target: manifest.target,
                beacon: None,
            },
            replay,
        })
    }

    /// Authenticate the exact beacon for an in-memory compact recovery record.
    /// This never updates a store, starts a panel or grants signing authority.
    pub fn attach_compact_beacon(&mut self, beacon: RandomOutput) -> Result<(), StoreError> {
        if !self.replay.is_compact() {
            return Err(invalid(
                "persisted selections must use durable beacon attachment",
            ));
        }
        if bincode::serialized_size(&beacon).map_err(invalid)? > MAX_BEACON_BYTES as u64 {
            return Err(invalid("checkpoint beacon exceeds byte limit"));
        }
        self.replay
            .request()
            .select(&self.parent, &self.record.certificate, &beacon)
            .map_err(invalid)?;
        if let Some(existing) = &self.record.beacon {
            if bincode::serialize(existing).map_err(invalid)?
                != bincode::serialize(&beacon).map_err(invalid)?
            {
                return Err(invalid("compact selection already has a different beacon"));
            }
        } else {
            self.record.beacon = Some(beacon);
        }
        Ok(())
    }

    /// Local proposal construction. Validators still replay a received manifest
    /// independently; this inventory never grants authority to account snapshots.
    pub(crate) fn prepare_next(
        store: &LmdbStore,
        network: NetworkId,
        initial_params: &ProtocolParams,
        cutoff: u64,
    ) -> Result<Option<ReplayedCheckpoint>, StoreError> {
        let chain = chain::load(store, network, initial_params)?;
        Self::prepare_next_in_chain(store, network, initial_params, cutoff, &chain)
    }

    pub(crate) fn prepare_next_in_chain(
        store: &LmdbStore,
        network: NetworkId,
        initial_params: &ProtocolParams,
        cutoff: u64,
        chain: &[chain::CertifiedEvent],
    ) -> Result<Option<ReplayedCheckpoint>, StoreError> {
        if chain.len() as u64 >= MAX_CHECKPOINTS {
            return Err(invalid("checkpoint chain requires incremental replay"));
        }
        let anchor = chain::next_anchor(chain, network, initial_params)?;
        let heads = ConfirmedHistory::heads_at(store, cutoff)?;
        let history = ConfirmedHistory::read(
            store,
            network,
            anchor.genesis_hash(),
            cutoff,
            &heads,
            chain.last().map_or(&[][..], |entry| entry.heads()),
        )?;
        let replay = crate::confirmed_replay::ConfirmedReplay::replay_with_events(
            &history,
            network,
            initial_params,
            chain,
        )
        .map_err(invalid)?;
        for target in replay.selection_targets() {
            let lifecycle = replay.selection_lifecycle(&target).map_err(invalid)?;
            let attempt = replay
                .verification(&target)
                .ok_or_else(|| invalid("missing selection state"))?
                .revote_count;
            if chain
                .iter()
                .filter_map(chain::CertifiedEvent::selection)
                .any(|entry| {
                    entry.replayed().request().target() == &target
                        && entry.replayed().lifecycle() == lifecycle
                        && entry.replayed().request().attempt() >= attempt
                })
            {
                continue;
            }
            return chain::selection_from_history(
                &history,
                &anchor,
                initial_params,
                &target,
                chain,
            )
            .map(Some);
        }
        Ok(None)
    }

    pub(crate) fn stored_tip(
        store: &LmdbStore,
        network: NetworkId,
        initial_params: &ProtocolParams,
    ) -> Result<u64, StoreError> {
        let anchor = ReplayedCheckpoint::genesis_anchor(network, initial_params)?;
        let txn = store.tx_begin_read()?;
        let bytes = record_storage::get_txn(&store.meta_store(), &txn, &format!("{}/head", prefix(&anchor)))?;
        decode_tip(bytes.as_deref())
    }

    #[cfg(test)]
    pub(crate) fn export_record(
        store: &LmdbStore,
        network: NetworkId,
        initial_params: &ProtocolParams,
        height: u64,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        Self::export_record_with_recovery(
            store,
            network,
            initial_params,
            height,
            &mut ExportRecovery::default(),
        )
    }

    /// Export logical record bytes. Receivers must authenticate them through
    /// replay or a matching authenticated snapshot receipt before use.
    pub(crate) fn export_record_with_recovery(
        store: &LmdbStore,
        network: NetworkId,
        initial_params: &ProtocolParams,
        height: u64,
        recovery: &mut ExportRecovery,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let anchor = ReplayedCheckpoint::genesis_anchor(network, initial_params)?;
        let prefix = prefix(&anchor);
        let meta = store.meta_store();
        let txn = store.tx_begin_read()?;
        let head = record_storage::get_txn(&meta, &txn, &format!("{prefix}/head"))?;
        if height == 0 || height > decode_tip(head.as_deref())? {
            return Ok(None);
        }
        let stored = match record_storage::get_txn(&meta, &txn, &format!("{prefix}/{height}"))? {
            Some(bytes) => Some(bytes),
            None => record_storage::get_txn(&meta, &txn, &archived_record_key(&anchor, height))?,
        };
        let bytes = match stored {
            Some(bytes) => {
                if bytes.len() as u64 > MAX_RECORD_BYTES {
                    return Err(invalid("checkpoint export exceeds byte limit"));
                }
                match chain::decode_record(&bytes)? {
                    chain::ParsedRecord::Selection(mut record) => {
                        let epoch = record.certificate.checkpoint.hash();
                        if let Some(beacon) =
                            record_storage::get_txn(&meta, &txn, &archive_selection::beacon_key(epoch))?
                        {
                            use crate::confirmed_replay::snapshot::installed;
                            let base = installed::read_base(
                                |key| record_storage::get_txn(&meta, &txn, key),
                                network,
                                initial_params,
                                store.env().path(),
                            )?;
                            let archive = match base {
                                Some((archive, _)) if height <= archive.anchor().height() => {
                                    recovery.clear();
                                    archive
                                }
                                base => {
                                    // Replay opens its own short read transactions. Never
                                    // nest them under this initial export inventory view.
                                    drop(txn);
                                    let fresh;
                                    let recovered = match base {
                                        Some((archive, _)) => {
                                            recovery.clear();
                                            fresh = archive.recover_stored_epoch(
                                                store,
                                                epoch,
                                                |state, plan, _| {
                                                    crate::checkpoint_sync::witness::read_plan(
                                                        store, state, plan,
                                                    )
                                                },
                                            )?;
                                            &fresh
                                        }
                                        None => recovery.genesis_selection(
                                            store,
                                            network,
                                            initial_params,
                                            &record.certificate,
                                        )?,
                                    };
                                    let batch = store.tx_begin_write()?;
                                    if !recovered.uses_snapshot_archive()
                                        && record_storage::get_batch(&batch, installed::BASE_KEY)?.is_some()
                                    {
                                        return Err(StoreError::StaleRead(
                                            "snapshot base appeared during export".into(),
                                        ));
                                    }
                                    recovered.verify_records_unchanged(&batch)?;
                                    let receipt = recovered
                                        .selections
                                        .find_selection(|selection| {
                                            selection.certificate().checkpoint.hash() == epoch
                                        })
                                        .map_err(invalid)?
                                        .ok_or_else(|| {
                                            invalid("archived beacon has no replayed selection")
                                        })?;
                                    archive_selection::merge_beacon(
                                        &mut record.beacon,
                                        archive_selection::decode_beacon(&beacon)?,
                                    )?;
                                    let encoded = encode(&record)?;
                                    if encoded != encode(&receipt.record)? {
                                        return Err(invalid(
                                            "archived selection changed during export",
                                        ));
                                    }
                                    return Ok(Some(encoded));
                                }
                            };
                            let mut receipt = archive
                                .recover_selection(record.certificate.checkpoint.height)
                                .map_err(invalid)?
                                .filter(|selection| {
                                    selection.certificate().checkpoint.hash() == epoch
                                })
                                .ok_or_else(|| {
                                    invalid("snapshot beacon has no selection receipt")
                                })?;
                            receipt
                                .parent()
                                .verify_certificate(&record.certificate)
                                .map_err(invalid)?;
                            if receipt.heads() != record.heads
                                || receipt.record.target != record.target
                            {
                                return Err(invalid(
                                    "retained selection differs from snapshot receipt",
                                ));
                            }
                            let beacon = archive_selection::decode_beacon(&beacon)?;
                            receipt.attach_compact_beacon(beacon.clone())?;
                            if let Some(existing) = &record.beacon {
                                if bincode::serialize(existing).map_err(invalid)?
                                    != bincode::serialize(&beacon).map_err(invalid)?
                                {
                                    return Err(invalid(
                                        "retained selection has a different beacon",
                                    ));
                                }
                            }
                            record.beacon = Some(beacon);
                            encode(&record)?
                        } else {
                            bytes
                        }
                    }
                    _ => bytes,
                }
            }
            None => {
                use crate::confirmed_replay::snapshot::installed;
                let (archive, _) = installed::read_base(
                    |key| record_storage::get_txn(&meta, &txn, key),
                    network,
                    initial_params,
                    store.env().path(),
                )?
                .ok_or_else(|| invalid("checkpoint export has a missing record"))?;
                if height > archive.anchor().height() {
                    return Err(invalid("checkpoint export has a missing successor"));
                }
                let Some(mut selection) = archive.recover_selection(height).map_err(invalid)?
                else {
                    return Ok(None);
                };
                if let Some(bytes) = record_storage::get_txn(&meta,
                    &txn,
                    &archive_selection::beacon_key(selection.certificate().checkpoint.hash()),
                )? {
                    selection.attach_compact_beacon(archive_selection::decode_beacon(&bytes)?)?;
                }
                encode(&selection.record)?
            }
        };
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err(invalid("checkpoint export exceeds byte limit"));
        }
        Ok(Some(bytes))
    }

    #[cfg(test)]
    pub(crate) fn import_record(
        store: &LmdbStore,
        network: NetworkId,
        initial_params: &ProtocolParams,
        height: u64,
        bytes: &[u8],
    ) -> Result<Self, StoreError> {
        // Reject every non-selection record before an import can persist it.
        decode(bytes)?;
        match chain::import_record(store, network, initial_params, height, bytes)? {
            chain::CertifiedEvent::Selection(value) => Ok(*value),
            chain::CertifiedEvent::Outcome(_)
            | chain::CertifiedEvent::Roster(_)
            | chain::CertifiedEvent::Activation(_)
            | chain::CertifiedEvent::Deadline(_)
            | chain::CertifiedEvent::Governance(_) => {
                Err(invalid("selection import returned an outcome"))
            }
        }
    }

    pub(crate) fn load_chain(
        store: &LmdbStore,
        network: NetworkId,
        initial_params: &ProtocolParams,
    ) -> Result<Vec<Self>, StoreError> {
        chain::load(store, network, initial_params)?
            .into_iter()
            .map(|event| match event {
                chain::CertifiedEvent::Selection(value) => Ok(*value),
                chain::CertifiedEvent::Outcome(_)
                | chain::CertifiedEvent::Roster(_)
                | chain::CertifiedEvent::Activation(_)
                | chain::CertifiedEvent::Deadline(_)
                | chain::CertifiedEvent::Governance(_) => {
                    Err(invalid("outcome-aware checkpoint chain access is required"))
                }
            })
            .collect()
    }

    pub fn load(
        store: &LmdbStore,
        network: NetworkId,
        initial_params: &ProtocolParams,
    ) -> Result<Option<Self>, StoreError> {
        Ok(Self::load_chain(store, network, initial_params)?.pop())
    }

    pub fn prepare(
        store: &LmdbStore,
        network: NetworkId,
        initial_params: &ProtocolParams,
        cutoff: u64,
        heads: &[HistoryHead],
        target: &WalletAddress,
    ) -> Result<ReplayedCheckpoint, StoreError> {
        chain::prepare_selection(store, network, initial_params, cutoff, heads, target)
    }

    /// Restore a pending proposal even when later certificates have since been
    /// imported. Its authority is the exact certified prefix, never today's tip.
    pub fn replay_manifest(
        store: &LmdbStore,
        initial_params: &ProtocolParams,
        manifest: &CheckpointManifest,
    ) -> Result<ReplayedCheckpoint, StoreError> {
        let checkpoint = &manifest.checkpoint;
        if checkpoint.height == 0 || checkpoint.height > MAX_CHECKPOINTS {
            return Err(invalid("pending checkpoint height outside replay bounds"));
        }
        let mut chain = chain::load(store, checkpoint.network, initial_params)?;
        let parent_height = checkpoint.height as usize - 1;
        if parent_height > chain.len() {
            return Err(invalid("pending checkpoint has no certified ancestry"));
        }
        chain.truncate(parent_height);
        let anchor = chain::next_anchor(&chain, checkpoint.network, initial_params)?;
        anchor.validate_successor(checkpoint).map_err(invalid)?;
        let history = ConfirmedHistory::read(
            store,
            checkpoint.network,
            anchor.genesis_hash(),
            checkpoint.cutoff,
            &manifest.heads,
            chain.last().map_or(&[][..], |parent| parent.heads()),
        )?;
        let replay = chain::selection_from_history(
            &history,
            &anchor,
            initial_params,
            &manifest.target,
            &chain,
        )?;
        if replay.manifest() != *manifest {
            return Err(invalid("pending manifest differs from confirmed replay"));
        }
        Ok(replay)
    }

    /// Accept after certificate validation and replay of locally available history,
    /// including authenticated archived blocks. This does not cement that history
    /// into the live ledger or install peer account snapshots.
    pub fn accept(
        store: &LmdbStore,
        network: NetworkId,
        initial_params: &ProtocolParams,
        certificate: CheckpointCertificate,
        heads: Vec<HistoryHead>,
        target: WalletAddress,
    ) -> Result<Self, StoreError> {
        chain::accept_selection(store, network, initial_params, certificate, heads, target)
    }

    /// Only this certificate's exact BLS-verified round can resolve the durable
    /// pending request. It may arrive after later checkpoints have been accepted.
    pub fn attach_beacon(
        &mut self,
        store: &LmdbStore,
        initial_params: &ProtocolParams,
        beacon: RandomOutput,
    ) -> Result<(), StoreError> {
        self.attach_beacon_checked(store, initial_params, beacon, |_| Ok(()))
    }

    pub(crate) fn attach_beacon_checked(
        &mut self,
        store: &LmdbStore,
        initial_params: &ProtocolParams,
        beacon: RandomOutput,
        check: impl FnOnce(&burst_store_lmdb::LmdbWriteTransaction<'_>) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let genesis = ReplayedCheckpoint::genesis_anchor(self.parent.network(), initial_params)?;
        if genesis.genesis_hash() != self.parent.genesis_hash() {
            return Err(invalid("beacon import has different genesis parameters"));
        }
        if bincode::serialized_size(&beacon).map_err(invalid)? > MAX_BEACON_BYTES as u64 {
            return Err(invalid("checkpoint beacon exceeds byte limit"));
        }
        self.replay
            .request()
            .select(&self.parent, &self.record.certificate, &beacon)
            .map_err(invalid)?;
        let mut batch = store.tx_begin_write()?;
        check(&batch)?;
        archive_selection::merge_beacon(&mut self.record.beacon.clone(), beacon.clone())?;
        // Recovery may already know the proof from a certified activation while
        // its original selection receipt still lacks the separate durable copy.
        let mut receipt = self.clone();
        receipt.record.beacon = None;
        if let Some(mut archive) = receipt.archive_selection(&batch, initial_params)? {
            let persisted = archive.beacon.is_some();
            archive_selection::merge_beacon(&mut archive.beacon, beacon.clone())?;
            if persisted {
                drop(batch);
            } else {
                archive_selection::persist_beacon(&mut batch, self, &beacon)?;
                batch.commit()?;
                store.force_sync().map_err(invalid)?;
            }
            self.record.beacon = Some(beacon);
            return Ok(());
        }
        let key = format!(
            "{}/{}",
            prefix(&self.parent),
            self.record.certificate.checkpoint.height
        );
        let (bytes, retired) = match record_storage::get_batch(&batch, &key)? {
            Some(bytes) => (bytes, false),
            None => (
                record_storage::get_batch(&batch, &archived_record_key(
                        &self.parent,
                        self.record.certificate.checkpoint.height,
                    ))?
                    .ok_or_else(|| invalid("checkpoint decision is not persisted"))?,
                true,
            ),
        };
        let mut stored = decode(&bytes)?;
        if stored.certificate != self.record.certificate
            || stored.heads != self.record.heads
            || stored.target != self.record.target
        {
            return Err(invalid(
                "checkpoint decision changed before beacon persistence",
            ));
        }
        if let Some(bytes) = record_storage::get_batch(&batch, &archive_selection::beacon_key(
            self.record.certificate.checkpoint.hash(),
        ))? {
            archive_selection::merge_beacon(
                &mut stored.beacon,
                archive_selection::decode_beacon(&bytes)?,
            )?;
        }
        let persisted = stored.beacon.is_some();
        archive_selection::merge_beacon(&mut stored.beacon, beacon.clone())?;
        if persisted {
            drop(batch);
            self.record = stored;
            return Ok(());
        }
        if retired {
            archive_selection::persist_beacon(&mut batch, self, &beacon)?;
        } else {
            record_storage::put(&mut batch, &key, &encode(&stored)?)?;
        }
        batch.commit()?;
        store.force_sync().map_err(invalid)?;
        self.record = stored;
        Ok(())
    }

    pub fn replayed(&self) -> &ReplayedCheckpoint {
        &self.replay
    }

    pub(crate) fn require_persisted(
        &self,
        batch: &burst_store_lmdb::LmdbWriteTransaction<'_>,
    ) -> Result<(), StoreError> {
        let key = format!(
            "{}/{}",
            prefix(&self.parent),
            self.record.certificate.checkpoint.height
        );
        let bytes = match record_storage::get_batch(&batch, &key)? {
            Some(bytes) => bytes,
            None => record_storage::get_batch(&batch, &archived_record_key(
                    &self.parent,
                    self.record.certificate.checkpoint.height,
                ))?
                .ok_or_else(|| invalid("selection certificate is not persisted"))?,
        };
        let mut stored = decode(&bytes)?;
        if let Some(bytes) = record_storage::get_batch(&batch, &archive_selection::beacon_key(
            self.record.certificate.checkpoint.hash(),
        ))? {
            let beacon = archive_selection::decode_beacon(&bytes)?;
            self.replay
                .request()
                .select(&self.parent, &self.record.certificate, &beacon)
                .map_err(invalid)?;
            archive_selection::merge_beacon(&mut stored.beacon, beacon)?;
        }
        let changed = if stored.certificate != self.record.certificate {
            Some("certificate")
        } else if stored.heads != self.record.heads {
            Some("heads")
        } else if stored.target != self.record.target {
            Some("target")
        } else if self.record.beacon.is_some()
            && bincode::serialize(&stored.beacon).map_err(invalid)?
                != bincode::serialize(&self.record.beacon).map_err(invalid)?
        {
            Some("beacon")
        } else {
            None
        };
        if let Some(field) = changed {
            return Err(invalid(format!(
                "selection {field} changed at height {}; reload the certified chain",
                self.record.certificate.checkpoint.height,
            )));
        }
        Ok(())
    }

    /// Certificate receipt alone cannot publish a committee. Its cut must be
    /// installed, or be an ancestor of the installed certified payment epoch.
    pub(crate) fn live_epoch_ready(
        &self,
        batch: &burst_store_lmdb::LmdbWriteTransaction<'_>,
    ) -> Result<bool, StoreError> {
        Ok(self.live_parameter_hash(batch)?.is_some())
    }

    pub(crate) fn live_parameter_hash(
        &self,
        batch: &burst_store_lmdb::LmdbWriteTransaction<'_>,
    ) -> Result<Option<burst_types::BlockHash>, StoreError> {
        self.live_parameter_hash_after(batch, self.certificate().checkpoint.height)
    }

    fn live_parameter_hash_after(
        &self,
        batch: &burst_store_lmdb::LmdbWriteTransaction<'_>,
        start: u64,
    ) -> Result<Option<burst_types::BlockHash>, StoreError> {
        let prefix = prefix(&self.parent);
        let tip = decode_tip(record_storage::get_batch(&batch, &format!("{prefix}/head"))?.as_deref())?;
        if tip < self.certificate().checkpoint.height {
            return Err(invalid("selection chain tip precedes its certificate"));
        }
        if record_storage::get_batch(&batch, crate::confirmed_replay::installation::INSTALLED_KEY)?
            .is_none()
        {
            return Ok(None);
        }
        let installed =
            crate::payment_authority::PaymentAuthority::read_at(|key| record_storage::get_batch(&batch, key))?
                .ok_or_else(|| invalid("installed payment authority disappeared"))?;
        publication::parameter_hash(
            &self.certificate().checkpoint,
            start,
            tip,
            &installed,
            |height| {
                let bytes = match record_storage::get_batch(&batch, &format!("{prefix}/{height}"))? {
                    Some(bytes) => bytes,
                    None => record_storage::get_batch(&batch, &archived_record_key(&self.parent, height))?
                        .ok_or_else(|| invalid("selection chain record is missing"))?,
                };
                let record = chain::decode_record(&bytes)?;
                Ok(record.certificate().checkpoint.clone())
            },
        )
    }

    pub fn certificate(&self) -> &CheckpointCertificate {
        &self.record.certificate
    }

    pub fn heads(&self) -> &[HistoryHead] {
        &self.record.heads
    }

    pub fn beacon(&self) -> Option<&RandomOutput> {
        self.record.beacon.as_ref()
    }

    pub fn parent(&self) -> &CheckpointAnchor {
        &self.parent
    }
}
