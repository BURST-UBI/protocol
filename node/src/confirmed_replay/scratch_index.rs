use crate::confirmed_replay::scratch_file::ScratchFile as File;
use burst_store::StoreError;
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

#[cfg(test)]
use std::io::{Seek, SeekFrom, Write};

type Key = [u8; 32];
const MAX_NODE_BYTES: u64 = 128;

pub(super) enum ReplayIndex {
    Memory(std::collections::HashMap<Key, Key>),
    Disk(ScratchIndex),
}

impl ReplayIndex {
    pub(super) fn new(directory: Option<&Path>) -> Result<Self, StoreError> {
        match directory {
            Some(directory) => ScratchIndex::new(directory).map(Self::Disk),
            None => Ok(Self::Memory(std::collections::HashMap::new())),
        }
    }

    pub(super) fn get(&self, key: &Key) -> Result<Option<Key>, StoreError> {
        match self {
            Self::Memory(values) => Ok(values.get(key).copied()),
            Self::Disk(values) => values.get(key),
        }
    }

    pub(super) fn insert(&mut self, key: Key, value: Key) -> Result<Option<Key>, StoreError> {
        match self {
            Self::Memory(values) => Ok(values.insert(key, value)),
            Self::Disk(values) => values.insert(key, value),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Link {
    offset: u64,
    length: u64,
    digest: Key,
}

#[derive(Serialize, Deserialize)]
#[serde(bound(serialize = "[u8; N]: Serialize", deserialize = "[u8; N]: Deserialize<'de>"))]
enum FixedNode<const N: usize> {
    Leaf { key: Key, value: [u8; N] },
    Branch { bit: u16, left: Link, right: Link },
}

impl<const N: usize> FixedNode<N> {
    // Preserve the private bincode fixed-integer layout: u32 variant, then
    // fixed-width little-endian fields. No untrusted length drives allocation.
    fn encode<'a>(&self, buffer: &'a mut [u8; 128]) -> &'a [u8] {
        match self {
            Self::Leaf { key, value } => {
                buffer[..4].copy_from_slice(&0u32.to_le_bytes());
                buffer[4..36].copy_from_slice(key);
                buffer[36..36 + N].copy_from_slice(value);
                &buffer[..36 + N]
            }
            Self::Branch { bit, left, right } => {
                buffer[..4].copy_from_slice(&1u32.to_le_bytes());
                buffer[4..6].copy_from_slice(&bit.to_le_bytes());
                Self::encode_link(left, &mut buffer[6..54]);
                Self::encode_link(right, &mut buffer[54..102]);
                &buffer[..102]
            }
        }
    }

    fn encode_link(link: &Link, bytes: &mut [u8]) {
        bytes[..8].copy_from_slice(&link.offset.to_le_bytes());
        bytes[8..16].copy_from_slice(&link.length.to_le_bytes());
        bytes[16..48].copy_from_slice(&link.digest);
    }

    fn decode(bytes: &[u8]) -> Result<Self, StoreError> {
        match bytes {
            [0, 0, 0, 0, ..] if bytes.len() == 36 + N => Ok(Self::Leaf {
                key: bytes[4..36].try_into().map_err(invalid)?,
                value: bytes[36..36 + N].try_into().map_err(invalid)?,
            }),
            [1, 0, 0, 0, ..] if bytes.len() == 102 => Ok(Self::Branch {
                bit: u16::from_le_bytes(bytes[4..6].try_into().map_err(invalid)?),
                left: Self::decode_link(&bytes[6..54])?,
                right: Self::decode_link(&bytes[54..102])?,
            }),
            _ => Err(invalid("invalid replay scratch node encoding")),
        }
    }

    fn decode_link(bytes: &[u8]) -> Result<Link, StoreError> {
        Ok(Link {
            offset: u64::from_le_bytes(bytes[..8].try_into().map_err(invalid)?),
            length: u64::from_le_bytes(bytes[8..16].try_into().map_err(invalid)?),
            digest: bytes[16..48].try_into().map_err(invalid)?,
        })
    }
}

struct Storage {
    file: File,
    length: u64,
    failed: bool,
}

/// Private append-only scratch. A root authenticates both membership and absence;
/// no hash index grows in RAM. Clones retain independent roots over immutable rows.
/// This proves scratch integrity, never protocol authority.
#[derive(Clone)]
pub(super) struct FixedScratchIndex<const N: usize> {
    storage: Arc<Mutex<Storage>>,
    root: Option<Link>,
    count: usize,
}

/// An in-process traversal of one immutable root. Keeping the root and pending
/// authenticated links avoids rescanning earlier pages. It is not a restart token.
pub(super) struct FixedScratchCursor<const N: usize> {
    index: FixedScratchIndex<N>,
    pending: Vec<(Link, Option<u16>)>,
    previous_key: Option<Key>,
    full_scan: bool,
    count: usize,
    failed: bool,
}

#[derive(Debug)]
pub(super) struct ScanPage {
    pub nodes: usize,
    pub rows: usize,
    pub done: bool,
}

impl<const N: usize> FixedScratchCursor<N> {
    /// Charge branches as well as leaves so sparse paths cannot evade the budget.
    /// A failed callback or integrity check permanently invalidates this cursor.
    pub(super) fn visit_next(
        &mut self, max_nodes: usize,
        visit: impl FnMut(Key, [u8; N]) -> Result<(), StoreError>,
    ) -> Result<ScanPage, StoreError> {
        self.visit_next_with(max_nodes, visit)
    }

    pub(super) fn visit_next_with<E: From<StoreError>>(
        &mut self,
        max_nodes: usize,
        mut visit: impl FnMut(Key, [u8; N]) -> Result<(), E>,
    ) -> Result<ScanPage, E> {
        if self.failed { return Err(invalid("scratch traversal already failed").into()); }
        self.failed = true;
        if max_nodes == 0 { return Err(invalid("zero scratch traversal budget").into()); }
        self.index.storage.lock().map_err(invalid)?.check_storage()?;
        let mut page = ScanPage { nodes: 0, rows: 0, done: false };
        while page.nodes < max_nodes {
            let Some((link, parent_bit)) = self.pending.pop() else { break; };
            let node = {
                let mut storage = self.index.storage.lock().map_err(invalid)?;
                storage.check_storage()?;
                storage.read::<N>(link)?
            };
            page.nodes += 1;
            match node {
                FixedNode::Branch { bit, left, right } => {
                    if bit >= 256 || parent_bit.is_some_and(|parent| bit <= parent) {
                        return Err(invalid("unordered replay scratch index branch").into());
                    }
                    self.pending.push((right, Some(bit)));
                    self.pending.push((left, Some(bit)));
                }
                FixedNode::Leaf { key, value } => {
                    if self.previous_key.is_some_and(|previous| key <= previous)
                        || self.count >= self.index.count {
                        return Err(invalid("invalid replay scratch index traversal").into());
                    }
                    self.previous_key = Some(key);
                    self.count += 1;
                    page.rows += 1;
                    visit(key, value)?;
                }
            }
        }
        page.done = self.pending.is_empty();
        if page.done && self.full_scan && self.count != self.index.count {
            return Err(invalid("incomplete replay scratch index traversal").into());
        }
        self.failed = false;
        Ok(page)
    }
}

#[cfg(test)]
pub(super) struct FixedPairedScratchCursor<const N: usize> {
    left: FixedScratchIndex<N>,
    right: FixedScratchIndex<N>,
    pending: Vec<(Link, Link, Option<u16>)>,
    previous: Option<Key>,
    full_scan: bool,
    count: usize,
    failed: bool,
}

#[cfg(test)]
impl<const N: usize> FixedPairedScratchCursor<N> {
    pub(super) fn visit_next<E: From<StoreError>>(
        &mut self,
        max_nodes: usize,
        mut visit: impl FnMut(Key, [u8; N], [u8; N]) -> Result<(), E>,
    ) -> Result<ScanPage, E> {
        if self.failed { return Err(invalid("paired scratch traversal already failed").into()); }
        self.failed = true;
        if max_nodes < 2 { return Err(invalid("paired scratch traversal needs two nodes").into()); }
        if self.pending.is_empty() {
            self.left.storage.lock().map_err(invalid)?.check_storage()?;
            self.right.storage.lock().map_err(invalid)?.check_storage()?;
        }
        let mut page = ScanPage { nodes: 0, rows: 0, done: false };
        while max_nodes - page.nodes >= 2 {
            let Some((left, right, parent)) = self.pending.pop() else { break; };
            let a = {
                let mut storage = self.left.storage.lock().map_err(invalid)?;
                storage.check_storage()?;
                storage.read::<N>(left)?
            };
            let b = {
                let mut storage = self.right.storage.lock().map_err(invalid)?;
                storage.check_storage()?;
                storage.read::<N>(right)?
            };
            page.nodes += 2;
            match (a, b) {
                (
                    FixedNode::Leaf { key, value },
                    FixedNode::Leaf {
                        key: other_key,
                        value: other_value,
                    },
                ) => {
                    if key != other_key
                        || self.previous.is_some_and(|old| key <= old)
                        || self.count >= self.left.count
                    {
                        return Err(
                            invalid("paired scratch index keys differ or are unordered").into()
                        );
                    }
                    self.previous = Some(key);
                    self.count += 1;
                    page.rows += 1;
                    visit(key, value, other_value)?;
                }
                (
                    FixedNode::Branch { bit, left, right },
                    FixedNode::Branch {
                        bit: other_bit,
                        left: other_left,
                        right: other_right,
                    },
                ) => {
                    if bit != other_bit || bit >= 256 || parent.is_some_and(|old| bit <= old) {
                        return Err(invalid(
                            "paired scratch index branches differ or are unordered",
                        )
                        .into());
                    }
                    self.pending.push((right, other_right, Some(bit)));
                    self.pending.push((left, other_left, Some(bit)));
                }
                _ => return Err(invalid("paired scratch index shapes differ").into()),
            }
        }
        page.done = self.pending.is_empty();
        if page.done && self.full_scan && self.count != self.left.count {
            return Err(invalid("incomplete paired scratch index traversal").into());
        }
        self.failed = false;
        Ok(page)
    }
}

/// Ordered incremental construction retains only the unfinished right edge.
/// No partial root is exposed, and any failed push prevents publication.
pub(super) struct FixedSortedIndexBuilder<const N: usize> {
    index: FixedScratchIndex<N>,
    builder: crate::ordered_radix::Builder<[u8; N], Link>,
}

impl<const N: usize> FixedSortedIndexBuilder<N> {
    pub(super) fn new(directory: &Path) -> Result<Self, StoreError> {
        Ok(Self { index: FixedScratchIndex::new(directory)?,
            builder: crate::ordered_radix::Builder::new() })
    }

    pub(super) fn push(&mut self, key: Key, value: [u8; N]) -> Result<(), StoreError> {
        let mut storage = self.index.storage.lock().map_err(invalid)?;
        self.builder.push(key, value, |node| storage.append_ordered(node))
    }

    pub(super) fn finish(mut self) -> Result<FixedScratchIndex<N>, StoreError> {
        let mut storage = self.index.storage.lock().map_err(invalid)?;
        storage.check_storage()?;
        let (root, count) = self.builder.finish(|node| storage.append_ordered(node))?;
        self.index.root = root;
        self.index.count = count;
        drop(storage);
        Ok(self.index)
    }
}

impl Storage {
    fn append_ordered<const N: usize>(
        &mut self, node: crate::ordered_radix::Node<[u8; N], Link>,
    ) -> Result<Link, StoreError> {
        use crate::ordered_radix::Node;
        self.append(&match node {
            Node::Leaf { key, value } => FixedNode::Leaf { key, value },
            Node::Branch { bit, left, right } => FixedNode::Branch { bit, left, right },
        })
    }
}

/// A private compaction of one immutable root. Callers must finish it before
/// mutating that root; publication checks identity again rather than merging.
pub(super) struct FixedScratchCompaction<const N: usize> {
    cursor: FixedScratchCursor<N>,
    builder: FixedSortedIndexBuilder<N>,
    complete: bool,
    failed: bool,
}
impl<const N: usize> FixedScratchCompaction<N> {
    pub(super) fn advance(&mut self, max_nodes: usize) -> Result<ScanPage, StoreError> {
        if self.failed { return Err(invalid("scratch compaction already failed")); }
        self.failed = true;
        let page = self.cursor.visit_next(max_nodes, |key, value| self.builder.push(key, value))?;
        self.complete = page.done;
        self.failed = false;
        Ok(page)
    }

    pub(super) fn finish(self, index: &mut FixedScratchIndex<N>) -> Result<(), StoreError> {
        if self.failed || !self.complete {
            return Err(invalid("scratch compaction is incomplete"));
        }
        let expected = &self.cursor.index;
        if !Arc::ptr_eq(&expected.storage, &index.storage)
            || expected.root != index.root || expected.count != index.count {
            return Err(invalid("scratch compaction root changed"));
        }
        // Catch truncation or a failed append through another pinned owner too.
        index.storage.lock().map_err(invalid)?.check_storage()?;
        let compact = self.builder.finish()?;
        if compact.count != index.count {
            return Err(invalid("scratch compaction changed membership count"));
        }
        *index = compact;
        Ok(())
    }
}

struct Branch {
    link: Link,
    bit: u16,
    left: Link,
    right: Link,
    rightward: bool,
}

fn invalid(error: impl ToString) -> StoreError {
    StoreError::Corruption(error.to_string())
}

fn rightward(key: &Key, bit: u16) -> bool {
    key[usize::from(bit / 8)] & (0x80 >> (bit % 8)) != 0
}

impl Storage {
    fn check_storage(&self) -> Result<(), StoreError> {
        if self.failed || self.file.metadata().map_err(invalid)?.len() != self.length {
            return Err(invalid("replay scratch index changed or a write failed"));
        }
        Ok(())
    }

    fn read<const N: usize>(&mut self, link: Link) -> Result<FixedNode<N>, StoreError> {
        if link.length == 0
            || link.length > MAX_NODE_BYTES
            || link
                .offset
                .checked_add(link.length)
                .is_none_or(|end| end > self.length)
        {
            return Err(invalid("invalid replay scratch index link"));
        }
        let mut buffer = [0; MAX_NODE_BYTES as usize];
        let bytes = &mut buffer[..link.length as usize];
        self.file.read_exact_at(bytes, link.offset).map_err(invalid)?;
        if burst_crypto::blake2b_256(bytes) != link.digest {
            return Err(invalid("replay scratch index digest changed"));
        }
        FixedNode::decode(bytes)
    }

    fn append<const N: usize>(&mut self, node: &FixedNode<N>) -> Result<Link, StoreError> {
        self.check_storage()?;
        let mut buffer = [0; 128];
        let bytes = node.encode(&mut buffer);
        let length = bytes.len() as u64;
        if length > MAX_NODE_BYTES {
            return Err(invalid("replay scratch index node exceeds limit"));
        }
        let end = self
            .length
            .checked_add(length)
            .ok_or_else(|| invalid("replay scratch index overflow"))?;
        let link = Link {
            offset: self.length,
            length,
            digest: burst_crypto::blake2b_256(bytes),
        };
        self.failed = true;
        self.file.write_all_at(bytes, self.length).map_err(invalid)?;
        self.length = end;
        self.failed = false;
        Ok(link)
    }
}

impl<const N: usize> FixedScratchIndex<N> {
    pub(super) fn new(directory: &Path) -> Result<Self, StoreError> {
        if N != 32 && N != 48 { return Err(invalid("unsupported scratch index value width")); }
        Ok(Self {
            storage: Arc::new(Mutex::new(Storage {
                file: File::new_in(directory).map_err(invalid)?,
                length: 0,
                failed: false,
            })),
            root: None,
            count: 0,
        })
    }

    /// Build a private immutable root from strictly ordered keys. Each leaf and
    /// branch is written once; the unfinished right edge has at most 256 levels.
    pub(super) fn from_sorted(
        directory: &Path,
        entries: impl IntoIterator<Item = (Key, [u8; N])>,
    ) -> Result<Self, StoreError> {
        let mut builder = FixedSortedIndexBuilder::new(directory)?;
        for (key, value) in entries { builder.push(key, value)?; }
        builder.finish()
    }

    /// Rebuild when the shared file exceeds twice this root's live tree size.
    /// The copy and original are both charged during construction. Clones retain
    /// the original file after replacement; writes through other roots also
    /// contribute to the shared file size.
    pub(super) fn compact_if_amplified(&mut self, directory: &Path) -> Result<(), StoreError> {
        self.compact_until(directory, || false).map(|_| ())
    }

    /// Cancellation discards only the private builder; the original root remains
    /// readable. Callers must discard their larger incomplete operation too.
    pub(super) fn compact_until(&mut self, directory: &Path, mut cancelled: impl FnMut() -> bool)
        -> Result<bool, StoreError> {
        if cancelled() { return Ok(false); }
        if let Some(mut compaction) = self.start_compaction(directory)? {
            loop {
                let page = compaction.advance(128)?;
                if cancelled() { return Ok(false); }
                if page.done { break; }
            }
            compaction.finish(self)?;
        }
        Ok(true)
    }

    pub(super) fn start_compaction(&self, directory: &Path)
        -> Result<Option<FixedScratchCompaction<N>>, StoreError> {
        let live = (self.count as u64).checked_mul(138 + N as u64)
            .and_then(|bytes| bytes.checked_sub(if self.count == 0 { 0 } else { 102 }))
            .ok_or_else(|| invalid("scratch index live size overflow"))?;
        {
            let storage = self.storage.lock().map_err(invalid)?;
            storage.check_storage()?;
            if storage.length <= live.saturating_mul(2) { return Ok(None); }
        }
        Ok(Some(FixedScratchCompaction { cursor: self.cursor()?,
            builder: FixedSortedIndexBuilder::new(directory)?, complete: false, failed: false }))
    }

    #[cfg(test)]
    pub(super) fn truncate_for_test(&self) {
        self.storage.lock().unwrap().file.set_len(0).unwrap();
    }

    fn path(
        storage: &mut Storage,
        root: Link,
        key: &Key,
        mut visit: impl FnMut(Branch),
    ) -> Result<(Link, Key, [u8; N]), StoreError> {
        let mut current = root;
        let mut previous = None;
        loop {
            match storage.read::<N>(current)? {
                FixedNode::Leaf { key, value } => return Ok((current, key, value)),
                FixedNode::Branch { bit, left, right } => {
                    if bit >= 256 || previous.is_some_and(|old| bit <= old) {
                        return Err(invalid("unordered replay scratch index branch"));
                    }
                    let rightward = rightward(key, bit);
                    visit(Branch {
                        link: current,
                        bit,
                        left,
                        right,
                        rightward,
                    });
                    current = if rightward { right } else { left };
                    previous = Some(bit);
                }
            }
        }
    }

    pub(super) fn get(&self, key: &Key) -> Result<Option<[u8; N]>, StoreError> {
        let mut storage = self.storage.lock().map_err(invalid)?;
        storage.check_storage()?;
        let Some(root) = self.root else {
            return Ok(None);
        };
        let (_, found, value) = Self::path(&mut storage, root, key, |_| {})?;
        Ok((found == *key).then_some(value))
    }

    pub(super) fn cursor(&self) -> Result<FixedScratchCursor<N>, StoreError> {
        self.storage.lock().map_err(invalid)?.check_storage()?;
        Ok(FixedScratchCursor {
            index: self.clone(),
            pending: self.root.into_iter().map(|root| (root, None)).collect(),
            previous_key: None,
            full_scan: true,
            count: 0,
            failed: false,
        })
    }

    /// Seek an exclusive key boundary in at most 256 authenticated branches plus one leaf.
    /// Completion covers only this suffix, not the count of skipped prefix rows.
    pub(super) fn cursor_after(&self, after: Key) -> Result<FixedScratchCursor<N>, StoreError> {
        let mut cursor = self.cursor()?;
        cursor.pending.clear();
        cursor.previous_key = Some(after);
        cursor.full_scan = false;
        let Some(root) = self.root else {
            if self.count != 0 { return Err(invalid("missing scratch index root")); }
            return Ok(cursor);
        };
        let mut storage = self.storage.lock().map_err(invalid)?;
        let mut path = Vec::new();
        let (leaf, found, _) = Self::path(&mut storage, root, &after, |branch| path.push(branch))?;
        let difference = found.iter().zip(after).enumerate().find_map(|(index, (a, b))| {
            let bits = a ^ b;
            (bits != 0).then(|| (index * 8 + bits.leading_zeros() as usize) as u16)
        }).unwrap_or(256);
        let mut parent = None;
        for branch in path {
            // Below the first differing bit, this whole subtree is on one
            // side of the boundary; compressed prefixes must not be ignored.
            if branch.bit >= difference {
                if found > after { cursor.pending.push((branch.link, parent)); }
                return Ok(cursor);
            }
            if !branch.rightward { cursor.pending.push((branch.right, Some(branch.bit))); }
            parent = Some(branch.bit);
        }
        if found > after { cursor.pending.push((leaf, parent)); }
        Ok(cursor)
    }

    #[cfg(test)]
    pub(super) fn visit(
        &self,
        mut visit: impl FnMut(Key, [u8; N]) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let mut cursor = self.cursor()?;
        while !cursor.visit_next(1024, &mut visit)?.done {}
        Ok(())
    }

    /// Traverse indexes with the same key set together. Every node is checked
    /// once and unequal inventories fail closed, including missing-only rows.
    #[cfg(test)]
    pub(super) fn visit_paired(
        &self,
        other: &Self,
        visit: impl FnMut(Key, [u8; N], [u8; N]) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.visit_paired_with(other, visit)
    }

    #[cfg(test)]
    pub(super) fn paired_cursor(&self, other: &Self) -> Result<FixedPairedScratchCursor<N>, StoreError> {
        self.storage.lock().map_err(invalid)?.check_storage()?;
        other.storage.lock().map_err(invalid)?.check_storage()?;
        if self.count != other.count { return Err(invalid("paired scratch index counts differ")); }
        let pending = match (self.root, other.root) {
            (Some(left), Some(right)) => vec![(left, right, None)],
            (None, None) if self.count == 0 => Vec::new(),
            _ => return Err(invalid("paired scratch index roots differ")),
        };
        Ok(FixedPairedScratchCursor { left: self.clone(), right: other.clone(), pending,
            previous: None, full_scan: true, count: 0, failed: false })
    }

    #[cfg(test)]
    pub(super) fn paired_cursor_after(&self, other: &Self, after: Key) -> Result<FixedPairedScratchCursor<N>, StoreError> {
        if self.count != other.count { return Err(invalid("paired scratch index counts differ")); }
        let left = self.cursor_after(after)?;
        let right = other.cursor_after(after)?;
        if left.pending.len() != right.pending.len() { return Err(invalid("paired scratch suffix shapes differ")); }
        let mut pending = Vec::new();
        for ((a, parent), (b, other_parent)) in left.pending.into_iter().zip(right.pending) {
            if parent != other_parent { return Err(invalid("paired scratch suffix branches differ")); }
            pending.push((a, b, parent));
        }
        Ok(FixedPairedScratchCursor { left: self.clone(), right: other.clone(), pending,
            previous: Some(after), full_scan: false, count: 0, failed: false })
    }

    #[cfg(test)]
    pub(super) fn visit_paired_with<E: From<StoreError>>(
        &self,
        other: &Self,
        mut visit: impl FnMut(Key, [u8; N], [u8; N]) -> Result<(), E>,
    ) -> Result<(), E> {
        let mut cursor = self.paired_cursor(other)?;
        while !cursor.visit_next(1024, &mut visit)?.done {}
        Ok(())
    }

    pub(super) fn insert(&mut self, key: Key, value: [u8; N]) -> Result<Option<[u8; N]>, StoreError> {
        let mut storage = self.storage.lock().map_err(invalid)?;
        storage.check_storage()?;
        let Some(root) = self.root else {
            self.root = Some(storage.append::<N>(&FixedNode::Leaf { key, value })?);
            self.count = 1;
            return Ok(None);
        };
        let mut path = Vec::new();
        let (leaf, found, old) = Self::path(&mut storage, root, &key, |branch| path.push(branch))?;
        let replaced = found == key;
        let count = if replaced {
            self.count
        } else {
            self.count
                .checked_add(1)
                .ok_or_else(|| invalid("replay scratch index count overflow"))?
        };
        let mut next = storage.append::<N>(&FixedNode::Leaf { key, value })?;
        let prefix = if replaced {
            path.len()
        } else {
            let bit = key
                .iter()
                .zip(found)
                .enumerate()
                .find_map(|(index, (a, b))| {
                    let difference = a ^ b;
                    (difference != 0)
                        .then(|| (index * 8 + difference.leading_zeros() as usize) as u16)
                })
                .ok_or_else(|| invalid("missing replay scratch index split"))?;
            let prefix = path
                .iter()
                .position(|branch| branch.bit >= bit)
                .unwrap_or(path.len());
            if path.get(prefix).is_some_and(|branch| branch.bit == bit) {
                return Err(invalid("inconsistent replay scratch index split"));
            }
            let sibling = path.get(prefix).map_or(leaf, |branch| branch.link);
            let (left, right) = if rightward(&key, bit) {
                (sibling, next)
            } else {
                (next, sibling)
            };
            next = storage.append::<N>(&FixedNode::Branch { bit, left, right })?;
            prefix
        };
        for branch in path[..prefix].iter().rev() {
            let (left, right) = if branch.rightward {
                (branch.left, next)
            } else {
                (next, branch.right)
            };
            next = storage.append::<N>(&FixedNode::Branch {
                bit: branch.bit,
                left,
                right,
            })?;
        }
        self.root = Some(next);
        self.count = count;
        Ok(replaced.then_some(old))
    }
}

pub(super) type ScratchIndex = FixedScratchIndex<32>;
pub(super) type ScratchCursor = FixedScratchCursor<32>;
#[cfg(test)]
type Node = FixedNode<32>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn wide_leaf_codec_preserves_descriptor_and_rejects_wrong_width() {
        let value = std::array::from_fn(|i| i as u8);
        let node = FixedNode::<48>::Leaf { key: [7; 32], value };
        let mut buffer = [0; 128];
        let bytes = node.encode(&mut buffer);
        assert_eq!(bytes.len(), 84);
        assert_eq!(&bytes[..4], &[0; 4]);
        assert_eq!(&bytes[4..36], &[7; 32]);
        assert_eq!(&bytes[36..], &value);
        assert!(matches!(FixedNode::<48>::decode(bytes).unwrap(),
            FixedNode::Leaf { key, value: decoded } if key == [7; 32] && decoded == value));
        assert!(FixedNode::<32>::decode(bytes).is_err());
        for length in 0..bytes.len() { assert!(FixedNode::<48>::decode(&bytes[..length]).is_err()); }
        let mut extra = bytes.to_vec(); extra.push(0);
        assert!(FixedNode::<48>::decode(&extra).is_err());
    }

    #[test]
    fn fixed_node_codec_matches_legacy_bytes_and_strict_decoder() {
        use bincode::Options;
        let legacy = |bytes: &[u8]| bincode::DefaultOptions::new()
            .with_fixint_encoding().reject_trailing_bytes().with_limit(MAX_NODE_BYTES)
            .deserialize::<Node>(bytes);
        let mut random = 0x4d595df4d0f33173u64;
        let mut next = || {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            random
        };
        for bit in [0, 1, 127, 255, 256, u16::MAX] {
            for _ in 0..32 {
                let mut key = [0; 32];
                let mut value = [0; 32];
                for bytes in key.chunks_exact_mut(8).chain(value.chunks_exact_mut(8)) {
                    bytes.copy_from_slice(&next().to_le_bytes());
                }
                for node in [Node::Leaf { key, value }, Node::Branch { bit,
                    left: Link { offset: next(), length: next(), digest: key },
                    right: Link { offset: next(), length: next(), digest: value } }] {
                    let expected = bincode::serialize(&node).unwrap();
                    let mut buffer = [0; 128];
                    assert_eq!(node.encode(&mut buffer), expected);
                    assert_eq!(bincode::serialize(&Node::decode(&expected).unwrap()).unwrap(), expected);
                    assert!(legacy(&expected).is_ok());
                    for length in 0..expected.len() { assert!(Node::decode(&expected[..length]).is_err()); }
                    let mut trailing = expected.clone(); trailing.push(0);
                    assert!(Node::decode(&trailing).is_err());
                    for tag in [2u32, 256, u32::MAX] {
                        let mut changed = expected.clone(); changed[..4].copy_from_slice(&tag.to_le_bytes());
                        assert!(Node::decode(&changed).is_err());
                        assert!(legacy(&changed).is_err());
                    }
                }
            }
        }
        // Include syntactically valid arbitrary payloads: link bounds and branch
        // order remain the authenticated traversal's responsibility.
        for length in 0..=129 {
            for tag in 0..=2u32 {
                let mut bytes: Vec<_> = (0..length).map(|_| next() as u8).collect();
                if length >= 4 { bytes[..4].copy_from_slice(&tag.to_le_bytes()); }
                let old = legacy(&bytes).map(|node| bincode::serialize(&node).unwrap());
                let new = Node::decode(&bytes).map(|node| bincode::serialize(&node).unwrap());
                assert_eq!(old.is_ok(), new.is_ok());
                if let (Ok(old), Ok(new)) = (old, new) { assert_eq!(old, new); }
            }
        }
    }

    #[test]
    fn disk_index_matches_memory_across_splits_replacements_and_absence() {
        let directory = tempfile::tempdir().unwrap();
        let mut disk = ScratchIndex::new(directory.path()).unwrap();
        let mut expected = BTreeMap::new();
        for round in 0u32..2048 {
            let key = burst_crypto::blake2b_256(&(round % 997).to_be_bytes());
            let value = burst_crypto::blake2b_256(&round.to_be_bytes());
            assert_eq!(
                disk.insert(key, value).unwrap(),
                expected.insert(key, value)
            );
            assert_eq!(disk.count, expected.len());
            assert_eq!(disk.get(&key).unwrap(), Some(value));
            let missing = burst_crypto::blake2b_256(&(round + 10000).to_be_bytes());
            assert_eq!(disk.get(&missing).unwrap(), expected.get(&missing).copied());
        }
        let mut visited = BTreeMap::new();
        disk.visit(|key, value| {
            assert_eq!(disk.get(&key)?, Some(value));
            assert!(visited.insert(key, value).is_none());
            Ok(())
        })
        .unwrap();
        assert_eq!(visited, expected);
        let mut calls = 0;
        assert!(disk
            .visit(|_, _| {
                calls += 1;
                Err(invalid("stop"))
            })
            .is_err());
        assert_eq!(calls, 1);
        for (key, value) in expected {
            assert_eq!(disk.get(&key).unwrap(), Some(value));
        }
    }

    #[test]
    fn forks_do_not_inherit_replacements_or_appends() {
        let directory = tempfile::tempdir().unwrap();
        let mut parent = ScratchIndex::new(directory.path()).unwrap();
        parent.insert([0; 32], [1; 32]).unwrap();
        let mut child = parent.clone();
        child.insert([0; 32], [2; 32]).unwrap();
        child.insert([255; 32], [3; 32]).unwrap();
        parent.insert([1; 32], [4; 32]).unwrap();
        assert_eq!(parent.get(&[0; 32]).unwrap(), Some([1; 32]));
        assert_eq!(child.get(&[0; 32]).unwrap(), Some([2; 32]));
        assert_eq!(parent.get(&[255; 32]).unwrap(), None);
        assert_eq!(child.get(&[1; 32]).unwrap(), None);
        assert_eq!(parent.count, 2);
        assert_eq!(child.count, 2);
        drop(parent);
        assert_eq!(child.get(&[255; 32]).unwrap(), Some([3; 32]));
        drop(child);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn every_key_bit_can_be_a_branch_without_unbounded_depth() {
        let directory = tempfile::tempdir().unwrap();
        let mut index = ScratchIndex::new(directory.path()).unwrap();
        index.insert([0; 32], [0; 32]).unwrap();
        for bit in (0..256).rev() {
            let mut key = [0; 32];
            key[bit / 8] = 0x80 >> (bit % 8);
            index.insert(key, key).unwrap();
        }
        for bit in 0..256 {
            let mut key = [0; 32];
            key[bit / 8] = 0x80 >> (bit % 8);
            assert_eq!(index.get(&key).unwrap(), Some(key));
        }
        assert_eq!(index.get(&[0; 32]).unwrap(), Some([0; 32]));
        assert_eq!(index.get(&[255; 32]).unwrap(), None);
    }

    #[test]
    fn compacted_index_bounds_obsolete_paths_and_preserves_pinned_roots() {
        use crate::confirmed_replay::scratch_file::ScratchLimit;
        let directory = tempfile::tempdir().unwrap();
        let limit = ScratchLimit::acquire(directory.path(), 3 << 20).unwrap();
        let mut index = ScratchIndex::new(directory.path()).unwrap();
        index.insert([1;32], [2;32]).unwrap();
        let pinned = index.clone();
        for number in 0..4096u64 {
            let key = burst_crypto::blake2b_256(&number.to_be_bytes());
            index.insert(key, key).unwrap();
            index.compact_if_amplified(directory.path()).unwrap();
        }
        for number in 0..4096u64 {
            let key = burst_crypto::blake2b_256(&number.to_be_bytes());
            assert_eq!(index.get(&key).unwrap(), Some(key));
            assert_eq!(pinned.get(&key).unwrap(), None);
        }
        assert_eq!(pinned.get(&[1;32]).unwrap(), Some([2;32]));
        index.insert([1;32], [3;32]).unwrap();
        index.compact_if_amplified(directory.path()).unwrap();
        assert_eq!(index.get(&[1;32]).unwrap(), Some([3;32]));
        assert_eq!(pinned.get(&[1;32]).unwrap(), Some([2;32]));
        index.storage.lock().unwrap().file.set_len(0).unwrap();
        assert!(index.compact_if_amplified(directory.path()).is_err());
        drop(index);
        drop(pinned);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
    }

    #[test]
    fn paged_compaction_bounds_scan_and_rejects_stale_or_incomplete_publication() {
        use crate::confirmed_replay::scratch_file::ScratchLimit;
        let directory = tempfile::tempdir().unwrap();
        let limit = ScratchLimit::acquire(directory.path(), 3 << 20).unwrap();
        let mut index = ScratchIndex::new(directory.path()).unwrap();
        for n in 0..512u64 {
            let key = burst_crypto::blake2b_256(&n.to_be_bytes());
            index.insert(key, key).unwrap();
        }
        let pinned = index.clone();
        let pending = index.start_compaction(directory.path()).unwrap().unwrap();
        assert!(pending.finish(&mut index).is_err());
        let mut pending = index.start_compaction(directory.path()).unwrap().unwrap();
        let mut pages = 0;
        let mut nodes = 0;
        loop {
            let page = pending.advance(7).unwrap();
            assert!(page.nodes <= 7);
            pages += 1;
            nodes += page.nodes;
            if page.done { break; }
        }
        assert_eq!(nodes, 1023);
        assert!(pages > 100);
        let mut stale = index.start_compaction(directory.path()).unwrap().unwrap();
        while !stale.advance(11).unwrap().done {}
        pending.finish(&mut index).unwrap();
        assert!(stale.finish(&mut index).is_err());
        for n in 0..512u64 {
            let key = burst_crypto::blake2b_256(&n.to_be_bytes());
            assert_eq!(index.get(&key).unwrap(), Some(key));
            assert_eq!(pinned.get(&key).unwrap(), Some(key));
        }
        // A zero-budget error invalidates the compaction and prevents publication.
        for n in 0..128u64 { index.insert([1;32], burst_crypto::blake2b_256(&n.to_be_bytes())).unwrap(); }
        let mut pending = index.start_compaction(directory.path()).unwrap().unwrap();
        assert!(pending.advance(0).is_err());
        assert!(pending.advance(7).is_err());
        assert!(pending.finish(&mut index).is_err());
        drop(index); drop(pinned);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
    }

    #[test]
    fn cancelled_compaction_discards_builder_and_preserves_original_root() {
        use crate::confirmed_replay::scratch_file::ScratchLimit;
        let directory = tempfile::tempdir().unwrap();
        let limit = ScratchLimit::acquire(directory.path(), 3 << 20).unwrap();
        let mut index = ScratchIndex::new(directory.path()).unwrap();
        for n in 0..512u64 {
            let key = burst_crypto::blake2b_256(&n.to_be_bytes());
            index.insert(key, key).unwrap();
        }
        let pinned = index.clone();
        let before = limit.used_temporary_bytes().unwrap();
        let mut calls = 0;
        assert!(!index.compact_until(directory.path(), || { calls += 1; calls == 3 }).unwrap());
        assert_eq!(calls, 3);
        assert_eq!(limit.used_temporary_bytes().unwrap(), before);
        assert!(Arc::ptr_eq(&index.storage, &pinned.storage));
        assert!(index.root == pinned.root);
        for n in 0..512u64 {
            let key = burst_crypto::blake2b_256(&n.to_be_bytes());
            assert_eq!(index.get(&key).unwrap(), Some(key));
        }
        assert!(index.compact_until(directory.path(), || false).unwrap());
        assert!(!Arc::ptr_eq(&index.storage, &pinned.storage));
        drop(index); drop(pinned);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
    }

    #[test]
    fn damaged_index_is_not_a_missing_lifecycle() {
        let directory = tempfile::tempdir().unwrap();
        let mut index = ScratchIndex::new(directory.path()).unwrap();
        index.insert([1; 32], [2; 32]).unwrap();
        {
            let mut storage = index.storage.lock().unwrap();
            storage.file.rewind().unwrap();
            storage.file.write_all(&[255]).unwrap();
        }
        assert!(index.get(&[1; 32]).is_err());
        assert!(index.get(&[99; 32]).is_err());
        assert!(index.insert([99; 32], [0; 32]).is_err());
        index.storage.lock().unwrap().file.set_len(0).unwrap();
        assert!(index.get(&[1; 32]).is_err());
        assert!(index.get(&[99; 32]).is_err());
    }
}

#[cfg(test)]
mod bulk_tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn bulk_index_is_linear_sized_and_preserves_lookup_forks_and_integrity() {
        let directory = tempfile::tempdir().unwrap();
        let mut expected = BTreeMap::new();
        expected.insert([0; 32], [9; 32]);
        for bit in 0..256 {
            let mut key = [0; 32];
            key[bit / 8] = 0x80 >> (bit % 8);
            expected.insert(key, key);
        }
        for i in 0u32..2048 {
            expected.insert(burst_crypto::blake2b_256(&i.to_be_bytes()), [7; 32]);
        }
        let index = ScratchIndex::from_sorted(directory.path(), expected.clone()).unwrap();
        let link = Link {
            offset: 0,
            length: 1,
            digest: [0; 32],
        };
        let leaf = bincode::serialized_size(&Node::Leaf {
            key: [0; 32],
            value: [0; 32],
        })
        .unwrap();
        let branch = bincode::serialized_size(&Node::Branch {
            bit: 0,
            left: link,
            right: link,
        })
        .unwrap();
        assert_eq!(
            index.storage.lock().unwrap().length,
            expected.len() as u64 * leaf + (expected.len() as u64 - 1) * branch
        );
        for (key, value) in &expected {
            assert_eq!(index.get(key).unwrap(), Some(*value));
        }
        let mut visited = BTreeMap::new();
        index
            .visit(|key, value| {
                assert!(visited.insert(key, value).is_none());
                Ok(())
            })
            .unwrap();
        assert_eq!(visited, expected);
        let mut child = index.clone();
        child.insert([0; 32], [8; 32]).unwrap();
        child.insert([255; 32], [6; 32]).unwrap();
        assert_eq!(index.get(&[0; 32]).unwrap(), Some([9; 32]));
        assert_eq!(index.get(&[255; 32]).unwrap(), None);
        assert_eq!(child.get(&[0; 32]).unwrap(), Some([8; 32]));
        let mut storage = index.storage.lock().unwrap();
        storage
            .file
            .seek(SeekFrom::Start(index.root.unwrap().offset))
            .unwrap();
        storage.file.write_all(&[255]).unwrap();
        drop(storage);
        assert!(index.get(&[0; 32]).is_err());
        assert!(index.get(&[255; 32]).is_err());
        assert!(index.visit(|_, _| Ok(())).is_err());
    }

    #[test]
    fn bulk_index_rejects_unsorted_and_duplicate_keys_and_handles_empty_singleton() {
        let directory = tempfile::tempdir().unwrap();
        let empty = ScratchIndex::from_sorted(directory.path(), []).unwrap();
        assert_eq!(empty.count, 0);
        assert_eq!(empty.storage.lock().unwrap().length, 0);
        assert_eq!(empty.get(&[1; 32]).unwrap(), None);
        let single = ScratchIndex::from_sorted(directory.path(), [([1; 32], [2; 32])]).unwrap();
        assert_eq!(single.get(&[1; 32]).unwrap(), Some([2; 32]));
        for entries in [
            [([1; 32], [2; 32]), ([1; 32], [3; 32])],
            [([2; 32], [2; 32]), ([1; 32], [3; 32])],
        ] {
            assert!(ScratchIndex::from_sorted(directory.path(), entries).is_err());
        }
    }
}

#[cfg(test)]
mod paired_tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn paired_scan_matches_different_build_orders_and_rejects_inventory_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let keys: BTreeMap<_, _> = (0u32..512)
            .map(|i| (burst_crypto::blake2b_256(&i.to_be_bytes()), [1; 32]))
            .collect();
        let left = ScratchIndex::from_sorted(directory.path(), keys.clone()).unwrap();
        let mut right = ScratchIndex::new(directory.path()).unwrap();
        for key in keys.keys().rev() {
            right.insert(*key, [2; 32]).unwrap();
        }
        let mut seen = Vec::new();
        left.visit_paired(&right, |key, a, b| {
            assert_eq!(a, [1; 32]);
            assert_eq!(b, [2; 32]);
            seen.push(key);
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, keys.keys().copied().collect::<Vec<_>>());
        left.visit_paired(&left, |_, a, b| {
            assert_eq!(a, b);
            Ok(())
        })
        .unwrap();
        let mut calls = 0;
        assert!(left
            .visit_paired(&right, |_, _, _| {
                calls += 1;
                Err(invalid("stop"))
            })
            .is_err());
        assert_eq!(calls, 1);
        let short =
            ScratchIndex::from_sorted(directory.path(), keys.clone().into_iter().skip(1)).unwrap();
        assert!(left.visit_paired(&short, |_, _, _| Ok(())).is_err());
        let mut other_keys = keys.clone();
        other_keys.pop_first();
        assert!(!keys.contains_key(&[255; 32]));
        other_keys.insert([255; 32], [3; 32]);
        let other = ScratchIndex::from_sorted(directory.path(), other_keys).unwrap();
        assert!(left.visit_paired(&other, |_, _, _| Ok(())).is_err());
        let empty = ScratchIndex::from_sorted(directory.path(), []).unwrap();
        empty
            .visit_paired(&empty, |_, _, _| panic!("empty scan"))
            .unwrap();
        assert!(left.visit_paired(&empty, |_, _, _| Ok(())).is_err());
        let mut storage = right.storage.lock().unwrap();
        storage
            .file
            .seek(SeekFrom::Start(right.root.unwrap().offset))
            .unwrap();
        storage.file.write_all(&[255]).unwrap();
        drop(storage);
        assert!(left.visit_paired(&right, |_, _, _| Ok(())).is_err());
    }
}

#[cfg(test)]
mod cursor_tests {
    use super::*;

    fn key(value: u64) -> Key {
        let mut key = [0; 32];
        key[24..].copy_from_slice(&value.to_be_bytes());
        key
    }

    #[test]
    fn bounded_pages_visit_each_node_once_and_keep_the_original_root() {
        let dir = tempfile::tempdir().unwrap();
        let rows: Vec<_> = (0..257).map(|n| (key(n * 3), key(n))).collect();
        let mut index = ScratchIndex::from_sorted(dir.path(), rows.iter().copied()).unwrap();
        for budget in [1, 2, 7, 1024] {
            let mut cursor = index.cursor().unwrap();
            let mut output = Vec::new();
            let mut nodes = 0;
            loop {
                let page = cursor.visit_next(budget, |k, v| { output.push((k, v)); Ok(()) }).unwrap();
                assert!(page.nodes <= budget && page.rows <= page.nodes);
                nodes += page.nodes;
                if page.done { break; }
                assert!(page.nodes > 0);
            }
            assert_eq!(output, rows);
            assert_eq!(nodes, 2 * rows.len() - 1, "earlier pages must not be rescanned");
            let page = cursor.visit_next(1, |_, _| panic!("exhausted cursor")).unwrap();
            assert!(page.done && page.nodes == 0 && page.rows == 0);
        }
        let mut cursor = index.cursor().unwrap();
        let mut output = Vec::new();
        cursor.visit_next(3, |k, v| { output.push((k, v)); Ok(()) }).unwrap();
        index.insert(key(3), key(999)).unwrap();
        index.insert(key(9999), key(9999)).unwrap();
        while !cursor.visit_next(5, |k, v| { output.push((k, v)); Ok(()) }).unwrap().done {}
        assert_eq!(output, rows, "later writes cannot replace the cursor root");
        assert_eq!(index.get(&key(3)).unwrap(), Some(key(999)));
    }

    #[test]
    fn failed_pages_cannot_resume_and_later_corruption_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let index = ScratchIndex::from_sorted(dir.path(), [(key(1), key(2))]).unwrap();
        let mut cursor = index.cursor().unwrap();
        assert!(cursor.visit_next(1, |_, _| Err(invalid("callback failed"))).is_err());
        assert!(cursor.visit_next(1, |_, _| Ok(())).is_err());
        let mut cursor = index.cursor().unwrap();
        assert!(cursor.visit_next(0, |_, _| Ok(())).is_err());
        assert!(cursor.visit_next(1, |_, _| Ok(())).is_err());
        let mut cursor = index.cursor().unwrap();
        {
            let mut storage = index.storage.lock().unwrap();
            storage.file.seek(SeekFrom::Start(index.root.unwrap().offset)).unwrap();
            storage.file.write_all(&[255]).unwrap();
        }
        assert!(cursor.visit_next(1, |_, _| Ok(())).is_err());
        assert!(cursor.visit_next(1, |_, _| Ok(())).is_err());
        let empty = ScratchIndex::new(dir.path()).unwrap();
        assert!(empty.cursor().unwrap().visit_next(1, |_, _| panic!("empty")).unwrap().done);
        let mut wrong_count = empty;
        wrong_count.count = 1;
        assert!(wrong_count.cursor().unwrap().visit_next(1, |_, _| Ok(())).is_err());
    }
}

#[cfg(test)]
mod suffix_tests {
    use super::*;

    #[test]
    fn exclusive_seek_matches_ordered_ranges_across_compressed_prefixes_and_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let mut keys: Vec<Key> = (0..64u64).map(|n| burst_crypto::blake2b_256(&n.to_be_bytes())).collect();
        keys.extend([[0;32], [255;32]]);
        keys.sort_unstable();
        keys.dedup();
        let index = ScratchIndex::from_sorted(dir.path(), keys.iter().map(|key| (*key, *key))).unwrap();
        let mut boundaries = keys.clone();
        boundaries.extend((64..128u64).map(|n| burst_crypto::blake2b_256(&n.to_be_bytes())));
        for after in boundaries {
            let expected: Vec<_> = keys.iter().copied().filter(|key| *key > after).collect();
            let mut cursor = index.cursor_after(after).unwrap();
            let mut actual = Vec::new();
            let mut nodes = 0;
            loop {
                let page = cursor.visit_next(3, |key, value| { assert_eq!(key, value); actual.push(key); Ok(()) }).unwrap();
                assert!(page.nodes <= 3);
                nodes += page.nodes;
                if page.done { break; }
            }
            assert_eq!(actual, expected);
            assert!(nodes <= 2 * expected.len() + 256, "seek must not visit the discarded prefix");
            let mut paired = index.paired_cursor_after(&index, after).unwrap();
            let mut both = Vec::new();
            while !paired.visit_next::<StoreError>(5, |key, a, b| {
                assert_eq!(a, b); both.push(key); Ok(())
            }).unwrap().done {}
            assert_eq!(both, expected);
        }
        let mut last = index.cursor_after(keys[keys.len()-2]).unwrap();
        let page = last.visit_next(1, |key, _| { assert_eq!(key, *keys.last().unwrap()); Ok(()) }).unwrap();
        assert!(page.done && page.nodes == 1 && page.rows == 1);
        let empty = ScratchIndex::new(dir.path()).unwrap();
        assert!(empty.cursor_after([0;32]).unwrap().visit_next(1, |_, _| panic!("empty")).unwrap().done);
    }
}
