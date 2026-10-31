//! UPnP mapping probe intended for the isolated gateway lab. Requests a TCP
//! mapping for local port 7076; it does not start a listener or RPC service.
//! The program itself does not enforce network isolation.
//!
//! BURST_PROBE_LEASE_SECS defaults to 3600. BURST_PROBE_HOLD_SECS defaults to
//! zero, which returns on the first active or terminal failure state. A nonzero
//! hold observes until its deadline and succeeds if a mapping was ever active,
//! even if it later failed. Shutdown requests cleanup, not proof of deletion
//! or external reachability.
use burst_network::{PortMapper, PortMapperConfig, UpnpState};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let hold_secs = std::env::var("BURST_PROBE_HOLD_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let lease_duration_secs = std::env::var("BURST_PROBE_LEASE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3600);
    let mut mapper = PortMapper::start_with_config(
        7076,
        "BURST isolated NAT validation".into(),
        PortMapperConfig {
            lease_duration_secs,
            retry_interval: Duration::from_secs(2),
        },
    );
    let mut states = mapper.subscribe();
    let result = tokio::time::timeout(Duration::from_secs(12 + hold_secs), async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(hold_secs);
        let mut ever_active = false;
        loop {
            let state = states.borrow().clone();
            println!("UPnP state: {state:?}");
            if hold_secs != 0 {
                ever_active |= matches!(state, UpnpState::Active { .. });
                tokio::select! {
                    result = states.changed() => result.map_err(|e| e.to_string())?,
                    _ = tokio::time::sleep_until(deadline) => return if ever_active { Ok(()) } else { Err("mapping never became active".into()) },
                }
                continue;
            }
            match state {
                UpnpState::Active { .. } => return Ok(()),
                UpnpState::NotFound | UpnpState::NonRoutable | UpnpState::Failed(_) =>
                    return Err(format!("UPnP did not establish a mapping: {state:?}")),
                _ => {}
            }
            states.changed().await.map_err(|e| e.to_string())?;
        }
    }).await;
    mapper.stop().await;
    result??;
    println!("UPnP mapping became active; shutdown cleanup requested");
    Ok(())
}
