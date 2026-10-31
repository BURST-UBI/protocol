use super::compact_authority::{recover_authority, Authority};
use super::*;
use crate::confirmed_replay::snapshot::installed::RecoveredSnapshot;
use crate::confirmed_replay::snapshot::installed::ReplayCache;
use burst_consensus::checkpoint::CheckpointAnchor;

struct Worker {
    anchor: CheckpointAnchor,
    authority: Authority,
    sender: mpsc::Sender<Incoming>,
    shutdown: Arc<ShutdownController>,
    task: Option<tokio::task::JoinHandle<Result<(), StoreError>>>,
}

impl Worker {
    fn start(
        settings: Settings,
        authority: Authority,
        registry: Arc<RwLock<ConnectionRegistry>>,
        clock: Clock,
    ) -> Self {
        let anchor = authority.base.continuation.anchor().clone();
        let shutdown = Arc::new(ShutdownController::new());
        let (sender, receiver) = mpsc::channel(INBOX_CAPACITY);
        let task = tokio::spawn(super::compact_observer::run_compact_height_with_authority(
            settings,
            super::compact_observer::HeightAuthority::Installed(authority.clone()),
            registry,
            receiver,
            clock,
            shutdown.clone(),
            Duration::from_secs(5),
        ));
        Self {
            anchor,
            authority,
            sender,
            shutdown,
            task: Some(task),
        }
    }

    async fn stop(&mut self) -> Result<(), StoreError> {
        self.shutdown.shutdown();
        if let Some(task) = &mut self.task {
            let result = task.await;
            self.task = None;
            result.map_err(invalid)??;
        }
        Ok(())
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.shutdown.shutdown();
    }
}

pub(crate) struct CompactRuntime {
    settings: Settings,
    registry: Arc<RwLock<ConnectionRegistry>>,
    clock: Clock,
    worker: Worker,
    previous: Option<(Context, Transport)>,
    uses_snapshot_archive: bool,
}

impl CompactRuntime {
    pub(crate) async fn new(
        settings: Settings,
        registry: Arc<RwLock<ConnectionRegistry>>,
        clock: Clock,
        cached: Option<ReplayCache>,
    ) -> Result<Self, StoreError> {
        let authority = recover_authority(&settings, registry.clone(), None, cached)
            .await?
            .ok_or_else(|| invalid("compact checkpoint runtime has no installed base"))?;
        let previous = decision_relay(authority.base.clone()).await?;
        let uses_snapshot_archive = authority.base.uses_snapshot_archive();
        let worker = Worker::start(settings.clone(), authority, registry.clone(), clock.clone());
        Ok(Self {
            settings,
            registry,
            clock,
            worker,
            previous,
            uses_snapshot_archive,
        })
    }

    /// Rotate to authenticated installed authority. A snapshot-source change
    /// stops the old worker before recovery; otherwise it runs until recovery succeeds.
    pub(crate) async fn refresh(&mut self) -> Result<(), StoreError> {
        let store = self.settings.store.clone();
        let (epoch, uses_snapshot_archive) = tokio::task::spawn_blocking(move || {
            let transaction = store.tx_begin_read()?;
            let meta = store.meta_store();
            let epoch = meta
                .get_meta_txn(&transaction, burst_store::meta::PAYMENT_EPOCH_KEY)?
                .ok_or_else(|| invalid("installed payment epoch disappeared"))?;
            let epoch = burst_types::BlockHash::new(epoch.as_slice().try_into().map_err(invalid)?);
            let uses_snapshot_archive = meta
                .get_meta_txn(
                    &transaction,
                    crate::confirmed_replay::snapshot::installed::BASE_KEY,
                )?
                .is_some();
            Ok::<_, StoreError>((epoch, uses_snapshot_archive))
        })
        .await
        .map_err(invalid)??;
        if uses_snapshot_archive != self.uses_snapshot_archive {
            // A source handoff must not leave the old signer running if the
            // replacement fails authentication or disappears during recovery.
            self.worker.stop().await?;
        }
        if self.uses_snapshot_archive && !uses_snapshot_archive {
            return Err(invalid("compact snapshot base disappeared"));
        }
        let same_epoch = epoch == self.worker.anchor.hash();
        if same_epoch && uses_snapshot_archive == self.uses_snapshot_archive {
            return Ok(());
        }
        let cached = self
            .worker
            .authority
            .replay_copy(&self.settings.store)
            .await?;
        let authority = recover_authority(
            &self.settings,
            self.registry.clone(),
            (!same_epoch).then(|| self.worker.anchor.clone()),
            Some(cached),
        )
        .await?
        .ok_or_else(|| invalid("compact authority disappeared"))?;
        if same_epoch && authority.base.continuation.anchor().hash() != epoch {
            return Err(invalid(
                "snapshot source handoff changed the installed epoch",
            ));
        }
        let previous = decision_relay(authority.base.clone())
            .await?
            .ok_or_else(|| {
                invalid("rotated compact authority has no replayed decision to relay")
            })?;
        self.worker.stop().await?;
        self.uses_snapshot_archive = authority.base.uses_snapshot_archive();
        self.worker = Worker::start(
            self.settings.clone(),
            authority,
            self.registry.clone(),
            self.clock.clone(),
        );
        self.previous = Some(previous);
        Ok(())
    }

    pub(crate) async fn stop(&mut self) -> Result<(), StoreError> {
        self.worker.stop().await
    }

    #[cfg(test)]
    pub(crate) fn anchor(&self) -> &CheckpointAnchor {
        &self.worker.anchor
    }

    #[cfg(test)]
    pub(crate) fn previous_context(&self) -> Option<Context> {
        self.previous.as_ref().map(|(context, _)| *context)
    }

    async fn dispatch(&mut self, incoming: Option<Incoming>) -> Result<(), StoreError> {
        let mut relay_input = None;
        if let Some(incoming) = incoming {
            if self
                .previous
                .as_ref()
                .is_some_and(|(context, _)| *context == incoming.packet.context)
            {
                if matches!(
                    incoming.packet.body,
                    crate::checkpoint_transport::Body::Request { .. }
                ) {
                    relay_input = Some(incoming);
                }
            } else {
                match self.worker.sender.try_send(incoming) {
                    Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => (),
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        return Err(invalid("compact checkpoint inbox closed"))
                    }
                }
            }
        }
        let peers = self
            .registry
            .read()
            .await
            .peer_ids()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let mut previous = self.previous.take();
        let (returned, sends) = tokio::task::spawn_blocking(move || {
            let mut sends = Vec::new();
            if let Some((_, transport)) = &mut previous {
                if let Some(incoming) = relay_input {
                    match transport.handle(&incoming.peer, incoming.packet, Instant::now()) {
                        Ok((reply, _)) => sends.extend(reply),
                        Err(error) => {
                            tracing::debug!(%error, "compact previous decision request rejected")
                        }
                    }
                }
                sends.extend(transport.poll(&peers, Instant::now()));
            }
            (previous, sends)
        })
        .await
        .map_err(invalid)?;
        self.previous = returned;
        let registry = self.registry.read().await;
        for send in sends {
            registry.send(
                &send.peer,
                WireMessage::CheckpointAgreement(send.packet)
                    .encode()
                    .map_err(invalid)?,
            );
        }
        Ok(())
    }
}

/// Coordinate compact checkpoint workers across installed economic epochs.
/// Economic installation remains owned by the node's separate publication path.
pub async fn run_compact_node(
    settings: Settings,
    registry: Arc<RwLock<ConnectionRegistry>>,
    inbox: mpsc::Receiver<Incoming>,
    clock: Clock,
    shutdown: Arc<ShutdownController>,
) -> Result<(), StoreError> {
    run_compact_node_with_initial(settings, registry, inbox, clock, shutdown, None, None).await
}

pub(super) async fn run_compact_node_with_initial(
    settings: Settings,
    registry: Arc<RwLock<ConnectionRegistry>>,
    mut inbox: mpsc::Receiver<Incoming>,
    clock: Clock,
    shutdown: Arc<ShutdownController>,
    mut initial: Option<Incoming>,
    cached: Option<ReplayCache>,
) -> Result<(), StoreError> {
    let mut stop = shutdown.subscribe();
    let mut runtime = tokio::select! {
        biased;
        _ = stop.recv() => return Ok(()),
        result = CompactRuntime::new(settings, registry, clock, cached) => result?,
    };
    let mut timer = tokio::time::interval(Duration::from_millis(250));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut refresh_at = Instant::now();
    let result = async {
        loop {
            let incoming =
                match input::next_with_initial(&mut stop, &mut inbox, &mut timer, &mut initial)
                    .await
                {
                    Ok(incoming) => incoming,
                    Err(()) => return Ok(()),
                };
            if runtime
                .worker
                .task
                .as_ref()
                .is_none_or(|task| task.is_finished())
            {
                runtime.worker.stop().await?;
                return Err(invalid("compact height worker stopped unexpectedly"));
            }
            runtime.dispatch(incoming).await?;
            if Instant::now() >= refresh_at {
                refresh_at = Instant::now() + Duration::from_secs(1);
                let result = tokio::select! {
                    biased;
                    _ = stop.recv() => return Ok(()),
                    result = runtime.refresh() => result,
                };
                if let Err(error) = result {
                    tracing::debug!(%error, "compact runtime awaits checked installed authority");
                }
            }
        }
    }
    .await;
    let stopped = runtime.stop().await;
    result.and(stopped)
}

fn invalid(error: impl ToString) -> StoreError {
    StoreError::Corruption(error.to_string())
}

async fn decision_relay(
    base: Arc<RecoveredSnapshot>,
) -> Result<Option<(Context, Transport)>, StoreError> {
    tokio::task::spawn_blocking(move || base.replayed_decision_relay(Instant::now()))
        .await
        .map_err(invalid)?
}
