use super::*;
use crate::checkpoint_sync::TRANSFER_TIMEOUT;
use crate::{
    connection_registry::ConnectionRegistry, shutdown::ShutdownController,
    wire_message::WireMessage,
};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

fn made_progress(
    before: usize,
    transfer: &Transfer,
    reply: &Result<Option<Incoming>, StoreError>,
) -> bool {
    reply.is_ok()
        && transfer
            .download
            .as_ref()
            .is_some_and(|download| download.received > before)
}

/// Stage an explicitly requested archive while ordinary admission is closed.
/// The caller still owns installation, force-sync and installed-state recovery.
pub(crate) async fn fetch_before_admission(
    fetch: Fetch,
    max_archive_bytes: u64,
    base: CheckpointAnchor,
    registry: Arc<RwLock<ConnectionRegistry>>,
    shutdown: Arc<ShutdownController>,
    #[cfg(test)] probe: Option<fn(&str)>,
) -> Result<(), StoreError> {
    if !registry.read().await.is_recovering() {
        return Err(invalid("snapshot fetch requires startup recovery"));
    }
    let mut transfer = tokio::task::spawn_blocking(move || {
        let staging = parent(&fetch.output).to_path_buf();
        Transfer::new(
            Config {
                max_archive_bytes,
                serve: None,
                fetch: Some(fetch),
            },
            base,
            &staging,
        )
    })
    .await
    .map_err(invalid)??;
    #[cfg(test)]
    {
        transfer.publication_probe = probe;
    }
    if transfer.fetch.is_none() {
        return Ok(());
    }
    let (sender, mut inbox) = mpsc::channel(crate::checkpoint_sync::INBOX_CAPACITY);
    if !registry.write().await.set_recovery_snapshot_inbox(sender) {
        return Err(invalid(
            "snapshot fetch requires exclusive startup recovery",
        ));
    }
    let mut stop = shutdown.subscribe();
    let deadline = tokio::time::sleep(TRANSFER_TIMEOUT);
    tokio::pin!(deadline);
    let mut timer = tokio::time::interval(RETRY);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut scheduler = crate::checkpoint_sync::input::Scheduler::default();
    while transfer.fetch.is_some() {
        let incoming = tokio::select! {
            biased;
            _ = &mut deadline => return Err(invalid("startup snapshot download timed out")),
            result = scheduler.next(&mut stop, &mut inbox, &mut timer) =>
                result.map_err(|()| invalid("startup snapshot download cancelled"))?,
        };
        let peers = registry
            .read()
            .await
            .peer_ids()
            .into_iter()
            .cloned()
            .collect();
        let (returned, reply) = tokio::task::spawn_blocking(move || {
            let before = transfer
                .download
                .as_ref()
                .map_or(0, |download| download.received);
            let reply = match incoming {
                Some(packet) => transfer.handle(packet, Instant::now()),
                None => Ok(transfer.poll(peers, Instant::now())),
            };
            let progressed = made_progress(before, &transfer, &reply);
            (transfer, (reply, progressed))
        })
        .await
        .map_err(invalid)?;
        transfer = returned;
        let (reply, progressed) = reply;
        if progressed {
            deadline
                .as_mut()
                .reset(tokio::time::Instant::now() + TRANSFER_TIMEOUT);
        }
        match reply {
            Ok(Some(reply)) => {
                let bytes = WireMessage::CheckpointSync(reply.message)
                    .encode()
                    .map_err(invalid)?;
                registry.read().await.send(&reply.peer, bytes);
            }
            Ok(None) => (),
            Err(error) => tracing::debug!(%error, "startup snapshot transfer deferred"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_deadline_only_renews_for_successful_new_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let genesis = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
            burst_types::NetworkId::Dev,
            &burst_types::ProtocolParams::default(),
        )
        .unwrap();
        let mut transfer = Transfer::new(
            Config {
                max_archive_bytes: Config::default().max_archive_bytes,
                serve: None,
                fetch: Some(Fetch {
                    epoch: Some(BlockHash::new([1; 32]).to_string()),
                    minimum_height: 1,
                    output: directory.path().join("snapshot"),
                }),
            },
            genesis,
            directory.path(),
        )
        .unwrap();
        let now = Instant::now();
        transfer.poll(vec!["peer".into()], now).unwrap();
        assert!(!made_progress(0, &transfer, &Ok(None)));
        let retry = transfer.poll(vec!["peer".into()], now + RETRY);
        assert!(!made_progress(0, &transfer, &Ok(retry)));
        transfer.download.as_mut().unwrap().received = CHUNK_BYTES;
        assert!(made_progress(0, &transfer, &Ok(None)));
        assert!(!made_progress(CHUNK_BYTES, &transfer, &Ok(None)));
        assert!(!made_progress(0, &transfer, &Err(invalid("invalid chunk"))));
        transfer.download = None;
        assert!(!made_progress(CHUNK_BYTES, &transfer, &Ok(None)));
    }

    #[tokio::test]
    async fn startup_snapshot_cancellation_does_not_publish_a_file() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("download.snapshot");
        let genesis = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
            burst_types::NetworkId::Dev,
            &burst_types::ProtocolParams::default(),
        )
        .unwrap();
        let registry = Arc::new(RwLock::new(ConnectionRegistry::for_recovery()));
        let shutdown = Arc::new(ShutdownController::new());
        shutdown.shutdown();
        let error = fetch_before_admission(
            Fetch {
                epoch: Some(BlockHash::new([1; 32]).to_string()),
                minimum_height: 1,
                output: output.clone(),
            },
            Config::default().max_archive_bytes,
            genesis,
            registry.clone(),
            shutdown,
            None,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("cancelled"), "{error}");
        assert!(!output.exists());
        assert!(registry.read().await.is_recovering());
        let (sender, _inbox) = mpsc::channel(1);
        assert!(registry.write().await.set_recovery_snapshot_inbox(sender));
    }
}
