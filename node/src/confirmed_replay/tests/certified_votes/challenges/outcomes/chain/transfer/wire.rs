use super::*;
use crate::{
    checkpoint_sync, connection_registry::ConnectionRegistry, shutdown::ShutdownController,
    wire_message::WireMessage,
};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream},
    sync::{mpsc, RwLock},
    task::JoinSet,
};

async fn sync_pair(source: Arc<LmdbStore>, destination: Arc<LmdbStore>, params: &ProtocolParams) {
    let stores = [source, destination];
    let before =
        bincode::serialize(&stores[1].account_store().get_account(&address(20)).unwrap()).unwrap();
    let mut installed_outcome = false;
    let mut registries = Vec::new();
    let mut receivers = Vec::new();
    let shutdown = Arc::new(ShutdownController::new());
    for store in &stores {
        let registry = Arc::new(RwLock::new(ConnectionRegistry::new()));
        let (sender, receiver) = mpsc::channel(checkpoint_sync::INBOX_CAPACITY);
        registry.write().await.set_checkpoint_inbox(sender);
        checkpoint_sync::witness::start(store.clone(), registry.clone(), shutdown.clone()).await;
        registries.push(registry);
        receivers.push(receiver);
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let outgoing = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (incoming, _) = listener.accept().await.unwrap();
    let mut readers = JoinSet::new();
    for (owner, socket) in [outgoing, incoming].into_iter().enumerate() {
        let peer = (1 - owner).to_string();
        let (mut reader, writer) = socket.into_split();
        registries[owner].write().await.insert(peer.clone(), writer);
        let registry = registries[owner].clone();
        readers.spawn(async move {
            while let Ok(size) = reader.read_u32().await {
                assert!(size as usize <= burst_types::MAX_P2P_MESSAGE_BYTES);
                let mut body = vec![0; size as usize];
                if reader.read_exact(&mut body).await.is_err() {
                    break;
                }
                let WireMessage::CheckpointSync(message) =
                    crate::wire_message::WireMessage::decode(&body).unwrap()
                else {
                    panic!("unexpected historical transfer frame");
                };
                registry.read().await.dispatch_checkpoint(&peer, message);
            }
        });
    }
    let mut workers = JoinSet::new();
    for (index, receiver) in receivers.into_iter().enumerate() {
        workers.spawn(checkpoint_sync::run(
            stores[index].clone(),
            checkpoint_sync::Settings {
                network: NetworkId::Dev,
                params: params.clone(),
                snapshots: Default::default(),
                recovery: None,
            },
            registries[index].clone(),
            receiver,
            shutdown.clone(),
            Arc::new(HistoricalAdmissions::default()),
        ));
    }
    let received = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let height = CertifiedCheckpoint::stored_tip(&stores[1], NetworkId::Dev, params).unwrap();
            if height == 3 && !installed_outcome {
                assert_eq!(
                    bincode::serialize(&stores[1].account_store().get_account(&address(20)).unwrap()).unwrap(),
                    before,
                    "record transfer must not install peer economic state"
                );
                let installed = crate::confirmed_replay::installation::PreparedInstallation::next(
                    &stores[1], NetworkId::Dev, params,
                ).unwrap().unwrap().commit(&stores[1]).unwrap();
                assert_eq!(installed.anchor.height(), 3);
                installed_outcome = true;
                registries[1].read().await.checkpoint_installed.notify_one();
            }
            if height == 4 {
                assert!(installed_outcome, "catch-up skipped the certified outcome installation");
                break;
            }
            tokio::select! {
                result = workers.join_next() => panic!("checkpoint worker exited before synchronization: {result:?}"),
                result = readers.join_next() => panic!("checkpoint reader exited before synchronization: {result:?}"),
                _ = tokio::time::sleep(Duration::from_millis(100)) => (),
            }
        }
    })
    .await;
    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(result) = workers.join_next().await {
            result.unwrap();
        }
    })
    .await
    .expect("sync workers did not shut down");
    readers.abort_all();
    while let Some(result) = readers.join_next().await {
        if let Err(error) = result {
            assert!(error.is_cancelled(), "{error}");
        }
    }
    for registry in registries {
        let tasks = {
            let mut registry = registry.write().await;
            registry.close();
            registry.take_tasks()
        };
        for task in tasks {
            if let Err(error) = task.await {
                assert!(error.is_cancelled(), "{error}");
            }
        }
    }
    received.expect("mixed chain did not synchronize");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mixed_historical_transfer_syncs_over_tcp_and_serves_again_after_restart() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let (mut fixture, selections) = completed(ChallengeReason::Fraud);
    pending_successor(&mut fixture);
    let (_source_dir, source, heads) =
        super::super::super::installation::installed_chain(&fixture, &selections);
    let outcome = install_outcome(&fixture, &source, &heads);
    let expected = bincode::serialize(outcome.state().account(&address(20)).unwrap()).unwrap();
    let next = CertifiedCheckpoint::prepare(
        &source,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 71,
        &heads,
        &address(21),
    )
    .unwrap();
    CertifiedCheckpoint::accept(
        &source,
        NetworkId::Dev,
        &fixture.params,
        certify(next.checkpoint(), &[0, 1, 2, 3, 4, 5, 6]),
        heads,
        address(21),
    )
    .unwrap();
    assert!(
        CertifiedCheckpoint::export_transfer(&source, NetworkId::Dev, &fixture.params, 3)
            .unwrap()
            .unwrap()
            .len()
            > 16 * 1024
    );
    let (dir, destination, _) =
        super::super::super::installation::installed_chain(&fixture, &selections);
    let destination = Arc::new(destination);
    sync_pair(Arc::new(source), destination.clone(), &fixture.params).await;
    assert_eq!(
        bincode::serialize(
            &destination
                .account_store()
                .get_account(&address(20))
                .unwrap()
        )
        .unwrap(),
        expected
    );
    assert_eq!(Arc::strong_count(&destination), 1);
    drop(destination);
    let recovered = Arc::new(LmdbStore::open(dir.path(), 32, 32 << 20).unwrap());
    let (_fresh_dir, fresh, _) =
        super::super::super::installation::installed_chain(&fixture, &selections);
    let fresh = Arc::new(fresh);
    sync_pair(recovered, fresh.clone(), &fixture.params).await;
    let events = chain::load(&fresh, NetworkId::Dev, &fixture.params).unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(events[3].next_roster().weight(&address(20)), 0);
    assert_eq!(
        bincode::serialize(&fresh.account_store().get_account(&address(20)).unwrap()).unwrap(),
        expected
    );
}
