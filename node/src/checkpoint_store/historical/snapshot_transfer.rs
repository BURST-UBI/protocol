//! Record transfer through authenticated snapshot or genesis replay.
//! Peer history is not persisted here; replay obtains cursor-checked witnesses.

use super::*;
use crate::confirmed_replay::snapshot::SelectionSource;
use crate::{confirmed_replay::snapshot::installed, connection_registry::ConnectionRegistry};
use std::sync::Arc;
use tokio::sync::RwLock;

mod suffix;

pub(super) fn encode(record: Vec<u8>) -> Result<Vec<u8>, StoreError> {
    let transfer = Transfer {
        version: 3,
        record,
        blocks: Vec::new(),
        contents: Vec::new(),
    };
    if bincode::serialized_size(&transfer).map_err(invalid)? > MAX_TRANSFER_BYTES {
        return Err(invalid("checkpoint history transfer exceeds byte limit"));
    }
    bincode::serialize(&transfer).map_err(invalid)
}

#[cfg(test)]
pub(crate) async fn import(
    store: Arc<LmdbStore>,
    network: NetworkId,
    params: ProtocolParams,
    height: u64,
    bytes: Vec<u8>,
    registry: Arc<RwLock<ConnectionRegistry>>,
) -> Result<(), StoreError> {
    Session::default()
        .import(store, network, params, height, bytes, registry)
        .await
        .map(|_| ())
}

#[derive(Default)]
pub(crate) struct Session {
    cached: Option<installed::ReplayCache>,
}

impl Session {
    #[cfg(test)]
    pub(crate) fn recovery_anchor(&self) -> Option<BlockHash> {
        self.cached
            .as_ref()
            .map(|cache| cache.recovered.continuation.anchor().hash())
    }

    pub(crate) fn from_recovery(cached: Option<installed::ReplayCache>) -> Self {
        Self { cached }
    }

    pub(crate) async fn import(
        &mut self,
        store: Arc<LmdbStore>,
        network: NetworkId,
        params: ProtocolParams,
        height: u64,
        bytes: Vec<u8>,
        registry: Arc<RwLock<ConnectionRegistry>>,
    ) -> Result<Option<BlockHash>, StoreError> {
        if !cfg!(test) && !burst_verification::checkpoint_selection_enabled(network) {
            return Err(invalid(
                "compact transfer import is behind the release gate",
            ));
        }
        let source = store.clone();
        let initial_params = params.clone();
        let pending = tokio::task::spawn_blocking(move || {
            let params = initial_params;
            if bytes.len() as u64 > MAX_TRANSFER_BYTES {
                return Err(invalid("checkpoint history transfer exceeds byte limit"));
            }
            let transfer: Transfer = bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .reject_trailing_bytes()
                .with_limit(MAX_TRANSFER_BYTES)
                .deserialize(&bytes)
                .map_err(invalid)?;
            if !matches!(transfer.version, 2 | 3)
                || (transfer.version == 3
                    && (!transfer.blocks.is_empty() || !transfer.contents.is_empty()))
            {
                return Err(invalid("invalid compact checkpoint transfer format"));
            }
            let record = chain::decode_record(&transfer.record)?;
            let certificate = record.certificate().clone();
            if certificate.checkpoint.network != network || certificate.checkpoint.height != height
            {
                return Err(invalid(
                    "checkpoint transfer has a different network or height",
                ));
            }
            let manifest = record.manifest();
            let beacon = match record {
                chain::ParsedRecord::Selection(record) => record.beacon,
                _ => None,
            };
            let read = source.tx_begin_read()?;
            let loaded = installed::read_base(
                |key| source.meta_store().get_meta_txn(&read, key),
                network,
                &params,
                source.env().path(),
            )?;
            let (base, commitment) = loaded.map_or((None, None), |(base, commitment)| {
                (Some(base), Some(commitment))
            });
            drop(read);
            if let Some(base) = base
                .as_ref()
                .filter(|base| height <= base.anchor().height())
            {
                let mut receipt = base
                    .recover_selection(height)
                    .map_err(invalid)?
                    .filter(|selection| {
                        selection.certificate().checkpoint == certificate.checkpoint
                    })
                    .ok_or_else(|| {
                        invalid("checkpoint transfer has no authenticated selection receipt")
                    })?;
                receipt
                    .parent()
                    .verify_certificate(&certificate)
                    .map_err(invalid)?;
                if crate::checkpoint_agreement::ReplayManifest::Selection(
                    receipt.replayed().manifest(),
                ) != manifest
                {
                    return Err(invalid(
                        "checkpoint transfer differs from snapshot selection receipt",
                    ));
                }
                if let Some(beacon) = beacon {
                    receipt.attach_beacon(&source, &params, beacon)?;
                }
                return Ok(None);
            }
            let known = height <= CertifiedCheckpoint::stored_tip(&source, network, &params)?;
            let tail = if transfer.version == 2 {
                Some(
                    crate::confirmed_replay::snapshot::tail::TransferredTail::from_blocks(
                        transfer.blocks,
                        transfer.contents,
                        source.env().path(),
                    )?,
                )
            } else {
                None
            };
            Ok::<_, StoreError>(Some((
                base,
                commitment,
                certificate,
                manifest,
                beacon,
                known,
                tail,
            )))
        })
        .await
        .map_err(invalid)??;
        let Some((base, commitment, certificate, manifest, beacon, known, tail)) = pending else {
            return Ok(None);
        };
        let replay_started = std::time::Instant::now();
        tracing::debug!(
            height,
            known,
            has_beacon = beacon.is_some(),
            cached_height =
                self.cached
                    .as_ref()
                    .map(|cache| cache.recovered.continuation.anchor().height()),
            "checkpoint import selected recovery input"
        );
        if known
            && matches!(
                &manifest,
                crate::checkpoint_agreement::ReplayManifest::Selection(_)
            )
        {
            if let Some(cached) = self.cached.take() {
                // Import keeps the replay parent until the accepted successor
                // is needed. Advance it once before refreshing an older receipt.
                let cached = installed::recover_current_tip_with_cache(
                    store.clone(),
                    network,
                    params.clone(),
                    registry.clone(),
                    Some(cached),
                )
                .await?;
                let source = store.clone();
                let checked_certificate = certificate.clone();
                let checked_manifest = manifest.clone();
                let checked_beacon = beacon.clone();
                let checked_params = params.clone();
                let (cached, checked) = tokio::task::spawn_blocking(move || {
                    let checked = cached.refresh_known_selection(
                        &source,
                        cached.base,
                        &checked_certificate,
                        &checked_manifest,
                        checked_beacon.as_ref(),
                        &checked_params,
                    );
                    (cached, checked)
                })
                .await
                .map_err(invalid)?;
                self.cached = Some(cached);
                if checked? {
                    tracing::debug!(
                        height,
                        elapsed_ms = replay_started.elapsed().as_millis(),
                        "checkpoint refresh reused authenticated receipt"
                    );
                    // Beacon persistence uses the checked receipt. Subsequent
                    // reuse still checks the durable records before advancing.
                    return Ok(None);
                }
            }
        }
        let target = if known {
            certificate.checkpoint.hash()
        } else {
            certificate.checkpoint.parent
        };
        if !known {
            let handoff = registry.read().await.recovery_projection.clone();
            let source = store.clone();
            let initial = params.params_hash();
            let installed = tokio::task::spawn_blocking(move || {
                handoff.replay(&source, network, initial, commitment, target)
            })
            .await
            .map_err(invalid)??;
            if let Some(installed) = installed {
                // Publication supplied this exact parent. The normal recover
                // path below still checks its durable observations and base.
                self.cached = Some(installed);
                tracing::debug!(height, %target, "checkpoint import reused installed replay parent");
            }
        }
        let reusable = self.cached.as_ref().is_some_and(|cached| {
            !known && cached.recovered.continuation.anchor().height() < height
        });
        let resumed = if reusable {
            self.cached
                .take()
                .unwrap()
                .recover(commitment, store.clone(), registry.clone(), target)
                .await?
        } else {
            None
        };
        let recovered = match (resumed, base) {
            (Some(recovered), _) => recovered,
            (None, Some(base)) => {
                base.recover_stored_epoch_from_peers(store.clone(), registry.clone(), target)
                    .await?
            }
            (None, None) => {
                installed::RecoveredSnapshot::recover_genesis_from_peers(
                    store.clone(),
                    network,
                    params.clone(),
                    registry.clone(),
                    Some(target),
                )
                .await?
            }
        };
        tracing::debug!(
            height,
            known,
            elapsed_ms = replay_started.elapsed().as_millis(),
            "checkpoint import recovered replay input"
        );
        if known {
            recovered.verify_replayed_decision(&certificate, &manifest)?;
            return tokio::task::spawn_blocking(move || {
                if let Some(beacon) = beacon {
                    let mut receipt = recovered
                        .selections
                        .find_selection(|selection| {
                            selection.certificate().checkpoint == certificate.checkpoint
                        })
                        .map_err(invalid)?
                        .ok_or_else(|| invalid("replayed checkpoint has no selection receipt"))?
                        .into_owned();
                    receipt.attach_beacon_checked(&store, &params, beacon, |batch| {
                        recovered.verify_records_unchanged(batch)
                    })
                } else {
                    let batch = store.tx_begin_write()?;
                    recovered.verify_records_unchanged(&batch)
                }
            })
            .await
            .map_err(invalid)?
            .map(|()| None);
        }
        let tail = match tail {
            Some(tail) => tail,
            None => suffix::fetch(&store, &registry, &recovered, &certificate, &manifest).await?,
        };
        let parent = recovered.continuation.anchor().hash();
        let recovered = Arc::new(recovered);
        let accepted = recovered
            .clone()
            .accept_transferred_decision(store, registry, certificate, manifest, beacon, Some(tail))
            .await;
        self.cached = Some(installed::ReplayCache {
            base: commitment,
            recovered: Arc::try_unwrap(recovered)
                .map_err(|_| invalid("checkpoint import retained its replay base"))?,
        });
        accepted?;
        Ok(Some(parent))
    }
}
