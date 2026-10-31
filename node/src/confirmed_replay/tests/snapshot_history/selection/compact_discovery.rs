use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn compact_startup_preserves_idle_base_when_no_event_is_due() {
    use crate::checkpoint_service::{CompactStart, Session, Settings};
    use std::sync::Arc;
    let (fixture, _dir, source, manifest, _) = setup();
    let snapshot = authenticate(&source, &fixture.params, manifest.checkpoint.hash());
    let base =
        authenticate(&source, &fixture.params, manifest.checkpoint.hash()).into_continuation();
    let (_dir, local) = cemented::sparse(&base, &source);
    local
        .meta_store()
        .put_meta(
            &format!(
                "checkpoint_chain/v1/dev/{}/head",
                base.anchor().genesis_hash()
            ),
            &base.anchor().height().to_be_bytes(),
        )
        .unwrap();
    let recovered = snapshot
        .recover_stored_successors(&local, |_, _, _| unreachable!())
        .unwrap();
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let state = recovered.continuation.state().snapshot_bytes().unwrap();
    let local = Arc::new(local);
    let service =
        crate::confirmed_replay::tests::snapshot_history::installed_recovery::WitnessService::start(
            Arc::new(source),
            local.clone(),
            0,
        )
        .await;
    let result = Session::resume_or_discover_compact(
        Settings {
            store: local.clone(),
            network: NetworkId::Dev,
            initial_params: fixture.params,
            key: Some(Arc::new(burst_crypto::keypair_from_seed(&[0; 32]).private)),
        },
        service.registry.clone(),
        recovered,
        Some(crate::checkpoint_replay::SigningClock {
            now: CUTOFF + 2,
            error_bound: 0,
        }),
        std::time::Duration::from_secs(30),
    )
    .await
    .unwrap();
    let CompactStart::Idle(recovered) = result else {
        panic!("startup invented a new candidate");
    };
    assert_eq!(
        recovered.continuation.state().snapshot_bytes().unwrap(),
        state
    );
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
    service.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn compact_selection_discovery_rechecks_local_authority_without_writes() {
    use std::sync::Arc;
    use tokio::sync::RwLock;
    let (fixture, _source_dir, source, expected, _) = setup();
    for changed in [false, true] {
        let snapshot = authenticate(&source, &fixture.params, expected.checkpoint.parent);
        let base =
            authenticate(&source, &fixture.params, expected.checkpoint.parent).into_continuation();
        let (_dir, local) = cemented::sparse(&base, &source);
        local
            .meta_store()
            .put_meta(
                &format!(
                    "checkpoint_chain/v1/dev/{}/head",
                    base.anchor().genesis_hash()
                ),
                &base.anchor().height().to_be_bytes(),
            )
            .unwrap();
        let recovered = Arc::new(
            snapshot
                .recover_stored_successors(&local, |_, _, _| unreachable!())
                .unwrap(),
        );
        if changed {
            local
                .meta_store()
                .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, &[99; 32])
                .unwrap();
        }
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let local = Arc::new(local);
        let registry = Arc::new(RwLock::new(
            crate::connection_registry::ConnectionRegistry::new(),
        ));
        let result = recovered
            .clone()
            .discover_selection_from_peers(
                local.clone(),
                registry.clone(),
                CUTOFF,
                expected.heads.clone(),
            )
            .await;
        if changed {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap().unwrap().replay().manifest(), expected);
        }
        let result = recovered
            .clone()
            .discover_cemented_selection_from_peers(local.clone(), registry.clone(), CUTOFF)
            .await;
        if changed {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap().unwrap().replay().manifest(), expected);
        }
        let result = recovered
            .discover_cemented_candidate_from_peers(
                local.clone(),
                registry,
                crate::checkpoint_replay::SigningClock {
                    now: CUTOFF,
                    error_bound: 0,
                },
            )
            .await;
        if changed {
            assert!(result.is_err());
        } else {
            assert_eq!(
                result.unwrap().unwrap().input().manifest(),
                ReplayManifest::Selection(expected.clone())
            );
        }
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before
        );
    }
}

#[test]
fn compact_selection_discovery_matches_full_replay_at_fresh_and_later_cuts() {
    let (fixture, _source_dir, source, original, _) = setup();
    let (_full_dir, full, _) = fixture.store();
    let prefix = format!("checkpoint_chain/v1/dev/{}", fixture.blocks[0].hash);
    full.meta_store()
        .put_meta(
            &format!("{prefix}/1"),
            &source
                .meta_store()
                .get_meta(&format!("{prefix}/1"))
                .unwrap(),
        )
        .unwrap();
    full.meta_store()
        .put_meta(&format!("{prefix}/head"), &1u64.to_be_bytes())
        .unwrap();
    let (_suffix_dir, suffix) = suffix_store(&[]);
    let mut previous_round = None;
    for cutoff in [CUTOFF, CUTOFF + 31] {
        let expected =
            CertifiedCheckpoint::prepare_next(&full, NetworkId::Dev, &fixture.params, cutoff)
                .unwrap()
                .unwrap();
        let snapshot = authenticate(&source, &fixture.params, original.checkpoint.parent);
        let witnesses = compact::retained(&snapshot, &fixture, &source, &[]);
        let base = snapshot.into_continuation();
        let before = base.state().snapshot_bytes().unwrap();
        let tail = base
            .read_tail(&suffix, cutoff, &expected.manifest().heads, witnesses)
            .unwrap();
        assert!(base.prepare_deadline(&tail, &[]).unwrap().is_none());
        let discovered = base.prepare_next_selection(tail, &[]).unwrap().unwrap();
        assert_eq!(discovered.replay().manifest(), expected.manifest());
        assert_eq!(base.state().snapshot_bytes().unwrap(), before);
        assert_eq!(base.anchor().hash(), original.checkpoint.parent);
        let release = discovered.replay().request().beacon_time().unwrap();
        assert!(release > cutoff);
        if let Some(previous) = previous_round {
            assert!(release > previous);
        }
        previous_round = Some(release);
        assert!(discovered.replay().validate_parent(base.anchor()).is_err());
    }
}

#[test]
fn compact_selection_discovery_does_not_resample_a_certified_request() {
    let (fixture, _source_dir, source, manifest, certificate) = setup();
    let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
    let selections = snapshot.recover_selections().unwrap();
    let witnesses = compact::retained(&snapshot, &fixture, &source, &[]);
    let base = snapshot.into_continuation();
    let (_suffix_dir, suffix) = suffix_store(&[]);
    let tail = base
        .read_tail(&suffix, CUTOFF + 1, &manifest.heads, witnesses)
        .unwrap();
    let before = base.state().snapshot_bytes().unwrap();
    let refs = selections.iter().collect::<Vec<_>>();
    assert!(base.prepare_deadline(&tail, &[]).is_err());
    assert!(base.prepare_deadline(&tail, &refs).unwrap().is_none());
    assert!(base.prepare_next_selection(tail, &refs).unwrap().is_none());
    assert_eq!(base.state().snapshot_bytes().unwrap(), before);
    let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
    let witnesses = compact::retained(&snapshot, &fixture, &source, &[]);
    let tail = base
        .read_tail(&suffix, CUTOFF + 1, &manifest.heads, witnesses)
        .unwrap();
    assert!(base.prepare_next_selection(tail, &[]).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn discovered_session_defers_soft_votes_drains_finals_and_releases_on_drop() {
    use crate::checkpoint_service::{Session, Settings};
    use std::{sync::Arc, time::Duration};
    use tokio::sync::RwLock;
    let (fixture, _source_dir, source, expected, _) = setup();
    for seed in [0, 99] {
        let snapshot = authenticate(&source, &fixture.params, expected.checkpoint.parent);
        let base = authenticate(&source, &fixture.params, expected.checkpoint.parent).into_continuation();
        let (_dir, local) = cemented::sparse(&base, &source);
        local.meta_store().put_meta(
            &format!("checkpoint_chain/v1/dev/{}/head", base.anchor().genesis_hash()),
            &base.anchor().height().to_be_bytes(),
        ).unwrap();
        let recovered = Arc::new(snapshot.recover_stored_successors(&local, |_, _, _| unreachable!()).unwrap());
        let local = Arc::new(local);
        let registry = Arc::new(RwLock::new(crate::connection_registry::ConnectionRegistry::new()));
        let session = Session::resume_or_discover_compact_shared(
            Settings { store: local.clone(), network: NetworkId::Dev, initial_params: fixture.params.clone(),
                key: Some(Arc::new(burst_crypto::keypair_from_seed(&[seed; 32]).private)) },
            registry, recovered, Some(crate::checkpoint_replay::SigningClock { now: CUTOFF, error_bound: 0 }),
            Duration::from_secs(5),
        ).await;
        let rep = address(seed);
        let epoch = base.anchor().hash();
        let hash = BlockHash::new([90; 32]);
        let reserve = |final_vote| local.meta_store().reserve_consensus_vote_at(
            &epoch, &rep, &address(99), &BlockHash::ZERO, &hash, final_vote, 0, Some(base.anchor().cutoff()),
        );
        if seed == 0 {
            let session = session.unwrap().expect("pending local selection");
            let error = reserve(false).unwrap_err();
            assert!(error.to_string().contains("new soft votes deferred"), "{error}");
            reserve(true).unwrap();
            drop(session);
            reserve(false).unwrap();
        } else {
            assert!(session.is_err(), "non-roster key acquired preparation authority");
            reserve(false).unwrap();
        }
    }
}
