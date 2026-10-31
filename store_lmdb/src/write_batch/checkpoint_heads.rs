use super::LmdbWriteTransaction;
use crate::checkpoint_heads::{digest, invalid, key, read, HeadNode, NodeHash, ReclaimedHeads};
use crate::LmdbError;
use burst_store::StoreError;

impl LmdbWriteTransaction<'_> {
    fn mutate_heads<T>(
        &mut self,
        apply: impl FnOnce(&mut Self) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let result = (|| {
            if self.failed || self.undo.is_some() || self.block_group.is_some() {
                return Err(invalid(
                    "historical head ownership requires a healthy non-undo writer",
                ));
            }
            apply(self)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn write_head_row(
        &mut self,
        kind: u8,
        hash: &NodeHash,
        value: Option<&[u8]>,
    ) -> Result<(), StoreError> {
        self.dirty = true;
        self.changed_value = true;
        self.invalidate_body_changes();
        let db = self.env.checkpoint_heads_db;
        match value {
            Some(bytes) => db
                .put(&mut self.txn, &key(kind, hash), bytes)
                .map_err(LmdbError::from)?,
            None => {
                db.delete(&mut self.txn, &key(kind, hash))
                    .map_err(LmdbError::from)?;
            }
        }
        Ok(())
    }

    fn change_head_references(&mut self, hash: &NodeHash, retain: bool) -> Result<(), StoreError> {
        let (count, node) = read(self.env.checkpoint_heads_db, &self.txn, hash)?
            .ok_or_else(|| invalid("historical head reference is missing"))?;
        self.change_checked_head_references(hash, count, &node, retain)
    }

    // Only callers holding a node checked in this writer may use this path.
    fn change_checked_head_references(
        &mut self,
        hash: &NodeHash,
        count: u64,
        node: &HeadNode,
        retain: bool,
    ) -> Result<(), StoreError> {
        let next = if retain {
            count.checked_add(1)
        } else {
            count.checked_sub(1)
        }
        .ok_or_else(|| invalid("historical head reference count overflow or underflow"))?;
        let mut row = next.to_be_bytes().to_vec();
        row.extend_from_slice(&node.encode()?);
        self.write_head_row(0, hash, Some(&row))?;
        if count == 0 {
            self.write_head_row(1, hash, None)?;
        }
        if next == 0 {
            self.write_head_row(1, hash, Some(&[]))?;
        }
        Ok(())
    }

    /// Intern a byte-checked node. Its edges are owned once, regardless of how
    /// many roots share it. A new unowned node is queued until a parent/root pins it.
    pub fn intern_checkpoint_head(&mut self, node: &HeadNode) -> Result<NodeHash, StoreError> {
        self.mutate_heads(|this| {
            let bytes = node.encode()?; let hash = digest(&bytes);
            if let Some((_, existing)) = read(this.env.checkpoint_heads_db, &this.txn, &hash)? {
                if existing != *node { return Err(invalid("historical head digest collision")); }
                return Ok(hash);
            }
            if let HeadNode::Branch { bit, left, right } = node {
                for child in [left, right] {
                    let (count, child_node) = read(this.env.checkpoint_heads_db, &this.txn, child)?
                        .ok_or_else(|| invalid("historical head child is missing"))?;
                    if matches!(child_node, HeadNode::Branch { bit: child_bit, .. } if child_bit <= *bit) {
                        return Err(invalid("historical head branch depth is not increasing"));
                    }
                    this.change_checked_head_references(child, count, &child_node, true)?;
                }
            }
            let mut row = 0u64.to_be_bytes().to_vec(); row.extend_from_slice(&bytes);
            this.write_head_row(0, &hash, Some(&row))?;
            this.write_head_row(1, &hash, Some(&[]))?;
            Ok(hash)
        })
    }

    /// Add one root owner. Publish its descriptor in this same transaction.
    /// Errors prevent commit; block-undo and grouped-block writers are rejected.
    pub fn retain_checkpoint_head(&mut self, hash: &NodeHash) -> Result<(), StoreError> {
        self.mutate_heads(|this| this.change_head_references(hash, true))
    }

    /// Remove one root owner and queue an unowned root for reclamation.
    /// Remove its descriptor in this same transaction. Errors prevent commit.
    pub fn release_checkpoint_head(&mut self, hash: &NodeHash) -> Result<(), StoreError> {
        self.mutate_heads(|this| this.change_head_references(hash, false))
    }

    /// Atomically replace one migration's bounded forest pins. Callers persist
    /// their continuation in this same transaction. Empty roots abandon the job.
    pub fn replace_checkpoint_head_staging(
        &mut self,
        id: &NodeHash,
        roots: &[NodeHash],
    ) -> Result<(), StoreError> {
        self.mutate_heads(|this| {
            let bytes = crate::checkpoint_heads::encode_staging(roots)?;
            let previous = crate::checkpoint_heads::read_staging(
                this.env.meta_db,
                this.env.checkpoint_heads_db,
                &this.txn,
                id,
            )?;
            let old: std::collections::BTreeSet<_> = previous.iter().collect();
            let new: std::collections::BTreeSet<_> = roots.iter().collect();
            // Pin the next forest before releasing the old one; shared roots
            // keep their existing staging owner without reference-count churn.
            for root in new.difference(&old) {
                this.change_head_references(root, true)?;
            }
            for root in old.difference(&new) {
                this.change_head_references(root, false)?;
            }
            let key = crate::checkpoint_heads::staging_key(id);
            if roots.is_empty() {
                this.delete_meta(&key)?;
            } else {
                this.put_meta(&key, &bytes)?;
            }
            Ok(())
        })
    }

    fn next_unowned_head(&self) -> Result<Option<NodeHash>, StoreError> {
        crate::checkpoint_heads::next_unowned(self.env.checkpoint_heads_db, &self.txn)
    }

    /// Reclaim at most `max_nodes` (1–4096); each node releases at most two children.
    /// Newly unowned children may be reclaimed in this call if budget remains;
    /// otherwise they stay queued across commits. Deletion never recurses.
    pub fn reclaim_checkpoint_heads(
        &mut self,
        max_nodes: usize,
    ) -> Result<ReclaimedHeads, StoreError> {
        self.mutate_heads(|this| {
            if max_nodes == 0 || max_nodes > 4096 {
                return Err(invalid("invalid historical head cleanup budget"));
            }
            let mut nodes = 0;
            while nodes < max_nodes {
                let Some(hash) = this.next_unowned_head()? else {
                    break;
                };
                let (count, node) = read(this.env.checkpoint_heads_db, &this.txn, &hash)?
                    .ok_or_else(|| invalid("queued historical head is missing"))?;
                if count != 0 {
                    return Err(invalid("owned historical head is queued for deletion"));
                }
                this.write_head_row(1, &hash, None)?;
                this.write_head_row(0, &hash, None)?;
                if let HeadNode::Branch { left, right, .. } = node {
                    this.change_head_references(&left, false)?;
                    this.change_head_references(&right, false)?;
                }
                nodes += 1;
            }
            Ok(ReclaimedHeads {
                nodes,
                complete: this.next_unowned_head()?.is_none(),
            })
        })
    }
}

#[cfg(test)]
mod tests;
