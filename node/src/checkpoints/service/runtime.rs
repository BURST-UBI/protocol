//! Height rotation and observer admission for the running node. Each new anchor
//! is derived from the stored, replay-validated certificate chain.

use super::*;
use crate::{
    checkpoint_agreement::{self, ReplayedCandidate},
    checkpoint_store::{
        chain::{self, CertifiedEvent},
        CertifiedCheckpoint,
    },
};
use burst_consensus::checkpoint::CheckpointAnchor;
use burst_crypto::{derive_address, public_from_private};
use burst_store::meta::MetaStore;
use burst_store_lmdb::LmdbStore;
use burst_types::{NetworkId, PrivateKey, ProtocolParams};

#[derive(Clone)]
pub struct Settings {
    pub store: Arc<LmdbStore>,
    pub network: NetworkId,
    pub initial_params: ProtocolParams,
    /// None means observer, even if some local wallet happens to have weight.
    pub key: Option<Arc<PrivateKey>>,
}

pub(crate) struct Runtime {
    settings: Settings,
    anchor: CheckpointAnchor,
    idle: Transport,
    previous: Option<(Context, Transport)>,
    session: Option<Session>,
    recovered: bool,
    refresh_at: Instant,
    candidate_at: Instant,
    // Certificate persistence alone cannot authorize the next signer context.
    awaiting_payment_epoch: bool,
}

fn invalid(error: impl ToString) -> StoreError {
    StoreError::Corruption(error.to_string())
}

impl Runtime {
    pub(crate) fn new(settings: Settings) -> Result<Self, StoreError> {
        let chain = chain::load(&settings.store, settings.network, &settings.initial_params)?;
        let anchor = chain::next_anchor(&chain, settings.network, &settings.initial_params)?;
        let awaiting_payment_epoch = settings.store.meta_store().payment_epoch()? != anchor.hash();
        let now = Instant::now();
        let idle = Transport::new(crate::checkpoint_transport::context_from_anchor(&anchor)?, now);
        let previous = Self::last_decision(chain.last(), now)?;
        Ok(Self {
            settings,
            anchor,
            idle,
            previous,
            session: None,
            recovered: false,
            refresh_at: now,
            candidate_at: now,
            awaiting_payment_epoch,
        })
    }

    fn signer(&self) -> Option<Arc<PrivateKey>> {
        if self.awaiting_payment_epoch {
            return None;
        }
        let key = self.settings.key.as_ref()?;
        let representative = derive_address(&public_from_private(key));
        (self.anchor.roster().weight(&representative) > 0).then(|| key.clone())
    }

    fn start(&mut self, initial: ReplayedCandidate) -> Result<(), StoreError> {
        let key = self
            .signer()
            .ok_or_else(|| invalid("observer cannot start a checkpoint signer"))?;
        let driver = CheckpointDriver::new_candidate(
            self.settings.store.clone(),
            self.settings.initial_params.clone(),
            key,
            initial,
        )?;
        if crate::checkpoint_transport::context_from_anchor(driver.anchor())? != crate::checkpoint_transport::context_from_anchor(&self.anchor)? {
            return Err(invalid(
                "checkpoint parent advanced during session recovery",
            ));
        }
        self.session = Some(Session::new(
            driver,
            Instant::now(),
            Duration::from_secs(5),
        )?);
        self.recovered = true;
        Ok(())
    }

    fn recover(&mut self) -> Result<(), StoreError> {
        let Some(key) = self.signer() else {
            return Ok(());
        };
        let representative = derive_address(&public_from_private(&key));
        let retained = checkpoint_agreement::read_candidates(
            &self.settings.store,
            &self.anchor,
            &representative,
            &self.settings.initial_params,
        )?;
        if let Some(initial) = retained.into_iter().next() {
            self.start(initial)?;
        }
        Ok(())
    }

    fn last_decision(
        parent: Option<&CertifiedEvent>,
        now: Instant,
    ) -> Result<Option<(Context, Transport)>, StoreError> {
        if let Some(parent) = parent {
            return crate::checkpoint_transport::decision_relay(
                parent.parent(),
                parent.certificate().clone(),
                parent.manifest(),
                now,
            )
            .map(Some);
        }
        Ok(None)
    }

    fn refresh(&mut self, now: Instant) -> Result<(), StoreError> {
        if let Some(session) = &self.session {
            if session.state.decision().is_some() && session.driver.persist_decision()? {
                self.refresh_at = now;
            }
        }
        if now < self.refresh_at {
            return Ok(());
        }
        self.refresh_at = now + Duration::from_secs(1);
        let tip = CertifiedCheckpoint::stored_tip(
            &self.settings.store,
            self.settings.network,
            &self.settings.initial_params,
        )?;
        if tip < self.anchor.height() {
            return Err(invalid("checkpoint chain moved backwards"));
        }
        if tip > self.anchor.height() {
            let chain = chain::load(
                &self.settings.store,
                self.settings.network,
                &self.settings.initial_params,
            )?;
            let anchor =
                chain::next_anchor(&chain, self.settings.network, &self.settings.initial_params)?;
            let previous = Self::last_decision(chain.last(), now)?;
            self.idle = Transport::new(crate::checkpoint_transport::context_from_anchor(&anchor)?, now);
            self.anchor = anchor;
            self.session = None;
            self.previous = previous;
            self.recovered = false;
        }
        let awaiting = self.settings.store.meta_store().payment_epoch()? != self.anchor.hash();
        if self.awaiting_payment_epoch && !awaiting {
            self.recovered = false;
            self.candidate_at = now;
        }
        self.awaiting_payment_epoch = awaiting;
        Ok(())
    }

    pub(crate) fn receive(
        &mut self,
        incoming: Incoming,
        now: Instant,
        clock: &Clock,
    ) -> Result<Vec<Send>, StoreError> {
        self.refresh(now)?;
        if let Some((context, transport)) = &mut self.previous {
            if incoming.packet.context == *context {
                // Finished heights serve only their authenticated durable
                // decision. Never download more ballots for an old height.
                if matches!(
                    incoming.packet.body,
                    crate::checkpoint_transport::Body::Request { .. }
                ) {
                    return transport
                        .handle(&incoming.peer, incoming.packet, now)
                        .map(|(reply, _)| reply.into_iter().collect());
                }
                return Ok(Vec::new());
            }
        }
        if let Some(session) = &mut self.session {
            return session.receive(incoming, now, clock);
        }
        let (reply, received) = self.idle.handle(&incoming.peer, incoming.packet, now)?;
        if let Some(received) = received {
            CheckpointDriver::authenticate_for(&self.anchor, &received.message)?;
            match &received.message {
                Message::Decision {
                    certificate,
                    manifest,
                } => {
                    chain::accept_manifest(
                        &self.settings.store,
                        self.settings.network,
                        &self.settings.initial_params,
                        certificate.clone(),
                        manifest,
                    )?;
                    self.refresh_at = now;
                }
                Message::Proposal { manifest, .. } | Message::Prepared { manifest, .. } => {
                    if !matches!(manifest, checkpoint_agreement::ReplayManifest::Selection(_)) {
                        chain::require_lifecycle_enabled(self.settings.network)?;
                    }
                    if self.awaiting_payment_epoch {
                        return Err(invalid("new agreement awaits installed payment epoch"));
                    }
                    let replay =
                        manifest.replay(&self.settings.store, &self.settings.initial_params)?;
                    if self.signer().is_some() {
                        self.start(replay)?;
                        self.session
                            .as_mut()
                            .expect("started checkpoint session")
                            .accept(received, clock)?;
                        return Ok(reply.into_iter().collect());
                    }
                }
                Message::Vote(_) | Message::CatchUp { .. } => {}
            }
            self.idle.publish(&received.message)?;
            self.idle.acknowledge(received.digest);
        }
        Ok(reply.into_iter().collect())
    }

    pub(crate) fn poll(
        &mut self,
        peers: &[String],
        now: Instant,
        clock: &Clock,
    ) -> Result<Vec<Send>, StoreError> {
        self.refresh(now)?;
        if !self.recovered {
            self.recover()?;
            self.recovered = true;
        }
        if self.session.is_none() && now >= self.candidate_at && self.signer().is_some() {
            self.candidate_at = now + Duration::from_secs(5);
            if let Some(time) = clock() {
                if let Some(candidate) = discover_candidate(
                    &self.settings.store,
                    self.settings.network,
                    &self.settings.initial_params,
                    time,
                )? {
                    self.start(candidate)?;
                }
            }
        }
        let mut sends = if let Some(session) = &mut self.session {
            session.poll(peers, now, clock)?
        } else {
            self.idle.poll(peers, now)
        };
        if let Some((_, previous)) = &mut self.previous {
            sends.extend(previous.poll(peers, now));
        }
        Ok(sends)
    }
}

fn uses_incremental_replay(settings: &Settings) -> Result<bool, StoreError> {
    crate::checkpoint_store::uses_incremental_replay(
        &settings.store,
        settings.network,
        &settings.initial_params,
    )
}

enum RuntimeStep {
    Full(Result<Vec<Send>, StoreError>),
    Compact(Option<Incoming>),
}

pub async fn run_node(
    settings: Settings,
    registry: Arc<RwLock<ConnectionRegistry>>,
    inbox: mpsc::Receiver<Incoming>,
    clock: Clock,
    shutdown: Arc<ShutdownController>,
) -> Result<(), StoreError> {
    run_node_with_recovery(settings, registry, inbox, clock, shutdown, None).await
}

pub(crate) async fn run_node_with_recovery(
    settings: Settings,
    registry: Arc<RwLock<ConnectionRegistry>>,
    mut inbox: mpsc::Receiver<Incoming>,
    clock: Clock,
    shutdown: Arc<ShutdownController>,
    cached: Option<crate::confirmed_replay::snapshot::installed::ReplayCache>,
) -> Result<(), StoreError> {
    let source = settings.clone();
    let compact = tokio::task::spawn_blocking(move || uses_incremental_replay(&source))
        .await
        .map_err(invalid)??;
    if compact {
        return super::compact_runtime::run_compact_node_with_initial(
            settings, registry, inbox, clock, shutdown, None, cached,
        )
        .await;
    }
    let mut stop = shutdown.subscribe();
    let initial_settings = settings.clone();
    let mut runtime = tokio::task::spawn_blocking(move || Runtime::new(initial_settings))
        .await
        .map_err(invalid)??;
    let mut timer = tokio::time::interval(Duration::from_millis(250));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let incoming = match super::input::next(&mut stop, &mut inbox, &mut timer).await {
            Ok(incoming) => incoming,
            Err(()) => return Ok(()),
        };
        let peers = registry
            .read()
            .await
            .peer_ids()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let observed_clock = clock.clone();
        let (returned, result) = tokio::task::spawn_blocking(move || {
            match uses_incremental_replay(&runtime.settings) {
                Ok(true) => return (runtime, RuntimeStep::Compact(incoming)),
                Err(error) => return (runtime, RuntimeStep::Full(Err(error))),
                Ok(false) => {}
            }
            let result = (|| {
                let mut sends = Vec::new();
                if let Some(incoming) = incoming {
                    match runtime.receive(incoming, Instant::now(), &observed_clock) {
                        Ok(replies) => sends.extend(replies),
                        Err(error) => tracing::debug!(%error, "checkpoint runtime packet rejected"),
                    }
                }
                sends.extend(runtime.poll(&peers, Instant::now(), &observed_clock)?);
                Ok::<_, StoreError>(sends)
            })();
            (runtime, RuntimeStep::Full(result))
        })
        .await
        .map_err(invalid)?;
        runtime = returned;
        let result = match result {
            RuntimeStep::Full(result) => result,
            RuntimeStep::Compact(incoming) => {
                // No full-history replay or signer remains active across the
                // handoff. Compact recovery reauthenticates installed authority
                // and durable journals before creating a height worker.
                drop(runtime);
                return super::compact_runtime::run_compact_node_with_initial(
                    settings, registry, inbox, clock, shutdown, incoming, None,
                )
                .await;
            }
        };
        match result {
            Ok(sends) => {
                let registry = registry.read().await;
                for send in sends {
                    let bytes = WireMessage::CheckpointAgreement(send.packet)
                        .encode()
                        .map_err(invalid)?;
                    registry.send(&send.peer, bytes);
                }
            }
            Err(error) => {
                tracing::debug!(%error, "checkpoint runtime awaiting valid history or evidence")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint_replay::ReplayedCheckpoint;
    use burst_store::meta::MetaStore;

    #[test]
    fn incremental_mode_follows_installed_epoch_without_requiring_an_archive() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(directory.path(), 32, 16 << 20).unwrap());
        let settings = Settings {
            store: store.clone(),
            network: NetworkId::Dev,
            initial_params: ProtocolParams::default(),
            key: None,
        };
        assert!(!uses_incremental_replay(&settings).unwrap());
        let genesis = crate::genesis_state::block(settings.network, &settings.initial_params).hash;
        for (epoch, expected) in [
            (genesis, false),
            (burst_types::BlockHash::new([7; 32]), true),
        ] {
            store
                .meta_store()
                .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, epoch.as_bytes())
                .unwrap();
            assert_eq!(uses_incremental_replay(&settings).unwrap(), expected);
        }
        store
            .meta_store()
            .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, &[1])
            .unwrap();
        assert!(uses_incremental_replay(&settings).is_err());
    }

    #[test]
    fn failed_recovery_stays_closed_and_is_retried_without_restarting_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap());
        let key = Arc::new(burst_crypto::keypair_from_seed(&[0; 32]).private);
        let representative = derive_address(&public_from_private(&key));
        let params = ProtocolParams::default();
        let anchor = ReplayedCheckpoint::genesis_anchor(NetworkId::Dev, &params).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        crate::payment_epoch::initialize(&mut batch, anchor.hash()).unwrap();
        batch.commit().unwrap();
        let state_key = format!(
            "checkpoint_round/v1/dev/{}/{representative}/1",
            anchor.genesis_hash()
        );
        store.meta_store().put_meta(&state_key, &[255]).unwrap();
        let mut runtime = Runtime::new(Settings {
            store: store.clone(),
            network: NetworkId::Dev,
            initial_params: params,
            key: Some(key),
        })
        .unwrap();
        let clock: Clock = Arc::new(|| None);
        for _ in 0..2 {
            assert!(runtime.poll(&[], Instant::now(), &clock).is_err());
            assert!(!runtime.recovered);
            assert!(runtime.session.is_none());
            assert_eq!(store.meta_store().get_meta(&state_key).unwrap(), vec![255]);
        }
        // Repair only this disposable corrupt fixture, not a signed reservation.
        store.meta_store().delete_meta(&state_key).unwrap();
        assert!(runtime
            .poll(&[], Instant::now(), &clock)
            .unwrap()
            .is_empty());
        assert!(runtime.recovered);
        assert!(runtime.session.is_none());
    }
}
