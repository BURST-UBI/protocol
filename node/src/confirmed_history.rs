//! Build a bounded, dependency-closed history from a locally cemented LMDB cut
//! or quarantined blocks supplied with a checkpoint certificate.
//!
//! This is input to state replay, not a verified-human snapshot or a
//! checkpoint approval. Detached prefixes require independent certificate
//! authentication before admission. It deliberately does not copy provisional account
//! balances, verification status, delegation or parameter state into a root.

use bincode::Options;
use blake2::{digest::consts::U32, Blake2b, Digest};
use burst_ledger::StateBlock;
use burst_store::StoreError;
use burst_store_lmdb::LmdbStore;
use burst_types::{BlockHash, NetworkId, WalletAddress};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

pub(crate) const MAX_BLOCKS: u64 = 131_072;
pub(crate) const MAX_BYTES: usize = 64 * 1024 * 1024;

pub(crate) mod content;
pub(crate) use content::archive_key as proposal_archive_key;
pub(crate) use content::ProposalWitness;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryHead {
    pub account: WalletAddress,
    pub height: u64,
    pub hash: BlockHash,
}

pub struct ConfirmedHistory {
    network: NetworkId,
    genesis: BlockHash,
    cutoff: u64,
    heads: Vec<HistoryHead>,
    blocks: Vec<StateBlock>,
    contents: Vec<ProposalWitness>,
    commitment: BlockHash,
}

fn invalid(message: &str) -> StoreError {
    StoreError::Corruption(message.into())
}

fn history_digest(network: NetworkId, genesis: BlockHash, cutoff: u64, heads: usize) -> Blake2b<U32> {
    let mut digest = Blake2b::<U32>::new();
    digest.update(b"BURST/confirmed-history/v1\0");
    digest.update([match network {
        NetworkId::Live => 0,
        NetworkId::Test => 1,
        NetworkId::Dev => 2,
    }]);
    digest.update(genesis.as_bytes());
    digest.update(cutoff.to_be_bytes());
    digest.update((heads as u64).to_be_bytes());
    digest
}

/// Commit canonical head metadata without reading blocks. This authenticates
/// nothing by itself; a certificate or snapshot must bind the result.
pub(crate) fn head_commitment(
    network: NetworkId,
    genesis: BlockHash,
    cutoff: u64,
    heads: &[HistoryHead],
) -> Result<BlockHash, StoreError> {
    if genesis.is_zero()
        || heads.is_empty()
        || heads.len() > MAX_BLOCKS as usize
        || heads
            .windows(2)
            .any(|pair| pair[0].account.as_str() >= pair[1].account.as_str())
    {
        return Err(invalid("invalid or noncanonical history heads"));
    }
    let mut digest = history_digest(network, genesis, cutoff, heads.len());
    for head in heads {
        let key = burst_crypto::decode_address(head.account.as_str())
            .ok_or_else(|| invalid("invalid history account"))?;
        if head.height == 0 || head.hash.is_zero() {
            return Err(invalid("invalid history head"));
        }
        digest.update(key);
        digest.update(head.height.to_be_bytes());
        digest.update(head.hash.as_bytes());
    }
    Ok(BlockHash::new(digest.finalize().into()))
}

impl ConfirmedHistory {
    /// Read exact signed ancestry after the caller authenticates its checkpoint
    /// certificate. This does not require another round of ordinary voting in a
    /// closed epoch. Semantic replay and installation finality checks still apply.
    pub(crate) fn read_certified(
        store: &LmdbStore,
        network: NetworkId,
        genesis: BlockHash,
        cutoff: u64,
        heads: &[HistoryHead],
        parent: &[HistoryHead],
    ) -> Result<Self, StoreError> {
        if heads.is_empty() || heads.len() > MAX_BLOCKS as usize {
            return Err(invalid("invalid certified history heads"));
        }
        let txn = store.tx_begin_read()?;
        let mut blocks = Vec::new();
        let mut contents = Vec::new();
        let mut bytes_read = 0usize;
        let mut seen = HashSet::new();
        for head in heads {
            if head.height > MAX_BLOCKS {
                return Err(invalid("certified history requires incremental replay"));
            }
            let mut hash = head.hash;
            for _ in 0..head.height {
                if blocks.len() == MAX_BLOCKS as usize || !seen.insert(hash) {
                    return Err(invalid("oversized or duplicate certified history"));
                }
                let bytes = match store.block_store().get_block_txn(&txn, &hash) {
                    Ok(bytes) => bytes,
                    Err(StoreError::NotFound(_)) => store
                        .meta_store()
                        .get_meta_txn(&txn, &format!("checkpoint_history/v1/{hash}"))?
                        .ok_or_else(|| StoreError::NotFound(hash.to_string()))?,
                    Err(error) => return Err(error),
                };
                bytes_read = bytes_read
                    .checked_add(bytes.len())
                    .filter(|n| *n <= MAX_BYTES)
                    .ok_or_else(|| invalid("certified history exceeds byte limit"))?;
                let block: StateBlock = bincode::DefaultOptions::new()
                    .with_fixint_encoding()
                    .with_limit(bytes.len() as u64)
                    .reject_trailing_bytes()
                    .deserialize(&bytes)
                    .map_err(|error| StoreError::Serialization(error.to_string()))?;
                if block.hash != hash {
                    return Err(invalid("certified history hash mismatch"));
                }
                if let Some(witness) = content::read(&block, &mut bytes_read, |key| {
                    store.meta_store().get_meta_txn(&txn, key)
                })? {
                    contents.push(witness);
                }
                hash = block.previous;
                blocks.push(block);
            }
        }
        Self::from_parts(network, genesis, cutoff, heads, parent, blocks, contents)
    }

    /// Extract an exact ancestral cut from this already bounded history. The
    /// requested heads must exist here; extra tail blocks are not part of it.
    pub(crate) fn prefix(
        &self,
        cutoff: u64,
        heads: &[HistoryHead],
        parent: &[HistoryHead],
    ) -> Result<Self, StoreError> {
        let total = heads
            .iter()
            .try_fold(0u64, |total, head| total.checked_add(head.height))
            .filter(|total| *total <= MAX_BLOCKS)
            .ok_or_else(|| invalid("history prefix exceeds replay limit"))?;
        if heads.is_empty() || heads.len() > MAX_BLOCKS as usize || cutoff > self.cutoff {
            return Err(invalid("invalid history prefix"));
        }
        let index: HashMap<_, _> = self
            .blocks
            .iter()
            .map(|block| (block.hash, block))
            .collect();
        let mut selected = Vec::with_capacity(total as usize);
        let mut seen = HashSet::new();
        for head in heads {
            let mut hash = head.hash;
            for _ in 0..head.height {
                let block = index
                    .get(&hash)
                    .ok_or_else(|| invalid("history prefix is not an ancestor"))?;
                if !seen.insert(hash) {
                    return Err(invalid("duplicate history prefix block"));
                }
                selected.push((*block).clone());
                hash = block.previous;
            }
        }
        let contents = self
            .contents
            .iter()
            .filter(|witness| seen.contains(&witness.block))
            .cloned()
            .collect();
        Self::from_parts(
            self.network,
            self.genesis,
            cutoff,
            heads,
            parent,
            selected,
            contents,
        )
    }

    /// Validate a bounded peer-supplied prefix without writing it to the ledger.
    /// This establishes structure only. A checkpoint certificate and semantic
    /// replay must authenticate it before it can authorize historical admission.
    #[cfg(test)]
    pub(crate) fn from_blocks(
        network: NetworkId,
        genesis: BlockHash,
        cutoff: u64,
        requested: &[HistoryHead],
        parent: &[HistoryHead],
        supplied: Vec<StateBlock>,
    ) -> Result<Self, StoreError> {
        Self::from_parts(
            network,
            genesis,
            cutoff,
            requested,
            parent,
            supplied,
            vec![],
        )
    }

    pub(crate) fn from_parts(
        network: NetworkId,
        genesis: BlockHash,
        cutoff: u64,
        requested: &[HistoryHead],
        parent: &[HistoryHead],
        supplied: Vec<StateBlock>,
        mut contents: Vec<ProposalWitness>,
    ) -> Result<Self, StoreError> {
        if genesis.is_zero()
            || requested.is_empty()
            || requested.len() > MAX_BLOCKS as usize
            || parent.len() > MAX_BLOCKS as usize
            || supplied.len() > MAX_BLOCKS as usize
        {
            return Err(invalid("invalid or oversized history manifest"));
        }
        let mut heads = requested.to_vec();
        heads.sort_by(|a, b| a.account.as_str().cmp(b.account.as_str()));
        let mut digest = history_digest(network, genesis, cutoff, heads.len());
        let mut accounts = HashSet::new();
        let mut total = 0u64;
        for head in &heads {
            let key = burst_crypto::decode_address(head.account.as_str())
                .ok_or_else(|| invalid("non-canonical history account"))?;
            if head.height == 0 || head.hash.is_zero() || !accounts.insert(&head.account) {
                return Err(invalid("duplicate or invalid history head"));
            }
            total = total
                .checked_add(head.height)
                .ok_or_else(|| invalid("history size overflow"))?;
            if total > MAX_BLOCKS {
                return Err(invalid("history requires incremental replay"));
            }
            digest.update(key);
            digest.update(head.height.to_be_bytes());
            digest.update(head.hash.as_bytes());
        }
        if total != supplied.len() as u64 {
            return Err(invalid("history block count differs from manifest"));
        }
        let content_bytes = content::validate(&supplied, &mut contents)?;
        let mut blocks = BTreeMap::new();
        let mut bytes = content_bytes as u64;
        for block in supplied {
            bytes = bytes
                .checked_add(bincode::serialized_size(&block).map_err(|e| invalid(&e.to_string()))?)
                .ok_or_else(|| invalid("history size overflow"))?;
            if bytes > MAX_BYTES as u64 {
                return Err(invalid("history exceeds replay byte limit"));
            }
            block.validate_format().map_err(invalid)?;
            if block.hash != block.compute_hash() || block.timestamp.as_secs() > cutoff {
                return Err(invalid(
                    "history contains an invalid block or a block after cutoff",
                ));
            }
            if blocks.insert(*block.hash.as_bytes(), block).is_some() {
                return Err(invalid("duplicate history block"));
            }
        }
        let mut visited = HashSet::new();
        for head in &heads {
            let mut hash = head.hash;
            for _ in 0..head.height {
                let block = blocks
                    .get(hash.as_bytes())
                    .ok_or_else(|| invalid("history has a missing predecessor"))?;
                if block.account != head.account || !visited.insert(hash) {
                    return Err(invalid("history contains an invalid account prefix"));
                }
                hash = block.previous;
            }
            if !hash.is_zero() {
                return Err(invalid("history head does not match its height"));
            }
        }
        if visited.len() != blocks.len() || !blocks.contains_key(genesis.as_bytes()) {
            return Err(invalid(
                "history contains extra blocks or omits trusted genesis",
            ));
        }
        let replay = causal_order(blocks.values(), |_| None)?
            .into_iter()
            .cloned()
            .collect();
        let history = Self {
            network,
            genesis,
            cutoff,
            heads,
            blocks: replay,
            contents,
            commitment: BlockHash::new(digest.finalize().into()),
        };
        history.validate_extension(parent)?;
        Ok(history)
    }

    /// Build the maximal dependency-closed cut of locally cemented prefixes at
    /// `cutoff`. A dependency beyond the time bound removes its descendants too;
    /// it must never be silently omitted from an otherwise included block.
    pub(crate) fn heads_at(store: &LmdbStore, cutoff: u64) -> Result<Vec<HistoryHead>, StoreError> {
        let txn = store.tx_begin_read()?;
        let accounts = store
            .account_store()
            .all_accounts_bounded_txn(&txn, MAX_BLOCKS as usize)?;
        let block_store = store.block_store();
        let mut blocks = HashMap::new();
        let mut heads = Vec::new();
        let mut bytes_read = 0usize;
        for account in accounts {
            if account.confirmation_height > account.block_count {
                return Err(invalid("confirmation exceeds account height"));
            }
            let mut previous = BlockHash::ZERO;
            let mut head = None;
            for height in 1..=account.confirmation_height {
                if blocks.len() as u64 >= MAX_BLOCKS {
                    return Err(invalid("history requires incremental replay"));
                }
                let hash = block_store
                    .block_at_height_txn(&txn, &account.address, height)?
                    .ok_or_else(|| invalid("confirmed history height missing"))?;
                let bytes = block_store.get_block_txn(&txn, &hash)?;
                bytes_read = bytes_read
                    .checked_add(bytes.len())
                    .ok_or_else(|| invalid("history size overflow"))?;
                if bytes_read > MAX_BYTES {
                    return Err(invalid("history exceeds replay byte limit"));
                }
                let block: StateBlock = bincode::DefaultOptions::new()
                    .with_fixint_encoding()
                    .reject_trailing_bytes()
                    .with_limit(bytes.len() as u64)
                    .deserialize(&bytes)
                    .map_err(|e| StoreError::Serialization(e.to_string()))?;
                if block.hash != hash
                    || block.compute_hash() != hash
                    || block.account != account.address
                    || block.previous != previous
                {
                    return Err(invalid("confirmed prefix identity mismatch"));
                }
                if block.timestamp.as_secs() > cutoff {
                    break;
                }
                previous = hash;
                head = Some(HistoryHead {
                    account: account.address.clone(),
                    height,
                    hash,
                });
                blocks.insert(hash, block);
            }
            if let Some(head) = head {
                heads.push(head);
            }
        }
        let mut children: HashMap<BlockHash, Vec<BlockHash>> = HashMap::new();
        let mut pending = std::collections::VecDeque::new();
        for (hash, block) in &blocks {
            for dependency in block.dependencies() {
                children.entry(dependency).or_default().push(*hash);
                if !blocks.contains_key(&dependency) {
                    pending.push_back(*hash);
                }
            }
        }
        let mut excluded = HashSet::new();
        while let Some(hash) = pending.pop_front() {
            if excluded.insert(hash) {
                if let Some(descendants) = children.get(&hash) {
                    pending.extend(descendants);
                }
            }
        }
        for head in &mut heads {
            while excluded.contains(&head.hash) {
                head.hash = blocks[&head.hash].previous;
                head.height -= 1;
            }
        }
        heads.retain(|head| head.height > 0);
        heads.sort_by(|a, b| a.account.as_str().cmp(b.account.as_str()));
        Ok(heads)
    }

    /// The caller supplies the trusted network/genesis anchor and previous
    /// checkpoint heads. Peer data cannot supply its own trust anchor.
    ///
    /// This reader walks complete prefixes and rejects histories beyond its
    /// bounds. Larger histories require the incremental replay path.
    pub fn read(
        store: &LmdbStore,
        network: NetworkId,
        genesis: BlockHash,
        cutoff: u64,
        requested: &[HistoryHead],
        parent: &[HistoryHead],
    ) -> Result<Self, StoreError> {
        if genesis.is_zero()
            || requested.is_empty()
            || requested.len() > MAX_BLOCKS as usize
            || parent.len() > MAX_BLOCKS as usize
        {
            return Err(invalid("invalid or oversized history manifest"));
        }
        let mut heads = requested.to_vec();
        heads.sort_by(|a, b| a.account.as_str().cmp(b.account.as_str()));
        let mut prefixes = HashMap::new();
        let mut total = 0u64;
        let mut digest = history_digest(network, genesis, cutoff, heads.len());
        for head in &heads {
            let key = burst_crypto::decode_address(head.account.as_str())
                .ok_or_else(|| invalid("non-canonical history account"))?;
            if head.height == 0
                || head.hash.is_zero()
                || prefixes.insert(head.account.clone(), head).is_some()
            {
                return Err(invalid("duplicate or invalid history head"));
            }
            total = total
                .checked_add(head.height)
                .ok_or_else(|| invalid("history size overflow"))?;
            if total > MAX_BLOCKS {
                return Err(invalid("history requires incremental replay"));
            }
            digest.update(key);
            digest.update(head.height.to_be_bytes());
            digest.update(head.hash.as_bytes());
        }
        let txn = store.tx_begin_read()?;
        let account_store = store.account_store();
        let block_store = store.block_store();
        let mut parent_accounts = HashSet::new();
        for old in parent {
            if old.height == 0 || old.hash.is_zero() || !parent_accounts.insert(&old.account) {
                return Err(invalid("invalid parent history manifest"));
            }
            let head = prefixes
                .get(&old.account)
                .ok_or_else(|| invalid("history drops a parent account"))?;
            if head.height < old.height
                || block_store.block_at_height_txn(&txn, &old.account, old.height)?
                    != Some(old.hash)
            {
                return Err(invalid("history does not extend its trusted parent"));
            }
        }
        let mut blocks = BTreeMap::new();
        let mut contents = Vec::new();
        let mut bytes_read = 0usize;
        for head in &heads {
            let account = account_store.get_account_txn(&txn, &head.account)?;
            if account.address != head.account
                || account.confirmation_height > account.block_count
                || head.height > account.confirmation_height
            {
                return Err(invalid("history head is not locally cemented"));
            }
            let mut previous = BlockHash::ZERO;
            for height in 1..=head.height {
                let hash = block_store
                    .block_at_height_txn(&txn, &head.account, height)?
                    .ok_or_else(|| invalid("history has a missing height"))?;
                let bytes = block_store.get_block_txn(&txn, &hash)?;
                bytes_read = bytes_read
                    .checked_add(bytes.len())
                    .ok_or_else(|| invalid("history size overflow"))?;
                if bytes_read > MAX_BYTES {
                    return Err(invalid("history exceeds replay byte limit"));
                }
                let block: StateBlock = bincode::DefaultOptions::new()
                    .with_fixint_encoding()
                    .with_limit(bytes.len() as u64)
                    .reject_trailing_bytes()
                    .deserialize(&bytes)
                    .map_err(|e| StoreError::Serialization(e.to_string()))?;
                block.validate_format().map_err(invalid)?;
                if block.hash != hash
                    || block.compute_hash() != hash
                    || block.account != head.account
                    || block.previous != previous
                    || block.timestamp.as_secs() > cutoff
                {
                    return Err(invalid(
                        "history contains an invalid prefix or a block after cutoff",
                    ));
                }
                if let Some(witness) = content::read(&block, &mut bytes_read, |key| {
                    store.meta_store().get_meta_txn(&txn, key)
                })? {
                    contents.push(witness);
                }
                if blocks.insert(*hash.as_bytes(), block).is_some() {
                    return Err(invalid("block appears in multiple account prefixes"));
                }
                previous = hash;
            }
            if previous != head.hash {
                return Err(invalid("history head does not match its height"));
            }
        }
        if !blocks.contains_key(genesis.as_bytes()) {
            return Err(invalid("history omits trusted genesis"));
        }
        for block in blocks.values() {
            for dependency in block.dependencies() {
                if !blocks.contains_key(dependency.as_bytes()) {
                    return Err(invalid("history omits a causal dependency"));
                }
            }
        }
        let replay = causal_order(blocks.values(), |_| None)?
            .into_iter()
            .cloned()
            .collect();
        contents.sort_by_key(|witness| *witness.block.as_bytes());
        Ok(Self {
            network,
            genesis,
            cutoff,
            heads,
            blocks: replay,
            contents,
            commitment: BlockHash::new(digest.finalize().into()),
        })
    }

    pub fn commitment(&self) -> BlockHash {
        self.commitment
    }
    pub fn network(&self) -> NetworkId {
        self.network
    }
    pub fn genesis(&self) -> BlockHash {
        self.genesis
    }
    pub fn cutoff(&self) -> u64 {
        self.cutoff
    }
    pub fn heads(&self) -> &[HistoryHead] {
        &self.heads
    }
    pub fn replay_blocks(&self) -> &[StateBlock] {
        &self.blocks
    }

    pub(crate) fn proposal_witnesses(&self) -> &[ProposalWitness] {
        &self.contents
    }

    pub fn proposal_content(&self, hash: &BlockHash) -> Option<&[u8]> {
        self.contents
            .binary_search_by_key(hash.as_bytes(), |witness| *witness.block.as_bytes())
            .ok()
            .map(|index| self.contents[index].bytes.as_slice())
    }

    pub(crate) fn validate_extension(&self, parent: &[HistoryHead]) -> Result<(), StoreError> {
        let mut accounts = HashSet::new();
        if parent
            .iter()
            .any(|head| head.height == 0 || head.hash.is_zero() || !accounts.insert(&head.account))
        {
            return Err(invalid("invalid parent history manifest"));
        }
        let mut heights = HashMap::new();
        for block in &self.blocks {
            let height = if block.previous.is_zero() {
                1
            } else {
                heights
                    .get(&block.previous)
                    .copied()
                    .ok_or_else(|| invalid("history predecessor is missing"))?
                    + 1
            };
            heights.insert(block.hash, height);
        }
        let blocks: HashMap<_, _> = self
            .blocks
            .iter()
            .map(|block| (block.hash, block))
            .collect();
        for head in parent {
            if heights.get(&head.hash) != Some(&head.height)
                || blocks
                    .get(&head.hash)
                    .is_none_or(|block| block.account != head.account)
            {
                return Err(invalid(
                    "history does not extend the certified parent manifest",
                ));
            }
        }
        Ok(())
    }
}

/// Stable root order with explicit and application-derived prerequisites.
/// Inputs must already satisfy the bounded history reader's structural checks.
pub(crate) fn causal_order<'a, I: IntoIterator<Item = BlockHash>>(
    blocks: impl IntoIterator<Item = &'a StateBlock>,
    additional: impl Fn(&StateBlock) -> I,
) -> Result<Vec<&'a StateBlock>, StoreError> {
    causal_order_after(blocks, &HashSet::new(), additional)
}

/// Dependencies in `applied` must have been authenticated independently. They
/// are already executed boundaries, not unverified missing blocks to ignore.
pub(crate) fn causal_order_after<'a, I: IntoIterator<Item = BlockHash>>(
    blocks: impl IntoIterator<Item = &'a StateBlock>,
    applied: &HashSet<BlockHash>,
    additional: impl Fn(&StateBlock) -> I,
) -> Result<Vec<&'a StateBlock>, StoreError> {
    let blocks: BTreeMap<_, _> = blocks
        .into_iter()
        .map(|block| (*block.hash.as_bytes(), block))
        .collect();
    let order = causal_hash_order_after(
        blocks.keys().copied().map(BlockHash::new),
        applied,
        |hash| {
            let block = blocks
                .get(hash.as_bytes())
                .ok_or_else(|| invalid("history omits a replay prerequisite"))?;
            Ok(block.dependencies().chain(additional(block)).collect())
        },
    )?;
    order
        .into_iter()
        .map(|hash| {
            blocks
                .get(hash.as_bytes())
                .copied()
                .ok_or_else(|| invalid("ordered history block disappeared"))
        })
        .collect()
}

/// Preserve the existing hash-rooted depth-first order while loading dependency
/// metadata on demand. A read failure invalidates the entire private order.
pub(crate) fn causal_hash_order_after(
    hashes: impl IntoIterator<Item = BlockHash>,
    applied: &HashSet<BlockHash>,
    dependencies: impl FnMut(&BlockHash) -> Result<Vec<BlockHash>, StoreError>,
) -> Result<Vec<BlockHash>, StoreError> {
    causal_fallible_hash_order_after(hashes.into_iter().map(Ok), applied, dependencies)
}

pub(crate) fn causal_fallible_hash_order_after(
    hashes: impl IntoIterator<Item = Result<BlockHash, StoreError>>,
    applied: &HashSet<BlockHash>,
    mut dependencies: impl FnMut(&BlockHash) -> Result<Vec<BlockHash>, StoreError>,
) -> Result<Vec<BlockHash>, StoreError> {
    #[derive(Clone, Copy)]
    enum Visit { New, Active, Complete }
    let mut states = HashMap::new();
    for hash in hashes { states.insert(*hash?.as_bytes(), Visit::New); }
    // Deduplicate before allocating root order, including repeated untrusted input.
    // Root order is consensus-visible; membership and traversal colors are not.
    let mut hashes: Vec<_> = states.keys().copied().collect();
    hashes.sort_unstable();
    let mut replay = Vec::with_capacity(hashes.len());
    for root in &hashes {
        let mut stack = vec![(*root, false)];
        while let Some((hash, complete)) = stack.pop() {
            if applied.contains(&BlockHash::new(hash)) {
                if states.contains_key(&hash) {
                    return Err(invalid("history replays an already applied block"));
                }
                continue;
            }
            let state = states.get_mut(&hash)
                .ok_or_else(|| invalid("history omits a replay prerequisite"))?;
            if matches!(state, Visit::Complete) { continue; }
            if complete {
                *state = Visit::Complete;
                replay.push(BlockHash::new(hash));
            } else {
                if matches!(state, Visit::Active) {
                    return Err(invalid("cyclic history dependency"));
                }
                *state = Visit::Active;
                stack.push((hash, true));
                let dependencies = dependencies(&BlockHash::new(hash))?;
                for dependency in dependencies.into_iter().rev() {
                    stack.push((*dependency.as_bytes(), false));
                }
            }
        }
    }
    Ok(replay)
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_ledger::BlockType;
    use burst_store::{account::AccountStore, block::BlockStore};
    use burst_types::{Signature, Timestamp, TxHash, WalletState};

    #[test]
    fn streamed_head_commitment_matches_legacy_bytes_through_the_head_limit() {
        let genesis = BlockHash::new([91; 32]);
        let mut heads: Vec<_> = (0..MAX_BLOCKS).map(|n| {
            let mut public = [0; 32];
            public[..8].copy_from_slice(&n.to_be_bytes());
            HistoryHead {
                account: burst_crypto::derive_address(&burst_types::PublicKey(public)),
                height: n + 1,
                hash: BlockHash::new([17; 32]),
            }
        }).collect();
        heads.sort_by(|a, b| a.account.as_str().cmp(b.account.as_str()));
        for size in [1, 17, MAX_BLOCKS as usize] {
            for (network, tag) in [(NetworkId::Live, 0), (NetworkId::Test, 1), (NetworkId::Dev, 2)] {
                // Independent oracle preserves the previous concatenated encoding.
                let mut bytes = b"BURST/confirmed-history/v1\0".to_vec();
                bytes.push(tag);
                bytes.extend_from_slice(genesis.as_bytes());
                bytes.extend_from_slice(&u64::MAX.to_be_bytes());
                bytes.extend_from_slice(&(size as u64).to_be_bytes());
                for head in &heads[..size] {
                    bytes.extend_from_slice(&burst_crypto::decode_address(head.account.as_str()).unwrap());
                    bytes.extend_from_slice(&head.height.to_be_bytes());
                    bytes.extend_from_slice(head.hash.as_bytes());
                }
                assert_eq!(head_commitment(network, genesis, u64::MAX, &heads[..size]).unwrap(),
                    BlockHash::new(burst_crypto::blake2b_256(&bytes)));
            }
        }
        assert!(head_commitment(NetworkId::Dev, genesis, 0, &[]).is_err());
        assert!(head_commitment(NetworkId::Dev, BlockHash::ZERO, 0, &heads[..1]).is_err());
        let mut invalid = heads[..2].to_vec();
        invalid.swap(0, 1);
        assert!(head_commitment(NetworkId::Dev, genesis, 0, &invalid).is_err());
        invalid[1] = invalid[0].clone();
        assert!(head_commitment(NetworkId::Dev, genesis, 0, &invalid).is_err());
        for (account, height, hash) in [
            (WalletAddress::new("brst_invalid"), 1, genesis),
            (heads[0].account.clone(), 0, genesis),
            (heads[0].account.clone(), 1, BlockHash::ZERO),
        ] {
            assert!(head_commitment(NetworkId::Dev, genesis, 0,
                &[HistoryHead { account, height, hash }]).is_err());
        }
        heads.push(heads[0].clone());
        assert!(head_commitment(NetworkId::Dev, genesis, 0, &heads).is_err());
    }

    #[test]
    fn hash_order_preserves_dependency_order_and_propagates_source_failures() {
        let [a, b, c, d] = [1, 2, 3, 4].map(|n| BlockHash::new([n; 32]));
        let graph = HashMap::from([(a, vec![d, b]), (b, vec![c]), (c, vec![]), (d, vec![c])]);
        for roots in [vec![a, b, c, d], vec![d, c, b, a]] {
            let mut reads = HashMap::new();
            let ordered = causal_hash_order_after(roots, &HashSet::new(), |hash| {
                *reads.entry(*hash).or_insert(0) += 1;
                Ok(graph[hash].clone())
            })
            .unwrap();
            assert_eq!(ordered, vec![c, d, b, a]);
            assert!(reads.values().all(|count| *count == 1));
        }
        assert_eq!(
            causal_hash_order_after([a, b, d], &HashSet::from([c]), |hash| Ok(
                graph[hash].clone()
            ))
            .unwrap(),
            vec![d, b, a]
        );
        assert!(causal_hash_order_after(
            [a, b, d],
            &HashSet::new(),
            |hash| Ok(graph[hash].clone())
        )
        .is_err());
        assert!(
            causal_hash_order_after([a], &HashSet::from([a]), |_| panic!(
                "must reject already applied roots before reading"
            ))
            .is_err()
        );
        assert!(
            causal_hash_order_after([a, b], &HashSet::new(), |hash| Ok(if *hash == a {
                vec![b]
            } else {
                vec![a]
            }))
            .is_err()
        );
        assert!(causal_hash_order_after([a], &HashSet::new(), |_| Ok(vec![a])).is_err());
        let error = causal_hash_order_after([a, b, c, d], &HashSet::new(), |hash| {
            if *hash == c {
                return Err(StoreError::Backend(
                    "injected dependency read failure".into(),
                ));
            }
            Ok(graph[hash].clone())
        })
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("injected dependency read failure"));
    }

    #[test]
    fn hash_order_handles_deep_chains_and_dense_shared_dependencies_once() {
        fn hash(n: usize) -> BlockHash {
            let mut bytes = [0; 32];
            bytes[24..].copy_from_slice(&(n as u64 + 1).to_be_bytes());
            BlockHash::new(bytes)
        }
        for (count, dense) in [(4096, false), (256, true)] {
            let roots: Vec<_> = (0..count).map(hash).collect();
            let positions: HashMap<_, _> = roots.iter().enumerate().map(|(n, h)| (*h, n)).collect();
            for reverse in [false, true] {
                let mut input = roots.clone();
                if reverse { input.reverse(); }
                input.extend_from_slice(&roots[..3]);
                let mut reads = vec![0; count];
                let order = causal_hash_order_after(input, &HashSet::new(), |h| {
                    let n = positions[h];
                    reads[n] += 1;
                    let mut edges = if dense { roots[n + 1..].to_vec() }
                        else { roots.get(n + 1).copied().into_iter().collect() };
                    // Repeated edges must not repeat resolution or invent a cycle.
                    if let Some(first) = edges.first().copied() { edges.push(first); }
                    Ok(edges)
                }).unwrap();
                assert_eq!(order, roots.iter().copied().rev().collect::<Vec<_>>());
                assert!(reads.into_iter().all(|n| n == 1));
            }
        }
    }

    #[test]
    fn fallible_hash_inventory_fails_before_loading_dependencies() {
        let hash = BlockHash::new([7; 32]);
        let error = causal_fallible_hash_order_after([
            Ok(hash), Err(StoreError::Backend("inventory read failed".into())), Ok(hash),
        ], &HashSet::new(), |_| panic!("partial inventory must never resolve dependencies")).unwrap_err();
        assert!(error.to_string().contains("inventory read failed"));
    }

    fn block(
        seed: u8,
        kind: BlockType,
        previous: BlockHash,
        link: BlockHash,
        time: u64,
    ) -> StateBlock {
        let account =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[seed; 32]).public);
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: burst_ledger::CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: kind,
            account: account.clone(),
            representative: account,
            previous,
            brn_spent: 0,
            trst_balance: 0,
            link,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: vec![],
            release_sources: Vec::new(),
            timestamp: Timestamp::new(time),
            params_hash: BlockHash::ZERO,
            work: 0,
            signature: Signature([0; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block
    }

    fn save(store: &LmdbStore, block: &StateBlock, height: u64, confirmed: u64) {
        let mut account =
            crate::genesis_state::account_to_write(None, block.account.clone(), block.hash)
                .unwrap();
        account.block_count = height;
        account.confirmation_height = confirmed;
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_block_with_account(
                &block.hash,
                &bincode::serialize(block).unwrap(),
                &block.account,
                height,
            )
            .unwrap();
        batch.put_account_info(&account, false).unwrap();
        batch.commit().unwrap();
    }

    fn head(block: &StateBlock, height: u64) -> HistoryHead {
        HistoryHead {
            account: block.account.clone(),
            hash: block.hash,
            height,
        }
    }

    fn fixture(store: &LmdbStore) -> (StateBlock, StateBlock, StateBlock) {
        let genesis = block(0, BlockType::Open, BlockHash::ZERO, BlockHash::ZERO, 0);
        let send = block(
            0,
            BlockType::Send,
            genesis.hash,
            BlockHash::new([9; 32]),
            10,
        );
        let receive = block(1, BlockType::Open, BlockHash::ZERO, send.hash, 20);
        save(store, &genesis, 1, 1);
        save(store, &send, 2, 2);
        save(store, &receive, 1, 1);
        (genesis, send, receive)
    }

    #[test]
    fn detached_history_matches_local_commitment_and_rejects_malformed_prefixes() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let (genesis, send, receive) = fixture(&store);
        let heads = vec![head(&send, 2), head(&receive, 1)];
        let blocks = vec![receive.clone(), send.clone(), genesis.clone()];
        let local =
            ConfirmedHistory::read(&store, NetworkId::Dev, genesis.hash, 20, &heads, &[]).unwrap();
        let detached = ConfirmedHistory::from_blocks(
            NetworkId::Dev,
            genesis.hash,
            20,
            &heads,
            &[head(&genesis, 1)],
            blocks.clone(),
        )
        .unwrap();
        assert_eq!(detached.commitment(), local.commitment());
        assert_eq!(detached.heads(), local.heads());
        assert_eq!(
            bincode::serialize(detached.replay_blocks()).unwrap(),
            bincode::serialize(local.replay_blocks()).unwrap()
        );
        for mutation in 0..8 {
            let mut heads = heads.clone();
            let mut blocks = blocks.clone();
            let mut parent = vec![head(&genesis, 1)];
            let mut cutoff = 20;
            match mutation {
                0 => {
                    blocks.pop();
                }
                1 => blocks[2] = blocks[1].clone(),
                2 => heads[0].height += 1,
                3 => heads[0].height -= 1,
                4 => heads[1].account = heads[0].account.clone(),
                5 => cutoff = 19,
                6 => parent[0].hash = receive.hash,
                _ => parent.push(parent[0].clone()),
            }
            assert!(
                ConfirmedHistory::from_blocks(
                    NetworkId::Dev,
                    genesis.hash,
                    cutoff,
                    &heads,
                    &parent,
                    blocks
                )
                .is_err(),
                "mutation {mutation}"
            );
        }
    }

    #[test]
    fn canonical_history_ignores_arrival_order_and_provisional_account_effects() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let (genesis, send, receive) = fixture(&store);
        let heads = vec![head(&send, 2), head(&receive, 1)];
        let read = |heads: &[HistoryHead]| {
            ConfirmedHistory::read(&store, NetworkId::Dev, genesis.hash, 20, heads, &[]).unwrap()
        };
        let original = read(&heads);
        assert_eq!(
            original
                .replay_blocks()
                .iter()
                .map(|b| b.hash)
                .collect::<Vec<_>>(),
            vec![genesis.hash, send.hash, receive.hash]
        );
        let mut reversed = heads.clone();
        reversed.reverse();
        assert_eq!(original.commitment(), read(&reversed).commitment());

        let provisional = block(0, BlockType::Send, send.hash, BlockHash::new([8; 32]), 30);
        save(&store, &provisional, 3, 2);
        let mut account = store.account_store().get_account(&send.account).unwrap();
        account.state = WalletState::Revoked;
        account.orv_evicted = true;
        account.total_brn_burned = 999;
        account.trst_balance = 999;
        account.representative = receive.account.clone();
        store.account_store().put_account(&account).unwrap();
        let after = read(&heads);
        assert_eq!(original.commitment(), after.commitment());
        assert_eq!(original.heads(), after.heads());
        assert!(!after
            .replay_blocks()
            .iter()
            .any(|b| b.hash == provisional.hash));
    }

    #[test]
    fn rejects_unconfirmed_heads_missing_sources_and_parent_regression() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let (genesis, send, receive) = fixture(&store);
        let read = |heads: &[HistoryHead], parent: &[HistoryHead]| {
            ConfirmedHistory::read(&store, NetworkId::Dev, genesis.hash, 100, heads, parent)
        };
        assert!(read(&[head(&genesis, 1), head(&receive, 1)], &[]).is_err());
        assert!(read(&[head(&genesis, 1)], &[head(&send, 2)]).is_err());
        assert!(read(&[head(&genesis, 1)], &[head(&receive, 1)]).is_err());
        let provisional = block(0, BlockType::Send, send.hash, BlockHash::ZERO, 30);
        save(&store, &provisional, 3, 2);
        assert!(read(&[head(&provisional, 3)], &[]).is_err());
        assert!(read(&[head(&send, 2), head(&receive, 1)], &[head(&genesis, 1)]).is_ok());
    }

    #[test]
    fn cutoff_checks_every_ancestor_not_only_the_head() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let (genesis, send, _) = fixture(&store);
        let future = block(0, BlockType::Send, send.hash, BlockHash::ZERO, 30);
        let backdated = block(
            0,
            BlockType::ChangeRepresentative,
            future.hash,
            BlockHash::ZERO,
            25,
        );
        save(&store, &future, 3, 3);
        save(&store, &backdated, 4, 4);
        assert!(ConfirmedHistory::read(
            &store,
            NetworkId::Dev,
            genesis.hash,
            25,
            &[head(&backdated, 4)],
            &[]
        )
        .is_err());
    }

    #[test]
    fn rejects_duplicate_heads_oversized_prefixes_and_modified_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let (genesis, mut send, _) = fixture(&store);
        let requested = head(&send, 2);
        let read = |heads: &[HistoryHead]| {
            ConfirmedHistory::read(&store, NetworkId::Dev, genesis.hash, 100, heads, &[])
        };
        assert!(read(&[requested.clone(), requested.clone()]).is_err());
        let mut oversized = requested.clone();
        oversized.height = MAX_BLOCKS + 1;
        assert!(read(&[oversized]).is_err());
        send.brn_spent += 1;
        store
            .block_store()
            .put_block(&send.hash, &bincode::serialize(&send).unwrap())
            .unwrap();
        assert!(read(&[requested]).is_err());
    }

    #[test]
    fn commitments_bind_cutoff_network_and_persisted_history() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let (genesis, send, receive) = fixture(&store);
        let heads = [head(&send, 2), head(&receive, 1)];
        let original =
            ConfirmedHistory::read(&store, NetworkId::Dev, genesis.hash, 20, &heads, &[])
                .unwrap()
                .commitment();
        let mut canonical = heads.to_vec();
        canonical.sort_by(|a, b| a.account.as_str().cmp(b.account.as_str()));
        assert_eq!(original, head_commitment(NetworkId::Dev, genesis.hash, 20, &canonical).unwrap());
        assert_ne!(
            original,
            ConfirmedHistory::read(&store, NetworkId::Dev, genesis.hash, 21, &heads, &[])
                .unwrap()
                .commitment()
        );
        assert_ne!(
            original,
            ConfirmedHistory::read(&store, NetworkId::Test, genesis.hash, 20, &heads, &[])
                .unwrap()
                .commitment()
        );
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        assert_eq!(
            original,
            ConfirmedHistory::read(&store, NetworkId::Dev, genesis.hash, 20, &heads, &[])
                .unwrap()
                .commitment()
        );
    }
}
