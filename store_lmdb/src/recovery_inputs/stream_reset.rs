use super::*;

impl RecoveryInputs {
    fn check_stream_reset_owner(&self, txn: &heed::RoTxn<'_>) -> Result<(), StoreError> {
        self.check_generation(txn)?;
        if stream_generation(self.manifest, txn, &self.context)? != self.stream_generation {
            return Err(invalid("stale recovery stream generation"));
        }
        if self.manifest.get(txn, b"reset").map_err(invalid)?.is_some() {
            return Err(invalid("recovery input session is being reset"));
        }
        Ok(())
    }

    pub fn stream_reset_pending(&self) -> Result<bool, StoreError> {
        let txn = self.env.read_txn().map_err(invalid)?;
        self.check_stream_reset_owner(&txn)?;
        Ok(self.manifest.get(&txn, &stream_metadata_key(b'r', &self.context)).map_err(invalid)?.is_some())
    }

    /// Invalidate only this raw stream. Other streams and their cached inputs
    /// remain available. The persisted marker allows cleanup to resume after
    /// interruption; it does not represent verified protocol state.
    pub fn begin_stream_reset(&self) -> Result<(), StoreError> {
        let mut txn = self.env.write_txn().map_err(invalid)?;
        self.check_stream_reset_owner(&txn)?;
        self.stream_generation.checked_add(1).ok_or_else(|| invalid("recovery stream generation exhausted"))?;
        let key = stream_metadata_key(b'r', &self.context);
        if self.manifest.get(&txn, &key).map_err(invalid)?.is_some() { return Ok(()); }
        progress(self.manifest, &txn, &self.context)?;
        self.manifest.put(&mut txn, &key, &0u64.to_be_bytes()).map_err(invalid)?;
        txn.commit().map_err(invalid)?;
        self.env.sync_after_commit().map_err(invalid)
    }

    /// Remove only this stream's positions, chunks and ownership rows. Each
    /// call charges at most 128 rows and 1 MiB + 64 key/value bytes, excluding
    /// bookkeeping writes. Legacy ownership registrations count too. Deleting a
    /// large chunk and its ownership row can take separate pages. Finalization
    /// follows the last deletion commit in a separate call. Partial state
    /// is readable only for cleanup, and accounting commits with each page.
    /// Legacy positioned chunks acquire an ownership row during cleanup. Legacy
    /// unpositioned chunks have no recoverable owner: keep their charge until
    /// whole-session cleanup, without exposing them to the new generation.
    pub fn reset_stream_page(&self, max_rows: usize, max_bytes: usize) -> Result<ResetPage, StoreError> {
        if max_rows == 0 || max_rows > 128 || max_bytes == 0 || max_bytes > MAX_CHUNK_BYTES + 64 {
            return Err(invalid("invalid recovery stream reset page budget"));
        }
        let mut txn = self.env.write_txn().map_err(invalid)?;
        self.check_stream_reset_owner(&txn)?;
        let marker = stream_metadata_key(b'r', &self.context);
        let mut next = counter(self.manifest.get(&txn, &marker).map_err(invalid)?)?;
        let mut state = progress(self.manifest, &txn, &self.context)?;
        if next > state.positions { return Err(invalid("recovery stream reset exceeds positions")); }
        let mut page = ResetPage { rows: 0, bytes: 0, complete: false };
        let mut total_positions = counter(self.manifest.get(&txn, b"positions").map_err(invalid)?)?;
        let mut total_bytes = counter(self.manifest.get(&txn, b"stream_bytes").map_err(invalid)?)?;
        while next < state.positions && page.rows < max_rows {
            let position = position_key(&self.context, next);
            let digest: [u8;32] = self.manifest.get(&txn, &position).map_err(invalid)?
                .ok_or_else(|| invalid("missing reset position"))?.try_into().map_err(invalid)?;
            let bytes = self.chunks.get(&txn, &digest).map_err(invalid)?.ok_or_else(|| invalid("missing reset chunk"))?;
            let owner = owned_chunk_key(&self.context, &digest);
            if self.manifest.get(&txn, &owner).map_err(invalid)?.is_none() {
                if self.stream_generation != 0 || bytes.is_empty() || bytes.len() > MAX_CHUNK_BYTES {
                    return Err(invalid("legacy reset chunk differs from stream"));
                }
                let size = owner.len() + 1;
                if size > max_bytes - page.bytes { break; }
                // This is raw bookkeeping from the position, not authenticated
                // ownership. Verify context and bytes before deleting the chunk.
                self.manifest.put(&mut txn, &owner, &[0]).map_err(invalid)?;
                page.rows += 1; page.bytes += size;
                continue;
            }
            let size = position.len() + digest.len();
            if size > max_bytes - page.bytes { break; }
            let length = bytes.len() as u64;
            state.bytes = state.bytes.checked_sub(length).ok_or_else(|| invalid("reset stream byte accounting underflow"))?;
            total_positions = total_positions.checked_sub(1).ok_or_else(|| invalid("reset position accounting underflow"))?;
            total_bytes = total_bytes.checked_sub(length).ok_or_else(|| invalid("reset aggregate byte accounting underflow"))?;
            self.manifest.delete(&mut txn, &position).map_err(invalid)?;
            next += 1; page.rows += 1; page.bytes += size;
        }
        self.manifest.put(&mut txn, &marker, &next.to_be_bytes()).map_err(invalid)?;
        self.manifest.put(&mut txn, b"positions", &total_positions.to_be_bytes()).map_err(invalid)?;
        self.manifest.put(&mut txn, b"stream_bytes", &total_bytes.to_be_bytes()).map_err(invalid)?;
        put_progress(self.manifest, &mut txn, &self.context, state)?;
        if next == state.positions {
            if state.bytes != 0 { return Err(invalid("reset stream bytes remain without positions")); }
            let prefix = owned_chunk_key(&self.context, &[0;32]);
            while page.rows < max_rows {
                let owner = self.manifest.prefix_iter(&txn, &prefix[..34]).map_err(invalid)?
                    .next().transpose().map_err(invalid)?;
                let Some((key, value)) = owner else { break; };
                if key.len() != 66 || value != [0] { return Err(invalid("invalid reset chunk ownership")); }
                let key: [u8;66] = key.try_into().map_err(invalid)?;
                let digest: [u8;32] = key[34..].try_into().map_err(invalid)?;
                if let Some(bytes) = self.chunks.get(&txn, &digest).map_err(invalid)? {
                    if bytes.is_empty() || bytes.len() > MAX_CHUNK_BYTES {
                        return Err(invalid("invalid reset chunk extent"));
                    }
                    let size = digest.len() + bytes.len();
                    if size > max_bytes - page.bytes { break; }
                    if self.digest(bytes) != digest { return Err(invalid("reset chunk differs from stream")); }
                    let count = counter(self.manifest.get(&txn, b"count").map_err(invalid)?)?
                        .checked_sub(1).ok_or_else(|| invalid("reset chunk accounting underflow"))?;
                    let retained = counter(self.manifest.get(&txn, b"bytes").map_err(invalid)?)?
                        .checked_sub(bytes.len() as u64).ok_or_else(|| invalid("reset byte accounting underflow"))?;
                    self.chunks.delete(&mut txn, &digest).map_err(invalid)?;
                    self.manifest.put(&mut txn, b"count", &count.to_be_bytes()).map_err(invalid)?;
                    self.manifest.put(&mut txn, b"bytes", &retained.to_be_bytes()).map_err(invalid)?;
                    page.rows += 1; page.bytes += size;
                } else {
                    let size = key.len() + 1;
                    if size > max_bytes - page.bytes { break; }
                    self.manifest.delete(&mut txn, &key).map_err(invalid)?;
                    page.rows += 1; page.bytes += size;
                }
            }
            let remains = self.manifest.prefix_iter(&txn, &prefix[..34]).map_err(invalid)?
                .next().transpose().map_err(invalid)?.is_some();
            // LMDB cannot reuse pages freed by its immediately preceding
            // transaction. Commit deletion first, then finalize in a separate
            // bounded call so the next payload write can reclaim those pages.
            if !remains && page.rows == 0 {
                let generation = self.stream_generation.checked_add(1).ok_or_else(|| invalid("recovery stream generation exhausted"))?;
                self.manifest.put(&mut txn, &stream_metadata_key(b'g', &self.context), &generation.to_be_bytes()).map_err(invalid)?;
                put_progress(self.manifest, &mut txn, &self.context, Progress { positions: 0, bytes: 0, complete: false })?;
                self.manifest.delete(&mut txn, &marker).map_err(invalid)?;
                page.complete = true;
            }
        }
        if !page.complete && page.rows == 0 { return Err(invalid("recovery stream reset page cannot fit next row")); }
        txn.commit().map_err(invalid)?;
        self.env.sync_after_commit().map_err(invalid)?;
        Ok(page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> Limits { Limits { chunks: 512, bytes: 1 << 20, map_bytes: 4 << 20 } }
    fn owner_count(inputs: &RecoveryInputs) -> u64 {
        inputs.manifest.prefix_iter(&inputs.env.read_txn().unwrap(), b"o/").unwrap().count() as u64
    }

    #[test]
    fn inventory_cleanup_at_map_pressure_preserves_history_and_recovers_capacity() {
        let directory = tempfile::tempdir().unwrap();
        let limits = Limits { chunks: 1024, bytes: 1 << 20, map_bytes: 1 << 20 };
        let root = RecoveryInputs::open(directory.path(), [1; 32], limits).unwrap();
        let history = root.select_stream([2; 32]).unwrap();
        let inventory = root.select_stream([3; 32]).unwrap();
        let retained = vec![7; 512 << 10];
        history.append(0, &retained).unwrap();
        let mut positions = 0u64;
        let failure = loop {
            let mut page = vec![3; 16 << 10];
            page[..8].copy_from_slice(&positions.to_be_bytes());
            match inventory.append_checked(positions, &page) {
                Ok(_) => positions += 1,
                Err(error) => break error,
            }
            assert!(positions < 64, "map pressure was not reached within its fixed budget");
        };
        assert!(positions > 0);
        assert!(matches!(failure, InputWriteError::Capacity), "unexpected failure: {failure}");
        assert!((positions + 1) * (16 << 10) + (retained.len() as u64) < limits.bytes,
            "pressure must come from physical map space, not the logical byte limit");
        eprintln!("inventory_map_pressure positions={positions} history_bytes={} failure={failure}", retained.len());
        assert_eq!(inventory.progress().unwrap().positions, positions);
        assert_eq!(history.read_position(0).unwrap().unwrap(), retained);
        drop(inventory); drop(history); drop(root);

        // Reopen before cleanup: no surviving transaction or allocator state
        // may provide hidden reserve capacity for these metadata writes.
        let root = RecoveryInputs::open(directory.path(), [1; 32], limits).unwrap();
        let inventory = root.select_stream([3; 32]).unwrap();
        let history = root.select_stream([2; 32]).unwrap();
        inventory.begin_stream_reset().expect("full cache must permit inventory reset marker");
        let mut complete = false;
        for _ in 0..16 {
            let page = inventory.reset_stream_page(128, MAX_CHUNK_BYTES + 64)
                .expect("full cache must permit bounded inventory cleanup");
            assert!(page.rows <= 128 && page.bytes <= MAX_CHUNK_BYTES + 64);
            if page.complete { complete = true; break; }
        }
        assert!(complete);
        assert_eq!(history.read_position(0).unwrap().unwrap(), retained);
        history.append(1, &vec![9; 256 << 10]).expect("inventory cleanup must return usable map capacity");
        history.seal(2).unwrap();
        drop(history); drop(inventory); drop(root);
        let root = RecoveryInputs::open(directory.path(), [1; 32], limits).unwrap();
        let history = root.select_stream([2; 32]).unwrap();
        assert_eq!(history.progress().unwrap().positions, 2);
        assert!(history.progress().unwrap().complete);
        assert_eq!(history.read_position(0).unwrap().unwrap(), retained);
        assert_eq!(history.read_position(1).unwrap().unwrap(), vec![9; 256 << 10]);
        assert_eq!(root.select_stream([3; 32]).unwrap().progress().unwrap().positions, 0);
    }

    #[test]
    fn paged_stream_reset_preserves_other_streams_and_rejects_stale_owners() {
        let dir = tempfile::tempdir().unwrap();
        let root = RecoveryInputs::open(dir.path(), [1;32], limits()).unwrap();
        for n in 10..110u8 { root.select_stream([n;32]).unwrap().append(0, b"history").unwrap(); }
        let history = root.select_stream([10;32]).unwrap();
        let inventory = root.select_stream([2;32]).unwrap();
        inventory.insert(b"raw").unwrap();
        inventory.append_batch(0, &[b"one".to_vec(), b"one".to_vec(), b"two".to_vec()]).unwrap();
        inventory.seal(3).unwrap();
        inventory.begin_stream_reset().unwrap();
        inventory.begin_stream_reset().unwrap();
        assert!(inventory.progress().is_err());
        assert!(inventory.append(3, b"x").is_err());
        let mut pages = 0;
        loop {
            let reopened = RecoveryInputs::open(dir.path(), [1;32], limits()).unwrap();
            let resumed = reopened.select_stream([2;32]).unwrap();
            assert!(resumed.stream_reset_pending().unwrap());
            assert_eq!(history.read_position(0).unwrap(), Some(b"history".to_vec()));
            let page = resumed.reset_stream_page(1, 81).unwrap();
            assert!(page.rows <= 1 && page.bytes <= 81);
            pages += 1;
            if page.complete { break; }
            assert!(pages < 12);
        }
        assert_eq!(pages, 10, "three positions, three chunks, three ownership rows, then finalization");
        assert!(inventory.read_position(0).is_err());
        assert!(inventory.insert(b"stale").is_err());
        assert!(inventory.begin_stream_reset().is_err());
        assert!(inventory.reset_stream_page(1, 81).is_err());
        assert!(inventory.begin_reset([1;32]).is_err());
        let fresh = root.select_stream([2;32]).unwrap();
        assert_eq!(fresh.progress().unwrap(), Progress { positions: 0, bytes: 0, complete: false });
        assert!(!fresh.stream_reset_pending().unwrap());
        assert_eq!(owner_count(&root), 100);
        fresh.append(0, b"new").unwrap();
        history.append(1, b"kept").unwrap();
        drop(fresh); drop(inventory); drop(history); drop(root);
        let reopened = RecoveryInputs::open(dir.path(), [1;32], limits()).unwrap();
        assert_eq!(reopened.select_stream([2;32]).unwrap().read_position(0).unwrap(), Some(b"new".to_vec()));
        assert_eq!(reopened.select_stream([10;32]).unwrap().read_position(1).unwrap(), Some(b"kept".to_vec()));
    }

    #[test]
    fn legacy_positioned_chunks_gain_ownership_without_discarding_unowned_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let root = RecoveryInputs::open(dir.path(), [1;32], limits()).unwrap();
        let inventory = root.select_stream([2;32]).unwrap();
        let orphan = inventory.insert(b"legacy-unpositioned").unwrap();
        inventory.append_batch(0, &[b"one".to_vec(), b"one".to_vec(), b"two".to_vec()]).unwrap();
        root.append(0, b"keep").unwrap();
        let mut txn = root.env.write_txn().unwrap();
        let keys: Vec<_> = root.manifest.prefix_iter(&txn, b"o/").unwrap()
            .map(|row| row.unwrap().0.to_vec()).collect();
        for key in keys { root.manifest.delete(&mut txn, &key).unwrap(); }
        let mut header = root.manifest.get(&txn, b"header").unwrap().unwrap().to_vec();
        header[..HEADER.len()].copy_from_slice(DIGEST_DOMAIN);
        root.manifest.put(&mut txn, b"header", &header).unwrap();
        txn.commit().unwrap();
        drop(inventory); drop(root);
        let root = RecoveryInputs::open(dir.path(), [1;32], limits()).unwrap();
        let inventory = root.select_stream([2;32]).unwrap();
        assert_eq!(inventory.get(&orphan).unwrap(), Some(b"legacy-unpositioned".to_vec()));
        assert_eq!(inventory.read_position(2).unwrap(), Some(b"two".to_vec()));
        inventory.begin_stream_reset().unwrap();
        let mut complete = false;
        for _ in 0..16 {
            let reopened = RecoveryInputs::open(dir.path(), [1;32], limits()).unwrap();
            let page = reopened.select_stream([2;32]).unwrap().reset_stream_page(1, 81).unwrap();
            assert!(page.rows <= 1 && page.bytes <= 81);
            if page.complete { complete = true; break; }
        }
        assert!(complete);
        let fresh = root.select_stream([2;32]).unwrap();
        assert_eq!(fresh.get(&orphan).unwrap(), None);
        assert_eq!(root.read_position(0).unwrap(), Some(b"keep".to_vec()));
        let txn = root.env.read_txn().unwrap();
        assert_eq!(counter(root.manifest.get(&txn, b"count").unwrap()).unwrap(), 2);
        assert_eq!(counter(root.manifest.get(&txn, b"bytes").unwrap()).unwrap(), 23);
        assert!(root.manifest.get(&txn, b"header").unwrap().unwrap().starts_with(HEADER));
        drop(txn);
        root.begin_reset([1;32]).unwrap();
        while !root.reset_page(128, MAX_CHUNK_BYTES + 64).unwrap().complete {}
        let empty = RecoveryInputs::open(dir.path(), [1;32], limits()).unwrap();
        assert_eq!(empty.chunks.len(&empty.env.read_txn().unwrap()).unwrap(), 0);
    }

    #[test]
    fn stream_reset_bounds_corruption_and_global_reset_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let root = RecoveryInputs::open(dir.path(), [1;32], limits()).unwrap();
        let foreign = root.append(0, b"keep").unwrap();
        let stream = root.select_stream([2;32]).unwrap();
        stream.append(0, b"one").unwrap();
        stream.begin_stream_reset().unwrap();
        for (rows, bytes) in [(0,81), (129,81), (1,0), (1,MAX_CHUNK_BYTES+65), (1,80)] {
            assert!(stream.reset_stream_page(rows, bytes).is_err());
        }
        let mut txn = root.env.write_txn().unwrap();
        root.manifest.put(&mut txn, &owned_chunk_key(&[2;32], &foreign), &[0]).unwrap();
        txn.commit().unwrap();
        assert!(stream.reset_stream_page(128, MAX_CHUNK_BYTES + 64).is_err());
        assert_eq!(root.read_position(0).unwrap(), Some(b"keep".to_vec()));
        assert_eq!(progress(root.manifest, &root.env.read_txn().unwrap(), &[2;32]).unwrap().positions, 1);
        let mut txn = root.env.write_txn().unwrap();
        root.manifest.delete(&mut txn, &owned_chunk_key(&[2;32], &foreign)).unwrap();
        txn.commit().unwrap();
        assert!(!stream.reset_stream_page(1,81).unwrap().complete);
        root.begin_reset([3;32]).unwrap();
        assert!(stream.reset_stream_page(1,81).is_err());
        while !root.reset_page(1,256).unwrap().complete {}
        assert!(stream.read_position(0).is_err());
        assert!(stream.begin_stream_reset().is_err());
        let reset_root = RecoveryInputs::open(dir.path(), [3;32], limits()).unwrap();
        assert_eq!(reset_root.progress().unwrap().positions, 0);
        reset_root.begin_stream_reset().unwrap();
        assert!(reset_root.reset_stream_page(1,1).unwrap().complete);
        let reset_root = RecoveryInputs::open(dir.path(), [3;32], limits()).unwrap();
        assert_eq!(reset_root.stream_generation, 1);
        reset_root.append(0, b"new").unwrap();
        reset_root.begin_reset([4;32]).unwrap();
        let mut complete = false;
        for _ in 0..32 {
            // This remains idempotent after cleanup deletes the per-stream
            // generation row, and still rejects a changed target.
            reset_root.begin_reset([4;32]).unwrap();
            assert!(reset_root.begin_reset([5;32]).is_err());
            let page = reset_root.reset_page(1,256).unwrap();
            if page.complete { complete = true; break; }
        }
        assert!(complete);
        assert!(reset_root.begin_reset([4;32]).is_err());
        assert_eq!(RecoveryInputs::open(dir.path(), [4;32], limits()).unwrap().progress().unwrap().positions, 0);
    }

    #[test]
    fn maximum_chunk_cleanup_and_empty_stream_reset_keep_exact_budgets() {
        let dir = tempfile::tempdir().unwrap();
        let root = RecoveryInputs::open(dir.path(), [1;32], limits()).unwrap();
        let stream = root.select_stream([2;32]).unwrap();
        stream.append(0, &vec![7;MAX_CHUNK_BYTES]).unwrap();
        stream.begin_stream_reset().unwrap();
        let first = stream.reset_stream_page(1,81).unwrap();
        assert_eq!(first, ResetPage { rows: 1, bytes: 81, complete: false });
        DIGEST_BYTES.with(|count| count.set(0));
        assert!(stream.reset_stream_page(1,MAX_CHUNK_BYTES+31).is_err());
        assert_eq!(DIGEST_BYTES.with(|count| count.get()), 0, "do not hash a payload outside the page budget");
        let chunk = stream.reset_stream_page(1,MAX_CHUNK_BYTES+32).unwrap();
        assert_eq!(DIGEST_BYTES.with(|count| count.get()), MAX_CHUNK_BYTES as u64);
        assert_eq!(chunk, ResetPage { rows: 1, bytes: MAX_CHUNK_BYTES+32, complete: false });
        let reopened = RecoveryInputs::open(dir.path(), [1;32], limits()).unwrap();
        let stream = reopened.select_stream([2;32]).unwrap();
        assert_eq!(stream.reset_stream_page(1,67).unwrap(), ResetPage { rows: 1, bytes: 67, complete: false });
        assert!(stream.stream_reset_pending().unwrap());
        assert!(stream.progress().is_err());
        let resumed = RecoveryInputs::open(dir.path(), [1;32], limits()).unwrap();
        let finishing = resumed.select_stream([2;32]).unwrap();
        DIGEST_BYTES.with(|count| count.set(0));
        assert_eq!(finishing.reset_stream_page(1,1).unwrap(), ResetPage { rows: 0, bytes: 0, complete: true });
        assert_eq!(DIGEST_BYTES.with(|count| count.get()), 0, "finalization does not reread payloads");
        assert!(stream.progress().is_err(), "completion invalidates the old owner");
        let fresh = reopened.select_stream([2;32]).unwrap();
        fresh.begin_stream_reset().unwrap();
        assert_eq!(fresh.reset_stream_page(1,1).unwrap(), ResetPage { rows: 0, bytes: 0, complete: true });
        let mut txn = root.env.write_txn().unwrap();
        root.manifest.put(&mut txn, &stream_metadata_key(b'g', &[2;32]), &u64::MAX.to_be_bytes()).unwrap();
        txn.commit().unwrap();
        let exhausted = root.select_stream([2;32]).unwrap();
        assert!(exhausted.begin_stream_reset().is_err());
    }
    #[test]
    fn legacy_registration_defers_payload_authentication_to_its_budgeted_deletion_page() {
        let dir = tempfile::tempdir().unwrap();
        let root = RecoveryInputs::open(dir.path(), [1;32], limits()).unwrap();
        let stream = root.select_stream([2;32]).unwrap();
        let digest = stream.append(0, &vec![7;MAX_CHUNK_BYTES]).unwrap();
        let mut txn = root.env.write_txn().unwrap();
        root.manifest.delete(&mut txn, &owned_chunk_key(&[2;32], &digest)).unwrap();
        txn.commit().unwrap();
        stream.begin_stream_reset().unwrap();
        DIGEST_BYTES.with(|count| count.set(0));
        let registration = stream.reset_stream_page(1,67).unwrap();
        assert_eq!(registration, ResetPage { rows: 1, bytes: 67, complete: false });
        assert_eq!(DIGEST_BYTES.with(|count| count.get()), 0);
        let reopened = RecoveryInputs::open(dir.path(), [1;32], limits()).unwrap();
        let resumed = reopened.select_stream([2;32]).unwrap();
        assert_eq!(resumed.reset_stream_page(1,81).unwrap().bytes, 81);
        // A saved ownership row must not authorize deletion of changed bytes.
        let mut txn = root.env.write_txn().unwrap();
        root.chunks.put(&mut txn, &digest, &vec![8;MAX_CHUNK_BYTES]).unwrap();
        txn.commit().unwrap();
        assert!(resumed.reset_stream_page(1,MAX_CHUNK_BYTES+32).is_err());
        assert_eq!(DIGEST_BYTES.with(|count| count.get()), MAX_CHUNK_BYTES as u64);
        let txn = root.env.read_txn().unwrap();
        assert!(root.chunks.get(&txn, &digest).unwrap().is_some());
        assert_eq!(counter(root.manifest.get(&txn, b"count").unwrap()).unwrap(), 1);
    }

}
