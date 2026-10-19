use super::{history, invalid, HistoricalBlock, Roots};
use burst_store::StoreError;
use burst_types::BlockHash;
use std::collections::VecDeque;

#[derive(Default)]
pub(super) struct Cache {
    entries: VecDeque<(HistoricalBlock, usize, bool)>,
    bytes: usize,
}

impl Cache {
    // Bodies are only transport input here. The caller must authenticate them
    // with its current cursor, even when an older context authenticated them.
    pub(super) fn read_chunk(&self, start: BlockHash) -> Result<Option<Vec<Vec<u8>>>, StoreError> {
        let mut hash = start;
        let mut bytes = 0usize;
        let mut blocks = Vec::new();
        while !hash.is_zero() && blocks.len() < history::MAX_WITNESS_BLOCKS {
            let Some((proof, _, _)) = self.entries.iter().find(|(proof, _, _)| proof.block().hash == hash) else {
                break;
            };
            let block = bincode::serialize(proof.block()).map_err(invalid)?;
            if bytes + block.len() > history::MAX_WITNESS_BYTES {
                break;
            }
            bytes += block.len();
            hash = proof.block().previous;
            blocks.push(block);
        }
        Ok((!blocks.is_empty()).then_some(blocks))
    }

    pub(super) fn available(&self, roots: &Roots, wanted: &[BlockHash]) -> Vec<HistoricalBlock> {
        let cursors = match roots {
            Roots::Suffix { .. } => return Vec::new(),
            Roots::Account(cursor) => std::slice::from_ref(cursor),
            Roots::Plan(cursors) | Roots::Exact(cursors) => cursors.as_slice(),
        };
        wanted
            .iter()
            .filter_map(|hash| {
                self.entries
                    .iter()
                    .find(|(proof, _, _)| {
                        proof.block().hash == *hash
                            && cursors.iter().any(|cursor| {
                                proof.context() == cursor.context()
                                    && cursor.next().is_some_and(|head| {
                                        proof.block().account == head.account
                                            && proof.height() <= head.height
                                            && (!matches!(roots, Roots::Exact(_))
                                                || proof.height() == head.height)
                                            && (proof.height() != head.height || *hash == head.hash)
                                    })
                            })
                    })
                    .map(|(proof, _, _)| proof.clone())
            })
            .collect()
    }

    pub(super) fn remember(&mut self, proofs: &[HistoricalBlock]) {
        self.retain(proofs, false);
    }

    pub(super) fn remember_requested(&mut self, proofs: &[HistoricalBlock]) {
        self.retain(proofs, true);
    }

    fn retain(&mut self, proofs: &[HistoricalBlock], requested: bool) {
        'proofs: for proof in proofs {
            let Ok(size) = bincode::serialized_size(proof.block()) else {
                continue;
            };
            if size > history::MAX_WITNESS_BYTES as u64 {
                continue;
            }
            if let Some(index) = self.entries.iter().position(|(old, _, _)| {
                old.context() == proof.context() && old.block().hash == proof.block().hash
            }) {
                let mut entry = self.entries.remove(index).expect("existing cache entry");
                entry.2 |= requested;
                self.entries.push_back(entry);
                continue;
            }
            let size = size as usize;
            while self.entries.len() >= history::MAX_WITNESS_BLOCKS
                || self.bytes + size > history::MAX_WITNESS_BYTES
            {
                let index = match self.entries.iter().position(|(_, _, requested)| !requested) {
                    Some(index) => index,
                    None if requested => 0,
                    None => continue 'proofs,
                };
                let (_, removed, _) = self.entries.remove(index).expect("bounded cache is nonempty");
                self.bytes -= removed;
            }
            self.entries.push_back((proof.clone(), size, requested));
            self.bytes += size;
        }
    }
}
