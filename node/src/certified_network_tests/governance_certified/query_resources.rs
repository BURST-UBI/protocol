//! Query resources over proposals admitted and confirmed by the ordinary network.
use super::*;
use std::{collections::BTreeSet, time::Instant};
mod retained;
pub(super) use retained::{copy_retained, copy_snapshot};

pub(super) fn report(stage: &str, count: usize, humans: usize, started: Instant) {
    let memory: Vec<_> = std::fs::read_to_string("/proc/self/status")
        .unwrap_or_default()
        .lines()
        .filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:"))
        .map(str::to_owned)
        .collect();
    eprintln!(
        "signed_governance_query {}",
        json!({
            "stage":stage,"proposals":count,"humans":humans,"elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
            "memory":memory,"scope":"signed initialized humans on three loopback nodes; scoped validation diagnostic, not release or arbitrary-population qualification"
        })
    );
}

async fn query(
    client: &NodeClient,
    method: &str,
    params: serde_json::Value,
    elapsed_ms: &mut Vec<f64>,
) -> serde_json::Value {
    // Respect the production IP throttle; measure the call separately from pacing.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let started = Instant::now();
    let result = client.rpc_call(method, params).await.unwrap();
    elapsed_ms.push(started.elapsed().as_secs_f64() * 1000.0);
    result
}

pub(super) async fn run(
    clients: &[NodeClient],
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    keys: &[KeyPair],
    params: &ProtocolParams,
    clock: &AtomicU64,
    base: u64,
    manifest: &std::path::Path,
) {
    if std::env::var_os("BURST_GOVERNANCE_QUEUE_PROBE").is_none() {
        return run_queries(clients, stores, keys, params, clock, base, manifest).await;
    }
    let mut samples = 0usize;
    let monitor = async {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let stores = stores.to_vec();
            let cuts = tokio::task::spawn_blocking(move || {
                stores
                    .iter()
                    .enumerate()
                    .map(|(peer, store)| {
                        // Same pinned-cut comparison as payment_load::capture_failure.
                        let _writer = store.tx_begin_write().unwrap();
                        let durable = store.meta_store().pending_confirmations().unwrap();
                        let selected = store.pending_confirmation_batch(None, 256).unwrap();
                        json!({"peer":peer,"generation":store.env().info().last_txn_id,
                        "matches":selected.iter().eq(durable.iter().take(256)),
                        "durable":durable,"selected":selected})
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap();
            samples += 1;
            for cut in cuts {
                assert_eq!(cut["matches"], true, "governance queue cut mismatch: {cut}");
            }
        }
    };
    tokio::select! {
        _ = monitor => unreachable!("queue monitor returned"),
        _ = run_queries(clients, stores, keys, params, clock, base, manifest) => {}
    }
    eprintln!("signed_governance_queue_probe samples={samples} scope=pinned_cut_comparison");
    assert!(samples > 0, "queue diagnostic did not sample");
}

async fn run_queries(
    clients: &[NodeClient],
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    keys: &[KeyPair],
    params: &ProtocolParams,
    clock: &AtomicU64,
    base: u64,
    manifest: &std::path::Path,
) {
    let count: usize = std::env::var("BURST_GOVERNANCE_QUERY_PROPOSALS")
        .unwrap_or_else(|_| "192".into())
        .parse()
        .expect("proposal count");
    assert!(
        (1..=192).contains(&count),
        "bounded governance resource fixture"
    );
    for store in stores {
        let authority = PaymentAuthority::read(store)
            .unwrap()
            .expect("ordinary payment authority");
        assert_eq!(authority.roster.total_humans(), keys.len() as u64);
        let view = store.tx_begin_read().unwrap();
        let electorate = store.read_verified_snapshot(&view).unwrap();
        let expected: BTreeSet<_> = keys
            .iter()
            .map(|key| burst_crypto::derive_address(&key.public))
            .collect();
        assert_eq!(
            electorate.iter().cloned().collect::<BTreeSet<_>>(),
            expected
        );
        assert_eq!(electorate.len(), keys.len());
        drop(view);
        assert_eq!(store.meta_store().payment_epoch().unwrap(), authority.epoch);
    }
    let save =
        |phase: &str, now: u64, expected: &BTreeSet<String>| {
            std::fs::write(manifest, serde_json::to_vec_pretty(&json!({
            "proposals":expected,"proposal_count":count,"human_count":keys.len(),
            "params_bincode_hex":hex::encode(bincode::serialize(params).unwrap()),"base":base,
            "epoch":stores[0].meta_store().payment_epoch().unwrap().to_string(),
            "phase":phase,"now":now,
            "retained":std::env::var("BURST_GOVERNANCE_QUERY_RETAINED").ok(),
            "scope":"signed proposal/support setup; retained mode is not fresh acceptance"
        })).unwrap()).unwrap();
        };
    // Preserve setup inputs even if reconciliation or a later submission fails.
    save("preparing", clock.load(Ordering::SeqCst), &BTreeSet::new());
    let prepared = Instant::now();
    let existing = retained::reconcile(clients, stores, keys, params, base).await;
    assert!(
        existing.len() <= count,
        "retained fixture exceeds requested count"
    );
    let mut expected: BTreeSet<_> = existing.iter().map(|p| p.hash.to_string()).collect();
    assert_eq!(expected.len(), existing.len());
    for n in expected.len()..count {
        let proposal = supported_rate_proposal(
            clients,
            &keys[0],
            &keys[1],
            params,
            base + 20,
            params.brn_rate + n as u128 + 1,
        )
        .await;
        assert!(expected.insert(proposal.hash.to_string()));
        if (n + 1) % 16 == 0 {
            report("prepare", n + 1, keys.len(), prepared);
        }
    }
    save("prepared", clock.load(Ordering::SeqCst), &expected);
    report("prepared", count, keys.len(), prepared);
    if std::env::var("BURST_GOVERNANCE_PREPARE_ONLY").as_deref() == Ok("1") {
        eprintln!("signed_governance_preparation_only proposals={count}; queries not executed");
        return;
    }
    // Exercise both live exploration and the counting boundary. The ordinary
    // node may certify rejection concurrently; record projected phases instead
    // of pretending every read still sees an uncounted proposal.
    let timing = burst_governance::GovernanceTiming::new(
        params,
        &burst_governance::ProposalContent::ParameterChange {
            param: burst_governance::GovernableParam::BrnRate,
            new_value: params.brn_rate + 1,
        },
    );
    let exploration = timing.proposal_end(Timestamp::new(base + 20)).unwrap();
    let counting = timing.counting_at(exploration).unwrap();
    for (stage, at) in [
        ("exploration", exploration.as_secs() + 1),
        ("counting", counting.as_secs()),
    ] {
        save(stage, at, &expected);
        clock.store(at, Ordering::SeqCst);
        let started = Instant::now();
        let mut requests = 0usize;
        let mut projected = 0usize;
        let mut elapsed_ms = Vec::new();
        for _ in 0..4 {
            for client in clients {
                let mut cursor = None;
                let mut seen = BTreeSet::new();
                for _ in 0..=count / 17 {
                    let page = query(
                        client,
                        "governance_proposals",
                        json!({"count":17,"cursor":cursor}),
                        &mut elapsed_ms,
                    )
                    .await;
                    requests += 1;
                    let rows = page["proposals"].as_array().expect("proposal page");
                    assert!(rows.len() <= 17);
                    for row in rows {
                        let hash = row["hash"].as_str().expect("proposal hash").to_owned();
                        assert!(expected.contains(&hash));
                        assert!(seen.insert(hash), "duplicate cursor row");
                        projected += usize::from(row["phase_projected"] == true);
                    }
                    cursor = page["cursor"].as_str().map(str::to_owned);
                    if cursor.is_none() {
                        break;
                    }
                }
                assert!(
                    cursor.is_none(),
                    "pagination did not terminate within input bound"
                );
                assert_eq!(seen, expected);
                let info = query(
                    client,
                    "governance_proposal_info",
                    json!({"hash":expected.first().unwrap()}),
                    &mut elapsed_ms,
                )
                .await;
                assert_eq!(info["hash"].as_str(), expected.first().map(String::as_str));
                requests += 1;
            }
        }
        eprintln!(
            "signed_governance_query_counts {}",
            json!({"stage":stage,"requests":requests,"projected_rows":projected,"request_ms":elapsed_ms})
        );
        report(stage, count, keys.len(), started);
    }
}
