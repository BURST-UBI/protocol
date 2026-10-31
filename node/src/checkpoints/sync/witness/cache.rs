use super::{history, invalid, HistoricalBlock, Roots};
use burst_store::StoreError;
use burst_types::{BlockHash, WalletAddress};
use std::collections::{HashMap, VecDeque};

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
            let Some((proof, _, _)) = self
                .entries
                .iter()
                .find(|(proof, _, _)| proof.block().hash == hash)
            else {
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

    pub(super) fn available(
        &self,
        roots: &Roots,
        wanted: &[BlockHash],
    ) -> Result<Vec<HistoricalBlock>, StoreError> {
        if let Roots::Stored(roots) = roots {
            // Cache size is bounded independently of the account population.
            // Inspect only cached requested proofs, resolving their roots by key.
            let requested: std::collections::HashSet<_> = wanted.iter().copied().collect();
            let mut found = HashMap::new();
            for (proof, _, _) in &self.entries {
                if !requested.contains(&proof.block().hash)
                    || found.contains_key(&proof.block().hash)
                {
                    continue;
                }
                let Some(cursor) = roots.cursor(&proof.block().account)? else {
                    continue;
                };
                let head = cursor.next().expect("stored nonempty root");
                if proof.context() == cursor.context()
                    && proof.height() <= head.height
                    && (proof.height() != head.height || proof.block().hash == head.hash)
                {
                    found.insert(proof.block().hash, proof.clone());
                }
            }
            return Ok(wanted
                .iter()
                .filter_map(|hash| found.remove(hash))
                .collect());
        }
        let cursors = match roots {
            Roots::Suffix { .. } => return Ok(Vec::new()),
            Roots::Stored(_) => unreachable!("handled stored roots"),
            Roots::Account(cursor) => std::slice::from_ref(cursor),
            Roots::Plan(cursors) | Roots::Exact(cursors) => cursors.as_slice(),
        };
        // Validated plans have unique accounts; exact requests have unique head
        // hashes (but may revisit one account at different heights). Index only
        // cached proofs. Expected work is linear in cursors, cache entries and
        // requested hashes; index space is linear in cache entries.
        #[derive(Hash, PartialEq, Eq)]
        enum Key<'a> {
            Account(&'a WalletAddress),
            Hash(BlockHash),
        }
        let exact = matches!(roots, Roots::Exact(_));
        let mut indexed: HashMap<_, Vec<_>> = HashMap::new();
        for (index, (proof, _, _)) in self.entries.iter().enumerate() {
            let key = if exact {
                Key::Hash(proof.block().hash)
            } else {
                Key::Account(&proof.block().account)
            };
            indexed.entry(key).or_default().push((index, proof));
        }
        let mut available: HashMap<BlockHash, (usize, &HistoricalBlock)> = HashMap::new();
        for cursor in cursors {
            let Some(head) = cursor.next() else {
                continue;
            };
            let key = if exact {
                Key::Hash(head.hash)
            } else {
                Key::Account(&head.account)
            };
            let Some(proofs) = indexed.get(&key) else {
                continue;
            };
            for &(index, proof) in proofs {
                if proof.context() == cursor.context()
                    && proof.block().account == head.account
                    && proof.height() <= head.height
                    && (!exact || proof.height() == head.height)
                    && (proof.height() != head.height || proof.block().hash == head.hash)
                {
                    let entry = available
                        .entry(proof.block().hash)
                        .or_insert((index, proof));
                    if index < entry.0 {
                        *entry = (index, proof);
                    }
                }
            }
        }
        Ok(wanted
            .iter()
            .filter_map(|hash| available.get(hash).map(|(_, proof)| (*proof).clone()))
            .collect())
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
                let (_, removed, _) = self
                    .entries
                    .remove(index)
                    .expect("bounded cache is nonempty");
                self.bytes -= removed;
            }
            self.entries.push_back((proof.clone(), size, requested));
            self.bytes += size;
        }
    }
}
