//! UPnP port mapping for NAT traversal.
//!
//! Discovers a UPnP-capable Internet Gateway Device (IGD) on the local network,
//! requests a TCP port mapping for the node's P2P port, and periodically renews
//! the lease. On shutdown, the mapping is removed so the router's port table
//! stays clean.
//!
//! Modeled after Nano's `port_mapping.cpp` behaviour:
//! - Discover IGD on startup
//! - Map the P2P TCP port with a 1-hour lease
//! - Refresh the mapping every 30 minutes (half the lease)
//! - Query the router for the external IP address
//! - Clean up mappings on shutdown

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use igd_next::PortMappingProtocol;
use tokio::sync::watch;

/// Lease duration requested from the router (seconds).
const LEASE_DURATION_SECS: u32 = 3600;

/// How often to renew the mapping — half the lease duration.
const RENEWAL_INTERVAL: Duration = Duration::from_secs(LEASE_DURATION_SECS as u64 / 2);

/// Maximum number of consecutive renewal failures before giving up.
const MAX_FAILURES: u32 = 10;

/// Current state of the UPnP port mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpnpState {
    /// Discovery is still in progress.
    Searching,
    /// No gateway was found on the network.
    NotFound,
    /// Gateway found but the external address is not publicly routable.
    NonRoutable,
    /// Port is actively mapped. Contains the external address and port.
    Active {
        external_ip: Ipv4Addr,
        external_port: u16,
    },
    /// Mapping failed after retries.
    Failed(String),
    /// UPnP is disabled by configuration.
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
        let (state_tx, state_rx) = watch::channel(UpnpState::Searching);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let task = tokio::spawn(async move {
            run_upnp_loop(local_port, description, state_tx, shutdown_rx).await;
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

    /// Returns the external (public) socket address if a mapping is active.
    pub fn external_address(&self) -> Option<SocketAddrV4> {
        match &*self.state_rx.borrow() {
            UpnpState::Active {
                external_ip,
                external_port,
            } => Some(SocketAddrV4::new(*external_ip, *external_port)),
            _ => None,
        }
    }

    /// Stop the mapper, removing the port mapping from the router.
    pub async fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
        if let Some(task) = self.task.take() {
            let _ = tokio::time::timeout(Duration::from_secs(10), task).await;
        }
    }
}

/// The main UPnP event loop. Runs entirely on a blocking thread pool because
/// the synchronous `igd_next::Gateway` API does actual network I/O.
async fn run_upnp_loop(
    local_port: u16,
    description: String,
    state: watch::Sender<UpnpState>,
    mut shutdown: watch::Receiver<bool>,
) {
    // Step 1: Discover the gateway (blocking I/O)
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

    // Step 2: Check if the external IP is publicly routable (blocking I/O)
    let gw_for_ip = gateway.clone();
    let external_ip = match tokio::task::spawn_blocking(move || gw_for_ip.get_external_ip()).await {
        Ok(Ok(ip)) => ip,
        Ok(Err(e)) => {
            state.send_replace(UpnpState::Failed(format!("cannot get external IP: {e}")));
            tracing::warn!("UPnP: failed to query external IP: {e}");
            return;
        }
        Err(e) => {
            state.send_replace(UpnpState::Failed(format!("task panicked: {e}")));
            return;
        }
    };

    let ipv4 = match external_ip {
        std::net::IpAddr::V4(ip) => ip,
        std::net::IpAddr::V6(_) => {
            state.send_replace(UpnpState::NonRoutable);
            tracing::info!("UPnP: gateway returned IPv6, not supported for UPnP");
            return;
        }
    };

    if !is_public_ipv4(ipv4) {
        state.send_replace(UpnpState::NonRoutable);
        tracing::info!(
            external_ip = %ipv4,
            "UPnP: gateway external IP is not publicly routable (double NAT?)"
        );
        return;
    }

    tracing::info!(external_ip = %ipv4, "UPnP: external IP is publicly routable");

    // Step 3: Add/renew the port mapping in a loop
    let local_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, local_port));
    let mut consecutive_failures: u32 = 0;

    loop {
        // Add port mapping (blocking I/O)
        let gw = gateway.clone();
        let desc = description.clone();
        let map_result = tokio::task::spawn_blocking(move || {
            gw.add_port(
                PortMappingProtocol::TCP,
                local_port,
                local_addr,
                LEASE_DURATION_SECS,
                &desc,
            )
        })
        .await;

        match map_result {
            Ok(Ok(())) => {
                // Refresh external IP (might change)
                let gw_ip = gateway.clone();
                let current_ip = tokio::task::spawn_blocking(move || gw_ip.get_external_ip())
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .and_then(|ip| match ip {
                        std::net::IpAddr::V4(v4) => Some(v4),
                        _ => None,
                    })
                    .unwrap_or(ipv4);

                consecutive_failures = 0;
                state.send_replace(UpnpState::Active {
                    external_ip: current_ip,
                    external_port: local_port,
                });
                tracing::info!(
                    external = %format!("{}:{}", current_ip, local_port),
                    "UPnP: TCP port mapped successfully"
                );
            }
            Ok(Err(e)) => {
                consecutive_failures += 1;
                tracing::warn!(
                    attempt = consecutive_failures,
                    max = MAX_FAILURES,
                    "UPnP: mapping failed: {e}"
                );
                if consecutive_failures >= MAX_FAILURES {
                    state.send_replace(UpnpState::Failed(e.to_string()));
                    tracing::error!("UPnP: giving up after {MAX_FAILURES} consecutive failures");
                    return;
                }
            }
            Err(e) => {
                state.send_replace(UpnpState::Failed(format!("task panicked: {e}")));
                return;
            }
        }

        // Wait for renewal interval or shutdown
        tokio::select! {
            biased;
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    break;
                }
            }
            _ = tokio::time::sleep(RENEWAL_INTERVAL) => {}
        }
    }

    // Cleanup: remove the mapping (blocking I/O)
    let gw = gateway.clone();
    let _ = tokio::task::spawn_blocking(move || {
        match gw.remove_port(PortMappingProtocol::TCP, local_port) {
            Ok(()) => tracing::info!(port = local_port, "UPnP: port mapping removed on shutdown"),
            Err(e) => tracing::warn!(port = local_port, "UPnP: failed to remove mapping: {e}"),
        }
    })
    .await;
}

/// Check if an IPv4 address is publicly routable.
fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    !ip.is_private()
        && !ip.is_loopback()
        && !ip.is_link_local()
        && !ip.is_broadcast()
        && !ip.is_unspecified()
        && !ip.is_documentation()
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
