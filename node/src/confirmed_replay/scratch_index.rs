use bincode::Options;
use burst_store::StoreError;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{Arc, Mutex},
};

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

#[derive(Clone, Copy, Serialize, Deserialize)]
struct Link {
    offset: u64,
    length: u64,
    digest: Key,
}

#[derive(Serialize, Deserialize)]
enum Node {
    Leaf { key: Key, value: Key },
    Branch { bit: u16, left: Link, right: Link },
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
pub(super) struct ScratchIndex {
    storage: Arc<Mutex<Storage>>,
    root: Option<Link>,
    count: usize,
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
    fn ready(&self) -> Result<(), StoreError> {
        if self.failed || self.file.metadata().map_err(invalid)?.len() != self.length {
            return Err(invalid("replay scratch index changed or a write failed"));
        }
        Ok(())
    }

    fn read(&mut self, link: Link) -> Result<Node, StoreError> {
        if link.length == 0
            || link.length > MAX_NODE_BYTES
            || link
                .offset
                .checked_add(link.length)
                .is_none_or(|end| end > self.length)
        {
            return Err(invalid("invalid replay scratch index link"));
        }
        self.file
            .seek(SeekFrom::Start(link.offset))
            .map_err(invalid)?;
        let mut bytes = vec![0; link.length as usize];
        self.file.read_exact(&mut bytes).map_err(invalid)?;
        if burst_crypto::blake2b_256(&bytes) != link.digest {
            return Err(invalid("replay scratch index digest changed"));
        }
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .with_limit(MAX_NODE_BYTES)
            .deserialize(&bytes)
            .map_err(invalid)
    }

    fn append(&mut self, node: &Node) -> Result<Link, StoreError> {
        self.ready()?;
        let bytes = bincode::serialize(node).map_err(invalid)?;
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
            digest: burst_crypto::blake2b_256(&bytes),
        };
        self.failed = true;
        self.file
            .seek(SeekFrom::Start(self.length))
            .map_err(invalid)?;
        self.file.write_all(&bytes).map_err(invalid)?;
        self.length = end;
        self.failed = false;
        Ok(link)
    }
}

impl ScratchIndex {
    pub(super) fn new(directory: &Path) -> Result<Self, StoreError> {
        Ok(Self {
            storage: Arc::new(Mutex::new(Storage {
                file: tempfile::tempfile_in(directory).map_err(invalid)?,
                length: 0,
                failed: false,
            })),
            root: None,
            count: 0,
        })
    }

    fn path(
        storage: &mut Storage,
        root: Link,
        key: &Key,
    ) -> Result<(Link, Key, Key, Vec<Branch>), StoreError> {
        let mut path = Vec::new();
        let mut current = root;
        let mut previous = None;
        loop {
            match storage.read(current)? {
                Node::Leaf { key, value } => return Ok((current, key, value, path)),
                Node::Branch { bit, left, right } => {
                    if bit >= 256 || previous.is_some_and(|old| bit <= old) {
                        return Err(invalid("unordered replay scratch index branch"));
                    }
                    let rightward = rightward(key, bit);
                    path.push(Branch {
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

    pub(super) fn get(&self, key: &Key) -> Result<Option<Key>, StoreError> {
        let mut storage = self.storage.lock().map_err(invalid)?;
        storage.ready()?;
        let Some(root) = self.root else {
            return Ok(None);
        };
        let (_, found, value, _) = Self::path(&mut storage, root, key)?;
        Ok((found == *key).then_some(value))
    }

    pub(super) fn visit(
        &self,
        mut visit: impl FnMut(Key, Key) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.storage.lock().map_err(invalid)?.ready()?;
        let mut pending = Vec::new();
        if let Some(root) = self.root {
            pending.push((root, None));
        }
        let mut previous_key = None;
        let mut count = 0usize;
        while let Some((link, parent_bit)) = pending.pop() {
            let node = {
                let mut storage = self.storage.lock().map_err(invalid)?;
                storage.ready()?;
                storage.read(link)?
            };
            match node {
                Node::Branch { bit, left, right } => {
                    if bit >= 256 || parent_bit.is_some_and(|parent| bit <= parent) {
                        return Err(invalid("unordered replay scratch index branch"));
                    }
                    pending.push((right, Some(bit)));
                    pending.push((left, Some(bit)));
                }
                Node::Leaf { key, value } => {
                    if previous_key.is_some_and(|previous| key <= previous) || count >= self.count {
                        return Err(invalid("invalid replay scratch index traversal"));
                    }
                    previous_key = Some(key);
                    count += 1;
                    visit(key, value)?;
                }
            }
        }
        if count != self.count {
            return Err(invalid("incomplete replay scratch index traversal"));
        }
        Ok(())
    }

    pub(super) fn insert(&mut self, key: Key, value: Key) -> Result<Option<Key>, StoreError> {
        let mut storage = self.storage.lock().map_err(invalid)?;
        storage.ready()?;
        let Some(root) = self.root else {
            self.root = Some(storage.append(&Node::Leaf { key, value })?);
            self.count = 1;
            return Ok(None);
        };
        let (leaf, found, old, path) = Self::path(&mut storage, root, &key)?;
        let replaced = found == key;
        let count = if replaced {
            self.count
        } else {
            self.count
                .checked_add(1)
                .ok_or_else(|| invalid("replay scratch index count overflow"))?
        };
        let mut next = storage.append(&Node::Leaf { key, value })?;
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
            next = storage.append(&Node::Branch { bit, left, right })?;
            prefix
        };
        for branch in path[..prefix].iter().rev() {
            let (left, right) = if branch.rightward {
                (branch.left, next)
            } else {
                (next, branch.right)
            };
            next = storage.append(&Node::Branch {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

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
