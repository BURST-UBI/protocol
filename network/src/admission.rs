//! Address-based capacity shared by pending handshakes and live connections.
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv6Addr},
    sync::{Arc, Mutex},
};

/// Bucket by IPv4 address or IPv6 /64, independent of claimed node identity.
/// IPv4-mapped IPv6 addresses use the matching IPv4 bucket.
pub fn peer_bucket(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & (u128::MAX << 64))),
        },
        other => other,
    }
}

fn subnet(ip: IpAddr) -> IpAddr {
    match peer_bucket(ip) {
        IpAddr::V4(v4) => IpAddr::V4((u32::from(v4) & 0xffff_ff00).into()),
        IpAddr::V6(v6) => IpAddr::V6((u128::from(v6) & (u128::MAX << 80)).into()),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PeerLimits {
    /// Per IPv4 address or IPv6 /64.
    pub per_ip: usize,
    /// Per IPv4 /24 or IPv6 /48, across both connection directions.
    pub per_subnet: usize,
    /// Reduces the inbound ceiling; outbound still obeys total and address caps.
    /// Effective reserve is capped at max_connections minus one.
    pub outbound_reserved: usize,
}

impl Default for PeerLimits {
    fn default() -> Self {
        Self {
            per_ip: 4,
            per_subnet: 16,
            outbound_reserved: 8,
        }
    }
}

impl PeerLimits {
    pub fn for_test_network(max_peers: usize) -> Self {
        Self {
            per_ip: max_peers.max(1),
            per_subnet: max_peers.max(1),
            outbound_reserved: 0,
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.per_ip == 0 || self.per_subnet < self.per_ip {
            return Err("peer limits require 0 < per_ip <= per_subnet");
        }
        Ok(())
    }
}

#[derive(Default)]
struct Counts {
    total: usize,
    inbound: usize,
    ips: HashMap<IpAddr, usize>,
    subnets: HashMap<IpAddr, usize>,
}

pub struct PeerAdmission {
    max: usize,
    limits: PeerLimits,
    counts: Mutex<Counts>,
}

/// One capacity reservation. Cloning its Arc shares the reservation; only the
/// final drop releases the counts. It does not authenticate a peer identity.
pub struct ConnectionPermit {
    owner: Arc<PeerAdmission>,
    ip: IpAddr,
    subnet: IpAddr,
    outbound: bool,
}

impl PeerAdmission {
    pub fn new(max: usize, limits: PeerLimits) -> Arc<Self> {
        Arc::new(Self {
            max,
            limits,
            counts: Mutex::new(Counts::default()),
        })
    }

    pub fn acquire(
        self: &Arc<Self>,
        address: IpAddr,
        outbound: bool,
    ) -> Option<Arc<ConnectionPermit>> {
        let ip = peer_bucket(address);
        let subnet = subnet(address);
        let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        let reserve = self
            .limits
            .outbound_reserved
            .min(self.max.saturating_sub(1));
        if counts.total >= self.max
            || (!outbound && counts.inbound >= self.max - reserve)
            || counts.ips.get(&ip).copied().unwrap_or(0) >= self.limits.per_ip
            || counts.subnets.get(&subnet).copied().unwrap_or(0) >= self.limits.per_subnet
        {
            return None;
        }
        counts.total += 1;
        counts.inbound += usize::from(!outbound);
        *counts.ips.entry(ip).or_default() += 1;
        *counts.subnets.entry(subnet).or_default() += 1;
        Some(Arc::new(ConnectionPermit {
            owner: self.clone(),
            ip,
            subnet,
            outbound,
        }))
    }
}

impl ConnectionPermit {
    pub fn bucket(&self) -> IpAddr { self.ip }

    /// Match the address bucket and direction, not the exact host or node key.
    pub fn matches(&self, ip: IpAddr, outbound: bool) -> bool {
        self.ip == peer_bucket(ip) && self.outbound == outbound
    }
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        let mut counts = self.owner.counts.lock().unwrap_or_else(|e| e.into_inner());
        counts.total -= 1;
        counts.inbound -= usize::from(!self.outbound);
        if let Some(count) = counts.ips.get_mut(&self.ip) {
            *count -= 1;
            if *count == 0 {
                counts.ips.remove(&self.ip);
            }
        }
        if let Some(count) = counts.subnets.get_mut(&self.subnet) {
            *count -= 1;
            if *count == 0 {
                counts.subnets.remove(&self.subnet);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }

    #[test]
    fn pending_and_live_share_capacity_until_last_owner_drops() {
        let admission = PeerAdmission::new(
            5,
            PeerLimits {
                per_ip: 1,
                ..Default::default()
            },
        );
        let pending = admission.acquire(ip("192.0.2.1"), true).unwrap();
        let live = pending.clone();
        drop(pending);
        assert!(admission.acquire(ip("::ffff:192.0.2.1"), true).is_none());
        drop(live);
        assert!(admission.acquire(ip("192.0.2.1"), true).is_some());
        assert!(admission.counts.lock().unwrap().ips.is_empty());
    }

    #[test]
    fn ipv6_host_rotation_and_subnet_rotation_cannot_reset_counts() {
        let a = PeerAdmission::new(
            20,
            PeerLimits {
                per_ip: 1,
                per_subnet: 2,
                outbound_reserved: 0,
            },
        );
        let _first = a.acquire(ip("2001:db8:1:2::123"), true).unwrap();
        assert!(a.acquire(ip("2001:db8:1:2::ffff"), true).is_none());
        let _second = a.acquire(ip("2001:db8:1:3::1"), true).unwrap();
        assert!(a.acquire(ip("2001:db8:1:4::1"), true).is_none());
        assert!(a.acquire(ip("2001:db8:2::1"), true).is_some());
    }

    #[test]
    fn inbound_flood_preserves_outbound_capacity_and_releases_on_failure() {
        let a = PeerAdmission::new(
            4,
            PeerLimits {
                per_ip: 4,
                per_subnet: 4,
                outbound_reserved: 2,
            },
        );
        let p1 = a.acquire(ip("192.0.2.1"), false).unwrap();
        let _p2 = a.acquire(ip("192.0.2.2"), false).unwrap();
        assert!(a.acquire(ip("198.51.100.1"), false).is_none());
        let _out = a.acquire(ip("198.51.100.1"), true).unwrap();
        drop(p1);
        assert!(a.acquire(ip("203.0.113.1"), false).is_some());
    }

    #[test]
    fn ipv4_subnet_cap_and_explicit_nat_policy() {
        let a = PeerAdmission::new(
            10,
            PeerLimits {
                per_ip: 1,
                per_subnet: 2,
                outbound_reserved: 0,
            },
        );
        let _p1 = a.acquire(ip("192.0.2.1"), false).unwrap();
        let _p2 = a.acquire(ip("192.0.2.2"), false).unwrap();
        assert!(a.acquire(ip("192.0.2.3"), false).is_none());
        let dev = PeerAdmission::new(10, PeerLimits::for_test_network(10));
        let peers: Vec<_> = (0..10)
            .map(|_| dev.acquire(ip("127.0.0.1"), false).unwrap())
            .collect();
        assert!(dev.acquire(ip("127.0.0.1"), false).is_none());
        drop(peers);
    }
}
