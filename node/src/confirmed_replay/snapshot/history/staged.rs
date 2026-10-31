use super::*;
use crate::confirmed_replay::scratch_records::{ScratchRecords, ScratchRecordsBuilder};

struct Position {
    hash: BlockHash,
    context: HistoryContext,
    height: u64,
}

/// Only opaque, authenticated witnesses can enter this collection. Restoring a
/// witness requires the exact body pinned at insertion, not just its signed hash.
/// Bodies live on disk; one position record per witness remains in memory.
pub(crate) struct HistoricalBlocks {
    bodies: ScratchRecords<burst_ledger::StateBlock>,
    positions: Vec<Position>,
}

impl HistoricalBlocks {
    pub(crate) fn stage(
        directory: &std::path::Path,
        proofs: impl IntoIterator<Item = HistoricalBlock>,
    ) -> Result<Self, StoreError> {
        let mut positions = Vec::new();
        let mut builder = ScratchRecordsBuilder::new(directory,
            super::super::tail::MAX_TAIL_BLOCKS,
            super::super::tail::MAX_TAIL_BYTES as u64)?;
        for proof in proofs {
            let block = proof.block;
            if block.hash != block.compute_hash()
                || bincode::serialized_size(&block).map_err(corrupt)? > burst_types::MAX_P2P_MESSAGE_BYTES as u64
            {
                return Err(corrupt("invalid staged historical block"));
            }
            builder.push(*block.hash.as_bytes(), &block)?;
            positions.push(Position { hash: block.hash, context: proof.context, height: proof.height });
        }
        let bodies = builder.finish()?;
        let result = Self { bodies, positions };
        Ok(result)
    }

    pub(crate) fn len(&self) -> usize {
        self.positions.len()
    }

    pub(crate) fn hashes(&self) -> impl Iterator<Item = BlockHash> + '_ {
        self.positions.iter().map(|position| position.hash)
    }

    pub(crate) fn iter(
        &self,
    ) -> impl ExactSizeIterator<Item = Result<HistoricalBlock, StoreError>> + '_ {
        self.positions.iter().map(|position| {
            let block = self
                .block(&position.hash)?
                .ok_or_else(|| {
                    StoreError::NotFound(format!("staged historical block {}", position.hash))
                })?;
            Ok(HistoricalBlock {
                context: position.context,
                height: position.height,
                block,
            })
        })
    }

    /// Validate and visit bodies in hash order. This sorts one reference per
    /// retained position in memory. Earlier callbacks may run before a later error.
    pub(crate) fn try_visit(
        &self,
        visit: impl FnMut(HistoricalBlock) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.try_visit_with(visit)
    }

    pub(in crate::confirmed_replay) fn try_visit_with<E: From<StoreError>>(
        &self,
        mut visit: impl FnMut(HistoricalBlock) -> Result<(), E>,
    ) -> Result<(), E> {
        let mut positions: Vec<_> = self.positions.iter().collect();
        positions.sort_unstable_by_key(|position| position.hash);
        let mut positions = positions.into_iter();
        self.bodies.visit_with(|hash, block| {
            Self::check_identity(&BlockHash::new(hash), &block)?;
            let position = positions
                .next()
                .filter(|position| position.hash == block.hash)
                .ok_or_else(|| {
                    StoreError::Corruption(
                        "historical witness inventory differs from bodies".into(),
                    )
                })?;
            visit(HistoricalBlock {
                context: position.context,
                height: position.height,
                block,
            })
        })?;
        if positions.next().is_some() {
            return Err(StoreError::Corruption("historical witness body missing".into()).into());
        }
        Ok(())
    }

    pub(in crate::confirmed_replay) fn input_block(
        &self,
        hash: &BlockHash,
    ) -> Result<Option<std::borrow::Cow<'_, burst_ledger::StateBlock>>, StoreError> {
        self.block(hash).map(|block| block.map(std::borrow::Cow::Owned))
    }

    fn check_identity(hash: &BlockHash, block: &burst_ledger::StateBlock) -> Result<(), StoreError> {
        if block.hash != *hash || block.compute_hash() != *hash {
            return Err(corrupt("staged historical block identity changed"));
        }
        Ok(())
    }

    fn block(&self, hash: &BlockHash) -> Result<Option<burst_ledger::StateBlock>, StoreError> {
        let block = self.bodies.get(hash.as_bytes())?;
        if let Some(block) = &block { Self::check_identity(hash, block)?; }
        Ok(block)
    }

    pub(crate) fn encoded(&self, hash: &BlockHash) -> Result<Option<Vec<u8>>, StoreError> {
        self.block(hash)?.map(|block| bincode::serialize(&block).map_err(corrupt)).transpose()
    }

    pub(crate) fn row(&self, index: usize) -> Option<Result<(BlockHash, Vec<u8>), StoreError>> {
        self.positions.get(index).map(|position| {
            self.encoded(&position.hash)?.map(|bytes| (position.hash, bytes))
                .ok_or_else(|| StoreError::NotFound(format!("staged historical block {}", position.hash)))
        })
    }

}

fn corrupt(error: impl ToString) -> StoreError { StoreError::Corruption(error.to_string()) }

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
        // Payload and index files must fit the shared scratch limit.
        let _limit = crate::confirmed_replay::scratch_file::ScratchLimit::acquire(
            directory.path(), bytes.len() as u64 + 256).unwrap();
        let stored = HistoricalBlocks::stage(directory.path(), [proof]).unwrap();
        assert_eq!(stored.hashes().collect::<Vec<_>>(), vec![block.hash]);
        let restored = stored.iter().next().unwrap().unwrap();
        assert_eq!(restored.context(), context);
        assert_eq!(restored.height(), 1);
        assert_eq!(bincode::serialize(restored.block()).unwrap(), bytes);
        assert_eq!(stored.row(0).unwrap().unwrap(), (block.hash, bytes.clone()));
        assert!(stored.row(1).is_none());
        let mut visited = 0;
        stored
            .try_visit(|proof| {
                assert_eq!(proof.context(), context);
                assert_eq!(proof.height(), 1);
                assert_eq!(bincode::serialize(proof.block()).unwrap(), bytes);
                visited += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(visited, 1);
        assert!(stored
            .try_visit(|_| Err(StoreError::Corruption("stop".into())))
            .is_err());
        assert_eq!(stored.encoded(&block.hash).unwrap(), Some(bytes));
        assert!(stored.encoded(&BlockHash::ZERO).unwrap().is_none());
        stored.bodies.truncate_for_test();
        assert!(stored.iter().next().unwrap().is_err());
        assert!(stored.try_visit(|_| Ok(())).is_err());
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
