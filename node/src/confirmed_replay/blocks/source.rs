use super::*;

/// Replay input bodies, separate from applied history. The caller authenticates
/// the inputs; constructing or scanning a source does not authorize economic use.
pub(in crate::confirmed_replay) enum Source<'a> {
    Memory {
        blocks: &'a [StateBlock],
        index: HashMap<BlockHash, usize>,
    },
    Tail {
        archive: crate::confirmed_replay::snapshot::tail::TailArchive,
        directory: std::path::PathBuf,
    },
    File {
        blocks: Box<staged::StagedBlocks>,
    },
}

impl<'a> Source<'a> {
    pub(in crate::confirmed_replay) fn directory(&self) -> Option<&std::path::Path> {
        match self {
            Self::Memory { .. } => None,
            Self::Tail { directory, .. } => Some(directory),
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

    pub(in crate::confirmed_replay) fn from_tail(
        tail: &crate::confirmed_replay::snapshot::tail::SnapshotTail,
    ) -> Source<'static> {
        Source::Tail {
            archive: tail.archive(),
            directory: tail.storage_directory().to_owned(),
        }
    }

    pub(in crate::confirmed_replay) fn hashes(
        &self,
    ) -> Box<dyn Iterator<Item = Result<BlockHash, StoreError>> + '_> {
        match self {
            Self::Memory { blocks, .. } => Box::new(blocks.iter().map(|block| Ok(block.hash))),
            Self::File { blocks } => Box::new(blocks.hashes()),
            Self::Tail { archive, .. } => Box::new(archive.input_hashes().map(Ok)),
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
            Self::Tail { archive, .. } => archive.input_block(hash)?,
        };
        block.ok_or_else(|| StoreError::NotFound(format!("replay input block {hash}")))
    }

    /// Visit every input in unspecified order. This does not verify signatures
    /// or certificates. Use `iter` when input order matters.
    /// Earlier callbacks may have run when a later read or callback fails.
    pub(in crate::confirmed_replay) fn visit_unordered<E: From<StoreError>>(
        &self,
        mut visit: impl FnMut(&StateBlock) -> Result<(), E>,
    ) -> Result<(), E> {
        match self {
            Self::Memory { blocks, .. } => blocks.iter().try_for_each(visit),
            Self::File { blocks } => blocks.visit_with(|block| visit(&block)),
            Self::Tail { archive, .. } => archive.visit_input(visit),
        }
    }

    pub(in crate::confirmed_replay) fn iter(
        &self,
    ) -> Box<dyn Iterator<Item = Result<Cow<'_, StateBlock>, StoreError>> + '_> {
        Box::new(self.hashes().map(|hash| self.get(&hash?)))
    }
}
