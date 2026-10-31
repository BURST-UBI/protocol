use super::*;

pub(in crate::confirmed_replay::tests) fn prepared(
    humans: u32,
    params: ProtocolParams,
    map_size: usize,
) -> (tempfile::TempDir, LmdbStore, Fixture, BlockHash, u64) {
    static CACHE: std::sync::LazyLock<std::sync::Mutex<tempfile::TempDir>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(tempfile::tempdir().unwrap()));
    let cache = CACHE.lock().unwrap();
    let params = crate::verification_test_support::electorate_params(params);
    let root = std::env::var_os("BURST_ELECTORATE_PREPARED")
        .map(PathBuf::from)
        .unwrap_or_else(|| cache.path().to_path_buf());
    let key = hex::encode(burst_crypto::blake2b_256(
        &bincode::serialize(&(humans, &params)).unwrap(),
    ));
    let saved = root.join(key);
    if saved.exists() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::copy(saved.join("data.mdb"), directory.path().join("data.mdb")).unwrap();
        let store = LmdbStore::open(directory.path(), 32, map_size).unwrap();
        let (fixture, population, epoch, now) = resume::restore(&store, &saved, humans).unwrap();
        assert_eq!(population, 3);
        assert_eq!(fixture.params, params);
        eprintln!(
            "electorate prepared reuse={} target={humans}",
            saved.display()
        );
        return (directory, store, fixture, epoch, now - 1);
    }
    let result = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(prepare(humans, params, map_size))
    })
    .join()
    .unwrap();
    std::fs::create_dir_all(saved.parent().unwrap()).unwrap();
    std::fs::create_dir(&saved).unwrap();
    let (_, store, fixture, epoch, cutoff) = &result;
    resume::save(
        &saved,
        store,
        &fixture.params,
        *epoch,
        3,
        cutoff + 1,
        humans,
    );
    std::fs::copy(store.env().path().join("data.mdb"), saved.join("data.mdb")).unwrap();
    eprintln!(
        "electorate prepared saved={} target={humans}",
        saved.display()
    );
    result
}

fn fresh(
    humans: u32,
    params: ProtocolParams,
    map_size: usize,
) -> (tempfile::TempDir, LmdbStore, Fixture) {
    let mut fixture = Fixture::new();
    fixture.params = crate::verification_test_support::electorate_params(params);
    fixture.blocks[0] = crate::genesis_state::block(NetworkId::Dev, &fixture.params);
    fixture.extend_bootstrap_electorate(1..3);
    fixture.push(1, BlockType::VerifierOptIn, 110, |_| {});
    let burn = fixture.push(0, BlockType::Burn, 111, |block| {
        block.brn_spent += 1;
        block.link = destination(1);
    });
    fixture.push(1, BlockType::Receive, 112, |block| {
        block.trst_balance = 1;
        block.link = burn;
    });
    for index in 3..humans {
        fixture.push(0, BlockType::Endorse, 1_595_434_000, |block| {
            block.brn_spent += 1;
            block.link = BlockHash::new(
                burst_crypto::keypair_from_seed(&crate::confirmed_replay::tests::electorate::seed(
                    index,
                ))
                .public
                .0,
            );
        });
    }
    let (directory, store, _) = fixture.store_with_map_size(map_size);
    let first =
        PreparedRoster::prepare_next(&store, NetworkId::Dev, &fixture.params, 1_595_434_001)
            .unwrap()
            .unwrap();
    let epoch = first.manifest().checkpoint.hash();
    chain::accept_manifest(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certified_votes::certify(&first.manifest().checkpoint, &[0]),
        &ReplayManifest::Roster(first.manifest().clone()),
    )
    .unwrap();
    install(&fixture, &store, epoch, &[0, 1, 2]);

    (directory, store, fixture)
}

async fn prepare(
    humans: u32,
    params: ProtocolParams,
    map_size: usize,
) -> (tempfile::TempDir, LmdbStore, Fixture, BlockHash, u64) {
    let started = Instant::now();
    let resumed = std::env::var_os("BURST_ELECTORATE_PREPARE_RESUME").map(PathBuf::from);
    let (directory, store, fixture) = if let Some(source) = &resumed {
        let directory = tempfile::tempdir().unwrap();
        std::fs::copy(source.join("data.mdb"), directory.path().join("data.mdb")).unwrap();
        let store = LmdbStore::open(directory.path(), 32, map_size).unwrap();
        let recovered = installed::load_base(&store, NetworkId::Dev, &params)
            .unwrap()
            .unwrap()
            .recover_stored_successors(&store, |base, plan, _| {
                crate::checkpoint_sync::witness::read_plan(&store, base, plan)
            })
            .unwrap();
        let fixture = resume::authenticated_heads(&store, &recovered.continuation, params).unwrap();
        let epoch = recovered.continuation.anchor().hash();
        if store.meta_store().payment_epoch().unwrap() != epoch {
            // An interrupted acceptance may have one unpublished successor.
            install(&fixture, &store, epoch, &[0, 1, 2]);
        }
        eprintln!(
            "electorate preparation resume={} target={humans}",
            source.display()
        );
        (directory, store, fixture)
    } else {
        fresh(humans, params, map_size)
    };
    let store = std::sync::Arc::new(store);
    let service = crate::confirmed_replay::tests::snapshot_history::installed_recovery::WitnessService::start(
        store.clone(), store.clone(), 0,
    ).await;
    let mut progress = None;
    let recover = || {
        installed::load_base(&store, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap()
            .recover_stored_successors(&store, |base, plan, _| {
                crate::checkpoint_sync::witness::read_plan(&store, base, plan)
            })
            .unwrap()
    };
    let mut recovered = recover();
    let base = &recovered.continuation;
    assert_eq!(base.anchor().roster().total_humans(), 3);
    assert_eq!(base.state().accounts.len(), 3);
    let targets: HashSet<_> = (3..humans)
        .map(|index| {
            burst_crypto::derive_address(
                &burst_crypto::keypair_from_seed(
                    &crate::confirmed_replay::tests::electorate::seed(index),
                )
                .public,
            )
        })
        .collect();
    for target in &targets {
        let state = base.state().verification.get_state(target).unwrap();
        assert!(state.endorser_set.contains(&address(0)));
        assert!(state.votes.is_empty());
        assert!(
            state.selected_verifiers.is_empty()
                || state.selected_verifiers.as_slice() == [address(1)]
        );
    }
    let selected: HashSet<_> = recovered
        .selections
        .iter_selections()
        .map(|selection| selection.unwrap().replayed().request().target().clone())
        .collect();
    assert!(selected.is_subset(&targets));
    let frozen = u32::try_from(selected.len()).unwrap();
    let activated = u32::try_from(
        targets
            .iter()
            .filter(|target| {
                !base
                    .state()
                    .verification
                    .get_state(target)
                    .unwrap()
                    .selected_verifiers
                    .is_empty()
            })
            .count(),
    )
    .unwrap();
    assert!(activated == 0 || frozen == humans - 3);
    let mut epoch = base.anchor().hash();
    // Discovery subtracts one second from signing time; the safe cut must
    // strictly advance the authenticated parent.
    let mut cutoff = (base.anchor().cutoff() + 2).max(1_595_434_050);
    eprintln!("electorate preparation recovered selections={frozen} activations={activated}");
    for index in 3 + frozen..humans {
        let base = &recovered.continuation;
        let mut heads: Vec<_> = base
            .state()
            .accounts
            .values()
            .map(|account| HistoryHead {
                account: account.address.clone(),
                height: account.block_count,
                hash: account.head,
            })
            .collect();
        heads.sort_by(|left, right| left.account.as_str().cmp(right.account.as_str()));
        let plan = base.plan_tail(&store, cutoff, &heads).unwrap();
        let proofs = crate::checkpoint_sync::witness::read_plan(&store, base, &plan).unwrap();
        let tail = plan.finish(proofs).unwrap();
        let coverage = base
            .prepare_next_selection(tail, &recovered.selections)
            .unwrap()
            .unwrap();
        let certificate = certified_votes::certify(coverage.replay().checkpoint(), &[0, 1, 2]);
        crate::checkpoint_store::compact::accept_selection(
            &store,
            base.anchor(),
            &coverage,
            &certificate,
        )
        .unwrap();
        epoch = certificate.checkpoint.hash();
        if index % 64 == 0 || index + 1 == humans {
            drop(recovered);
            install(&fixture, &store, epoch, &[0, 1, 2]);
            recovered = recover();
            progress = None;
        } else {
            let metadata = installed::recover_metadata_retaining_progress(
                store.clone(),
                NetworkId::Dev,
                fixture.params.clone(),
                service.registry.clone(),
                &mut progress,
            )
            .await
            .unwrap()
            .unwrap();
            let (_, cache, _) = metadata
                .prepare_next_from_peers(store.clone(), service.registry.clone())
                .await
                .unwrap()
                .unwrap()
                .commit_with_recovery(&store)
                .unwrap();
            recovered = cache.recovered.fork(store.env().path()).unwrap();
            progress = Some(cache);
        }
        if index % 32 == 0 || index + 1 == humans {
            eprintln!(
                "electorate setup stage=selection completed={} elapsed_ms={}",
                index - 2,
                started.elapsed().as_millis()
            );
        }
        cutoff += 1;
    }
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../../../../tests/fixtures/electorate-beacons.json"
    ))
    .unwrap();
    let client = burst_vrf::DrandClient::mainnet().unwrap();
    let chain = client.chain_info().unwrap();
    for selection in recovered.selections.iter_selections() {
        let mut selection = selection.unwrap().into_owned();
        let round = chain
            .round_after(selection.certificate().checkpoint.cutoff)
            .unwrap();
        let beacon = beacons
            .iter()
            .find(|beacon| beacon.round == round)
            .expect("electorate fixture needs an authenticated beacon for this round");
        if selection.beacon().is_none() {
            selection
                .attach_beacon(
                    &store,
                    &fixture.params,
                    burst_vrf::DrandClient::beacon_to_random_output(beacon).unwrap(),
                )
                .unwrap();
        }
        cutoff = cutoff.max(chain.time_of_round(round).unwrap() + 1);
    }
    drop(recovered);
    progress = None;
    let mut recovered = recover();
    for index in 3 + activated..humans {
        let base = &recovered.continuation;
        let tail = cemented::finish(base, &store, &store);
        let candidate = base
            .prepare_next_cemented_candidate(
                &store,
                &tail,
                &recovered.selections,
                SigningClock {
                    now: cutoff,
                    error_bound: 0,
                },
            )
            .unwrap()
            .unwrap();
        assert!(matches!(&candidate, CompactReplayCandidate::Activation(_)));
        let certificate =
            certified_votes::certify(candidate.input().manifest().checkpoint(), &[0, 1, 2]);
        crate::checkpoint_store::compact::accept_candidate(
            &store,
            base.anchor(),
            candidate.input(),
            &certificate,
        )
        .unwrap();
        epoch = certificate.checkpoint.hash();
        if index % 64 == 0 || index + 1 == humans {
            drop(recovered);
            install(&fixture, &store, epoch, &[0, 1, 2]);
            recovered = recover();
            progress = None;
        } else {
            let metadata = installed::recover_metadata_retaining_progress(
                store.clone(),
                NetworkId::Dev,
                fixture.params.clone(),
                service.registry.clone(),
                &mut progress,
            )
            .await
            .unwrap()
            .unwrap();
            let (_, cache, _) = metadata
                .prepare_next_from_peers(store.clone(), service.registry.clone())
                .await
                .unwrap()
                .unwrap()
                .commit_with_recovery(&store)
                .unwrap();
            recovered = cache.recovered.fork(store.env().path()).unwrap();
            progress = Some(cache);
        }
        if index % 32 == 0 || index + 1 == humans {
            eprintln!(
                "electorate setup stage=activation completed={} elapsed_ms={}",
                index - 2,
                started.elapsed().as_millis()
            );
        }
        cutoff += 1;
    }
    drop(recovered);
    let base = installed::load_base(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .into_continuation();
    assert_eq!(base.anchor().roster().total_humans(), 3);
    assert_eq!(base.state().accounts.len(), 3);
    assert_eq!(base.anchor().hash(), epoch);
    let cutoff = base.anchor().cutoff();
    drop(base);
    drop(progress);
    service.stop().await;
    let store = std::sync::Arc::try_unwrap(store).ok().unwrap();
    (directory, store, fixture, epoch, cutoff)
}

#[test]
fn ordinary_verification_prepares_electorate_growth() {
    grow(6, 3, 6, None, false, None);
}
