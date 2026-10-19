use super::*;
use crate::confirmed_replay::tests::snapshot_history::installed_recovery::Service;

pub(super) async fn assert_observer(
    fixture: &Fixture,
    source: Arc<LmdbStore>,
    manifest: &ReplayManifest,
    certificate: &CheckpointCertificate,
) {
    let snapshot = authenticate(&source, &fixture.params, manifest.checkpoint().parent);
    let base =
        authenticate(&source, &fixture.params, manifest.checkpoint().parent).into_continuation();
    let (dir, local) = cemented::sparse(&base, &source);
    local
        .meta_store()
        .put_meta(
            crate::confirmed_replay::snapshot::installed::BASE_KEY,
            &crate::confirmed_replay::tests::snapshot_history::installed_recovery::archive(
                &source,
                fixture,
                base.anchor().hash(),
            ),
        )
        .unwrap();
    let prefix = format!("checkpoint_chain/v1/dev/{}", base.anchor().genesis_hash());
    for event in chain::load(&source, NetworkId::Dev, &fixture.params).unwrap() {
        if event.certificate().checkpoint.height <= base.anchor().height()
            && event
                .selection()
                .is_some_and(|selection| selection.beacon().is_some())
        {
            let key = format!("{prefix}/{}", event.certificate().checkpoint.height);
            local
                .meta_store()
                .put_meta(&key, &source.meta_store().get_meta(&key).unwrap())
                .unwrap();
        }
    }
    local
        .meta_store()
        .put_meta(
            &format!("{prefix}/head"),
            &base.anchor().height().to_be_bytes(),
        )
        .unwrap();
    let mut heights = HashMap::new();
    for block in &fixture.blocks {
        let height = heights.entry(block.account.clone()).or_insert(0u64);
        *height += 1;
        if *height
            > base
                .state()
                .account(&block.account)
                .map_or(0, |account| account.block_count)
        {
            local
                .meta_store()
                .put_meta(
                    &format!("checkpoint_history/v1/{}", block.hash),
                    &bincode::serialize(block).unwrap(),
                )
                .unwrap();
        }
    }
    let recovered = Arc::new(
        snapshot
            .recover_stored_successors(&local, |_, _, _| unreachable!())
            .unwrap(),
    );
    let local = Arc::new(local);
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let accounts = bincode::serialize(
        &local
            .tx_begin_write()
            .unwrap()
            .bounded_accounts(131_072)
            .unwrap(),
    )
    .unwrap();
    let registry = Arc::new(tokio::sync::RwLock::new(
        crate::connection_registry::ConnectionRegistry::new(),
    ));
    let mut invalid = certificate.clone();
    invalid.signatures.clear();
    assert!(recovered
        .clone()
        .accept_decision_from_peers(local.clone(), registry.clone(), invalid, manifest.clone())
        .await
        .is_err());
    let mut mismatch = certificate.clone();
    mismatch.checkpoint.params_hash = BlockHash::ZERO;
    assert!(recovered
        .clone()
        .accept_decision_from_peers(local.clone(), registry, mismatch, manifest.clone())
        .await
        .is_err());
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
    let service = Service::start(source.clone(), local.clone(), 0).await;
    receive_over_tcp(
        local.clone(),
        recovered,
        service.registry.clone(),
        certificate,
        manifest,
    )
    .await;
    let prepared_metadata = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let installed = crate::confirmed_replay::snapshot::installed::recover_metadata_from_peers(
        local.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
    )
    .await
    .unwrap()
    .unwrap();
    let prepared = installed
        .prepare_next_from_peers(local.clone(), service.registry.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(prepared.anchor().hash(), certificate.checkpoint.hash());
    prepared
        .verify_unchanged(&local.tx_begin_write().unwrap())
        .unwrap();
    drop(prepared);
    let recovered = crate::confirmed_replay::snapshot::installed::recover_metadata_from_peers(
        local.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
    )
    .await
    .unwrap()
    .unwrap();
    let staged = recovered
        .project_token_indexes(&local)
        .unwrap()
        .stage_undo()
        .unwrap();
    drop(staged);
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        prepared_metadata
    );
    service.stop().await;
    assert_eq!(
        CertifiedCheckpoint::stored_tip(&local, NetworkId::Dev, &fixture.params).unwrap(),
        manifest.checkpoint().height
    );
    assert_eq!(
        local.meta_store().payment_epoch().unwrap(),
        base.anchor().hash()
    );
    assert_eq!(
        bincode::serialize(
            &local
                .tx_begin_write()
                .unwrap()
                .bounded_accounts(131_072)
                .unwrap()
        )
        .unwrap(),
        accounts
    );
    let persisted = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    assert!(!persisted
        .iter()
        .any(|(key, _)| key.starts_with(b"checkpoint_round/v1/")));
    drop(local);
    let local = Arc::new(LmdbStore::open(dir.path(), 32, 32 << 20).unwrap());
    let recovered = Arc::new(
        authenticate(&source, &fixture.params, manifest.checkpoint().parent)
            .recover_stored_epoch(&local, base.anchor().hash(), |_, _, _| unreachable!())
            .unwrap(),
    );
    let service = Service::start(source, local.clone(), 0).await;
    recovered
        .accept_decision_from_peers(
            local.clone(),
            service.registry.clone(),
            certificate.clone(),
            manifest.clone(),
        )
        .await
        .unwrap();
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        persisted
    );
    let metadata = crate::confirmed_replay::snapshot::installed::recover_metadata_from_peers(
        local.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
    )
    .await
    .unwrap()
    .unwrap();
    let candidate = metadata
        .prepare_next_from_peers(local.clone(), service.registry.clone())
        .await
        .unwrap()
        .unwrap();
    let expected = candidate.state().snapshot_bytes().unwrap();
    let installed = candidate.commit(&local).unwrap();
    assert_eq!(installed.anchor.hash(), certificate.checkpoint.hash());
    assert!(installed.minority_promises.is_empty());
    assert_eq!(
        local.meta_store().payment_epoch().unwrap(),
        certificate.checkpoint.hash()
    );
    let metadata = crate::confirmed_replay::snapshot::installed::recover_metadata_from_peers(
        local.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        metadata
            .recovered()
            .continuation
            .state()
            .snapshot_bytes()
            .unwrap(),
        expected
    );
    let prepared = metadata
        .prepare_next_from_peers(local.clone(), service.registry.clone())
        .await
        .unwrap();
    assert!(prepared.is_none());
    service.stop().await;
}

async fn receive_over_tcp(
    local: Arc<LmdbStore>,
    recovered: Arc<crate::confirmed_replay::snapshot::installed::RecoveredSnapshot>,
    registry: Arc<tokio::sync::RwLock<crate::connection_registry::ConnectionRegistry>>,
    certificate: &CheckpointCertificate,
    manifest: &ReplayManifest,
) {
    use crate::checkpoint_transport::{Body, Context, Transport};
    use tokio::io::AsyncWriteExt;
    let context = Context::from_anchor(recovered.continuation.anchor()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (client, server) = tokio::try_join!(
        TcpStream::connect(listener.local_addr().unwrap()),
        listener.accept()
    )
    .unwrap();
    let (mut reader, writer) = server.0.into_split();
    registry
        .write()
        .await
        .insert("observer-source".into(), writer);
    let (sender, receiver) = mpsc::channel(checkpoint_service::INBOX_CAPACITY);
    let router = tokio::spawn(async move {
        while let Ok(size) = reader.read_u32().await {
            assert!(size as usize <= burst_types::MAX_P2P_MESSAGE_BYTES);
            let mut body = vec![0; size as usize];
            reader.read_exact(&mut body).await.unwrap();
            let WireMessage::CheckpointAgreement(packet) = WireMessage::decode(&body).unwrap()
            else {
                panic!("unexpected observer frame")
            };
            if sender
                .send(Incoming {
                    peer: "observer-source".into(),
                    packet,
                })
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let shutdown = Arc::new(ShutdownController::new());
    let worker = tokio::spawn(checkpoint_service::run_compact_observer(
        local,
        recovered,
        registry.clone(),
        receiver,
        shutdown.clone(),
    ));
    let (mut reader, mut writer) = client.into_split();
    let (sender, mut receiver) = mpsc::channel(32);
    let client_reader = tokio::spawn(async move {
        while let Ok(size) = reader.read_u32().await {
            assert!(size as usize <= burst_types::MAX_P2P_MESSAGE_BYTES);
            let mut body = vec![0; size as usize];
            reader.read_exact(&mut body).await.unwrap();
            let WireMessage::CheckpointAgreement(packet) = WireMessage::decode(&body).unwrap()
            else {
                panic!("unexpected observer relay")
            };
            if sender.send(packet).await.is_err() {
                break;
            }
        }
    });
    let decision = crate::checkpoint_driver::Message::Decision {
        certificate: certificate.clone(),
        manifest: manifest.clone(),
    };
    let mut publisher = Transport::new(context, std::time::Instant::now());
    publisher.publish(&decision).unwrap();
    let mut relay_reader = Transport::new(context, std::time::Instant::now());
    let mut timer = tokio::time::interval(Duration::from_millis(50));
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let mut sends = Vec::new();
            tokio::select! {
                _ = timer.tick() => {},
                packet = receiver.recv() => {
                    let packet = packet.expect("observer stopped before relaying");
                    let transport = if matches!(packet.body, Body::Request { .. }) {
                        &mut publisher
                    } else {
                        &mut relay_reader
                    };
                    let (reply, received) = transport.handle("observer", packet, std::time::Instant::now()).unwrap();
                    sends.extend(reply);
                    if let Some(received) = received {
                        assert_eq!(
                            bincode::serialize(&received.message).unwrap(),
                            bincode::serialize(&decision).unwrap()
                        );
                        break;
                    }
                }
            }
            let peers = ["observer".to_owned()];
            sends.extend(publisher.poll(&peers, std::time::Instant::now()));
            sends.extend(relay_reader.poll(&peers, std::time::Instant::now()));
            for send in sends {
                let bytes = WireMessage::CheckpointAgreement(send.packet).encode().unwrap();
                writer.write_u32(bytes.len() as u32).await.unwrap();
                writer.write_all(&bytes).await.unwrap();
            }
        }
    }).await;
    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    registry.write().await.remove("observer-source");
    router.abort();
    client_reader.abort();
    let _ = router.await;
    let _ = client_reader.await;
    result.expect("compact observer did not accept and relay decision");
}
