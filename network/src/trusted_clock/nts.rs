//! NTS key exchange, authenticated UDP sampling and signing-time observations.
//! ntpd-rs validates packets; this client applies the additional uncertainty and
//! clock-continuity checks required by TrustedClock. It does not discipline time.

use ntp_proto::{
    KeyExchangeClient, NtpDuration, NtpInstant, NtpLeapIndicator, NtpSource, NtpSourceAction,
    NtsClientConfig, SourceConfig, System,
};
use std::{
    collections::HashSet,
    io,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::{TcpStream, UdpSocket},
    sync::watch,
    task::JoinSet,
    time::timeout,
};

use super::{ClockError, TrustedClock, MAX_WALL_DIVERGENCE, SECOND_NS};
#[path = "measurement.rs"]
mod measurement;
use measurement::{timestamp, CaptureController, CaptureSource, ReadOnlyClock, Sample};

const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(10);
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_secs(64);
const SESSION_LIFETIME: Duration = Duration::from_secs(3600);

/// An NTS endpoint and its declared operator. Configuration rejects repeated
/// operator labels and hostnames, but cannot prove operational independence.
/// Choose independently administered sources; aliases do not add fault tolerance.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NtsSource {
    pub operator: String,
    pub server_name: String,
    pub port: u16,
}

pub struct NtsClock {
    clock: Arc<TrustedClock>,
    sources: Vec<NtsSource>,
}

impl NtsClock {
    /// Default endpoint names and declared operator labels. Availability and
    /// operational independence must be assessed for the deployment.
    pub fn public_sources() -> Vec<NtsSource> {
        [
            ("cloudflare", "time.cloudflare.com"),
            ("netnod", "nts.netnod.se"),
            ("sidn", "ntppool1.time.nl"),
            ("canonical", "1.ntp.ubuntu.com"),
        ]
        .into_iter()
        .map(|(operator, server_name)| NtsSource {
            operator: operator.into(),
            server_name: server_name.into(),
            port: 4460,
        })
        .collect()
    }

    pub fn new(sources: Vec<NtsSource>) -> Result<Self, ClockError> {
        let clock = Arc::new(TrustedClock::new(sources.len())?);
        let mut operators = HashSet::new();
        let mut hosts = HashSet::new();
        for source in &sources {
            if source.operator.trim().is_empty()
                || source.operator.len() > 128
                || source.server_name.len() > 253
                || source.port == 0
                || rustls::pki_types::DnsName::try_from(source.server_name.as_str()).is_err()
                || !operators.insert(source.operator.trim().to_ascii_lowercase())
                || !hosts.insert(
                    source
                        .server_name
                        .trim_end_matches('.')
                        .to_ascii_lowercase(),
                )
            {
                return Err(ClockError::SourceCount);
            }
        }
        // Respect any provider already selected by the application. The pinned
        // ntpd-rs API otherwise requires a process-wide rustls provider.
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        Ok(Self { clock, sources })
    }

    pub fn clock(&self) -> Arc<TrustedClock> {
        self.clock.clone()
    }

    /// Run one worker per configured source. Shutdown aborts and joins them.
    /// Failed queries discard their session but leave the last accepted sample
    /// to age under TrustedClock's freshness rule. There is no unauthenticated
    /// fallback. Exchange/query deadlines are ten/five seconds; polling waits
    /// at least 64 seconds between attempts.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        if *shutdown.borrow() {
            return;
        }
        let mut workers = JoinSet::new();
        for (index, source) in self.sources.into_iter().enumerate() {
            let clock = self.clock.clone();
            workers.spawn(async move {
                let mut session = None;
                loop {
                    let result = async {
                        if session.as_ref().is_none_or(|s: &Session| s.created.elapsed() > SESSION_LIFETIME) {
                            session = Some(timeout(EXCHANGE_TIMEOUT, Session::connect(&source, &NtsClientConfig::default()))
                                .await.map_err(io::Error::other)??);
                        }
                        let live = session.as_mut().ok_or_else(|| io::Error::other("missing NTS session"))?;
                        let observation = timeout(QUERY_TIMEOUT, live.query()).await.map_err(io::Error::other)??;
                        clock.record(index, observation.utc_ns, observation.uncertainty_ns,
                            observation.received, observation.wall).map_err(io::Error::other)?;
                        tracing::debug!(operator = source.operator,
                            uncertainty_ns = observation.uncertainty_ns, "authenticated time observation accepted");
                        Ok::<_, io::Error>(())
                    }.await;
                    if let Err(error) = result {
                        tracing::debug!(operator = source.operator, %error, "authenticated time source unavailable");
                        session = None;
                    }
                    let wait = session.as_ref().map_or(POLL_INTERVAL, |live|
                        live.source.current_poll_interval().as_system_duration().max(POLL_INTERVAL));
                    tokio::time::sleep(wait).await;
                }
            });
        }
        let mut health = tokio::time::interval(Duration::from_secs(5));
        let mut was_ready = None;
        loop {
            tokio::select! {
                _ = health.tick() => {
                    let reading = self.clock.reading();
                    let ready = reading.is_ok();
                    if was_ready != Some(ready) {
                        match reading {
                            Ok(time) => tracing::info!(error_bound_secs = time.error_bound,
                                "authenticated signing time available"),
                            Err(error) => tracing::warn!(%error,
                                "authenticated signing time unavailable; time-dependent signing is paused"),
                        }
                        was_ready = Some(ready);
                    }
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { break; }
                }
                result = workers.join_next() => {
                    if let Some(Err(error)) = result {
                        tracing::error!(%error, "authenticated time worker stopped");
                    }
                    if workers.is_empty() { break; }
                }
            }
        }
        workers.abort_all();
        while workers.join_next().await.is_some() {}
    }
}

struct Session {
    socket: UdpSocket,
    source: NtpSource<CaptureSource<NtpDuration>>,
    capture: ReadOnlyClock,
    created: Instant,
}

impl Session {
    /// Authenticate key exchange, then use the first resolved address of the
    /// negotiated UDP endpoint. The worker supplies the outer timeout.
    async fn connect(endpoint: &NtsSource, config: &NtsClientConfig) -> io::Result<Self> {
        let tcp = TcpStream::connect((endpoint.server_name.as_str(), endpoint.port)).await?;
        let client = KeyExchangeClient::new(config).map_err(io::Error::other)?;
        let exchange = client
            .exchange_keys(tcp, endpoint.server_name.clone(), [])
            .await
            .map_err(io::Error::other)?;
        let address = tokio::net::lookup_host((exchange.remote.as_str(), exchange.port))
            .await?
            .next()
            .ok_or_else(|| io::Error::other("NTS server has no address"))?;
        let socket = UdpSocket::bind(if address.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        })
        .await?;
        socket.connect(address).await?;
        let capture = ReadOnlyClock::default();
        let mut system = System::<usize, CaptureController>::new(
            capture.clone(),
            Default::default(),
            (),
            Arc::new([]),
        )?;
        let (source, _) = system.create_ntp_source(
            0,
            SourceConfig::default(),
            address,
            exchange.protocol_version,
            Some(exchange.nts),
        )?;
        Ok(Self {
            socket,
            source,
            capture,
            created: Instant::now(),
        })
    }

    /// Accept at most sixteen datagrams while waiting for a validated sample.
    /// Full-buffer packets are discarded; the worker bounds the overall wait.
    async fn query(&mut self) -> io::Result<AuthenticatedObservation> {
        let packet = self
            .source
            .handle_timer()
            .find_map(|action| match action {
                NtpSourceAction::Send(bytes) => Some(bytes),
                _ => None,
            })
            .ok_or_else(|| io::Error::other("NTS source cannot issue a request"))?;
        self.capture
            .0
            .lock()
            .map_err(|_| io::Error::other("NTS capture unavailable"))?
            .take();
        let sent = Instant::now();
        let send_wall = SystemTime::now();
        let send_time = timestamp(send_wall)?;
        self.socket.send(&packet).await?;
        let mut buffer = [0; 2048];
        // Bound work even when the connected endpoint floods invalid packets.
        for _ in 0..16 {
            let len = self.socket.recv(&mut buffer).await?;
            let received = Instant::now();
            let wall = SystemTime::now();
            if len == buffer.len() {
                continue;
            }
            for action in self.source.handle_incoming(
                &buffer[..len],
                NtpInstant::now(),
                send_time,
                timestamp(wall)?,
            ) {
                if matches!(action, NtpSourceAction::Reset | NtpSourceAction::Demobilize) {
                    return Err(io::Error::other("NTS server ended the association"));
                }
            }
            let sample = self
                .capture
                .0
                .lock()
                .map_err(|_| io::Error::other("NTS capture unavailable"))?
                .take();
            if let Some(sample) = sample {
                return AuthenticatedObservation::from_sample(
                    sample, sent, send_wall, received, wall,
                );
            }
        }
        Err(io::Error::other("NTS response budget exhausted"))
    }
}

struct AuthenticatedObservation {
    utc_ns: i128,
    uncertainty_ns: u64,
    received: Instant,
    wall: SystemTime,
}

impl AuthenticatedObservation {
    fn from_sample(
        sample: Sample,
        sent: Instant,
        send_wall: SystemTime,
        received: Instant,
        wall: SystemTime,
    ) -> io::Result<Self> {
        let invalid = || io::Error::other("NTS measurement cannot bound signing time");
        let elapsed = received.checked_duration_since(sent).ok_or_else(invalid)?;
        let wall_elapsed = wall.duration_since(send_wall).map_err(|_| invalid())?;
        let delay = sample.delay.to_seconds();
        let offset = sample.offset.to_seconds();
        let root_delay = sample.root_delay.to_seconds();
        let dispersion = sample.root_dispersion.to_seconds();
        if elapsed > QUERY_TIMEOUT
            || elapsed.abs_diff(wall_elapsed) > MAX_WALL_DIVERGENCE
            || !(1..16).contains(&sample.stratum)
            || sample.leap != NtpLeapIndicator::NoWarning
            || !(-0.001..=5.0).contains(&delay)
            || !(-86400.0..=86400.0).contains(&offset)
            || !(0.0..=2.0).contains(&root_delay)
            || !(0.0..=2.0).contains(&dispersion)
            || sample.precision > 0
        {
            return Err(invalid());
        }
        // A whole observed RTT bounds network asymmetry conservatively. Include
        // root distance, precision, local drift during the query and a one-second
        // scheduling/quantization cushion. This is not a statistical confidence.
        let uncertainty = elapsed.as_secs_f64() * 1.01
            + root_delay
            + dispersion
            + 2_f64.powi(i32::from(sample.precision))
            + 1.0;
        if uncertainty > 5.0 {
            return Err(invalid());
        }
        let unix = wall.duration_since(UNIX_EPOCH).map_err(|_| invalid())?;
        let utc_ns = unix.as_nanos() as i128 + (offset * SECOND_NS as f64).round() as i128;
        Ok(Self {
            utc_ns,
            uncertainty_ns: (uncertainty * SECOND_NS as f64).ceil() as u64,
            received,
            wall,
        })
    }
}

#[cfg(test)]
#[path = "nts_tests.rs"]
mod tests;
