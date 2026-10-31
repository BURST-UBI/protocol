//! UPnP discovery and finite TCP port leases for the node's P2P port.
//! Renews at half the requested lease duration and retries failed sessions.
//! Gateway-reported mapping success is not an external reachability test.
//! Shutdown attempts removal; router failure can leave the lease until expiry.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::Duration;

use igd_next::PortMappingProtocol;
use tokio::sync::watch;

/// Lease duration requested from the router (seconds).
const LEASE_DURATION_SECS: u32 = 3600;

/// Lease and retry policy. Short leases also make lifecycle tests deterministic.
#[derive(Clone, Copy, Debug)]
pub struct PortMapperConfig {
    pub lease_duration_secs: u32,
    pub retry_interval: Duration,
}

impl Default for PortMapperConfig {
    fn default() -> Self {
        Self {
            lease_duration_secs: LEASE_DURATION_SECS,
            retry_interval: Duration::from_secs(30),
        }
    }
}

/// Current state of the UPnP port mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpnpState {
    /// Discovery is still in progress.
    Searching,
    /// The latest gateway search failed; does not prove no gateway exists.
    NotFound,
    /// Gateway returned IPv6 or an IPv4 address rejected by the local filter.
    NonRoutable,
    /// Gateway accepted the mapping; contains its reported address and port.
    Active {
        external_ip: Ipv4Addr,
        external_port: u16,
    },
    /// Mapping/query failed; discovery retries after the configured fixed delay.
    Failed(String),
    /// Mapper task has stopped.
    Disabled,
}

/// Handle to the running UPnP background task.
pub struct PortMapper {
    state_rx: watch::Receiver<UpnpState>,
    shutdown_tx: Option<watch::Sender<bool>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl PortMapper {
    /// Create and start the UPnP port mapper.
    ///
    /// - `local_port`: the local TCP port the node is listening on.
    /// - `description`: human-readable label for the mapping (shown in router UI).
    pub fn start(local_port: u16, description: String) -> Self {
        Self::start_with_config(local_port, description, PortMapperConfig::default())
    }

    /// Start with a requested finite lease, clamped to at least two seconds.
    /// Renew at half that duration; retry delay is at least one second. The router
    /// must honor lease expiry for crash cleanup; permanent leases are not requested.
    pub fn start_with_config(
        local_port: u16,
        description: String,
        mut config: PortMapperConfig,
    ) -> Self {
        config.lease_duration_secs = config.lease_duration_secs.max(2);
        config.retry_interval = config.retry_interval.max(Duration::from_secs(1));
        let (state_tx, state_rx) = watch::channel(UpnpState::Searching);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let task = tokio::spawn(async move {
            run_upnp_loop(local_port, description, config, state_tx, shutdown_rx).await;
        });

        Self {
            state_rx,
            shutdown_tx: Some(shutdown_tx),
            task: Some(task),
        }
    }

    /// Current UPnP state.
    pub fn state(&self) -> UpnpState {
        self.state_rx.borrow().clone()
    }

    /// Subscribe to UPnP state changes.
    pub fn subscribe(&self) -> watch::Receiver<UpnpState> {
        self.state_rx.clone()
    }

    /// Return the address from the latest Active state; no connectivity probe.
    pub fn external_address(&self) -> Option<SocketAddrV4> {
        match &*self.state_rx.borrow() {
            UpnpState::Active {
                external_ip,
                external_port,
            } => Some(SocketAddrV4::new(*external_ip, *external_port)),
            _ => None,
        }
    }

    /// Request shutdown and join the mapper, including its removal attempt.
    /// Router and task failures are logged or ignored, not returned to the caller.
    pub async fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
        if let Some(task) = self.task.take() {
            // Do not detach a task that may still create a mapping after stop.
            let _ = task.await;
        }
    }
}

impl Drop for PortMapper {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
    }
}

async fn stopped_or_wait(shutdown: &mut watch::Receiver<bool>, delay: Duration) -> bool {
    if *shutdown.borrow() {
        return true;
    }
    tokio::select! {
        _ = shutdown.changed() => true,
        _ = tokio::time::sleep(delay) => false,
    }
}

/// Select the interface the OS routes to the IGD. UDP connect selects a route
/// without transmitting a datagram; 0.0.0.0 is a bind address, never a LAN host.
fn gateway_local_address(gateway: SocketAddr, port: u16) -> std::io::Result<SocketAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect(gateway)?;
    let ip = socket.local_addr()?.ip();
    if ip.is_unspecified() {
        return Err(std::io::Error::other("no routed LAN address"));
    }
    Ok(SocketAddr::new(ip, port))
}

/// Runs gateway I/O on blocking workers; waits and shutdown stay asynchronous.
async fn run_upnp_loop(
    local_port: u16,
    description: String,
    config: PortMapperConfig,
    state: watch::Sender<UpnpState>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            break;
        }
        run_gateway_session(local_port, &description, config, &state, &mut shutdown).await;
        if stopped_or_wait(&mut shutdown, config.retry_interval).await {
            break;
        }
    }
    state.send_replace(UpnpState::Disabled);
}

async fn run_gateway_session(
    local_port: u16,
    description: &str,
    config: PortMapperConfig,
    state: &watch::Sender<UpnpState>,
    shutdown: &mut watch::Receiver<bool>,
) {
    let gateway = match tokio::task::spawn_blocking(|| {
        igd_next::search_gateway(igd_next::SearchOptions {
            timeout: Some(Duration::from_secs(5)),
            ..Default::default()
        })
    })
    .await
    {
        Ok(Ok(gw)) => gw,
        Ok(Err(e)) => {
            state.send_replace(UpnpState::NotFound);
            tracing::info!("UPnP: no IGD gateway found: {e}");
            return;
        }
        Err(e) => {
            state.send_replace(UpnpState::Failed(format!("task panicked: {e}")));
            return;
        }
    };

    tracing::info!(gateway = %gateway.addr, "UPnP: gateway discovered");

    let local_addr = match gateway_local_address(gateway.addr, local_port) {
        Ok(addr) => addr,
        Err(e) => {
            state.send_replace(UpnpState::Failed(e.to_string()));
            return;
        }
    };
    let mut mapped_port = None;
    loop {
        if *shutdown.borrow() {
            break;
        }
        // Revalidate on EVERY renewal. Never advertise an old public IP after
        // the WAN changes to CGNAT, or after the gateway stops responding.
        let gw = gateway.clone();
        let ipv4 = match tokio::task::spawn_blocking(move || gw.get_external_ip()).await {
            Ok(Ok(IpAddr::V4(ip))) if is_public_ipv4(ip) => ip,
            Ok(Ok(_)) => {
                state.send_replace(UpnpState::NonRoutable);
                break;
            }
            result => {
                state.send_replace(UpnpState::Failed(format!(
                    "external IP query failed: {result:?}"
                )));
                break;
            }
        };
        if *shutdown.borrow() {
            break;
        }
        let gw = gateway.clone();
        let desc = description.to_owned();
        let requested_port = mapped_port.unwrap_or(local_port);
        let map_result = tokio::task::spawn_blocking(move || {
            match gw.add_port(
                PortMappingProtocol::TCP,
                requested_port,
                local_addr,
                config.lease_duration_secs,
                &desc,
            ) {
                Ok(()) => Ok(requested_port),
                // Another LAN node may already own the default external port.
                // Let the IGD choose a different port; publish that exact port.
                Err(igd_next::AddPortError::PortInUse) if mapped_port.is_none() => gw
                    .add_any_port(
                        PortMappingProtocol::TCP,
                        local_addr,
                        config.lease_duration_secs,
                        &desc,
                    )
                    .map_err(|e| (e.to_string(), false)),
                // If ownership changed after a router restart, do not delete
                // the new owner's mapping while cleaning up our expired lease.
                Err(igd_next::AddPortError::PortInUse) => {
                    Err(("mapping now belongs to another client".into(), true))
                }
                Err(e) => Err((e.to_string(), false)),
            }
        })
        .await;

        match map_result {
            Ok(Ok(port)) => {
                mapped_port = Some(port);
                state.send_replace(UpnpState::Active {
                    external_ip: ipv4,
                    external_port: port,
                });
                tracing::info!(
                    external = %format!("{}:{}", ipv4, port),
                    "UPnP: TCP port mapped successfully"
                );
            }
            Ok(Err((e, lost_ownership))) => {
                if lost_ownership {
                    mapped_port = None;
                }
                state.send_replace(UpnpState::Failed(e));
                break;
            }
            Err(e) => {
                state.send_replace(UpnpState::Failed(format!("task panicked: {e}")));
                break;
            }
        }

        if stopped_or_wait(
            shutdown,
            Duration::from_secs(config.lease_duration_secs as u64 / 2),
        )
        .await
        {
            break;
        }
    }

    // Only remove a mapping that we actually created, on all exit paths.
    if let Some(port) = mapped_port {
        let _ = tokio::task::spawn_blocking(move || {
            match gateway.remove_port(PortMappingProtocol::TCP, port) {
                Ok(()) => tracing::info!(port, "UPnP: port mapping removed"),
                Err(e) => tracing::warn!(
                    port,
                    "UPnP: failed to remove mapping: {e}; finite lease will expire"
                ),
            }
        })
        .await;
    }
}

/// Exclude the special-use ranges listed below from advertisements.
/// This address filter does not test routing, NAT or inbound connectivity.
pub fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    !ip.is_private()
        && !ip.is_loopback()
        && !ip.is_link_local()
        && !ip.is_broadcast()
        && !ip.is_unspecified()
        && !ip.is_documentation()
        && !ip.is_multicast()
        && ip.octets()[0] != 0
        && ip.octets()[0] < 240
        && !(ip.octets()[0] == 198 && (ip.octets()[1] == 18 || ip.octets()[1] == 19))
        && !(ip.octets()[0] == 192 && ip.octets()[1] == 0 && ip.octets()[2] == 0)
        // 100.64.0.0/10 (Carrier-Grade NAT)
        && !(ip.octets()[0] == 100 && (ip.octets()[1] & 0xC0) == 64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_ip_detection() {
        assert!(is_public_ipv4(Ipv4Addr::new(8, 8, 8, 8)));
        assert!(is_public_ipv4(Ipv4Addr::new(1, 1, 1, 1)));
        assert!(!is_public_ipv4(Ipv4Addr::new(192, 168, 1, 1)));
        assert!(!is_public_ipv4(Ipv4Addr::new(10, 0, 0, 1)));
        assert!(!is_public_ipv4(Ipv4Addr::new(172, 16, 0, 1)));
        assert!(!is_public_ipv4(Ipv4Addr::LOCALHOST));
        assert!(!is_public_ipv4(Ipv4Addr::new(169, 254, 1, 1)));
        assert!(!is_public_ipv4(Ipv4Addr::new(100, 64, 0, 1)));
        assert!(!is_public_ipv4(Ipv4Addr::new(100, 127, 255, 254)));
        assert!(!is_public_ipv4(Ipv4Addr::BROADCAST));
        assert!(!is_public_ipv4(Ipv4Addr::UNSPECIFIED));
    }

    #[test]
    fn documentation_range_not_public() {
        assert!(!is_public_ipv4(Ipv4Addr::new(192, 0, 2, 1)));
        assert!(!is_public_ipv4(Ipv4Addr::new(198, 51, 100, 1)));
        assert!(!is_public_ipv4(Ipv4Addr::new(203, 0, 113, 1)));
    }

    #[test]
    fn special_use_ranges_are_not_advertised() {
        for ip in [
            "0.1.2.3",
            "192.0.0.1",
            "198.18.0.1",
            "198.19.255.254",
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
        ] {
            assert!(!is_public_ipv4(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn selects_routed_interface_not_unspecified_bind_address() {
        let addr = gateway_local_address("127.0.0.1:1901".parse().unwrap(), 7076).unwrap();
        assert_eq!(addr, "127.0.0.1:7076".parse().unwrap());
    }

    #[test]
    fn state_equality() {
        assert_eq!(UpnpState::Disabled, UpnpState::Disabled);
        assert_eq!(UpnpState::Searching, UpnpState::Searching);
        assert_ne!(UpnpState::NotFound, UpnpState::Searching);
    }

    #[test]
    fn active_state_contains_address() {
        let state = UpnpState::Active {
            external_ip: Ipv4Addr::new(1, 2, 3, 4),
            external_port: 17076,
        };
        match state {
            UpnpState::Active {
                external_ip,
                external_port,
            } => {
                assert_eq!(external_ip, Ipv4Addr::new(1, 2, 3, 4));
                assert_eq!(external_port, 17076);
            }
            _ => panic!("expected Active state"),
        }
    }
}
