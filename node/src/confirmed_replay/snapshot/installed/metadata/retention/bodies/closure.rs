use super::*;
use crate::confirmed_replay::scratch_records::ScratchSequence;
use burst_ledger::StateBlock;
use crate::confirmed_replay::blocks::{staged::StagedBlocks, AppliedBlocks};

mod recovery;
mod retirement;
pub(crate) use retirement::OwnedRetirement;
mod walk;
use walk::ClosureWalk;

/// Per-attempt work limits, bounded by the existing history/tail budgets.
/// They do not bound initial recovery or root collection.
#[derive(Clone, Copy)]
pub(crate) struct BodyClosureLimits {
    pub bodies: u64,
    pub bytes: u64,
    pub edges: u64,
}
impl Default for BodyClosureLimits {
    fn default() -> Self {
        Self {
            bodies: crate::confirmed_history::MAX_BLOCKS,
            bytes: crate::confirmed_history::MAX_BYTES as u64,
            edges: crate::confirmed_history::MAX_BYTES as u64 / 32,
        }
    }
}
/// The local allowance comes only from the pinned recovered inventory. The
/// caller's limits still bound discovery and peer work independently.
#[derive(Clone, Copy)]
struct ClosureBudget {
    total: BodyClosureLimits,
    expansion: BodyClosureLimits,
}
impl ClosureBudget {
    fn new(limits: BodyClosureLimits, local: Option<(u64, u64)>) -> Result<Self, BodyClosureError> {
        validate_limits(limits, 0)?;
        let (bodies, bytes) = local.unwrap_or_default();
        Ok(Self { total: BodyClosureLimits {
            bodies: limits.bodies.checked_add(bodies).ok_or(BodyClosureError::Budget("body count overflow"))?,
            bytes: limits.bytes.checked_add(bytes).ok_or(BodyClosureError::Budget("body bytes overflow"))?,
            edges: limits.edges,
        }, expansion: limits })
    }

    fn check_peer_count(&self, count: u64) -> Result<(), BodyClosureError> {
        if count > self.expansion.bodies { return Err(BodyClosureError::Budget("peer body count")); }
        Ok(())
    }

    fn peer_bytes(&self, used: u64, length: u64) -> Result<u64, BodyClosureError> {
        used.checked_add(length).filter(|bytes| *bytes <= self.expansion.bytes)
            .ok_or(BodyClosureError::Budget("peer body bytes"))
    }
}
#[derive(Debug)]
pub(crate) enum BodyClosureError {
    Unresolved(BlockHash),
    Budget(&'static str),
    MissingRollbackProof,
    Storage(StoreError),
}
impl std::fmt::Display for BodyClosureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unresolved(hash) => write!(f, "unresolved body {hash}"),
            Self::Budget(reason) => write!(f, "body closure budget: {reason}"),
            Self::MissingRollbackProof => f.write_str("missing rollback proof"),
            Self::Storage(error) => write!(f, "body closure storage: {error}"),
        }
    }
}
impl From<StoreError> for BodyClosureError {
    fn from(error: StoreError) -> Self { Self::Storage(error) }
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ClosureWork {
    pub bodies: u64,
    pub bytes: u64,
    pub edges: u64,
}

/// Complete declared body dependency graph relative to the certified base.
/// Snapshot-prefix bodies are leaves because the complete certified economic
/// state is retained. This is not economic state GC, archive availability,
/// portable finality, or permission to delete physical rows.
pub(crate) struct ClosedBodyRequirements<'proof, 'db> {
    required: BodyRequirements<'proof, 'db>,
    _work: ClosureWork,
    _recovered: Option<StagedBlocks>,
}
impl ClosedBodyRequirements<'_, '_> {
    /// Bodies recovered from peers remain owned by this plan's checked scratch.
    /// Returning bytes does not grant permission to install or execute them.
    #[cfg(test)]
    pub(crate) fn body(&self, hash: BlockHash) -> Result<Option<StateBlock>, StoreError> {
        if !self.has_body(hash)? { return Ok(None); }
        resolve(&self.required.cut, self._recovered.as_ref(), hash)?
            .map(|(block, _)| Some(block))
            .ok_or_else(|| invalid("closed body became unavailable"))
    }
    #[cfg(test)]
    pub(crate) fn work(&self) -> &ClosureWork { &self._work }
    pub(crate) fn has_body(&self, hash: BlockHash) -> Result<bool, StoreError> {
        self.required.has_root(hash)
    }
    #[cfg(test)]
    pub(crate) fn visit_bodies(&self, visit: impl FnMut(BlockHash) -> Result<(), StoreError>) -> Result<(), StoreError> {
        self.required.visit_roots(visit)
    }
    #[cfg(test)]
    pub(crate) fn check_unchanged_cut(&self, batch: &LmdbWriteTransaction<'_>) -> Result<(), StoreError> {
        self.required.check_unchanged_cut(batch)
    }
    #[cfg(test)]
    pub(crate) fn check_retention_dependencies(&self, batch: &LmdbWriteTransaction<'_>) -> Result<(), StoreError> {
        batch.check_body_retention_cut(&self.required.cut.reader)
    }
}
impl<'proof, 'db> BodyRequirements<'proof, 'db> {
    #[cfg(test)]
    pub(crate) fn close(self, limits: BodyClosureLimits) -> Result<ClosedBodyRequirements<'proof, 'db>, BodyClosureError> {
        self.close_recovered(limits, None)
    }

    #[cfg(test)]
    pub(crate) async fn close_retained(mut self, limits: BodyClosureLimits)
        -> Result<ClosedBodyRequirements<'proof, 'db>, BodyClosureError> {
        if !self.rollback_checked { return Err(BodyClosureError::MissingRollbackProof); }
        let budget = self.retained_budget(limits).await?;
        let cut = &self.cut;
        let mut walk = ClosureWalk::with_budget(&self.roots, cut.store.env().path(), budget)?;
        while !walk.advance(&mut self.roots, 128, |hash| resolve(cut, None, hash),
            |hash| Err(BodyClosureError::Unresolved(hash)))? {
            tokio::task::yield_now().await;
        }
        Ok(ClosedBodyRequirements { required: self, _work: walk.finish()?, _recovered: None })
    }

    async fn retained_budget(&self, limits: BodyClosureLimits) -> Result<ClosureBudget, BodyClosureError> {
        validate_limits(limits, 0)?;
        let local = self.cut.metadata.inventory.closure_allowance(self.cut.store.env().path()).await?;
        ClosureBudget::new(limits, local)
    }

    #[cfg(test)]
    fn close_recovered(mut self, limits: BodyClosureLimits, recovered: Option<StagedBlocks>)
        -> Result<ClosedBodyRequirements<'proof, 'db>, BodyClosureError> {
        if !self.rollback_checked { return Err(BodyClosureError::MissingRollbackProof); }
        let cut = &self.cut;
        let work = walk(&mut self.roots, cut.store.env().path(), limits, |hash| {
            resolve(cut, recovered.as_ref(), hash)
        })?;
        Ok(ClosedBodyRequirements { required: self, _work: work, _recovered: recovered })
    }
}

fn resolve(cut: &RecoveredTrstReferences<'_, '_>, recovered: Option<&StagedBlocks>, hash: BlockHash)
    -> Result<Option<(StateBlock, BodyOrigin)>, StoreError> {
    if let Some(body) = cut.metadata.resolve_body(cut.store, cut.reader.view(), &hash)? {
        if body.context() != cut.metadata.recovered().continuation.history_context() {
            return Err(invalid("foreign body closure context"));
        }
        return Ok(Some(body.into_parts()));
    }
    recovered.map(|archive| archive.get(&hash)
        .map(|body| body.map(|body| (body.into_owned(), BodyOrigin::SnapshotPrefix))))
        .transpose().map(Option::flatten)
}

fn validate_limits(limits: BodyClosureLimits, roots: u64) -> Result<(), BodyClosureError> {
    let hard = BodyClosureLimits::default();
    if limits.bodies == 0 || limits.bodies > hard.bodies || limits.bytes == 0 || limits.bytes > hard.bytes
        || limits.edges == 0 || limits.edges > hard.edges {
        return Err(BodyClosureError::Budget("invalid closure limits"));
    }
    if roots > limits.bodies { return Err(BodyClosureError::Budget("body count")); }
    Ok(())
}

// The production resolver above supplies authenticated origin classes. This
// walker only enforces graph/work completeness; it cannot authenticate inputs.
#[cfg(test)]
fn walk(
    roots: &mut Roots,
    directory: &std::path::Path,
    limits: BodyClosureLimits,
    mut resolve: impl FnMut(BlockHash) -> Result<Option<(StateBlock, BodyOrigin)>, StoreError>,
) -> Result<ClosureWork, BodyClosureError> {
    walk_with_missing(roots, directory, limits, &mut resolve, |hash| {
        Err(BodyClosureError::Unresolved(hash))
    })
}

// Missing bodies count against the same graph budget. Only authenticated
// snapshot-prefix witnesses may later fill them: such bodies are leaves.
#[cfg(test)]
fn walk_with_missing(
    roots: &mut Roots,
    directory: &std::path::Path,
    limits: BodyClosureLimits,
    mut resolve: impl FnMut(BlockHash) -> Result<Option<(StateBlock, BodyOrigin)>, StoreError>,
    mut missing: impl FnMut(BlockHash) -> Result<(), BodyClosureError>,
) -> Result<ClosureWork, BodyClosureError> {
    let mut walk = ClosureWalk::new(roots, directory, limits)?;
    while !walk.advance(roots, 128, &mut resolve, &mut missing)? {}
    walk.finish()

}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn block(previous: BlockHash, timestamp: u64) -> StateBlock {
        let mut block = crate::genesis_state::block(burst_types::NetworkId::Dev, &burst_types::ProtocolParams::default());
        block.block_type = burst_ledger::BlockType::Send;
        block.previous = previous;
        block.timestamp = burst_types::Timestamp::new(timestamp);
        block.hash = block.compute_hash();
        block
    }
    fn graph() -> (BlockHash, HashMap<BlockHash, (StateBlock, BodyOrigin)>) {
        // The prefix's own dependency is deliberately unavailable: its state is
        // supplied by the certified base, so it is a traversal boundary.
        let base = block(BlockHash::new([99; 32]), 1);
        let left = block(base.hash, 2);
        let right = block(base.hash, 3);
        let mut join = block(left.hash, 4);
        join.block_type = burst_ledger::BlockType::Receive;
        join.link = right.hash;
        join.release_sources = vec![left.hash, base.hash];
        join.hash = join.compute_hash();
        let head = join.hash;
        (head, [(base, BodyOrigin::SnapshotPrefix), (left, BodyOrigin::CementedSuffix),
            (right, BodyOrigin::ProvisionalSuffix), (join, BodyOrigin::ProvisionalSuffix)]
            .into_iter().map(|(b, o)| (b.hash, (b, o))).collect())
    }
    fn run(head: BlockHash, graph: &HashMap<BlockHash, (StateBlock, BodyOrigin)>, limits: BodyClosureLimits)
        -> Result<ClosureWork, BodyClosureError> {
        let dir = tempfile::tempdir().unwrap();
        let mut roots = Roots::new(dir.path()).unwrap();
        roots.add(head, Source::State).unwrap();
        let mut visits = std::collections::HashSet::new();
        walk(&mut roots, dir.path(), limits, |hash| {
            assert!(visits.insert(hash), "a body was resolved twice");
            Ok(graph.get(&hash).cloned())
        })
    }
    #[test]
    fn retained_closure_allowance_preserves_legacy_and_discovery_limits() {
        let dir = tempfile::tempdir().unwrap();
        let (head, graph) = graph();
        let bytes = graph.values().map(|(body, _)| bincode::serialized_size(body).unwrap()).sum();
        let limits = BodyClosureLimits { bodies: 1, bytes: 1, edges: 6 };
        let mut roots = Roots::new_bulk(dir.path()).unwrap();
        for hash in graph.keys() { roots.add(*hash, Source::State).unwrap(); }
        let mut roots = roots.finish_bulk().unwrap();
        assert!(matches!(ClosureWalk::new(&roots, dir.path(), limits),
            Err(BodyClosureError::Budget("body count"))));
        let budget = ClosureBudget::new(limits, Some((4, bytes))).unwrap();
        budget.check_peer_count(1).unwrap();
        assert!(budget.check_peer_count(2).is_err());
        assert_eq!(budget.peer_bytes(0, 1).unwrap(), 1);
        assert!(budget.peer_bytes(1, 1).is_err());
        assert!(budget.peer_bytes(u64::MAX, 1).is_err());
        let mut walk = ClosureWalk::with_budget(&roots, dir.path(), budget).unwrap();
        while !walk.advance(&mut roots, 2, |hash| Ok(graph.get(&hash).cloned()),
            |_| panic!("all bodies available")).unwrap() {}
        assert_eq!(walk.queued_dependencies(), 0);
        assert_eq!(walk.finish().unwrap(), ClosureWork { bodies: 4, bytes, edges: 6 });

        // Unused local allowance cannot become an unbounded discovery queue.
        let mut roots = Roots::new(dir.path()).unwrap();
        roots.add(head, Source::State).unwrap();
        let mut walk = ClosureWalk::with_budget(&roots, dir.path(), budget).unwrap();
        let error = loop {
            match walk.advance(&mut roots, 2, |hash| Ok(graph.get(&hash).cloned()), |_| Ok(())) {
                Ok(false) => {},
                Ok(true) => panic!("discovery exceeded its independent limit"),
                Err(error) => break error,
            }
        };
        assert!(matches!(error, BodyClosureError::Budget("discovered body count")));
        assert!(walk.finish().is_err());
        assert!(ClosureBudget::new(limits, Some((u64::MAX, 0))).is_err());
        assert!(ClosureBudget::new(limits, Some((0, u64::MAX))).is_err());
        assert!(ClosureBudget::new(BodyClosureLimits {
            bodies: BodyClosureLimits::default().bodies + 1, ..limits
        }, Some((4, bytes))).is_err());
    }

    #[test]
    fn closure_deduplicates_diamond_and_stops_at_checked_prefix() {
        let (head, graph) = graph();
        let bytes = graph.values().map(|(b, _)| bincode::serialized_size(b).unwrap()).sum();
        assert_eq!(run(head, &graph, BodyClosureLimits::default()).unwrap(),
            ClosureWork { bodies: 4, bytes, edges: 6 });
    }
    #[test]
    fn closure_discovers_dependencies_from_paged_roots_without_revisiting_seeds() {
        let (head, graph) = graph();
        let dir = tempfile::tempdir().unwrap();
        let mut roots = Roots::new_bulk(dir.path()).unwrap();
        roots.add(head, Source::State).unwrap();
        let mut roots = roots.finish_bulk().unwrap();
        let mut visits = std::collections::HashSet::new();
        let work = walk(&mut roots, dir.path(), BodyClosureLimits::default(), |hash| {
            assert!(visits.insert(hash));
            Ok(graph.get(&hash).cloned())
        }).unwrap();
        assert_eq!(work.bodies, 4);
        assert_eq!(work.edges, 6);
        assert_eq!(roots.count, 4);
        let mut closed = std::collections::HashSet::new();
        roots.visit(|hash| { assert!(closed.insert(hash)); Ok(()) }).unwrap();
        assert_eq!(closed, visits);
    }

    #[test]
    fn closure_rejects_missing_mismatched_and_over_budget_bodies() {
        let (head, mut graph) = graph();
        let bytes = graph.values().map(|(b, _)| bincode::serialized_size(b).unwrap()).sum::<u64>();
        for (limits, expected) in [
            (BodyClosureLimits { bodies: 3, ..Default::default() }, "body count"),
            (BodyClosureLimits { bytes: bytes - 1, ..Default::default() }, "body bytes"),
            (BodyClosureLimits { edges: 5, ..Default::default() }, "dependency edges"),
            (BodyClosureLimits { bodies: 0, ..Default::default() }, "invalid closure limits"),
            (BodyClosureLimits { bodies: BodyClosureLimits::default().bodies + 1, ..Default::default() }, "invalid closure limits"),
        ] {
            assert!(matches!(run(head, &graph, limits), Err(BodyClosureError::Budget(label)) if label == expected));
        }
        let base = *graph.iter().find(|(_, (_, origin))| *origin == BodyOrigin::SnapshotPrefix).unwrap().0;
        let saved = graph.remove(&base).unwrap();
        assert!(matches!(run(head, &graph, Default::default()), Err(BodyClosureError::Unresolved(hash)) if hash == base));
        graph.insert(base, saved);
        graph.get_mut(&head).unwrap().0.previous = BlockHash::ZERO;
        assert!(matches!(run(head, &graph, Default::default()), Err(BodyClosureError::Storage(_))));
    }
    #[test]
    fn closure_discovers_missing_transitive_prefix_once() {
        let (head, mut graph) = graph();
        let prefix = *graph.iter().find(|(_, (_, origin))|
            *origin == BodyOrigin::SnapshotPrefix).unwrap().0;
        graph.remove(&prefix);
        let dir = tempfile::tempdir().unwrap();
        let mut roots = Roots::new(dir.path()).unwrap();
        roots.add(head, Source::State).unwrap();
        assert!(roots.rows.get(prefix.as_bytes()).unwrap().is_none());
        let mut visits = std::collections::HashSet::new();
        let mut missing = Vec::new();
        let work = walk_with_missing(&mut roots, dir.path(), Default::default(), |hash| {
            assert!(visits.insert(hash), "local body resolved twice");
            Ok(graph.get(&hash).cloned())
        }, |hash| { missing.push(hash); Ok(()) }).unwrap();
        assert_eq!(missing, vec![prefix]);
        assert_eq!(visits.len(), 4);
        assert_eq!(work, ClosureWork { bodies: 4, edges: 6,
            bytes: graph.values().map(|(body, _)| bincode::serialized_size(body).unwrap()).sum() });
        // Missing leaves still consume the whole-attempt body budget.
        let mut roots = Roots::new(dir.path()).unwrap();
        roots.add(head, Source::State).unwrap();
        assert!(matches!(walk_with_missing(&mut roots, dir.path(),
            BodyClosureLimits { bodies: 3, ..Default::default() },
            |hash| Ok(graph.get(&hash).cloned()), |_| Ok(())),
            Err(BodyClosureError::Budget("body count"))));
    }
    #[test]
    fn closure_chain_resolves_each_unique_body_once() {
        for count in [512, 1024] {
            let mut graph = HashMap::new();
            let mut head = BlockHash::ZERO;
            for n in 0..count {
                let body = block(head, n + 1);
                head = body.hash;
                graph.insert(head, (body, if n == 0 { BodyOrigin::SnapshotPrefix } else { BodyOrigin::ProvisionalSuffix }));
            }
            let directory = tempfile::tempdir().unwrap();
            let limit = crate::confirmed_replay::scratch_file::ScratchLimit::acquire(directory.path(), 4 << 20).unwrap();
            let mut roots = Roots::new(directory.path()).unwrap();
            roots.add(head, Source::State).unwrap();
            let mut walk = ClosureWalk::new(&roots, directory.path(), Default::default()).unwrap();
            let mut seen = std::collections::HashSet::new();
            let mut compacted = 0;
            loop {
                let before = walk.work().bodies + walk.work().edges;
                let done = walk.advance(&mut roots, 7, |hash| {
                    assert!(seen.insert(hash), "body resolved again after compaction");
                    Ok(graph.get(&hash).cloned())
                }, |hash| Err(BodyClosureError::Unresolved(hash))).unwrap();
                let maintenance = walk.compaction_nodes();
                compacted += maintenance;
                assert!(walk.work().bodies + walk.work().edges - before + maintenance as u64 <= 7);
                if done { break; }
            }
            assert!(compacted > 0, "fixture did not exercise paged compaction");
            assert_eq!(seen.len() as u64, count);
            let work = walk.finish().unwrap();
            drop(roots);
            assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
            assert_eq!(work.bodies, count);
            assert_eq!(work.edges, count - 1);
        }
    }
    #[test]
    fn paged_closure_preserves_edge_progress_and_resolves_each_body_once() {
        let dir = tempfile::tempdir().unwrap();
        let base = block(BlockHash::new([99; 32]), 1);
        let mut head = block(base.hash, 2);
        head.release_sources = vec![base.hash; 513];
        head.hash = head.compute_hash();
        assert_eq!(head.dependencies().collect::<Vec<_>>(),
            head.clone().into_dependencies().collect::<Vec<_>>());
        let graph = HashMap::from([(head.hash, (head.clone(), BodyOrigin::ProvisionalSuffix)),
            (base.hash, (base, BodyOrigin::SnapshotPrefix))]);
        let mut roots = Roots::new(dir.path()).unwrap();
        roots.add(head.hash, Source::State).unwrap();
        let mut walk = ClosureWalk::new(&roots, dir.path(), Default::default()).unwrap();
        let mut seen = std::collections::HashSet::new();
        let mut pages = 0;
        loop {
            let before = walk.work().bodies + walk.work().edges;
            let done = walk.advance(&mut roots, 2, |hash| {
                assert!(seen.insert(hash), "resumed body was resolved again");
                Ok(graph.get(&hash).cloned())
            }, |hash| Err(BodyClosureError::Unresolved(hash))).unwrap();
            assert!(walk.work().bodies + walk.work().edges - before <= 2);
            pages += 1;
            if done { break; }
            assert!(pages < 1000);
        }
        assert_eq!(seen.len(), 2);
        assert!(pages > 250);
        assert!(walk.advance(&mut roots, 2, |_| panic!("completed walk resolved a body"),
            |_| panic!("completed walk requested a body")).unwrap());
        assert_eq!(walk.finish().unwrap(), ClosureWork { bodies: 2, edges: 514,
            bytes: graph.values().map(|(body, _)| bincode::serialized_size(body).unwrap()).sum() });
    }

    #[test]
    fn incomplete_or_failed_closure_cannot_be_finished_or_resumed() {
        let dir = tempfile::tempdir().unwrap();
        let (head, graph) = graph();
        let mut roots = Roots::new(dir.path()).unwrap();
        roots.add(head, Source::State).unwrap();
        assert!(ClosureWalk::new(&roots, dir.path(), Default::default()).unwrap().finish().is_err());
        let mut walk = ClosureWalk::new(&roots, dir.path(), Default::default()).unwrap();
        let mut failed = false;
        for _ in 0..20 {
            match walk.advance(&mut roots, 2, |_| Err(invalid("injected resolver failure")), |_| Ok(())) {
                Ok(false) => {},
                Err(_) => { failed = true; break; },
                Ok(true) => panic!("failed resolver completed closure"),
            }
        }
        assert!(failed);
        assert!(walk.advance(&mut roots, 2, |hash| Ok(graph.get(&hash).cloned()), |_| Ok(())).is_err());
        assert!(walk.finish().is_err());
    }

    #[test]
    fn closure_streams_large_root_sets_without_copying_them_to_the_queue() {
        let dir = tempfile::tempdir().unwrap();
        let mut roots = Roots::new(dir.path()).unwrap();
        let mut graph = HashMap::new();
        for n in 1..=260 {
            let body = block(BlockHash::ZERO, n);
            roots.add(body.hash, Source::State).unwrap();
            graph.insert(body.hash, (body, BodyOrigin::SnapshotPrefix));
        }
        let mut walk = ClosureWalk::new(&roots, dir.path(), Default::default()).unwrap();
        let mut resolved = 0;
        let mut pages = 0;
        loop {
            let before = resolved;
            let done = walk.advance(&mut roots, 2, |hash| {
                resolved += 1;
                Ok(graph.get(&hash).cloned())
            }, |_| panic!("all prefix roots are available")).unwrap();
            assert!(resolved - before <= 2);
            assert_eq!(walk.queued_dependencies(), 0, "original roots must stay out of the queue");
            pages += 1;
            if pages == 1 { assert_eq!(resolved, 0); }
            if done { break; }
            assert!(pages < 2000);
        }
        assert!(pages > 260);
        assert_eq!(resolved, 260);
        assert_eq!(walk.finish().unwrap().bodies, 260);
    }

    #[test]
    fn closure_pinned_seeds_and_new_dependencies_are_each_resolved_once() {
        let dir = tempfile::tempdir().unwrap();
        let (head, graph) = graph();
        let mut roots = Roots::new(dir.path()).unwrap();
        roots.add(head, Source::State).unwrap();
        let base = *graph.iter().find(|(_, (_, origin))| *origin == BodyOrigin::SnapshotPrefix).unwrap().0;
        roots.add(base, Source::Signer).unwrap();
        let mut walk = ClosureWalk::new(&roots, dir.path(), Default::default()).unwrap();
        let mut seen = std::collections::HashSet::new();
        let mut pages = 0;
        loop {
            let before = walk.work().bodies + walk.work().edges;
            let done = walk.advance(&mut roots, 3, |hash| {
                assert!(seen.insert(hash), "seed or discovered body was resolved twice");
                Ok(graph.get(&hash).cloned())
            }, |hash| Err(BodyClosureError::Unresolved(hash))).unwrap();
            assert!(walk.work().bodies + walk.work().edges - before <= 3);
            pages += 1;
            if done { break; }
            assert!(pages < 100);
        }
        assert_eq!(seen, graph.keys().copied().collect());
        assert_eq!(walk.queued_dependencies(), 2);
        assert_eq!(roots.count, 4);
        assert_eq!(walk.finish().unwrap().bodies, 4);
    }

}
