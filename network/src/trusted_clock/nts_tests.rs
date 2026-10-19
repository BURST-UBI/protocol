use super::*;
use ntp_proto::{
    FilterAction, FilterList, KeyExchangeServer, KeySetProvider, NtpClock, NtpVersion,
    NtsServerConfig, Server, ServerAction, ServerConfig, ServerReason, ServerResponse,
    ServerStatHandler, SystemSnapshot, TimeSnapshot,
};
use tokio::{net::TcpListener, task::JoinHandle};

struct Stats;
impl ServerStatHandler for Stats {
    fn register(&mut self, _: u8, _: bool, _: ServerReason, _: ServerResponse) {}
}

#[derive(Clone, Copy)]
enum Reply {
    Valid,
    CorruptOnly,
    CorruptAndReplayThenValid,
    Unsynchronized,
}

struct Fixture {
    endpoint: NtsSource,
    client: NtsClientConfig,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Fixture {
    async fn start(reply: Reply) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let endpoint = NtsSource {
            operator: "test".into(),
            server_name: "localhost".into(),
            port: listener.local_addr().unwrap().port(),
        };
        let client = NtsClientConfig {
            certificates: Arc::new([cert.cert.der().clone()]),
            ..Default::default()
        };
        let ke = KeyExchangeServer::new(NtsServerConfig {
            certificate_chain: vec![cert.cert.der().clone()],
            private_key: rustls::pki_types::PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der())
                .into(),
            accepted_versions: vec![NtpVersion::V4],
            server: Some("127.0.0.1".into()),
            port: Some(udp.local_addr().unwrap().port()),
            pool_authentication_tokens: vec![],
        })
        .unwrap();
        let keys = KeySetProvider::new(1).get();
        let ke_keys = keys.clone();
        let tcp_task = tokio::spawn(async move {
            while let Ok((io, _)) = listener.accept().await {
                let _ = ke.handle_connection(io, &ke_keys, || None::<()>).await;
            }
        });
        let config = ServerConfig {
            denylist: FilterList {
                filter: vec![],
                action: FilterAction::Deny,
            },
            allowlist: FilterList {
                filter: vec!["127.0.0.0/8".parse().unwrap()],
                action: FilterAction::Ignore,
            },
            rate_limiting_cache_size: 0,
            rate_limiting_cutoff: Duration::ZERO,
            require_nts: Some(FilterAction::Ignore),
            accepted_versions: vec![NtpVersion::V4],
        };
        let snapshot = SystemSnapshot {
            stratum: 1,
            time_snapshot: TimeSnapshot {
                leap_indicator: if matches!(reply, Reply::Unsynchronized) {
                    NtpLeapIndicator::Unknown
                } else {
                    NtpLeapIndicator::NoWarning
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let mut server = Server::new(config, ReadOnlyClock::default(), snapshot, keys);
        let udp_task = tokio::spawn(async move {
            let mut input = [0; 2048];
            let mut output = [0; 2048];
            let mut previous = Vec::new();
            while let Ok((len, addr)) = udp.recv_from(&mut input).await {
                let recv = timestamp(SystemTime::now()).unwrap();
                if let ServerAction::Respond { message } =
                    server.handle(addr.ip(), recv, &input[..len], &mut output, &mut Stats)
                {
                    if matches!(reply, Reply::CorruptOnly | Reply::CorruptAndReplayThenValid) {
                        let mut corrupt = message.to_vec();
                        // The fixed header is authenticated too. Alter its time
                        // without recomputing the NTS authenticator.
                        corrupt[40] ^= 0x40;
                        udp.send_to(&corrupt, addr).await.unwrap();
                    }
                    if matches!(reply, Reply::CorruptAndReplayThenValid) && !previous.is_empty() {
                        udp.send_to(&previous, addr).await.unwrap();
                    }
                    if !matches!(reply, Reply::CorruptOnly) {
                        udp.send_to(message, addr).await.unwrap();
                    }
                    previous = message.to_vec();
                }
            }
        });
        Self {
            endpoint,
            client,
            tasks: vec![tcp_task, udp_task],
        }
    }
}

#[tokio::test]
async fn authenticated_tls_and_nts_udp_survive_tampering_and_replay() {
    let fixture = Fixture::start(Reply::CorruptAndReplayThenValid).await;
    let mut session = timeout(
        EXCHANGE_TIMEOUT,
        Session::connect(&fixture.endpoint, &fixture.client),
    )
    .await
    .unwrap()
    .unwrap();
    for _ in 0..2 {
        let reading = timeout(QUERY_TIMEOUT, session.query())
            .await
            .unwrap()
            .unwrap();
        let wall_ns = reading.wall.duration_since(UNIX_EPOCH).unwrap().as_nanos() as i128;
        assert!((reading.utc_ns - wall_ns).abs() < SECOND_NS);
        assert!(reading.uncertainty_ns >= SECOND_NS as u64);
        assert!(reading.uncertainty_ns < 5 * SECOND_NS as u64);
    }
}

#[tokio::test]
async fn no_observation_from_bad_mac_or_unsynchronized_server() {
    let fixture = Fixture::start(Reply::CorruptOnly).await;
    let mut session = Session::connect(&fixture.endpoint, &fixture.client)
        .await
        .unwrap();
    assert!(timeout(Duration::from_millis(150), session.query())
        .await
        .is_err());
    assert!(session.capture.0.lock().unwrap().is_none());
    let fixture = Fixture::start(Reply::Unsynchronized).await;
    let mut session = Session::connect(&fixture.endpoint, &fixture.client)
        .await
        .unwrap();
    assert!(timeout(QUERY_TIMEOUT, session.query())
        .await
        .unwrap()
        .is_err());
}

#[tokio::test]
async fn certificate_trust_and_hostname_are_enforced() {
    let fixture = Fixture::start(Reply::Valid).await;
    assert!(timeout(
        EXCHANGE_TIMEOUT,
        Session::connect(&fixture.endpoint, &NtsClientConfig::default())
    )
    .await
    .unwrap()
    .is_err());
    let client = KeyExchangeClient::new(&fixture.client).unwrap();
    let tcp = TcpStream::connect(("127.0.0.1", fixture.endpoint.port))
        .await
        .unwrap();
    assert!(timeout(
        EXCHANGE_TIMEOUT,
        client.exchange_keys(tcp, "incorrect.example".into(), [])
    )
    .await
    .unwrap()
    .is_err());
}

#[test]
fn duplicate_hosts_and_operators_cannot_be_counted_as_independent() {
    let sources = || {
        (0..3)
            .map(|i| NtsSource {
                operator: format!("operator{i}"),
                server_name: format!("nts{i}.example"),
                port: 4460,
            })
            .collect::<Vec<_>>()
    };
    assert!(NtsClock::new(sources()).is_ok());
    let mut duplicate = sources();
    duplicate[1].operator = " OPERATOR0 ".into();
    assert!(NtsClock::new(duplicate).is_err());
    let mut duplicate = sources();
    duplicate[1].server_name = "NTS0.EXAMPLE.".into();
    assert!(NtsClock::new(duplicate).is_err());
    let mut invalid = sources();
    invalid[1].server_name = "http://host/path".into();
    assert!(NtsClock::new(invalid).is_err());
}

#[test]
fn adapter_cannot_mutate_system_time_or_claim_unobserved_precision() {
    let clock = ReadOnlyClock::default();
    assert!(clock.now().is_ok());
    assert!(clock.set_frequency(0.0).is_err());
    assert!(clock.get_frequency().is_err());
    assert!(clock.step_clock(NtpDuration::ZERO).is_err());
    assert!(clock.disable_ntp_algorithm().is_err());
    assert!(clock
        .error_estimate_update(NtpDuration::ZERO, NtpDuration::ZERO)
        .is_err());
    assert!(clock.status_update(NtpLeapIndicator::NoWarning).is_err());
}

#[test]
fn measurement_bounds_reject_clock_steps_bad_strata_and_large_root_error() {
    let sent = Instant::now();
    let send_wall = UNIX_EPOCH + Duration::from_secs(1000);
    let received = sent + Duration::from_millis(100);
    let wall = send_wall + Duration::from_millis(100);
    let sample = Sample {
        delay: NtpDuration::from_seconds(0.1),
        offset: NtpDuration::from_seconds(-0.2),
        localtime: timestamp(wall).unwrap(),
        monotime: NtpInstant::now(),
        stratum: 2,
        root_delay: NtpDuration::from_seconds(0.001),
        root_dispersion: NtpDuration::from_seconds(0.002),
        leap: NtpLeapIndicator::NoWarning,
        precision: -20,
    };
    let valid =
        AuthenticatedObservation::from_sample(sample, sent, send_wall, received, wall).unwrap();
    assert!((valid.utc_ns - (999 * SECOND_NS + 900_000_000)).abs() < 10);
    assert!(valid.uncertainty_ns > 1_104_000_000);
    for stratum in [0, 16, 255] {
        assert!(AuthenticatedObservation::from_sample(
            Sample { stratum, ..sample },
            sent,
            send_wall,
            received,
            wall
        )
        .is_err());
    }
    for invalid in [
        Sample {
            root_delay: NtpDuration::MAX,
            ..sample
        },
        Sample {
            root_dispersion: NtpDuration::from_seconds(-1.0),
            ..sample
        },
        Sample {
            delay: NtpDuration::from_seconds(-1.0),
            ..sample
        },
        Sample {
            offset: NtpDuration::MAX,
            ..sample
        },
        Sample {
            precision: 10,
            ..sample
        },
    ] {
        assert!(
            AuthenticatedObservation::from_sample(invalid, sent, send_wall, received, wall)
                .is_err()
        );
    }
    assert!(AuthenticatedObservation::from_sample(
        sample,
        sent,
        send_wall,
        received,
        wall + Duration::from_secs(1)
    )
    .is_err());
    assert!(AuthenticatedObservation::from_sample(
        sample,
        sent,
        send_wall,
        received,
        send_wall - Duration::from_secs(1)
    )
    .is_err());
}
