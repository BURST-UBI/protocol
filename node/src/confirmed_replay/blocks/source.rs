use super::*;

/// Authenticated input bodies are not yet applied. Repeated scans may fail and
/// must never be treated as permission to expose these blocks to economic lookup.
pub(in crate::confirmed_replay) enum Source<'a> {
    Memory {
        blocks: &'a [StateBlock],
        index: HashMap<BlockHash, usize>,
    },
    File {
        blocks: Box<staged::StagedBlocks>,
    },
}

impl<'a> Source<'a> {
    pub(in crate::confirmed_replay) fn directory(&self) -> Option<&std::path::Path> {
        match self {
            Self::Memory { .. } => None,
            Self::File { blocks, .. } => Some(blocks.directory()),
        }
    }

    pub(in crate::confirmed_replay) fn new(
        blocks: &'a [StateBlock],
        directory: Option<&std::path::Path>,
    ) -> Result<Self, StoreError> {
        if let Some(directory) = directory {
            Self::stage(
                blocks.iter().map(|block| Ok(Cow::Borrowed(block))),
                directory,
            )
        } else {
            let mut index = HashMap::with_capacity(blocks.len());
            for (position, block) in blocks.iter().enumerate() {
                if index.insert(block.hash, position).is_some() {
                    return Err(StoreError::Corruption(
                        "duplicate replay input block".into(),
                    ));
                }
            }
            Ok(Self::Memory { blocks, index })
        }
    }

    pub(in crate::confirmed_replay) fn stage<'b>(
        blocks: impl IntoIterator<Item = Result<Cow<'b, StateBlock>, StoreError>>,
        directory: &std::path::Path,
    ) -> Result<Source<'static>, StoreError> {
        let mut staged = staged::StagedBlocks::new(directory)?;
        for block in blocks {
            let block = block?;
            staged.insert(&block)?;
        }
        Ok(Source::File {
            blocks: Box::new(staged),
        })
    }

    pub(in crate::confirmed_replay) fn hashes(
        &self,
    ) -> Box<dyn Iterator<Item = Result<BlockHash, StoreError>> + '_> {
        match self {
            Self::Memory { blocks, .. } => Box::new(blocks.iter().map(|block| Ok(block.hash))),
            Self::File { blocks } => Box::new(blocks.hashes()),
        }
    }

    pub(in crate::confirmed_replay) fn get(
        &self,
        hash: &BlockHash,
    ) -> Result<Cow<'_, StateBlock>, StoreError> {
        let block = match self {
            Self::Memory { blocks, index } => index
                .get(hash)
                .map(|position| Cow::Borrowed(&blocks[*position])),
            Self::File { blocks, .. } => blocks.get(hash)?,
        };
        block.ok_or_else(|| StoreError::NotFound(format!("replay input block {hash}")))
    }

    pub(in crate::confirmed_replay) fn iter(
        &self,
    ) -> Box<dyn Iterator<Item = Result<Cow<'_, StateBlock>, StoreError>> + '_> {
        Box::new(self.hashes().map(|hash| self.get(&hash?)))
    }
}
