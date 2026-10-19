//! Operator-triggered exports from full history or an authenticated installed
//! base plus replayed successors. Unsigned peer state cannot be attested.

use super::*;
use bincode::Options;
use burst_store::meta::MetaStore;
use burst_store_lmdb::LmdbStore;
use std::io::{Read, Write};

const MAX_ATTESTATION_BYTES: u64 = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotAttestation {
    pub manifest: SnapshotManifest,
    pub signer: CheckpointSignature,
}

impl SnapshotAttestation {
    pub fn read(reader: &mut impl Read) -> Result<Self, ReplayError> {
        let mut bytes = Vec::new();
        reader
            .take(MAX_ATTESTATION_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| invalid(e.to_string()))?;
        if bytes.len() as u64 > MAX_ATTESTATION_BYTES {
            return Err(invalid("snapshot attestation exceeds byte limit"));
        }
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .with_limit(MAX_ATTESTATION_BYTES)
            .deserialize(&bytes)
            .map_err(|e| invalid(e.to_string()))
    }

    pub fn write(&self, writer: &mut impl Write) -> Result<(), ReplayError> {
        if bincode::serialized_size(self).map_err(|e| invalid(e.to_string()))?
            > MAX_ATTESTATION_BYTES
        {
            return Err(invalid("snapshot attestation exceeds byte limit"));
        }
        bincode::serialize_into(writer, self).map_err(|e| invalid(e.to_string()))
    }
}

pub struct SnapshotExport {
    initial_params: ProtocolParams,
    base: CheckpointAnchor,
    anchor: CheckpointAnchor,
    authority: super::installed::authority_history::AuthorityHistory,
    beacons: Vec<(BlockHash, burst_vrf::RandomOutput)>,
    prepared: PreparedSnapshot<body::StagedBody>,
}

impl SnapshotExport {
    /// Require an existing initialized ledger. Export must not create genesis
    /// or infer its original parameters from today's governed state.
    pub fn from_config(
        config: &crate::NodeConfig,
        epoch: BlockHash,
    ) -> Result<(Self, LmdbStore), ReplayError> {
        if !config.data_dir.join("data.mdb").is_file() {
            return Err(invalid("snapshot export requires an existing ledger"));
        }
        let store = LmdbStore::open(
            &config.data_dir,
            crate::node::MAX_DBS,
            crate::node::DEFAULT_MAP_SIZE,
        )
        .map_err(|e| invalid(e.to_string()))?;
        let meta = store.meta_store();
        if meta
            .get_meta("ledger_network")
            .map_err(|e| invalid(e.to_string()))?
            != config.network.as_str().as_bytes()
        {
            return Err(invalid("snapshot ledger belongs to another network"));
        }
        // Refuse legacy unpinned configuration here rather than initialize it.
        meta.get_meta("genesis_params/v1")
            .map_err(|e| invalid(e.to_string()))?;
        let params =
            crate::network_identity::genesis_params(&store, config.network, &config.params)
                .map_err(|e| invalid(e.to_string()))?;
        let export = Self::prepare(&store, config.network, &params, epoch)?;
        Ok((export, store))
    }

    pub fn attest_config(
        config: &crate::NodeConfig,
        epoch: BlockHash,
    ) -> Result<SnapshotAttestation, ReplayError> {
        let key = config
            .representative_key()
            .map_err(|e| invalid(e.to_string()))?
            .ok_or_else(|| invalid("snapshot attestation requires a representative key file"))?;
        let (export, store) = Self::from_config(config, epoch)?;
        export.attest(&store, &key.private)
    }

    /// Pin the epoch explicitly so independent operators can attest the same
    /// state even after their nodes have advanced to a newer checkpoint.
    pub fn prepare(
        store: &LmdbStore,
        network: NetworkId,
        initial_params: &ProtocolParams,
        epoch: BlockHash,
    ) -> Result<Self, ReplayError> {
        let installed = {
            let txn = store.tx_begin_read().map_err(|e| invalid(e.to_string()))?;
            super::installed::read_base(
                |key| store.meta_store().get_meta_txn(&txn, key),
                network,
                initial_params,
                store.env().path(),
            )
            .map_err(|e| invalid(e.to_string()))?
        };
        if let Some((snapshot, commitment)) = installed {
            // Older named epochs still require their historical economic state.
            // The installed body can only seed its own epoch and successors.
            let mut historical = false;
            for step in snapshot.authority.steps() {
                if step?.certificate.checkpoint.hash() == epoch {
                    historical = true;
                }
            }
            if epoch == snapshot.anchor().hash() || !historical {
                return Self::prepare_installed(store, initial_params, epoch, snapshot, commitment);
            }
        }
        let base =
            crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(network, initial_params)
                .map_err(|e| invalid(e.to_string()))?;
        let recovered = super::installed::RecoveredSnapshot::recover_genesis_locally(
            store,
            network,
            initial_params,
            epoch,
        )
        .map_err(|error| invalid(error.to_string()))?;
        let anchor = recovered.continuation.anchor().clone();
        let prepared = recovered
            .continuation
            .state()
            .prepare_snapshot_in(&anchor, store.env().path())?;
        let beacons = recovered
            .selections
            .read_all()
            .filter_map(|selection| match selection {
                Err(error) => Some(Err(error)),
                Ok(selection) => selection
                    .beacon()
                    .map(|beacon| Ok((selection.certificate().checkpoint.hash(), beacon.clone()))),
            })
            .collect::<Result<Vec<_>, ReplayError>>()?;
        let batch = store
            .tx_begin_write()
            .map_err(|error| invalid(error.to_string()))?;
        recovered
            .verify_records_unchanged(&batch)
            .map_err(|error| invalid(error.to_string()))?;
        drop(batch);
        let authority = recovered.into_authority();
        Ok(Self {
            initial_params: initial_params.clone(),
            base,
            anchor,
            authority,
            beacons,
            prepared,
        })
    }

    fn prepare_installed(
        store: &LmdbStore,
        initial_params: &ProtocolParams,
        epoch: BlockHash,
        snapshot: super::archive::AuthenticatedSnapshot,
        commitment: super::installed::BaseCommitment,
    ) -> Result<Self, ReplayError> {
        let base = snapshot.authority_base.clone();
        let recovered = snapshot
            .recover_export(store, epoch)
            .map_err(|e| invalid(e.to_string()))?;
        let anchor = recovered.continuation.anchor().clone();
        let prepared = recovered
            .continuation
            .state()
            .prepare_snapshot_in(&anchor, store.env().path())?;
        let beacons = recovered
            .selections
            .read_all()
            .filter_map(|selection| match selection {
                Err(error) => Some(Err(error)),
                Ok(selection) => selection
                    .beacon()
                    .map(|beacon| Ok((selection.certificate().checkpoint.hash(), beacon.clone()))),
            })
            .collect::<Result<Vec<_>, ReplayError>>()?;
        // A retrieval/replay race must not produce an export from mixed bases.
        let batch = store.tx_begin_write().map_err(|e| invalid(e.to_string()))?;
        commitment
            .verify(|key| batch.get_meta(key))
            .map_err(|e| invalid(e.to_string()))?;
        recovered
            .verify_records_unchanged(&batch)
            .map_err(|e| invalid(e.to_string()))?;
        drop(batch);
        let authority = recovered.into_authority();
        Ok(Self {
            initial_params: initial_params.clone(),
            base,
            anchor,
            authority,
            beacons,
            prepared,
        })
    }

    pub fn manifest(&self) -> &SnapshotManifest {
        self.prepared.manifest()
    }

    pub fn genesis_params(&self) -> &ProtocolParams {
        &self.initial_params
    }

    /// Commit the signer's exact choice and force it to disk before returning a
    /// signature. Repeat export is idempotent; conflicting state fails closed.
    pub fn attest(
        &self,
        store: &LmdbStore,
        key: &PrivateKey,
    ) -> Result<SnapshotAttestation, ReplayError> {
        let signer = self.prepared.sign(&self.anchor, key)?;
        let name = format!(
            "snapshot_attestation/v1/{}/{}/{}/{}",
            self.anchor.network().as_str(),
            self.anchor.genesis_hash(),
            self.anchor.hash(),
            signer.representative
        );
        let bytes = bincode::serialize(self.manifest()).map_err(|e| invalid(e.to_string()))?;
        let mut batch = store.tx_begin_write().map_err(|e| invalid(e.to_string()))?;
        if let Some(previous) = batch.get_meta(&name).map_err(|e| invalid(e.to_string()))? {
            if previous != bytes {
                return Err(invalid(
                    "snapshot signer already attested different state for this epoch",
                ));
            }
        } else {
            batch
                .put_meta(&name, &bytes)
                .map_err(|e| invalid(e.to_string()))?;
        }
        batch.commit().map_err(|e| invalid(e.to_string()))?;
        store.force_sync().map_err(|e| invalid(e.to_string()))?;
        Ok(SnapshotAttestation {
            manifest: self.manifest().clone(),
            signer,
        })
    }

    pub fn write(
        &self,
        attestations: &[SnapshotAttestation],
        writer: &mut impl Write,
    ) -> Result<(), ReplayError> {
        if attestations.len() > self.anchor.roster().humans().len() {
            return Err(invalid("too many snapshot attestations"));
        }
        let mut signatures = Vec::with_capacity(attestations.len());
        for attestation in attestations {
            if &attestation.manifest != self.manifest() {
                return Err(invalid(
                    "snapshot attestation names different state or epoch",
                ));
            }
            signatures.push(attestation.signer.clone());
        }
        signatures.sort_by(|a, b| a.representative.as_str().cmp(b.representative.as_str()));
        let certificate = SnapshotCertificate {
            manifest: self.manifest().clone(),
            signatures,
        };
        // Archive validation rejects duplicate/unknown signers and a minority,
        // before writing even the magic bytes to the destination.
        self.prepared.write_archive_with_beacons(
            &self.base,
            &self.authority,
            &certificate,
            &self.beacons,
            writer,
        )
    }
}
