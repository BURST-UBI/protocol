//! Controlled-clock overlap coverage; production throughput is a separate gate.
use super::*;
use std::sync::atomic::AtomicUsize;
use std::time::Instant;

type Progress = Arc<[[AtomicU64; 4]; 3]>;

pub(super) struct Traffic {
    stop: Arc<AtomicBool>,
    phase: Arc<AtomicUsize>,
    progress: Progress,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    started: Instant,
}

impl Drop for Traffic {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for task in &self.tasks { task.abort(); }
    }
}

impl Traffic {
    pub(super) async fn start(
        clients: &[NodeClient], stores: &[Arc<burst_store_lmdb::LmdbStore>],
        params: &ProtocolParams, clock: &Arc<AtomicU64>, target: &WalletAddress,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let phase = Arc::new(AtomicUsize::new(0));
        let progress: Progress = Arc::new(std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))));
        let mut traffic = Self { stop, phase, progress, tasks: Vec::new(), started: Instant::now() };
        for worker in 0..2 {
            let clients = independent_clients(clients, 20 + worker as u8);
            let stores = stores.to_vec();
            let params = params.clone();
            let clock = clock.clone();
            let stop = traffic.stop.clone();
            let phase = traffic.phase.clone();
            let progress = traffic.progress.clone();
            traffic.tasks.push(tokio::spawn(async move {
                let key = burst_crypto::keypair_from_seed(&[5 + worker as u8; 32]);
                let address = burst_crypto::derive_address(&key.public);
                let before = stores[0].account_store().get_account(&address).unwrap();
                let mut pairs = 0u64;
                while !stop.load(Ordering::SeqCst) {
                    let phase_before = phase.load(Ordering::SeqCst);
                    let burn = confirmed_payment(&clients, &stores[0], &key, &params, &clock, |now| {
                        Transaction::Burn(build_burn_tx(&address, &address, 1, now).unwrap())
                    }).await;
                    // The traffic clock trails the fixed node clock by at most
                    // one second; its origins outlive the scenario.
                    let now = clock.load(Ordering::SeqCst);
                    assert!(now < burn.timestamp.as_secs() + params.trst_expiry_secs);
                    confirmed_payment(&clients, &stores[0], &key, &params, &clock, |now| {
                        Transaction::Receive(build_receive_tx(&address, burn.hash.into_tx_hash(), 1, now).unwrap())
                    }).await;
                    pairs += 1;
                    if phase.load(Ordering::SeqCst) == phase_before {
                        progress[worker][phase_before].fetch_add(1, Ordering::SeqCst);
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                for store in &stores {
                    let after = store.account_store().get_account(&address).unwrap();
                    assert_eq!(after.block_count, before.block_count + pairs * 2);
                    assert_eq!(after.confirmation_height, after.block_count);
                    assert_eq!(after.total_brn_burned, before.total_brn_burned + u128::from(pairs));
                    assert_eq!(after.trst_balance, before.trst_balance + u128::from(pairs));
                    assert_eq!(after.representative, before.representative);
                    assert!(after.state.is_verified());
                    assert!(store.pending_store().get_pending_for_account(&address).unwrap().is_empty());
                }
                eprintln!("mixed_lifecycle payments worker={worker} confirmed_pairs={pairs}");
            }));
        }
        let clients = independent_clients(clients, 22);
        let target = target.clone();
        let stop = traffic.stop.clone();
        let phase = traffic.phase.clone();
        let progress = traffic.progress.clone();
        traffic.tasks.push(tokio::spawn(async move {
            while !stop.load(Ordering::SeqCst) {
                let phase_before = phase.load(Ordering::SeqCst);
                for client in &clients {
                    let telemetry = payment_load::admitted_read(|| client.rpc_call("telemetry", json!({}))).await;
                    assert_eq!(telemetry["network"], "dev");
                    payment_load::admitted_read(|| client.rpc_call("verification_status", json!({"account":target.as_str()}))).await;
                    payment_load::admitted_read(|| client.rpc_call("trst_portfolio", json!({"account":target.as_str()}))).await;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                if phase.load(Ordering::SeqCst) == phase_before {
                    progress[2][phase_before].fetch_add(1, Ordering::SeqCst);
                }
            }
        }));
        traffic
    }

    pub(super) fn set_phase(&self, phase: usize) {
        assert!(phase < 4);
        self.phase.store(phase, Ordering::SeqCst);
        eprintln!("mixed_lifecycle phase={phase} elapsed_ms={}", self.started.elapsed().as_millis());
    }

    pub(super) async fn wait_for_progress(&self, phase: usize) {
        tokio::time::timeout(Duration::from_secs(25), async {
            while self.progress.iter().any(|worker| worker[phase].load(Ordering::SeqCst) < 2) {
                assert!(self.tasks.iter().all(|task| !task.is_finished()), "mixed worker stopped before phase progress");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }).await.expect("mixed workload failed to progress within the existing25s payment bound");
    }

    pub(super) async fn finish(mut self) {
        self.wait_for_progress(3).await;
        // Keep workers running for at least one minute across the lifecycle.
        while self.started.elapsed() < Duration::from_secs(60) {
            assert!(self.tasks.iter().all(|task| !task.is_finished()), "mixed worker stopped during sustained interval");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.stop.store(true, Ordering::SeqCst);
        while let Some(task) = self.tasks.pop() { task.await.unwrap(); }
        let counts: Vec<Vec<u64>> = self.progress.iter().map(|worker|
            worker.iter().map(|count| count.load(Ordering::SeqCst)).collect()).collect();
        eprintln!("mixed_lifecycle completed elapsed_ms={} phase_counts={counts:?} scope=controlled_clock_loopback", self.started.elapsed().as_millis());
    }
}

fn independent_clients(clients: &[NodeClient], suffix: u8) -> Vec<NodeClient> {
    clients.iter().map(|client| client.clone().with_local_address(
        std::net::Ipv4Addr::new(127, 0, 0, suffix).into()).unwrap()).collect()
}

async fn confirmed_payment(
    clients: &[NodeClient], store: &burst_store_lmdb::LmdbStore,
    key: &KeyPair, params: &ProtocolParams, clock: &AtomicU64,
    transaction: impl Fn(Timestamp) -> Transaction,
) -> StateBlock {
    payment_load::confirmed_payment(&clients, key, params, || {
        // This exact-expiry fixture deliberately freezes node time. Keep traffic
        // inside its closed second so activation can preserve every confirmed
        // head. Real-clock sustained throughput remains a separate gate.
        let cutoff = store.meta_store()
            .get_meta(crate::confirmed_replay::installation::CUTOFF_KEY).unwrap();
        let cutoff = u64::from_be_bytes(cutoff.as_slice().try_into().unwrap());
        let now = clock.load(Ordering::SeqCst);
        assert!(cutoff <= now, "installed cutoff exceeds controlled clock");
        let timestamp = now.saturating_sub(1).max(cutoff);
        transaction(Timestamp::new(timestamp))
    }).await
}
