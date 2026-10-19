//! Opt-in external interoperability check. This uses public NTS and a future
//! drand round, but fixture history and genesis authority, not a live network.
use super::*;
use crate::checkpoint_agreement::{self, Command, Outgoing, ReplayedCandidate};
use crate::checkpoint_replay::{ReplayedCheckpoint, SigningClock};
use crate::checkpoint_store::CertifiedCheckpoint;
use burst_consensus::checkpoint::CheckpointCertificate;
use burst_consensus::checkpoint_round::Quorum;
use burst_network::trusted_clock::{NtsClock, TrustedClock};
use std::{sync::Arc, time::Duration};

async fn fresh_clock(clock: &TrustedClock) -> SigningClock {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(time) = clock.reading() {
                return SigningClock {
                    now: time.now,
                    error_bound: time.error_bound,
                };
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("three authenticated public time sources did not become available")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires public NTS (TCP 4460/UDP 123), HTTPS drand and a future beacon"]
async fn authenticated_live_clock_prepares_then_recovers_the_exact_future_beacon() {
    let service = NtsClock::new(NtsClock::public_sources()).unwrap();
    let clock = service.clock();
    let (stop_time, stopped_time) = tokio::sync::watch::channel(false);
    let time_worker = tokio::spawn(service.run(stopped_time));
    let client = burst_vrf::DrandClient::mainnet()
        .unwrap()
        .with_relays(&[burst_vrf::drand::DRAND_MAINNET_URL.into()])
        .unwrap();
    let chain_info = client.chain_info().unwrap().clone();
    // Enter early in a period. Replay and durable signing must still recheck
    // the actual bound; this wait does not authorize a future cutoff.
    let observed = tokio::time::timeout(Duration::from_secs(65), async {
        loop {
            let time = fresh_clock(&clock).await;
            let release = chain_info
                .time_of_round(chain_info.round_after(time.now).unwrap())
                .unwrap();
            if time.now + time.error_bound + 15 < release {
                break time;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("no usable pre-beacon signing window");
    let fixture = Fixture::selection();
    let (directory, store, heads) = fixture.store();
    let replay = CertifiedCheckpoint::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        observed.now,
        &heads,
        &address(20),
    )
    .unwrap();
    let request = replay.request().clone();
    let checkpoint = replay.checkpoint().clone();
    let release = request.beacon_time().unwrap();
    let anchor = ReplayedCheckpoint::genesis_anchor(NetworkId::Dev, &fixture.params).unwrap();
    let signer = burst_crypto::keypair_from_seed(&[0; 32]);
    let signing_time = fresh_clock(&clock).await;
    assert!(signing_time.now + signing_time.error_bound < release);
    let proposed = checkpoint_agreement::apply(
        &store,
        &anchor,
        &signer.private,
        Command::Propose(checkpoint.clone()),
        &[&replay],
        Some(signing_time),
    )
    .unwrap();
    let Outgoing::Proposal(proposal) = &proposed[0] else {
        panic!("missing proposal")
    };
    let votes = checkpoint_agreement::apply(
        &store,
        &anchor,
        &signer.private,
        Command::Proposal(proposal),
        &[&replay],
        Some(fresh_clock(&clock).await),
    )
    .unwrap();
    let Outgoing::Vote(prevote) = &votes[0] else {
        panic!("missing pre-beacon prevote")
    };
    let prepared = Quorum {
        ballot: prevote.ballot.clone(),
        signatures: vec![prevote.signer.clone()],
    };
    store.force_sync().unwrap();
    let durable_at = fresh_clock(&clock).await;
    assert!(
        durable_at.now + durable_at.error_bound < release,
        "fixture missed its window; do not count this run as pre-beacon evidence"
    );
    eprintln!(
        "live prepared round={} cutoff={} durable UTC=[{}, {}] release={}",
        request.round(),
        checkpoint.cutoff,
        durable_at.now,
        durable_at.now + durable_at.error_bound,
        release
    );
    drop(store);
    tokio::time::timeout(Duration::from_secs(65), async {
        loop {
            if fresh_clock(&clock).await.now >= release {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("authenticated time did not cross the beacon boundary");
    // A new signer without a prepared value cannot introduce the old request
    // after release, even if its content and signature would otherwise be valid.
    let (_late_directory, late, _) = fixture.store();
    assert!(checkpoint_agreement::apply(
        &late,
        &anchor,
        &signer.private,
        Command::Propose(checkpoint.clone()),
        &[&replay],
        Some(fresh_clock(&clock).await)
    )
    .is_err());
    assert!(
        checkpoint_agreement::read_state(&late, &anchor, &address(0))
            .unwrap()
            .is_none()
    );
    let store = Arc::new(LmdbStore::open(directory.path(), 32, 32 << 20).unwrap());
    let recovered =
        checkpoint_agreement::read_candidates(&store, &anchor, &address(0), &fixture.params)
            .unwrap();
    let ReplayedCandidate::Selection(replayed) = recovered.into_iter().next().unwrap() else {
        panic!("recovered another candidate kind")
    };
    assert_eq!(replayed.request(), &request);
    let votes = checkpoint_agreement::apply(
        &store,
        &anchor,
        &signer.private,
        Command::Prepared(proposal, &prepared),
        &[&replayed],
        None,
    )
    .unwrap();
    let Outgoing::Vote(precommit) = &votes[0] else {
        panic!("missing recovered precommit")
    };
    let certificate = CheckpointCertificate {
        checkpoint,
        round: proposal.body.round,
        signatures: vec![precommit.signer.clone()],
    };
    checkpoint_agreement::apply(
        &store,
        &anchor,
        &signer.private,
        Command::Decide(certificate.clone()),
        &[&replayed],
        None,
    )
    .unwrap();
    let accepted = CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certificate.clone(),
        heads,
        address(20),
    )
    .unwrap();
    assert!(accepted.beacon().is_none());
    let shutdown = Arc::new(crate::shutdown::ShutdownController::new());
    let worker = tokio::spawn(crate::checkpoint_beacon::run(
        store.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        Arc::new(client),
        shutdown.clone(),
        Duration::from_millis(500),
        None,
    ));
    let proof = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            assert!(
                shutdown.failure().is_none(),
                "beacon worker stopped: {:?}",
                shutdown.failure()
            );
            let recovered = CertifiedCheckpoint::load(&store, NetworkId::Dev, &fixture.params)
                .unwrap()
                .unwrap();
            assert_eq!(recovered.certificate(), &certificate);
            assert_eq!(recovered.replayed().request(), &request);
            if let Some(proof) = recovered.beacon() {
                break proof.clone();
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await;
    shutdown.shutdown();
    worker.await.unwrap();
    stop_time.send(true).unwrap();
    time_worker.await.unwrap();
    let proof = proof.expect("exact future drand proof was not recovered");
    assert_eq!(proof.round, request.round());
    let committee = request.select(&anchor, &certificate, &proof).unwrap();
    drop(store);
    let reopened = LmdbStore::open(directory.path(), 32, 32 << 20).unwrap();
    let recovered = CertifiedCheckpoint::load(&reopened, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap();
    assert_eq!(
        request
            .select(
                &anchor,
                recovered.certificate(),
                recovered.beacon().unwrap()
            )
            .unwrap(),
        committee
    );
    eprintln!("PASS live NTS + exact future round {}: pre-beacon durable promise, late-fresh rejection, restart and identical committee", proof.round);
}
