use super::*;
use crate::confirmed_replay::blocks::{staged::StagedBlocks, AppliedBlocks};
use burst_store_lmdb::write_batch::checkpoint_projection::CheckpointBlock;
use std::{borrow::Cow, collections::BTreeMap};

/// Membership has already been authenticated. Keep positions in memory and pin
/// the bodies in ledger-local scratch until the installation writer consumes them.
pub(super) struct Rows {
    bodies: StagedBlocks,
    positions: BTreeMap<[u8; 32], (burst_types::WalletAddress, u64)>,
    bytes: usize,
}

impl Rows {
    pub(super) fn new(directory: &std::path::Path) -> Result<Self, StoreError> {
        Ok(Self {
            bodies: StagedBlocks::new(directory)?,
            positions: BTreeMap::new(),
            bytes: 0,
        })
    }

    pub(super) fn insert(&mut self, witness: &HistoricalBlock) -> Result<(), StoreError> {
        let block = witness.block();
        if let Some((account, height)) = self.positions.get(block.hash.as_bytes()) {
            if *account != block.account
                || *height != witness.height()
                || self.bodies.encoded(&block.hash)?
                    != bincode::serialize(block).map_err(invalid)?
            {
                return Err(invalid("compact projection witness changed"));
            }
            return Ok(());
        }
        let size =
            usize::try_from(bincode::serialized_size(block).map_err(invalid)?).map_err(invalid)?;
        let bytes = self
            .bytes
            .checked_add(size)
            .filter(|size| *size <= super::super::tail::MAX_TAIL_BYTES)
            .ok_or_else(|| invalid("compact projection witnesses exceed byte budget"))?;
        if self.positions.len() >= super::super::tail::MAX_TAIL_BLOCKS {
            return Err(invalid("compact projection witnesses exceed block budget"));
        }
        self.bodies.insert(block)?;
        self.positions.insert(
            *block.hash.as_bytes(),
            (block.account.clone(), witness.height()),
        );
        self.bytes = bytes;
        Ok(())
    }

    pub(super) fn iter(
        &self,
    ) -> impl Iterator<Item = Result<Cow<'static, CheckpointBlock>, StoreError>> + '_ {
        self.positions.iter().map(|(hash, (account, height))| {
            let hash = BlockHash::new(*hash);
            Ok(Cow::Owned(CheckpointBlock {
                hash,
                account: account.clone(),
                height: *height,
                bytes: self.bodies.encoded(&hash)?,
            }))
        })
    }
}
