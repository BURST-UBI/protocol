use super::*;
use burst_types::Timestamp;
mod owned;
pub(crate) use owned::OwnedRetirement;

/// Physical prefix bodies only. Economic rows and logical transaction IDs never
/// enter this batch. The caller's operator policy supplies an age cutoff/budget.
/// This is still gated. A committed batch invalidates its physical inventory;
/// checkpoint replay caches retain certified state and need not be discarded.
#[cfg(test)]
pub(crate) struct BodyRetirement<'scan, 'proof, 'db> {
    closed: &'scan ClosedBodyRequirements<'proof, 'db>,
    hashes: Vec<BlockHash>,
    bytes: u64,
    progress: RetirementProgress,
}

/// Cursor advances only over examined rows that fit the current selection.
/// It is a scheduling hint, not evidence that a different cut is closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RetirementProgress {
    pub examined: usize,
    pub resume_after: Option<BlockHash>,
    pub complete: bool,
    /// An eligible body did not fit. Retry from resume_after with enough bytes;
    /// do not spin on an unchanged byte budget.
    pub blocked_body_bytes: Option<u64>,
}

impl<'proof, 'db> ClosedBodyRequirements<'proof, 'db> {
    #[cfg(test)]
    pub(crate) fn prepare_retirement(
        &self,
        before: Timestamp,
        max_bodies: usize,
        max_bytes: u64,
    ) -> Result<BodyRetirement<'_, 'proof, 'db>, StoreError> {
        self.prepare_retirement_page(before, max_bodies, max_bytes, None,
            BodyClosureLimits::default().bodies as usize)
    }

    /// Bound candidate examination separately from deletion. Seeking a cursor
    /// uses an authenticated fixed-width key seek; rejected or required rows consume the scan budget.
    /// Pages borrow the same checked closure, so scanning does not repeat recovery
    /// or root construction. The commit guard tolerates covered soft sequence
    /// reservations; other intervening writes, including sibling deletions,
    /// invalidate the plan.
    #[cfg(test)]
    pub(crate) fn prepare_retirement_page(
        &self,
        before: Timestamp,
        max_bodies: usize,
        max_bytes: u64,
        after: Option<BlockHash>,
        max_examined: usize,
    ) -> Result<BodyRetirement<'_, 'proof, 'db>, StoreError> {
        let hard = BodyClosureLimits::default();
        if max_bodies == 0 || max_bodies as u64 > hard.bodies || max_bytes == 0 || max_bytes > hard.bytes
            || max_examined == 0 || max_examined as u64 > hard.bodies {
            return Err(invalid("invalid physical retirement budget"));
        }
        let cut = &self.required.cut;
        let mut hashes = Vec::new();
        let mut bytes = 0u64;
        let mut progress = RetirementProgress {
            examined: 0, resume_after: after, complete: false, blocked_body_bytes: None,
        };
        let mut rows = cut.metadata.inventory.observed_hashes_after(after)?.peekable();
        while rows.peek().is_some() && progress.examined < max_examined && hashes.len() < max_bodies {
            let (hash, length) = rows.next().unwrap()?;
            progress.examined += 1;
            if !self.has_body(hash)? {
                let body = cut.metadata.resolve_body(cut.store, cut.reader.view(), &hash)?
                    .ok_or_else(|| invalid("retirement candidate is unresolved"))?;
                if body.origin() == BodyOrigin::SnapshotPrefix && body.block().timestamp <= before {
                    let next = bytes.checked_add(length as u64).ok_or_else(|| invalid("retirement byte overflow"))?;
                    if next > max_bytes {
                        progress.blocked_body_bytes = Some(length as u64);
                        break;
                    }
                    hashes.push(hash);
                    bytes = next;
                }
            }
            progress.resume_after = Some(hash);
        }
        if rows.peek().is_some_and(Result::is_err) { return Err(rows.next().unwrap().unwrap_err()); }
        progress.complete = progress.blocked_body_bytes.is_none() && rows.peek().is_none();
        drop(rows);
        Ok(BodyRetirement { closed: self, hashes, bytes, progress })
    }
}

#[cfg(test)]
impl BodyRetirement<'_, '_, '_> {
    #[cfg(test)]
    pub(crate) fn progress(&self) -> RetirementProgress { self.progress }

    #[cfg(test)]
    pub(crate) fn hashes(&self) -> &[BlockHash] { &self.hashes }

    /// The coordinator holds its publication exclusion through this synchronous
    /// call. Failed/empty commits do not change counts; successful deletion and
    /// the additive cache update cannot be separated by async cancellation.
    #[cfg(test)]
    pub(crate) fn commit_counted(self, counts: &crate::ledger_cache::LedgerCache) -> Result<(usize, u64), StoreError> {
        if counts.block_count() < self.hashes.len() as u64 {
            return Err(invalid("physical body count is behind retirement batch"));
        }
        let result = self.commit()?;
        if result.0 != 0 { counts.retire_blocks(result.0 as u64); }
        Ok(result)
    }

    /// Own the writer so any failure aborts all deletions. The cut guard is
    /// checked before the first change (only covered soft sequence reservations
    /// may intervene); commit publishes bodies and both height
    /// indexes atomically. Returned bytes are encoded body bytes, not file shrink.
    #[cfg(test)]
    pub(crate) fn commit(self) -> Result<(usize, u64), StoreError> {
        self.commit_inner(|_| {})
    }

    #[cfg(test)]
    pub(crate) fn commit_with_probe(self, probe: impl Fn(&str)) -> Result<(usize, u64), StoreError> {
        self.commit_inner(probe)
    }

    #[cfg(test)]
    fn commit_inner(self, probe: impl Fn(&str)) -> Result<(usize, u64), StoreError> {
        let cut = &self.closed.required.cut;
        if !cfg!(test) {
            return Err(invalid("physical body retirement requires coordinator and semantic qualification"));
        }
        let mut writer = cut.store.tx_begin_write()?;
        self.closed.check_retention_dependencies(&writer)?;
        if self.hashes.is_empty() { return Ok((0, 0)); }
        for hash in &self.hashes {
            writer.delete_block(hash)?;
            probe("deleted");
        }
        probe("prepared");
        writer.commit()?;
        probe("committed");
        Ok((self.hashes.len(), self.bytes))
    }
}
