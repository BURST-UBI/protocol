use super::{authority_history::AuthorityHistory, invalid};
use crate::{
    checkpoint_replay::SelectionReceipt,
    checkpoint_store::CertifiedCheckpoint,
    confirmed_replay::{scratch_records::ScratchRecords, snapshot::SelectionSource, ReplayError},
};
use burst_consensus::checkpoint::{CheckpointAnchor, CheckpointCertificate};
use burst_store::StoreError;
use burst_types::BlockHash;
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, path::Path};

#[derive(Serialize, Deserialize)]
struct Row {
    certificate: CheckpointCertificate,
    receipt: SelectionReceipt,
    beacon: Option<burst_vrf::RandomOutput>,
}

#[derive(Serialize, Deserialize)]
struct RefreshHint {
    height: u64,
    hash: BlockHash,
    has_beacon: bool,
}

impl Row {
    fn refresh_hint(&self) -> RefreshHint {
        RefreshHint {
            height: self.certificate.checkpoint.height,
            hash: self.certificate.checkpoint.hash(),
            has_beacon: self.beacon.is_some(),
        }
    }
}

/// Private replay history, not durable authority or a signing inventory.
/// Certificates are reconstructed against the retained authority proof stream.
pub struct SelectionHistory {
    pub(super) authority: AuthorityHistory,
    rows: ScratchRecords<Row>,
    hints: ScratchRecords<RefreshHint>,
    count: usize,
    last_height: u64,
}

/// A private scheduling cursor. These rows cannot authorize imports or signing.
pub(crate) struct SelectionRefresh {
    rows: ScratchRecords<RefreshHint>,
    count: usize,
    position: usize,
}

impl SelectionRefresh {
    pub(crate) fn at_cycle_start(&self) -> bool {
        self.position == 0
    }

    /// Advance one row, returning its height only if its beacon is missing.
    /// `None` can also mean a skipped row; use `at_cycle_start` to detect wraparound.
    pub(crate) fn next(
        &mut self,
        store: &burst_store_lmdb::LmdbStore,
    ) -> Result<Option<u64>, StoreError> {
        if self.position == self.count {
            self.position = 0;
            return Ok(None);
        }
        let row = self
            .rows
            .get(&key(self.position))?
            .ok_or_else(|| invalid("missing selection refresh row"))?;
        self.position += 1;
        let txn = store.tx_begin_read()?;
        let stored = store.meta_store().get_meta_txn(
            &txn,
            &crate::checkpoint_store::archive_selection::beacon_key(row.hash),
        )?;
        Ok((!row.has_beacon && stored.is_none()).then_some(row.height))
    }
}

fn key(position: usize) -> [u8; 32] {
    let mut key = [0; 32];
    key[..8].copy_from_slice(&(position as u64).to_be_bytes());
    key
}

impl SelectionHistory {
    pub(crate) fn refresh_cursor(&self) -> Result<SelectionRefresh, StoreError> {
        Ok(SelectionRefresh {
            rows: self.hints.fork()?,
            count: self.count,
            position: 0,
        })
    }

    #[cfg(test)]
    pub(in crate::confirmed_replay) fn bytes_for_test(&self) -> u64 {
        self.rows.bytes_for_test() + self.hints.bytes_for_test()
    }

    #[cfg(test)]
    pub(in crate::confirmed_replay) fn truncate_for_test(&self) {
        self.rows.truncate_for_test();
    }

    pub(super) fn new(authority: AuthorityHistory, directory: &Path) -> Result<Self, StoreError> {
        Ok(Self {
            authority,
            rows: ScratchRecords::new(directory)?,
            hints: ScratchRecords::new(directory)?,
            count: 0,
            last_height: 0,
        })
    }

    pub(super) fn fork(&self, _directory: &Path) -> Result<Self, StoreError> {
        Ok(Self {
            authority: self.authority.fork()?,
            rows: self.rows.fork()?,
            hints: self.hints.fork()?,
            count: self.count,
            last_height: self.last_height,
        })
    }

    pub(super) fn push(&mut self, selection: CertifiedCheckpoint) -> Result<(), StoreError> {
        let height = selection.certificate().checkpoint.height;
        if height <= self.last_height {
            return Err(invalid("selection history is not strictly ordered"));
        }
        let count = self
            .count
            .checked_add(1)
            .ok_or_else(|| invalid("selection history overflow"))?;
        let row = Row {
            certificate: selection.certificate().clone(),
            receipt: selection.replayed().receipt(),
            beacon: selection.beacon().cloned(),
        };
        let mut rows = self.rows.fork()?;
        let mut hints = self.hints.fork()?;
        if rows.put(key(self.count), &row)? || hints.put(key(self.count), &row.refresh_hint())? {
            return Err(invalid("duplicate selection history position"));
        }
        self.rows = rows;
        self.hints = hints;
        self.count = count;
        self.last_height = height;
        Ok(())
    }

    fn row(&self, position: usize) -> Result<Row, StoreError> {
        if position >= self.count {
            return Err(invalid("selection history position is out of bounds"));
        }
        self.rows
            .get(&key(position))?
            .ok_or_else(|| invalid("missing selection history row"))
    }

    pub(crate) fn attach_beacon(
        &mut self,
        hash: BlockHash,
        beacon: burst_vrf::RandomOutput,
    ) -> Result<(), StoreError> {
        let mut found = None;
        for (position, selection) in self.iter_selections().enumerate() {
            let mut selection = selection.map_err(invalid)?.into_owned();
            if selection.certificate().checkpoint.hash() == hash {
                selection.attach_compact_beacon(beacon.clone())?;
                found = Some((position, selection.beacon().cloned()));
            }
        }
        let (position, beacon) =
            found.ok_or_else(|| invalid("beacon has no recovered selection"))?;
        let mut row = self.row(position)?;
        if row.beacon.is_some() {
            return Ok(());
        }
        row.beacon = beacon;
        let mut rows = self.rows.fork()?;
        let mut hints = self.hints.fork()?;
        if !rows.put(key(position), &row)? || !hints.put(key(position), &row.refresh_hint())? {
            return Err(invalid("updated selection history row was absent"));
        }
        self.rows = rows;
        self.hints = hints;
        Ok(())
    }

    /// Authenticate the history once for a batch, keeping both the input index
    /// and staged updates on disk. A failed input or trailing authority proof
    /// must not publish even the earlier, valid attachments.
    pub(crate) fn attach_beacons(
        &mut self,
        directory: &Path,
        beacons: impl IntoIterator<Item = Result<(BlockHash, burst_vrf::RandomOutput), StoreError>>,
    ) -> Result<(), StoreError> {
        let mut pending = ScratchRecords::<burst_vrf::RandomOutput>::new(directory)?;
        let mut count = 0usize;
        for entry in beacons {
            let (hash, beacon) = entry?;
            let hash = *hash.as_bytes();
            if let Some(existing) = pending.get(&hash)? {
                if bincode::serialize(&existing).map_err(invalid)?
                    != bincode::serialize(&beacon).map_err(invalid)?
                {
                    return Err(invalid("conflicting recovery beacons"));
                }
            } else {
                pending.put(hash, &beacon)?;
                count = count
                    .checked_add(1)
                    .ok_or_else(|| invalid("beacon count overflow"))?;
            }
        }
        if count == 0 {
            return Ok(());
        }
        let mut updated = self.rows.fork()?;
        let mut hints = self.hints.fork()?;
        let mut found = 0usize;
        for (position, selection) in self.iter_selections().enumerate() {
            let mut selection = selection.map_err(invalid)?.into_owned();
            let hash = selection.certificate().checkpoint.hash();
            if let Some(beacon) = pending.get(hash.as_bytes())? {
                selection.attach_compact_beacon(beacon)?;
                let mut row = self.row(position)?;
                if row.beacon.is_none() {
                    row.beacon = selection.beacon().cloned();
                    if !updated.put(key(position), &row)?
                        || !hints.put(key(position), &row.refresh_hint())?
                    {
                        return Err(invalid("updated selection history row was absent"));
                    }
                }
                found += 1;
            }
        }
        if found != count {
            return Err(invalid("beacon has no recovered selection"));
        }
        self.rows = updated;
        self.hints = hints;
        Ok(())
    }
}

impl SelectionSource for SelectionHistory {
    fn len(&self) -> usize {
        self.count
    }

    fn read_selection(&self, position: usize) -> Result<Cow<'_, CertifiedCheckpoint>, ReplayError> {
        let mut found = None;
        for (index, selection) in self.iter_selections().enumerate() {
            let selection = selection?;
            if position == index {
                found = Some(selection);
            }
        }
        found.ok_or_else(|| super::super::super::invalid("missing authenticated selection"))
    }

    fn iter_selections(
        &self,
    ) -> impl Iterator<Item = Result<Cow<'_, CertifiedCheckpoint>, ReplayError>> {
        let mut anchors = self.authority.iter();
        let mut current: Option<CheckpointAnchor> = None;
        let mut position = 0;
        let mut finished = false;
        std::iter::from_fn(move || {
            if finished {
                return None;
            }
            let result: Result<Option<Cow<'_, CertifiedCheckpoint>>, StoreError> = (|| {
                if position == self.count {
                    for anchor in anchors.by_ref() {
                        anchor?;
                    }
                    finished = true;
                    return Ok(None);
                }
                let row = self.row(position)?;
                let parent_height = row
                    .certificate
                    .checkpoint
                    .height
                    .checked_sub(1)
                    .ok_or_else(|| invalid("selection history includes genesis"))?;
                while current
                    .as_ref()
                    .is_none_or(|anchor| anchor.height() < parent_height)
                {
                    current = Some(
                        anchors
                            .next()
                            .ok_or_else(|| invalid("missing historical parent"))??,
                    );
                }
                let parent = current
                    .as_ref()
                    .ok_or_else(|| invalid("missing historical parent"))?;
                if parent.height() != parent_height {
                    return Err(invalid("selection history is out of authority order"));
                }
                let next = anchors
                    .next()
                    .ok_or_else(|| invalid("missing selection authority"))??;
                if next.hash() != row.certificate.checkpoint.hash() {
                    return Err(invalid("selection differs from historical authority"));
                }
                row.receipt.validate_at(&next)?;
                let replay = row.receipt.restore(parent)?;
                let mut selection =
                    CertifiedCheckpoint::from_compact(parent.clone(), row.certificate, replay)?;
                if let Some(beacon) = row.beacon {
                    selection.attach_compact_beacon(beacon)?;
                }
                current = Some(next);
                position += 1;
                Ok(Some(Cow::Owned(selection)))
            })();
            match result {
                Ok(value) => value.map(Ok),
                Err(error) => {
                    finished = true;
                    Some(Err(super::super::super::invalid(error.to_string())))
                }
            }
        })
    }
}
