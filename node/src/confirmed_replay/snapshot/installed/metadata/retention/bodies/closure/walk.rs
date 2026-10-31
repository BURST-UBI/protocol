use super::*;
use crate::confirmed_replay::scratch_index::{FixedScratchCompaction};

/// In-process progress over one pinned cut. This is neither a restart token nor
/// evidence of closure until finish succeeds. Any error poisons the traversal.
pub(super) struct ClosureWalk {
    seed: Option<RootCursor>,
    compaction: Option<FixedScratchCompaction<32>>,
    #[cfg(test)]
    compaction_nodes: usize,
    seed_count: u64,
    seed_seen: u64,
    pending_body: Option<BlockHash>,
    queue_next: u64,
    queue: ScratchSequence<BlockHash>,
    pending_edges: Box<dyn Iterator<Item = BlockHash> + Send>,
    limits: BodyClosureLimits,
    expansion_bodies: u64,
    work: ClosureWork,
    complete: bool,
    failed: bool,
}

impl ClosureWalk {
    pub(super) fn new(roots: &Roots, directory: &std::path::Path, limits: BodyClosureLimits)
        -> Result<Self, BodyClosureError> {
        Self::with_budget(roots, directory, ClosureBudget::new(limits, None)?)
    }

    pub(super) fn with_budget(roots: &Roots, directory: &std::path::Path, budget: ClosureBudget)
        -> Result<Self, BodyClosureError> {
        let limits = budget.total;
        if roots.count > limits.bodies { return Err(BodyClosureError::Budget("body count")); }
        Ok(Self { #[cfg(test)] compaction_nodes: 0, compaction: None, seed: Some(roots.rows.cursor()?), seed_count: roots.count, seed_seen: 0, pending_body: None, queue_next: 0,
            queue: ScratchSequence::new(directory)?, pending_edges: Box::new(std::iter::empty()),
            limits, expansion_bodies: budget.expansion.bodies, work: ClosureWork { bodies: 0, bytes: 0, edges: 0 },
            complete: false, failed: false })
    }

    /// A seed step charges authenticated scratch nodes; body resolution and
    /// each dependency edge consume separate steps. Whole-attempt body, byte and
    /// edge limits remain cumulative. No previous body is resolved again.
    pub(super) fn advance(
        &mut self, roots: &mut Roots, budget: usize,
        mut resolve: impl FnMut(BlockHash) -> Result<Option<(StateBlock, BodyOrigin)>, StoreError>,
        mut missing: impl FnMut(BlockHash) -> Result<(), BodyClosureError>,
    ) -> Result<bool, BodyClosureError> {
        if self.failed { return Err(invalid("closure traversal already failed").into()); }
        self.failed = true;
        #[cfg(test)]
        { self.compaction_nodes = 0; }
        if budget < 2 { return Err(BodyClosureError::Budget("invalid closure page budget")); }
        if self.complete { self.failed = false; return Ok(true); }
        let mut remaining = budget;
        while remaining > 0 {
            if let Some(compaction) = &mut self.compaction {
                let page = compaction.advance(remaining)?;
                remaining -= page.nodes;
                #[cfg(test)]
                { self.compaction_nodes += page.nodes; }
                if page.done {
                    self.compaction.take().expect("active compaction").finish(&mut roots.rows.index)?;
                }
                continue;
            }
            if let Some(dependency) = self.pending_edges.next() {
                remaining -= 1;
                self.work.edges = self.work.edges.checked_add(1).filter(|n| *n <= self.limits.edges)
                    .ok_or(BodyClosureError::Budget("dependency edges"))?;
                if roots.rows.get(dependency.as_bytes())?.is_some() { continue; }
                if roots.count >= self.limits.bodies { return Err(BodyClosureError::Budget("body count")); }
                if self.queue.len() >= self.expansion_bodies {
                    return Err(BodyClosureError::Budget("discovered body count"));
                }
                roots.add_with_compaction(dependency, Source::Replay, false)?;
                self.queue.push(&dependency)?;
                self.compaction = roots.rows.index.start_compaction(&roots.rows.directory)?;
                continue;
            }
            let hash = if let Some(hash) = self.pending_body.take() {
                hash
            } else if let Some(seed) = &mut self.seed {
                // The cursor owns the original root index. New dependencies go
                // into the queue; original roots never need a second disk copy.
                let page = seed.visit_next(1, |key, value| {
                    let sources = RootRows::decode(value)?;
                    let hash = BlockHash::new(key);
                    if hash.is_zero() || sources == 0 || sources & !127 != 0 {
                        return Err(invalid("invalid required body entry").into());
                    }
                    if self.pending_body.replace(hash).is_some() {
                        return Err(invalid("closure seed page emitted multiple bodies").into());
                    }
                    self.seed_seen += 1;
                    Ok(())
                })?;
                remaining -= page.nodes;
                if self.seed_seen > self.seed_count {
                    return Err(invalid("closure seed exceeds root inventory").into());
                }
                if page.done {
                    if self.seed_seen != self.seed_count {
                        return Err(invalid("closure seed differs from root inventory").into());
                    }
                    self.seed = None;
                }
                continue;
            } else if self.queue_next < self.queue.len() {
                let hash = self.queue.get(self.queue_next)?;
                self.queue_next += 1;
                hash
            } else {
                if self.work.bodies != roots.count
                    || self.seed_count.checked_add(self.queue.len()) != Some(roots.count) {
                    return Err(invalid("closure queue differs from root inventory").into());
                }
                self.complete = true;
                self.failed = false;
                return Ok(true);
            };
            remaining -= 1;
            let Some((block, origin)) = resolve(hash)? else {
                missing(hash)?;
                self.work.bodies += 1;
                continue;
            };
            if block.hash != hash || block.compute_hash() != hash {
                return Err(invalid("resolved closure body differs from requested hash").into());
            }
            let length = bincode::serialized_size(&block).map_err(invalid)?;
            self.work.bytes = self.work.bytes.checked_add(length).filter(|n| *n <= self.limits.bytes)
                .ok_or(BodyClosureError::Budget("body bytes"))?;
            self.work.bodies += 1;
            if origin != BodyOrigin::SnapshotPrefix {
                self.pending_edges = Box::new(block.into_dependencies());
            }
        }
        self.failed = false;
        Ok(false)
    }

    #[cfg(test)]
    pub(super) fn compaction_nodes(&self) -> usize { self.compaction_nodes }

    #[cfg(test)]
    pub(super) fn queued_dependencies(&self) -> u64 { self.queue.len() }

    #[cfg(test)]
    pub(super) fn work(&self) -> &ClosureWork { &self.work }

    pub(super) fn finish(self) -> Result<ClosureWork, BodyClosureError> {
        if self.failed || !self.complete {
            return Err(invalid("body closure traversal is incomplete").into());
        }
        Ok(self.work)
    }
}
