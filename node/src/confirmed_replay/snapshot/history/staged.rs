use super::*;
use crate::confirmed_replay::blocks::{staged::StagedBlocks, AppliedBlocks};

struct Position {
    hash: BlockHash,
    context: HistoryContext,
    height: u64,
}

/// Only opaque, authenticated witnesses can enter this collection. Restoring a
/// witness requires the exact body pinned at insertion, not just its signed hash.
pub(crate) struct HistoricalBlocks {
    bodies: StagedBlocks,
    positions: Vec<Position>,
}

impl HistoricalBlocks {
    pub(crate) fn stage(
        directory: &std::path::Path,
        proofs: impl IntoIterator<Item = HistoricalBlock>,
    ) -> Result<Self, StoreError> {
        let mut result = Self {
            bodies: StagedBlocks::new(directory)?,
            positions: Vec::new(),
        };
        let mut bytes = 0u64;
        for proof in proofs {
            bytes = bytes
                .checked_add(
                    bincode::serialized_size(&proof.block)
                        .map_err(|error| StoreError::Corruption(error.to_string()))?,
                )
                .filter(|bytes| *bytes <= super::super::tail::MAX_TAIL_BYTES as u64)
                .ok_or_else(|| {
                    StoreError::Corruption("historical witnesses exceed byte budget".into())
                })?;
            if result.positions.len() >= super::super::tail::MAX_TAIL_BLOCKS {
                return Err(StoreError::Corruption(
                    "historical witnesses exceed block budget".into(),
                ));
            }
            result.bodies.insert(&proof.block)?;
            result.positions.push(Position {
                hash: proof.block.hash,
                context: proof.context,
                height: proof.height,
            });
        }
        Ok(result)
    }

    pub(crate) fn len(&self) -> usize {
        self.positions.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    pub(crate) fn hashes(&self) -> impl Iterator<Item = BlockHash> + '_ {
        self.positions.iter().map(|position| position.hash)
    }

    pub(crate) fn iter(
        &self,
    ) -> impl ExactSizeIterator<Item = Result<HistoricalBlock, StoreError>> + '_ {
        self.positions.iter().map(|position| {
            let block = self
                .bodies
                .get(&position.hash)?
                .ok_or_else(|| {
                    StoreError::NotFound(format!("staged historical block {}", position.hash))
                })?
                .into_owned();
            Ok(HistoricalBlock {
                context: position.context,
                height: position.height,
                block,
            })
        })
    }

    pub(crate) fn encoded(&self, hash: &BlockHash) -> Result<Option<Vec<u8>>, StoreError> {
        if self.bodies.contains(hash)? {
            self.bodies.encoded(hash).map(Some)
        } else {
            Ok(None)
        }
    }

    pub(crate) fn row(&self, index: usize) -> Option<Result<(BlockHash, Vec<u8>), StoreError>> {
        self.positions.get(index).map(|position| {
            self.bodies
                .encoded(&position.hash)
                .map(|bytes| (position.hash, bytes))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_historical_proofs_preserve_authority_and_propagate_body_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let params = burst_types::ProtocolParams::default();
        let block = crate::genesis_state::block(NetworkId::Dev, &params);
        let anchor =
            crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(NetworkId::Dev, &params)
                .unwrap();
        let mut account = AccountInfo::unopened(block.account.clone());
        account.head = block.hash;
        account.block_count = 1;
        account.confirmation_height = 1;
        let mut cursor = HistoryCursor::new(
            &anchor,
            BlockHash::new([7; 32]),
            &account.address,
            Some(&account),
        )
        .unwrap()
        .unwrap();
        let bytes = bincode::serialize(&block).unwrap();
        let proof = cursor
            .accept(std::slice::from_ref(&bytes))
            .unwrap()
            .pop()
            .unwrap();
        let context = proof.context();
        assert!(HistoricalBlocks::stage(directory.path(), [proof.clone(), proof.clone()]).is_err());
        let stored = HistoricalBlocks::stage(directory.path(), [proof]).unwrap();
        assert_eq!(stored.hashes().collect::<Vec<_>>(), vec![block.hash]);
        let restored = stored.iter().next().unwrap().unwrap();
        assert_eq!(restored.context(), context);
        assert_eq!(restored.height(), 1);
        assert_eq!(bincode::serialize(restored.block()).unwrap(), bytes);
        assert_eq!(stored.row(0).unwrap().unwrap(), (block.hash, bytes.clone()));
        assert!(stored.row(1).is_none());
        assert_eq!(stored.encoded(&block.hash).unwrap(), Some(bytes));
        assert!(stored.encoded(&BlockHash::ZERO).unwrap().is_none());
        stored.bodies.truncate_for_test();
        assert!(stored.iter().next().unwrap().is_err());
        assert!(stored.row(0).unwrap().is_err());
        assert!(stored.encoded(&block.hash).is_err());
        let source = crate::confirmed_replay::blocks::source::Source::stage(
            stored
                .iter()
                .map(|proof| proof.map(|proof| std::borrow::Cow::Owned(proof.into_block()))),
            directory.path(),
        );
        assert!(source.is_err());
    }
}
