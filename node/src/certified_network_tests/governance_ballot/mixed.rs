//! Real-clock payment/query overlap with the existing signed ballot journey.
use super::*;
use std::{sync::atomic::AtomicUsize, time::Instant};

pub(super) struct Traffic {
    stop: Arc<AtomicBool>,
    phase: Arc<AtomicUsize>,
    counts: Arc<[[AtomicU64; 4]; 2]>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    started: Instant,
}

impl Drop for Traffic {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Traffic {
    pub(super) fn start(
        clients: &[NodeClient],
        params: ProtocolParams,
        signing: Arc<tokio::sync::Mutex<()>>,
    ) -> Self {
        let mut this = Self {
            stop: Arc::new(AtomicBool::new(false)),
            phase: Arc::new(AtomicUsize::new(0)),
            counts: Arc::new(std::array::from_fn(|_| {
                std::array::from_fn(|_| AtomicU64::new(0))
            })),
            tasks: Vec::new(),
            started: Instant::now(),
        };
        for worker in 0..2 {
            let clients: Vec<_> = clients
                .iter()
                .map(|client| {
                    client
                        .clone()
                        .with_local_address(
                            std::net::Ipv4Addr::new(127, 0, 0, 30 + worker as u8).into(),
                        )
                        .unwrap()
                })
                .collect();
            let stop = this.stop.clone();
            let phase = this.phase.clone();
            let counts = this.counts.clone();
            let signing = signing.clone();
            let params = params.clone();
            this.tasks.push(tokio::spawn(async move {
                let key = burst_crypto::keypair_from_seed(&[0; 32]);
                let address = burst_crypto::derive_address(&key.public);
                while !stop.load(Ordering::SeqCst) {
                    let before = phase.load(Ordering::SeqCst);
                    if worker == 0 {
                        // The existing genesis signer has no new-wallet exemption.
                        // Serialize its own chain with proposal/ballot writes.
                        let _signing = signing.lock().await;
                        let burn = payment_load::confirmed_payment(&clients, &key, &params, || {
                            Transaction::Burn(
                                build_burn_tx(&address, &address, 1, Timestamp::now()).unwrap(),
                            )
                        })
                        .await;
                        payment_load::confirmed_payment(&clients, &key, &params, || {
                            Transaction::Receive(
                                build_receive_tx(
                                    &address,
                                    burn.hash.into_tx_hash(),
                                    1,
                                    Timestamp::now(),
                                )
                                .unwrap(),
                            )
                        })
                        .await;
                    } else {
                        for client in &clients {
                            let telemetry = payment_load::admitted_read(|| {
                                client.rpc_call("telemetry", json!({}))
                            })
                            .await;
                            assert_eq!(telemetry["network"], "dev");
                            let constitution = payment_load::admitted_read(|| {
                                client.rpc_call("consti", json!({}))
                            })
                            .await;
                            assert!(matches!(constitution["version"].as_u64(), Some(0 | 1)));
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                    if before == phase.load(Ordering::SeqCst) {
                        counts[worker][before].fetch_add(1, Ordering::SeqCst);
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }));
        }
        this
    }

    pub(super) fn phase(&self, phase: usize) {
        assert!(phase < 4);
        self.phase.store(phase, Ordering::SeqCst);
    }

    pub(super) async fn progress(&self, phase: usize) {
        tokio::time::timeout(Duration::from_secs(25), async {
            while self
                .counts
                .iter()
                .any(|worker| worker[phase].load(Ordering::SeqCst) < 2)
            {
                assert!(
                    self.tasks.iter().all(|task| !task.is_finished()),
                    "real-clock mixed worker stopped"
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("real-clock mixed phase did not progress within25s");
    }

    pub(super) async fn finish(mut self) {
        self.progress(3).await;
        while self.started.elapsed() < Duration::from_secs(60) {
            assert!(self.tasks.iter().all(|task| !task.is_finished()));
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.stop.store(true, Ordering::SeqCst);
        while let Some(task) = self.tasks.pop() {
            task.await.unwrap();
        }
        let counts: Vec<Vec<u64>> = self
            .counts
            .iter()
            .map(|worker| {
                worker
                    .iter()
                    .map(|count| count.load(Ordering::SeqCst))
                    .collect()
            })
            .collect();
        eprintln!("real_clock_governance_mixed elapsed_ms={} counts={counts:?} scope=short_governance_windows_zero_work_genesis_payments", self.started.elapsed().as_millis());
    }
}
