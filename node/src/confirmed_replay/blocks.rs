use burst_ledger::StateBlock;
use burst_store::StoreError;
use burst_types::BlockHash;
use std::{borrow::Cow, collections::HashMap};
pub(super) mod source;
pub(super) mod staged;

pub(super) enum AppliedHistory<'a> {
    Memory(HashMap<BlockHash, Cow<'a, StateBlock>>),
    Pinned {
        source: &'a source::Source<'a>,
        certified: &'a std::collections::HashSet<BlockHash>,
        appended: Box<AppliedHistory<'a>>,
    },
    Disk(Box<staged::StagedBlocks>),
}

impl<'a> AppliedHistory<'a> {
    pub(super) fn new(directory: Option<&std::path::Path>) -> Result<Self, StoreError> {
        match directory {
            Some(directory) => staged::StagedBlocks::new(directory)
                .map(Box::new)
                .map(Self::Disk),
            None => Ok(Self::Memory(HashMap::new())),
        }
    }

    /// Reference caller-certified source blocks and store new replay blocks
    /// separately. This checks source coverage, not certificate authority.
    pub(super) fn from_certified_input(
        source: &'a source::Source<'a>,
        certified: &'a std::collections::HashSet<BlockHash>,
        directory: Option<&std::path::Path>,
    ) -> Result<Self, StoreError> {
        let mut found = 0usize;
        source.visit_unordered(|block| {
            found += usize::from(certified.contains(&block.hash));
            Ok::<_, StoreError>(())
        })?;
        if found != certified.len() {
            return Err(StoreError::Corruption(
                "certified replay input is incomplete".into(),
            ));
        }
        Ok(Self::Pinned {
            source,
            certified,
            appended: Box::new(Self::new(directory)?),
        })
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        match self {
            Self::Memory(blocks) => blocks.len(),
            Self::Pinned {
                certified,
                appended,
                ..
            } => certified.len() + appended.len(),
            Self::Disk(blocks) => blocks.len(),
        }
    }

    #[cfg(test)]
    pub(super) fn insert(&mut self, block: &'a StateBlock) -> Result<(), StoreError> {
        self.insert_cow(Cow::Borrowed(block))
    }

    pub(super) fn insert_cow(&mut self, block: Cow<'a, StateBlock>) -> Result<(), StoreError> {
        match self {
            Self::Memory(blocks) => {
                if blocks.contains_key(&block.hash) {
                    return Err(StoreError::Corruption(
                        "duplicate applied replay block".into(),
                    ));
                }
                blocks.insert(block.hash, block);
                Ok(())
            }
            Self::Pinned {
                certified,
                appended,
                ..
            } => {
                if certified.contains(&block.hash) {
                    return Err(StoreError::Corruption(
                        "duplicate applied replay block".into(),
                    ));
                }
                appended.insert_cow(block)
            }
            Self::Disk(blocks) => blocks.insert(&block),
        }
    }
}

impl AppliedBlocks for AppliedHistory<'_> {
    fn get(&self, hash: &BlockHash) -> Result<Option<Cow<'_, StateBlock>>, StoreError> {
        match self {
            Self::Memory(blocks) => AppliedBlocks::get(blocks, hash),
            Self::Pinned {
                source,
                certified,
                appended,
            } => {
                if certified.contains(hash) {
                    source.get(hash).map(Some)
                } else {
                    appended.get(hash)
                }
            }
            Self::Disk(blocks) => blocks.get(hash),
        }
    }

    fn contains(&self, hash: &BlockHash) -> Result<bool, StoreError> {
        match self {
            Self::Memory(blocks) => blocks.contains(hash),
            Self::Pinned {
                source,
                certified,
                appended,
            } => {
                if certified.contains(hash) {
                    source.get(hash).map(|_| true)
                } else {
                    appended.contains(hash)
                }
            }
            Self::Disk(blocks) => blocks.contains(hash),
        }
    }

    fn encoded(&self, hash: &BlockHash) -> Result<Vec<u8>, StoreError> {
        match self {
            Self::Memory(blocks) => blocks.encoded(hash),
            Self::Pinned {
                source,
                certified,
                appended,
            } => {
                if certified.contains(hash) {
                    bincode::serialize(source.get(hash)?.as_ref())
                        .map_err(|error| StoreError::Serialization(error.to_string()))
                } else {
                    appended.encoded(hash)
                }
            }
            Self::Disk(blocks) => blocks.encoded(hash),
        }
    }
}

/// Access to blocks the caller has authenticated and applied to this replay
/// candidate. Implementations do not establish that authority themselves.
/// Storage failure is distinct from absence and must abort replay.
pub(crate) trait AppliedBlocks {
    fn get(&self, hash: &BlockHash) -> Result<Option<Cow<'_, StateBlock>>, StoreError>;

    fn contains(&self, hash: &BlockHash) -> Result<bool, StoreError> {
        self.get(hash).map(|block| block.is_some())
    }

    fn encoded(&self, hash: &BlockHash) -> Result<Vec<u8>, StoreError> {
        let block = self
            .get(hash)?
            .ok_or_else(|| StoreError::NotFound(format!("replayed block {hash}")))?;
        bincode::serialize(block.as_ref())
            .map_err(|error| StoreError::Serialization(error.to_string()))
    }
}

impl<B: std::borrow::Borrow<StateBlock>> AppliedBlocks for HashMap<BlockHash, B> {
    fn get(&self, hash: &BlockHash) -> Result<Option<Cow<'_, StateBlock>>, StoreError> {
        Ok(HashMap::get(self, hash).map(|block| Cow::Borrowed(block.borrow())))
    }

    fn contains(&self, hash: &BlockHash) -> Result<bool, StoreError> {
        Ok(self.contains_key(hash))
    }
}
