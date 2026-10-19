use super::*;
use crate::confirmed_replay::snapshot::installed::RecoveredSnapshot;
use burst_store_lmdb::LmdbStore;

/// Receive and relay a single-height compact decision without a signing key.
/// The owner must recover the next authority and install its economic state
/// before rotating this worker. Acceptance alone does not install that state.
pub async fn run_compact_observer(
    store: Arc<LmdbStore>,
    base: Arc<RecoveredSnapshot>,
    registry: Arc<RwLock<ConnectionRegistry>>,
    inbox: mpsc::Receiver<Incoming>,
    shutdown: Arc<ShutdownController>,
) -> Result<(), StoreError> {
    run_height(store, base, registry, inbox, shutdown, None, None).await
}

struct Signer {
    settings: Settings,
    clock: Clock,
    step_timeout: Duration,
}

pub(crate) enum HeightAuthority {
    Recovered(Arc<RecoveredSnapshot>),
    Installed(super::compact_authority::Authority),
}

/// Run one compact height, retrying idle discovery and accepting peer proposals.
/// The owner remains responsible for economic installation and height rotation.
pub async fn run_compact_height(
    settings: Settings,
    base: Arc<RecoveredSnapshot>,
    registry: Arc<RwLock<ConnectionRegistry>>,
    inbox: mpsc::Receiver<Incoming>,
    clock: Clock,
    shutdown: Arc<ShutdownController>,
    step_timeout: Duration,
) -> Result<(), StoreError> {
    run_compact_height_with_authority(
        settings,
        HeightAuthority::Recovered(base),
        registry,
        inbox,
        clock,
        shutdown,
        step_timeout,
    )
    .await
}

pub(crate) async fn run_compact_height_with_authority(
    settings: Settings,
    authority: HeightAuthority,
    registry: Arc<RwLock<ConnectionRegistry>>,
    inbox: mpsc::Receiver<Incoming>,
    clock: Clock,
    shutdown: Arc<ShutdownController>,
    step_timeout: Duration,
) -> Result<(), StoreError> {
    let (base, authority) = match authority {
        HeightAuthority::Recovered(base) => (base, None),
        HeightAuthority::Installed(authority) => (authority.base.clone(), Some(authority)),
    };
    if settings.network != base.continuation.anchor().network() {
        return Err(invalid(
            "compact worker network differs from recovered authority",
        ));
    }
    if step_timeout.is_zero() || step_timeout > Duration::from_secs(60) {
        return Err(invalid("invalid checkpoint step timeout"));
    }
    let store = settings.store.clone();
    let eligible = settings.key.as_ref().is_some_and(|key| {
        let representative = burst_crypto::derive_address(&burst_crypto::public_from_private(key));
        base.continuation.anchor().roster().weight(&representative) > 0
    });
    let signer = eligible.then_some(Signer {
        settings,
        clock,
        step_timeout,
    });
    run_height(store, base, registry, inbox, shutdown, signer, authority).await
}

async fn run_height(
    store: Arc<LmdbStore>,
    mut base: Arc<RecoveredSnapshot>,
    registry: Arc<RwLock<ConnectionRegistry>>,
    mut inbox: mpsc::Receiver<Incoming>,
    shutdown: Arc<ShutdownController>,
    signer: Option<Signer>,
    mut reusable_authority: Option<super::compact_authority::Authority>,
) -> Result<(), StoreError> {
    if !cfg!(test)
        && !burst_verification::checkpoint_selection_enabled(base.continuation.anchor().network())
    {
        return Err(StoreError::Corruption(
            "compact observer is behind the release gate".into(),
        ));
    }
    let mut transport = Transport::new(
        Context::from_anchor(base.continuation.anchor())?,
        Instant::now(),
    );
    let mut accepted = false;
    let mut retry_at = Instant::now();
    let mut stop = shutdown.subscribe();
    let mut timer = tokio::time::interval(Duration::from_millis(250));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let incoming = match input::next(&mut stop, &mut inbox, &mut timer).await {
            Ok(incoming) => incoming,
            Err(()) => return Ok(()),
        };
        let authority = base.clone();
        let can_sign = signer.is_some();
        let (returned, result) = tokio::task::spawn_blocking(move || {
            let result = match incoming {
                Some(incoming)
                    if !accepted
                        || matches!(
                            incoming.packet.body,
                            crate::checkpoint_transport::Body::Request { .. }
                        ) =>
                {
                    transport.handle(&incoming.peer, incoming.packet, Instant::now())
                }
                _ => Ok((None, None)),
            }
            .and_then(|(reply, received)| {
                if can_sign {
                    if let Some(received) = &received {
                        if matches!(
                            received.message,
                            Message::Proposal { .. } | Message::Prepared { .. }
                        ) {
                            CheckpointDriver::authenticate_for(
                                authority.continuation.anchor(),
                                &received.message,
                            )?;
                        }
                    }
                }
                Ok((reply, received))
            });
            (transport, result)
        })
        .await
        .map_err(invalid)?;
        transport = returned;
        let (reply, received) = match result {
            Ok(result) => result,
            Err(error) => {
                tracing::debug!(%error, "compact observer packet rejected");
                (None, None)
            }
        };
        let mut admitted = None;
        if let Some(received) = received {
            if let Some(signer) = &signer {
                if let Message::Proposal { manifest, .. } | Message::Prepared { manifest, .. } =
                    &received.message
                {
                    let start = Session::from_compact_candidate_shared(
                        signer.settings.clone(),
                        registry.clone(),
                        base.clone(),
                        Some(manifest.clone()),
                        signer.step_timeout,
                    );
                    let result = tokio::select! {
                        biased;
                        _ = stop.recv() => return Ok(()),
                        result = start => result,
                    };
                    match result {
                        Ok(mut session) => {
                            let clock = signer.clock.clone();
                            let session = tokio::task::spawn_blocking(move || {
                                if let Err(error) = session.accept(received, &clock) {
                                    tracing::debug!(%error, "compact startup proposal deferred");
                                }
                                session
                            })
                            .await
                            .map_err(invalid)?;
                            return super::run(
                                session,
                                registry,
                                inbox,
                                signer.clock.clone(),
                                shutdown,
                            )
                            .await;
                        }
                        Err(error) => {
                            tracing::debug!(%error, "compact incoming startup deferred")
                        }
                    }
                }
            }
            if let Message::Decision {
                certificate,
                manifest,
            } = &received.message
            {
                let admission = base.clone().accept_decision_from_peers(
                    store.clone(),
                    registry.clone(),
                    certificate.clone(),
                    manifest.clone(),
                );
                let result = tokio::select! {
                    biased;
                    _ = stop.recv() => return Ok(()),
                    result = admission => result,
                };
                match result {
                    Ok(()) => {
                        accepted = true;
                        admitted = Some(received);
                    }
                    Err(error) => tracing::debug!(%error, "compact observer decision rejected"),
                }
            }
        }
        let peers = registry
            .read()
            .await
            .peer_ids()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let (returned, sends) = tokio::task::spawn_blocking(move || {
            let result = (|| {
                if let Some(received) = admitted {
                    transport.publish(&received.message)?;
                    transport.acknowledge(received.digest);
                }
                let mut sends = reply.into_iter().collect::<Vec<_>>();
                sends.extend(transport.poll(&peers, Instant::now()));
                Ok::<_, StoreError>(sends)
            })();
            (transport, result)
        })
        .await
        .map_err(invalid)?;
        transport = returned;
        let connections = registry.read().await;
        for send in sends? {
            connections.send(
                &send.peer,
                WireMessage::CheckpointAgreement(send.packet)
                    .encode()
                    .map_err(invalid)?,
            );
        }
        drop(connections);
        if !accepted && Instant::now() >= retry_at {
            retry_at = Instant::now() + Duration::from_secs(5);
            if let Some(signer) = &signer {
                let start = Session::resume_or_discover_compact_shared(
                    signer.settings.clone(),
                    registry.clone(),
                    base.clone(),
                    (signer.clock)(),
                    signer.step_timeout,
                );
                let result = tokio::select! {
                    biased;
                    _ = stop.recv() => return Ok(()),
                    result = start => result,
                };
                match result {
                    Ok(Some(session)) => {
                        return super::run(
                            *session,
                            registry,
                            inbox,
                            signer.clock.clone(),
                            shutdown,
                        )
                        .await
                    }
                    Ok(None) => (),
                    Err(StoreError::StaleRead(error)) => {
                        tracing::debug!(%error, "compact idle authority changed; recovering");
                        let recovery = async {
                            let cached = match &reusable_authority {
                                Some(authority) => Some(authority.replay_copy(&store).await?),
                                None => None,
                            };
                            super::compact_authority::recover_authority(
                                &signer.settings,
                                registry.clone(),
                                None,
                                cached,
                            )
                            .await
                        };
                        let recovered = tokio::select! {
                            biased;
                            _ = stop.recv() => return Ok(()),
                            result = recovery => result,
                        };
                        match recovered {
                            Ok(Some(current))
                                if current.base.continuation.anchor().hash()
                                    == base.continuation.anchor().hash() =>
                            {
                                // Refresh only authenticated observations. The owner
                                // rotates heights; startup still restores signed journals.
                                base = current.base.clone();
                                reusable_authority = Some(current);
                            }
                            Ok(Some(_)) => (),
                            Ok(None) => return Err(invalid("compact idle authority disappeared")),
                            Err(error) => {
                                tracing::debug!(%error, "compact idle authority recovery deferred");
                            }
                        }
                    }
                    Err(error) => tracing::debug!(%error, "compact idle startup deferred"),
                }
            }
        }
    }
}

fn invalid(error: impl ToString) -> StoreError {
    StoreError::Corruption(error.to_string())
}
