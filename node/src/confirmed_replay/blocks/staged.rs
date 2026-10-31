use super::*;
use crate::confirmed_replay::scratch_file::ScratchFile as File;
use bincode::Options;
use std::{
    io::{Read, Seek, SeekFrom, Write},
    sync::Mutex,
};

#[derive(serde::Serialize, serde::Deserialize)]
struct Entry {
    offset: u64,
    length: usize,
    digest: [u8; 32],
}

/// Private scratch, never a ledger or proof of finality. The caller distinguishes
/// authenticated input from applied history. Per-block bytes are bounded; the
/// lookup and insertion order are stored in authenticated scratch records.
/// `contains` checks the index only; `encoded` verifies stored bytes; `get` also
/// decodes the block and checks its hash. None verifies its signature.
pub(in crate::confirmed_replay) struct StagedBlocks {
    directory: std::path::PathBuf,
    file: Mutex<File>,
    index: crate::confirmed_replay::scratch_records::ScratchRecords<Entry>,
    order: crate::confirmed_replay::scratch_records::ScratchRecords<BlockHash>,
    count: usize,
    length: u64,
    failed: bool,
}

fn invalid(error: impl ToString) -> StoreError {
    StoreError::Corruption(error.to_string())
}

impl StagedBlocks {
    pub(in crate::confirmed_replay) fn new(
        directory: &std::path::Path,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            directory: directory.to_owned(),
            file: Mutex::new(File::new_in(directory).map_err(invalid)?),
            index: crate::confirmed_replay::scratch_records::ScratchRecords::new(directory)?,
            order: crate::confirmed_replay::scratch_records::ScratchRecords::new(directory)?,
            count: 0,
            length: 0,
            failed: false,
        })
    }

    #[cfg(test)]
    pub(in crate::confirmed_replay) fn from_blocks(
        directory: &std::path::Path,
        blocks: impl IntoIterator<Item = StateBlock>,
        max_blocks: usize,
        max_bytes: u64,
    ) -> Result<Self, StoreError> {
        let mut result = Self::new(directory)?;
        let mut entries = Vec::new();
        let mut order = Vec::new();
        for block in blocks {
            if entries.len() >= max_blocks || block.hash != block.compute_hash() {
                return Err(invalid(
                    "bulk staged blocks exceed budget or have a changed hash",
                ));
            }
            if bincode::serialized_size(&block).map_err(invalid)?
                > burst_types::MAX_P2P_MESSAGE_BYTES as u64
            {
                return Err(invalid("applied replay block exceeds wire size"));
            }
            let bytes = bincode::serialize(&block).map_err(invalid)?;
            let end = result
                .length
                .checked_add(bytes.len() as u64)
                .filter(|end| *end <= max_bytes)
                .ok_or_else(|| invalid("bulk staged blocks exceed byte budget"))?;
            result.ready()?;
            result
                .file
                .get_mut()
                .map_err(invalid)?
                .write_all(&bytes)
                .map_err(invalid)?;
            entries.push((
                *block.hash.as_bytes(),
                Entry {
                    offset: result.length,
                    length: bytes.len(),
                    digest: burst_crypto::blake2b_256(&bytes),
                },
            ));
            order.push((Self::position_key(result.count)?, block.hash));
            result.length = end;
            result.count += 1;
        }
        let descriptor_bytes = u64::try_from(max_blocks)
            .map_err(invalid)?
            .checked_mul(48)
            .ok_or_else(|| invalid("bulk staged descriptor budget overflow"))?;
        result.index = crate::confirmed_replay::scratch_records::ScratchRecords::from_rows(
            directory,
            entries,
            max_blocks,
            descriptor_bytes,
        )?;
        result.order = crate::confirmed_replay::scratch_records::ScratchRecords::from_rows(
            directory,
            order,
            max_blocks,
            descriptor_bytes,
        )?;
        Ok(result)
    }

    pub(in crate::confirmed_replay) fn len(&self) -> usize {
        self.count
    }

    pub(in crate::confirmed_replay) fn directory(&self) -> &std::path::Path {
        &self.directory
    }

    pub(in crate::confirmed_replay) fn hashes(
        &self,
    ) -> impl Iterator<Item = Result<BlockHash, StoreError>> + '_ {
        (0..self.count).map(|position| {
            self.ready()?;
            self.order
                .get(&Self::position_key(position)?)?
                .ok_or_else(|| invalid("missing staged block position"))
        })
    }

    fn position_key(position: usize) -> Result<[u8; 32], StoreError> {
        let mut key = [0; 32];
        key[..8].copy_from_slice(&u64::try_from(position).map_err(invalid)?.to_be_bytes());
        Ok(key)
    }

    fn ready(&self) -> Result<(), StoreError> {
        if self.failed
            || self
                .file
                .lock()
                .map_err(invalid)?
                .metadata()
                .map_err(invalid)?
                .len()
                != self.length
        {
            return Err(invalid("staged replay block write previously failed"));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(in crate::confirmed_replay) fn truncate_for_test(&self) {
        self.file.lock().unwrap().set_len(0).unwrap();
    }

    pub(in crate::confirmed_replay) fn insert(
        &mut self,
        block: &StateBlock,
    ) -> Result<(), StoreError> {
        self.ready()?;
        if self.index.get(block.hash.as_bytes())?.is_some() || block.hash != block.compute_hash() {
            return Err(invalid("duplicate or changed applied replay block"));
        }
        if bincode::serialized_size(block).map_err(invalid)?
            > burst_types::MAX_P2P_MESSAGE_BYTES as u64
        {
            return Err(invalid("applied replay block exceeds wire size"));
        }
        let bytes = bincode::serialize(block).map_err(invalid)?;
        self.insert_encoded(block.hash, &bytes)
    }

    /// Pin exact local bytes, including fields not covered by the block hash.
    /// The bytes need not decode as a block or match `hash`. Callers authenticate
    /// them before use. A failure after writing begins disables further access.
    pub(in crate::confirmed_replay) fn insert_encoded(
        &mut self,
        hash: BlockHash,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        self.ready()?;
        if self.index.get(hash.as_bytes())?.is_some() {
            return Err(invalid("duplicate staged block"));
        }
        let length = bytes.len() as u64;
        if length == 0 || length > burst_types::MAX_P2P_MESSAGE_BYTES as u64 {
            return Err(invalid("applied replay block exceeds wire size"));
        }
        let end = self
            .length
            .checked_add(length)
            .ok_or_else(|| invalid("staged replay length overflow"))?;
        let count = self
            .count
            .checked_add(1)
            .ok_or_else(|| invalid("staged block count overflow"))?;
        let position = Self::position_key(self.count)?;
        self.failed = true;
        let file = self.file.get_mut().map_err(invalid)?;
        if file.metadata().map_err(invalid)?.len() != self.length {
            return Err(invalid("staged replay file length changed"));
        }
        file.seek(SeekFrom::Start(self.length)).map_err(invalid)?;
        file.write_all(bytes).map_err(invalid)?;
        self.index.put(
            *hash.as_bytes(),
            &Entry {
                offset: self.length,
                length: bytes.len(),
                digest: burst_crypto::blake2b_256(bytes),
            },
        )?;
        if self.order.put(position, &hash)? {
            return Err(invalid("duplicate staged block position"));
        }
        self.count = count;
        self.length = end;
        self.failed = false;
        Ok(())
    }

    fn read(&self, hash: &BlockHash) -> Result<Option<Vec<u8>>, StoreError> {
        self.ready()?;
        let Some(entry) = self.index.get(hash.as_bytes())? else {
            return Ok(None);
        };
        self.read_entry(entry).map(Some)
    }

    fn read_entry(&self, entry: Entry) -> Result<Vec<u8>, StoreError> {
        self.ready()?;
        if entry.length == 0
            || entry.length > burst_types::MAX_P2P_MESSAGE_BYTES
            || entry
                .offset
                .checked_add(entry.length as u64)
                .is_none_or(|end| end > self.length)
        {
            return Err(invalid("invalid staged block location"));
        }
        let mut file = self.file.lock().map_err(invalid)?;
        if file.metadata().map_err(invalid)?.len() != self.length {
            return Err(invalid("staged replay file length changed"));
        }
        file.seek(SeekFrom::Start(entry.offset)).map_err(invalid)?;
        let mut bytes = vec![0; entry.length];
        file.read_exact(&mut bytes).map_err(invalid)?;
        if burst_crypto::blake2b_256(&bytes) != entry.digest {
            return Err(invalid("staged replay block changed"));
        }
        Ok(bytes)
    }

    #[cfg(test)]
    pub(in crate::confirmed_replay) fn visit(
        &self,
        visit: impl FnMut(StateBlock) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.visit_with(visit)
    }

    pub(in crate::confirmed_replay) fn visit_with<E: From<StoreError>>(
        &self,
        mut visit: impl FnMut(StateBlock) -> Result<(), E>,
    ) -> Result<(), E> {
        self.ready()?;
        let mut count = 0usize;
        self.index.visit_with(|hash, entry| {
            let block = Self::decode(&BlockHash::new(hash), &self.read_entry(entry)?)?;
            count += 1;
            if count > self.count {
                return Err(invalid("extra staged block index row").into());
            }
            visit(block)
        })?;
        if count != self.count {
            return Err(invalid("missing staged block index row").into());
        }
        Ok(())
    }

    fn decode(hash: &BlockHash, bytes: &[u8]) -> Result<StateBlock, StoreError> {
        let block: StateBlock = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .with_limit(bytes.len() as u64)
            .deserialize(bytes)
            .map_err(invalid)?;
        if block.hash != *hash || block.compute_hash() != *hash {
            return Err(invalid("staged replay block identity changed"));
        }
        Ok(block)
    }
}

impl AppliedBlocks for StagedBlocks {
    fn get(&self, hash: &BlockHash) -> Result<Option<Cow<'_, StateBlock>>, StoreError> {
        let Some(bytes) = self.read(hash)? else {
            return Ok(None);
        };
        let block = Self::decode(hash, &bytes)?;
        Ok(Some(Cow::Owned(block)))
    }

    fn contains(&self, hash: &BlockHash) -> Result<bool, StoreError> {
        self.ready()?;
        Ok(self.index.get(hash.as_bytes())?.is_some())
    }

    fn encoded(&self, hash: &BlockHash) -> Result<Vec<u8>, StoreError> {
        self.read(hash)?
            .ok_or_else(|| StoreError::NotFound(format!("replayed block {hash}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_encoded_rows_preserve_exact_bytes_without_granting_authority() {
        let directory = tempfile::tempdir().unwrap();
        let hash = BlockHash::new([3; 32]);
        let bytes = b"not an authenticated block";
        let mut blocks = StagedBlocks::new(directory.path()).unwrap();
        blocks.insert_encoded(hash, bytes).unwrap();
        assert_eq!(blocks.encoded(&hash).unwrap(), bytes);
        assert!(blocks.get(&hash).is_err());
        assert!(blocks.insert_encoded(hash, bytes).is_err());
        assert!(blocks.insert_encoded(BlockHash::new([4; 32]), &[]).is_err());
        blocks.truncate_for_test();
        assert!(blocks.encoded(&hash).is_err());
        assert!(blocks.hashes().next().unwrap().is_err());
        assert!(blocks.contains(&BlockHash::ZERO).is_err());
    }

    #[test]
    fn input_scans_propagate_staged_read_failures() {
        let directory = tempfile::tempdir().unwrap();
        let genesis = crate::genesis_state::block(
            burst_types::NetworkId::Dev,
            &burst_types::ProtocolParams::default(),
        );
        let mut blocks = StagedBlocks::new(directory.path()).unwrap();
        blocks.insert(&genesis).unwrap();
        blocks.file.get_mut().unwrap().set_len(0).unwrap();
        let source = super::super::source::Source::File {
            blocks: Box::new(blocks),
        };
        assert!(source.visit_unordered(|_| Ok::<_, StoreError>(())).is_err());
        assert!(source.get(&genesis.hash).is_err());
        assert!(source.iter().next().unwrap().is_err());
        assert!(crate::confirmed_replay::governance::streamed_dependencies(&source).is_err());
        assert!(
            crate::confirmed_replay::selections::Selections::for_certified_epoch(
                &[] as &[&crate::checkpoint_store::CertifiedCheckpoint],
                &source
            )
            .is_err()
        );
    }

    #[test]
    fn staged_order_is_repeatable_and_rejects_index_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let mut blocks = StagedBlocks::new(directory.path()).unwrap();
        let expected = [9, 3, 7].map(|byte| BlockHash::new([byte; 32]));
        for hash in expected {
            blocks.insert_encoded(hash, hash.as_bytes()).unwrap();
        }
        for _ in 0..2 {
            assert_eq!(
                blocks.hashes().collect::<Result<Vec<_>, _>>().unwrap(),
                expected
            );
            for hash in expected {
                assert_eq!(blocks.encoded(&hash).unwrap(), hash.as_bytes());
            }
        }
        blocks.order.truncate_for_test();
        assert!(blocks.hashes().next().unwrap().is_err());
        blocks.index.truncate_for_test();
        assert!(blocks.contains(&expected[0]).is_err());
        assert!(blocks.contains(&BlockHash::ZERO).is_err());
        assert!(blocks.encoded(&expected[0]).is_err());
    }

    #[test]
    fn staged_blocks_bind_bytes_and_poison_failed_appends() {
        let directory = tempfile::tempdir().unwrap();
        let genesis = crate::genesis_state::block(
            burst_types::NetworkId::Dev,
            &burst_types::ProtocolParams::default(),
        );
        let mut blocks = StagedBlocks::new(directory.path()).unwrap();
        assert!(!blocks.contains(&genesis.hash).unwrap());
        blocks.insert(&genesis).unwrap();
        let expected = bincode::serialize(&genesis).unwrap();
        for _ in 0..2 {
            assert_eq!(blocks.encoded(&genesis.hash).unwrap(), expected);
        }
        assert!(matches!(
            blocks.get(&genesis.hash).unwrap(),
            Some(Cow::Owned(_))
        ));
        assert!(blocks.insert(&genesis).is_err());
        {
            let file = blocks.file.get_mut().unwrap();
            file.rewind().unwrap();
            file.write_all(&[expected[0] ^ 1]).unwrap();
        }
        assert!(blocks.encoded(&genesis.hash).is_err());
        {
            let file = blocks.file.get_mut().unwrap();
            file.rewind().unwrap();
            file.write_all(&expected).unwrap();
        }
        assert_eq!(blocks.encoded(&genesis.hash).unwrap(), expected);
        blocks.file.get_mut().unwrap().set_len(0).unwrap();
        assert!(blocks.get(&genesis.hash).is_err());
        let mut next = genesis.clone();
        next.timestamp = burst_types::Timestamp::new(next.timestamp.as_secs() + 1);
        next.hash = next.compute_hash();
        assert!(blocks.insert(&next).is_err());
        assert!(blocks.contains(&genesis.hash).is_err());
        assert!(blocks.get(&BlockHash::ZERO).is_err());
        assert_eq!(blocks.len(), 1);
        drop(blocks);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        assert!(StagedBlocks::new(&directory.path().join("missing")).is_err());
    }
}

#[cfg(test)]
mod bulk_tests {
    use super::*;

    #[test]
    fn bulk_blocks_match_incremental_order_bytes_and_reject_limits_duplicates_and_damage() {
        let directory = tempfile::tempdir().unwrap();
        let blocks: Vec<_> = (0u32..128)
            .rev()
            .map(|i| {
                let mut block = crate::genesis_state::block(
                    burst_types::NetworkId::Dev,
                    &burst_types::ProtocolParams::default(),
                );
                block.previous = BlockHash::new(burst_crypto::blake2b_256(&i.to_be_bytes()));
                block.hash = block.compute_hash();
                block
            })
            .collect();
        let mut incremental = StagedBlocks::new(directory.path()).unwrap();
        for block in &blocks {
            incremental.insert(block).unwrap();
        }
        let bulk =
            StagedBlocks::from_blocks(directory.path(), blocks.clone(), 128, 1 << 20).unwrap();
        assert_eq!(
            bulk.hashes().collect::<Result<Vec<_>, _>>().unwrap(),
            incremental.hashes().collect::<Result<Vec<_>, _>>().unwrap()
        );
        for block in &blocks {
            assert_eq!(
                bulk.encoded(&block.hash).unwrap(),
                incremental.encoded(&block.hash).unwrap()
            );
            assert_eq!(bulk.get(&block.hash).unwrap().unwrap().hash, block.hash);
        }
        let mut scanned = std::collections::BTreeMap::new();
        bulk.visit(|block| {
            assert!(scanned
                .insert(block.hash, bincode::serialize(&block).unwrap())
                .is_none());
            Ok(())
        })
        .unwrap();
        assert_eq!(
            scanned,
            blocks
                .iter()
                .map(|block| (block.hash, bincode::serialize(block).unwrap()))
                .collect()
        );
        assert!(bulk.visit(|_| Err(invalid("stop"))).is_err());
        assert!(!bulk.contains(&BlockHash::ZERO).unwrap());
        assert!(StagedBlocks::from_blocks(directory.path(), blocks.clone(), 127, 1 << 20).is_err());
        assert!(StagedBlocks::from_blocks(directory.path(), blocks.clone(), 128, 1).is_err());
        assert!(StagedBlocks::from_blocks(
            directory.path(),
            [blocks[0].clone(), blocks[0].clone()],
            2,
            1 << 20
        )
        .is_err());
        let mut bad = blocks[0].clone();
        bad.hash = BlockHash::ZERO;
        assert!(StagedBlocks::from_blocks(directory.path(), [bad], 1, 1 << 20).is_err());
        bulk.truncate_for_test();
        assert!(bulk.get(&blocks[0].hash).is_err());
        assert!(bulk.hashes().next().unwrap().is_err());
        assert!(bulk.visit(|_| Ok(())).is_err());
    }
}
