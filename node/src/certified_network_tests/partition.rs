use super::*;
use crate::connection_registry::ConnectionRegistry;
use std::collections::HashSet;
use tokio::sync::RwLock;

pub(super) type Registry = Arc<RwLock<ConnectionRegistry>>;

pub(super) struct Partition<'a> {
    registries: &'a [Registry],
    before: Vec<u64>,
}

impl<'a> Partition<'a> {
    /// Split an established full mesh. Keep each half connected internally.
    pub(super) async fn split(registries: &'a [Registry], boundary: usize) -> Self {
        assert!(boundary > 0 && boundary < registries.len());
        let mut peer_sets = Vec::new();
        let mut all = HashSet::new();
        for registry in registries {
            let peers: HashSet<_> = registry
                .read()
                .await
                .peer_ids()
                .into_iter()
                .cloned()
                .collect();
            assert_eq!(
                peers.len(),
                registries.len() - 1,
                "partition requires a full mesh"
            );
            all.extend(peers.iter().cloned());
            peer_sets.push(peers);
        }
        assert_eq!(all.len(), registries.len());
        let identities: Vec<_> = peer_sets
            .iter()
            .map(|peers| {
                let missing: Vec<_> = all.difference(peers).cloned().collect();
                assert_eq!(missing.len(), 1);
                missing[0].clone()
            })
            .collect();
        assert_eq!(
            identities.iter().collect::<HashSet<_>>().len(),
            registries.len()
        );
        let mut before = Vec::new();
        for (index, registry) in registries.iter().enumerate() {
            let mut registry = registry.write().await;
            assert!(registry.partitioned_peers.is_empty());
            before.push(registry.partition_drops.load(Ordering::SeqCst));
            registry.partitioned_peers = identities
                .iter()
                .enumerate()
                .filter(|(peer, _)| (*peer < boundary) != (index < boundary))
                .map(|(_, identity)| identity.clone())
                .collect();
        }
        Self { registries, before }
    }

    pub(super) async fn heal(self) {
        let mut drops = 0;
        for (registry, before) in self.registries.iter().zip(self.before) {
            let mut registry = registry.write().await;
            drops += registry.partition_drops.load(Ordering::SeqCst) - before;
            registry.partitioned_peers.clear();
        }
        assert!(drops > 0, "partition did not intercept any peer traffic");
        eprintln!("healed network partition after dropping {drops} peer frames");
    }
}
