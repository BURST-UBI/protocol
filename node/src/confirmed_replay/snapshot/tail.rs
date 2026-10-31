//! Bounded account-chain suffixes rooted in authenticated snapshot heads.
//! Reading checks structure and signed context; replay uses the ordinary
//! economic executor. Neither operation installs state or grants finality.

use super::continuation::Base;
use super::{
    archive::AuthenticatedSnapshot,
    history::{HistoricalBlock, HistoricalBlocks},
    ReplayError,
};
use crate::confirmed_history::HistoryHead;
use crate::confirmed_replay::blocks::{staged::StagedBlocks, AppliedBlocks};
use bincode::Options;
use burst_ledger::{BlockType, StateBlock};
use burst_store::StoreError;
use burst_store_lmdb::LmdbStore;
use burst_types::{BlockHash, NetworkId};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

// Compact export, installation and startup must support boundaries accepted by
// full replay. These are per-boundary work budgets, not account lifetime limits.
pub const MAX_TAIL_BLOCKS: usize = crate::confirmed_history::MAX_BLOCKS as usize;
pub const MAX_TAIL_BYTES: usize = crate::confirmed_history::MAX_BYTES;
const MAX_HEADS: usize = 131_072;

pub mod cemented;

fn invalid(message: impl ToString) -> ReplayError {
    ReplayError::Invalid(message.to_string())
}

/// A candidate suffix, never a certificate or a replacement for semantic replay.
/// Old dependency bodies retain their snapshot-bound membership witnesses.
pub struct SnapshotTail {
    pub(super) directory: std::path::PathBuf,
    context: super::history::HistoryContext,
    cutoff: u64,
    heads: Vec<HistoryHead>,
    archive: TailArchive,
    commitment: BlockHash,
}

/// Replay coverage shares the checked tail's storage. It never materializes a
/// second collection of serialized bodies merely to retain signing evidence.
#[derive(Clone)]
pub(crate) struct TailArchive {
    blocks: Arc<StagedBlocks>,
    order: Arc<[BlockHash]>,
    retained: Arc<HistoricalBlocks>,
    contents: Arc<[crate::confirmed_history::ProposalWitness]>,
}

pub(crate) struct ArchiveRows<'a> {
    archive: &'a TailArchive,
    index: usize,
}

impl TailArchive {
    pub(in crate::confirmed_replay) fn visit_input<E: From<StoreError>>(
        &self,
        mut visit: impl FnMut(&StateBlock) -> Result<(), E>,
    ) -> Result<(), E> {
        self.retained.try_visit_with(|proof| visit(proof.block()))?;
        self.blocks.visit_with(|block| visit(&block))
    }

    // Replay input order is retained witnesses followed by the signed suffix.
    pub(in crate::confirmed_replay) fn input_hashes(&self) -> impl Iterator<Item = BlockHash> + '_ {
        self.retained.hashes().chain(self.order.iter().copied())
    }

    pub(in crate::confirmed_replay) fn input_block(
        &self,
        hash: &BlockHash,
    ) -> Result<Option<std::borrow::Cow<'_, StateBlock>>, StoreError> {
        match self.blocks.get(hash)? {
            Some(block) => Ok(Some(block)),
            None => self.retained.input_block(hash),
        }
    }

    pub(super) fn encoded(&self, hash: &BlockHash) -> Result<Option<Vec<u8>>, StoreError> {
        if self.blocks.contains(hash)? {
            return self.blocks.encoded(hash).map(Some);
        }
        self.retained.encoded(hash)
    }

    pub(crate) fn rows(&self) -> ArchiveRows<'_> {
        ArchiveRows {
            archive: self,
            index: 0,
        }
    }
}

impl Iterator for ArchiveRows<'_> {
    type Item = Result<(String, Vec<u8>), StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        let archive = self.archive;
        let index = self.index;
        if index >= archive.order.len() + archive.retained.len() + archive.contents.len() {
            return None;
        }
        self.index += 1;
        if let Some(hash) = archive.order.get(index) {
            return Some(
                archive
                    .blocks
                    .encoded(hash)
                    .map(|bytes| (format!("checkpoint_history/v1/{hash}"), bytes)),
            );
        }
        let index = index - archive.order.len();
        if let Some(row) = archive.retained.row(index) {
            return Some(row.map(|(hash, bytes)| (format!("checkpoint_history/v1/{hash}"), bytes)));
        }
        archive
            .contents
            .get(index - archive.retained.len())
            .map(|content| {
                Ok((
                    crate::confirmed_history::proposal_archive_key(content.block),
                    content.bytes.clone(),
                ))
            })
    }
}

/// Signed suffix bytes and exact old-body requests. Planning proves neither
/// historical membership nor economic validity; finish requires authenticated
/// witnesses and the resulting tail still needs semantic replay.
pub struct SnapshotTailPlan {
    directory: std::path::PathBuf,
    context: super::history::HistoryContext,
    cutoff: u64,
    heads: Vec<HistoryHead>,
    blocks: StagedBlocks,
    contents: Vec<crate::confirmed_history::ProposalWitness>,
    commitment: BlockHash,
    applied: HashSet<BlockHash>,
    required: Vec<BlockHash>,
    used_bytes: u64,
}

/// Untrusted transport bodies. Planning checks the signed suffix; it does not
/// establish finality or apply economics. Legacy transfers can contain a prefix
/// that is deliberately not replayed again.
pub(crate) struct TransferredTail {
    blocks: StagedBlocks,
    pub contents: Vec<crate::confirmed_history::ProposalWitness>,
}

impl TransferredTail {
    pub(crate) fn new(directory: &std::path::Path) -> Result<Self, StoreError> {
        Ok(Self {
            blocks: StagedBlocks::new(directory)?,
            contents: Vec::new(),
        })
    }

    pub(crate) fn insert(&mut self, block: &StateBlock) -> Result<(), StoreError> {
        self.blocks.insert(block)
    }

    pub(crate) fn blocks(&self) -> impl Iterator<Item = Result<StateBlock, StoreError>> + '_ {
        self.blocks.hashes().map(|hash| {
            let hash = hash?;
            self.blocks
                .get(&hash)?
                .map(|block| block.into_owned())
                .ok_or_else(|| StoreError::NotFound(format!("transferred tail block {hash}")))
        })
    }

    pub(crate) fn from_blocks(
        blocks: Vec<StateBlock>,
        contents: Vec<crate::confirmed_history::ProposalWitness>,
        directory: &std::path::Path,
    ) -> Result<Self, StoreError> {
        let mut tail = Self::new(directory)?;
        for block in blocks {
            tail.insert(&block)?;
        }
        tail.contents = contents;
        Ok(tail)
    }

    pub(super) fn plan(
        self,
        base: Base<'_>,
        cutoff: u64,
        heads: &[HistoryHead],
        directory: &std::path::Path,
    ) -> Result<SnapshotTailPlan, ReplayError> {
        let mut contents = HashMap::new();
        for content in self.contents {
            let key = crate::confirmed_history::proposal_archive_key(content.block);
            if contents.insert(key, content.bytes).is_some() {
                return Err(invalid("duplicate transferred proposal body"));
            }
        }
        SnapshotTailPlan::read(
            base,
            directory,
            cutoff,
            heads,
            |hash| self.blocks.encoded(&hash).map_err(invalid),
            |key| Ok(contents.get(key).cloned()),
        )
    }
}

impl SnapshotTail {
    pub(in crate::confirmed_replay) fn storage_directory(&self) -> &std::path::Path {
        &self.directory
    }

    pub(crate) fn archive(&self) -> TailArchive {
        self.archive.clone()
    }

    pub(super) fn contents(&self) -> &[crate::confirmed_history::ProposalWitness] {
        &self.archive.contents
    }

    pub fn context(&self) -> super::history::HistoryContext {
        self.context
    }
    pub fn cutoff(&self) -> u64 {
        self.cutoff
    }
    pub fn heads(&self) -> &[HistoryHead] {
        &self.heads
    }
    pub fn block_count(&self) -> usize {
        self.archive.order.len()
    }
    pub fn block_hashes(&self) -> impl Iterator<Item = BlockHash> + '_ {
        self.archive.order.iter().copied()
    }
    pub fn blocks(&self) -> impl Iterator<Item = Result<StateBlock, StoreError>> + '_ {
        self.archive.order.iter().map(|hash| {
            self.archive
                .blocks
                .get(hash)?
                .map(|block| block.into_owned())
                .ok_or_else(|| StoreError::NotFound(format!("staged tail block {hash}")))
        })
    }
    #[cfg(test)]
    pub(crate) fn collected_blocks(&self) -> Vec<StateBlock> {
        self.blocks().collect::<Result<_, _>>().unwrap()
    }
    pub(crate) fn retained(&self) -> &HistoricalBlocks {
        &self.archive.retained
    }
    pub(super) fn retained_storage(&self) -> Arc<HistoricalBlocks> {
        self.archive.retained.clone()
    }
    pub fn commitment(&self) -> BlockHash {
        self.commitment
    }
}

impl SnapshotTailPlan {
    pub fn context(&self) -> super::history::HistoryContext {
        self.context
    }
    pub fn heads(&self) -> &[HistoryHead] {
        &self.heads
    }
    pub fn witness_hashes(&self) -> &[BlockHash] {
        &self.required
    }

    pub fn finish(self, retained: Vec<HistoricalBlock>) -> Result<SnapshotTail, ReplayError> {
        self.finish_structure(retained)
    }

    fn read(
        base: Base<'_>,
        directory: &std::path::Path,
        cutoff: u64,
        requested: &[HistoryHead],
        mut get: impl FnMut(BlockHash) -> Result<Vec<u8>, ReplayError>,
        mut get_meta: impl FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>,
    ) -> Result<Self, ReplayError> {
        if cutoff < base.anchor.cutoff() || requested.is_empty() || requested.len() > MAX_HEADS {
            return Err(invalid("invalid snapshot tail bounds"));
        }
        let context = base.context;
        let accounts = &base.state.accounts;
        let mut heads = requested.to_vec();
        heads.sort_by(|a, b| a.account.as_str().cmp(b.account.as_str()));
        let mut seen_accounts = HashSet::new();
        let mut expected = 0u64;
        let mut encoding = b"BURST/confirmed-history/v1\0".to_vec();
        encoding.push(match context.network {
            NetworkId::Live => 0,
            NetworkId::Test => 1,
            NetworkId::Dev => 2,
        });
        encoding.extend_from_slice(context.genesis.as_bytes());
        encoding.extend_from_slice(&cutoff.to_be_bytes());
        encoding.extend_from_slice(&(heads.len() as u64).to_be_bytes());
        for head in &heads {
            let key = burst_crypto::decode_address(head.account.as_str())
                .ok_or_else(|| invalid("invalid tail account"))?;
            if head.height == 0 || head.hash.is_zero() || !seen_accounts.insert(&head.account) {
                return Err(invalid("duplicate or invalid tail head"));
            }
            let height = accounts
                .get(&head.account)
                .map_or(0, |account| account.block_count);
            expected = expected
                .checked_add(
                    head.height
                        .checked_sub(height)
                        .ok_or_else(|| invalid("tail rolls back snapshot account"))?,
                )
                .filter(|count| *count <= MAX_TAIL_BLOCKS as u64)
                .ok_or_else(|| invalid("snapshot tail exceeds block budget"))?;
            encoding.extend_from_slice(&key);
            encoding.extend_from_slice(&head.height.to_be_bytes());
            encoding.extend_from_slice(head.hash.as_bytes());
        }
        if accounts
            .values()
            .any(|account| account.block_count > 0 && !seen_accounts.contains(&account.address))
        {
            return Err(invalid("tail omits a snapshot account"));
        }
        let applied: HashSet<_> = accounts
            .values()
            .filter(|account| account.block_count > 0)
            .map(|account| account.head)
            .collect();
        let mut used_bytes = 0u64;
        let mut blocks = StagedBlocks::new(directory).map_err(invalid)?;
        let mut contents = Vec::new();
        for head in &heads {
            let account = accounts.get(&head.account);
            let base_height = account.map_or(0, |account| account.block_count);
            let base_head = account.map_or(BlockHash::ZERO, |account| account.head);
            burst_crypto::decode_address(head.account.as_str())
                .ok_or_else(|| invalid("invalid tail account"))?;
            let mut hash = head.hash;
            for offset in (base_height..head.height).rev() {
                let height = offset + 1;
                let bytes = get(hash)?;
                used_bytes = used_bytes
                    .checked_add(bytes.len() as u64)
                    .filter(|size| *size <= MAX_TAIL_BYTES as u64)
                    .ok_or_else(|| invalid("snapshot tail exceeds byte budget"))?;
                if bytes.is_empty() || bytes.len() > burst_types::MAX_P2P_MESSAGE_BYTES {
                    return Err(invalid("tail block exceeds wire limit"));
                }
                let block: StateBlock = bincode::DefaultOptions::new()
                    .with_fixint_encoding()
                    .reject_trailing_bytes()
                    .with_limit(bytes.len() as u64)
                    .deserialize(&bytes)
                    .map_err(invalid)?;
                block.validate_format().map_err(invalid)?;
                let key = crate::block_processor::signing_key(
                    &block,
                    &crate::genesis_key::genesis_address(context.network),
                )
                .map_err(invalid)?;
                if block.hash != hash
                    || block.compute_hash() != hash
                    || block.account != head.account
                    || block.payment_epoch != context.epoch
                    || block.timestamp.as_secs() < base.anchor.cutoff()
                    || block.timestamp.as_secs() > cutoff
                    || block.previous.is_zero() != (height == 1)
                    || (block.block_type == BlockType::Open) != (height == 1)
                    || !burst_crypto::verify_signature(hash.as_bytes(), &block.signature, &key)
                    || applied.contains(&hash)
                {
                    return Err(invalid("invalid signed snapshot tail block"));
                }
                let mut content_bytes = used_bytes as usize;
                if let Some(content) = crate::confirmed_history::content::read_bounded(
                    &block,
                    &mut content_bytes,
                    MAX_TAIL_BYTES,
                    &mut get_meta,
                )
                .map_err(invalid)?
                {
                    contents.push(content);
                }
                used_bytes = content_bytes as u64;
                hash = block.previous;
                blocks.insert(&block).map_err(invalid)?;
            }
            if hash != base_head {
                return Err(invalid("tail does not extend authenticated snapshot head"));
            }
        }
        let mut required = HashSet::new();
        let mut require = |hash| -> Result<(), ReplayError> {
            if !blocks.contains(&hash).map_err(invalid)? {
                required.insert(hash);
            }
            if required.len() > MAX_TAIL_BLOCKS {
                return Err(invalid(
                    "historical witness requests exceed tail block budget",
                ));
            }
            Ok(())
        };
        for hash in base.state.replay_dependencies.iter().copied() {
            require(hash)?;
        }
        for receipt in base.state.selection_receipts.iter() {
            for head in receipt?.heads() {
                require(head.hash)?;
            }
        }
        for hash in blocks.hashes() {
            let hash = hash.map_err(invalid)?;
            let block = blocks
                .get(&hash)
                .map_err(invalid)?
                .ok_or_else(|| invalid("staged tail block missing"))?;
            for dependency in block.dependencies() {
                if !blocks.contains(&dependency).map_err(invalid)? {
                    required.insert(dependency);
                }
            }
            if required.len() > MAX_TAIL_BLOCKS {
                return Err(invalid(
                    "historical witness requests exceed tail block budget",
                ));
            }
        }
        if required.len() > MAX_TAIL_BLOCKS {
            return Err(invalid(
                "historical witness requests exceed tail block budget",
            ));
        }
        let mut required: Vec<_> = required.into_iter().collect();
        required.sort_by_key(|hash| *hash.as_bytes());
        Ok(Self {
            directory: directory.to_owned(),
            context,
            cutoff,
            heads,
            blocks,
            contents,
            commitment: BlockHash::new(burst_crypto::blake2b_256(&encoding)),
            applied,
            required,
            used_bytes,
        })
    }

    fn finish_structure(self, retained: Vec<HistoricalBlock>) -> Result<SnapshotTail, ReplayError> {
        let retained = HistoricalBlocks::stage(&self.directory, retained).map_err(invalid)?;
        self.finish_staged(Arc::new(retained))
    }

    pub(super) fn finish_staged(
        mut self,
        retained: Arc<HistoricalBlocks>,
    ) -> Result<SnapshotTail, ReplayError> {
        if retained.len() > MAX_TAIL_BLOCKS {
            return Err(invalid("retained witnesses exceed tail block budget"));
        }
        let supplied: HashSet<_> = retained.hashes().collect();
        if self.required.iter().any(|hash| !supplied.contains(hash)) {
            return Err(invalid(
                "planned tail is missing a required historical witness",
            ));
        }
        let mut witnesses = HashSet::new();
        retained
            .try_visit(|witness| {
                if witness.context() != self.context
                    || self.blocks.contains(&witness.block().hash)?
                    || !witnesses.insert(witness.block().hash)
                {
                    return Err(StoreError::Corruption(
                        "duplicate or unrelated historical witness".into(),
                    ));
                }
                self.used_bytes = self
                    .used_bytes
                    .checked_add(
                        bincode::serialized_size(witness.block())
                            .map_err(|error| StoreError::Corruption(error.to_string()))?,
                    )
                    .filter(|size| *size <= MAX_TAIL_BYTES as u64)
                    .ok_or_else(|| {
                        StoreError::Corruption("retained witnesses exceed tail byte budget".into())
                    })?;
                self.applied.insert(witness.block().hash);
                Ok(())
            })
            .map_err(invalid)?;
        let ordered = crate::confirmed_history::causal_fallible_hash_order_after(
            self.blocks.hashes(),
            &self.applied,
            |hash| {
                let block = self
                    .blocks
                    .get(hash)?
                    .ok_or_else(|| StoreError::NotFound(format!("staged tail block {hash}")))?;
                Ok(block.dependencies().collect())
            },
        )
        .map_err(invalid)?;
        Ok(SnapshotTail {
            directory: self.directory,
            context: self.context,
            cutoff: self.cutoff,
            heads: self.heads,
            archive: TailArchive {
                blocks: Arc::new(self.blocks),
                order: ordered.into(),
                retained,
                contents: self.contents.into(),
            },
            commitment: self.commitment,
        })
    }
}

#[cfg(test)]
mod staged_witness_tests {
    use super::*;

    #[test]
    fn staged_tail_requires_historical_dependencies_even_without_suffix_blocks() {
        let directory = tempfile::tempdir().unwrap();
        let plan = SnapshotTailPlan {
            directory: directory.path().to_path_buf(),
            context: super::super::history::HistoryContext {
                network: NetworkId::Dev,
                genesis: BlockHash::new([1; 32]),
                epoch: BlockHash::new([2; 32]),
                state_hash: BlockHash::new([3; 32]),
            },
            cutoff: 1,
            heads: Vec::new(),
            blocks: StagedBlocks::new(directory.path()).unwrap(),
            contents: Vec::new(),
            commitment: BlockHash::ZERO,
            applied: HashSet::new(),
            required: vec![BlockHash::new([4; 32])],
            used_bytes: 0,
        };
        let witnesses = Arc::new(HistoricalBlocks::stage(directory.path(), Vec::new()).unwrap());
        let error = plan
            .finish_staged(witnesses)
            .err()
            .expect("missing witness must fail");
        assert!(error
            .to_string()
            .contains("missing a required historical witness"));
    }
}

impl AuthenticatedSnapshot {
    pub fn plan_tail(
        &self,
        store: &LmdbStore,
        cutoff: u64,
        heads: &[HistoryHead],
    ) -> Result<SnapshotTailPlan, ReplayError> {
        plan_for(self.base(), store, cutoff, heads)
    }
    /// Read only new account suffixes. Historic cross-account dependencies must
    /// be supplied as authenticated witnesses, not arbitrary database entries.
    /// Proposal bodies count against the same byte budget. This does not execute
    /// economics or write the store.
    pub fn read_tail(
        &self,
        store: &LmdbStore,
        cutoff: u64,
        heads: &[HistoryHead],
        retained: Vec<HistoricalBlock>,
    ) -> Result<SnapshotTail, ReplayError> {
        read_for(self.base(), store, cutoff, heads, retained)
    }

    /// Mandatory historic vote/challenge/governance ordering witnesses. Direct
    /// dependencies of new blocks and selection heads may require more witnesses.
    pub fn ordering_dependencies(&self) -> Vec<BlockHash> {
        let mut hashes: Vec<_> = self.state.replay_dependencies.iter().copied().collect();
        hashes.sort_by_key(|hash| *hash.as_bytes());
        hashes
    }

    /// Execute a same-epoch candidate and return its replay state without
    /// installing it in the database.
    pub fn replay_tail(
        self,
        tail: SnapshotTail,
        certificates: &[crate::checkpoint_store::CertifiedCheckpoint],
    ) -> Result<super::ConfirmedReplay, ReplayError> {
        self.into_continuation().replay_tail(tail, certificates)
    }
}

pub(super) fn read_for(
    base: Base<'_>,
    store: &LmdbStore,
    cutoff: u64,
    heads: &[HistoryHead],
    retained: Vec<HistoricalBlock>,
) -> Result<SnapshotTail, ReplayError> {
    plan_for(base, store, cutoff, heads)?.finish_structure(retained)
}

pub(super) fn plan_for(
    base: Base<'_>,
    store: &LmdbStore,
    cutoff: u64,
    heads: &[HistoryHead],
) -> Result<SnapshotTailPlan, ReplayError> {
    let txn = store.tx_begin_read().map_err(invalid)?;
    SnapshotTailPlan::read(
        base,
        store.env().path(),
        cutoff,
        heads,
        |hash| match store.block_store().get_block_txn(&txn, &hash) {
            Ok(bytes) => Ok(bytes),
            Err(StoreError::NotFound(_)) => store
                .meta_store()
                .get_meta_txn(&txn, &format!("checkpoint_history/v1/{hash}"))
                .map_err(invalid)?
                .ok_or_else(|| invalid(format!("missing tail block {hash}"))),
            Err(error) => Err(invalid(error)),
        },
        |key| store.meta_store().get_meta_txn(&txn, key),
    )
}

#[cfg(test)]
mod archive_tests {
    use super::*;

    #[test]
    fn signing_archive_shares_pinned_storage_and_propagates_read_failures() {
        let directory = tempfile::tempdir().unwrap();
        let block =
            crate::genesis_state::block(NetworkId::Dev, &burst_types::ProtocolParams::default());
        let bytes = bincode::serialize(&block).unwrap();
        let mut blocks = StagedBlocks::new(directory.path()).unwrap();
        blocks.insert(&block).unwrap();
        let archive = TailArchive {
            blocks: Arc::new(blocks),
            order: vec![block.hash].into(),
            retained: Arc::new(HistoricalBlocks::stage(directory.path(), []).unwrap()),
            contents: Vec::new().into(),
        };
        let shared = archive.clone();
        let input = crate::confirmed_replay::blocks::source::Source::Tail {
            archive: archive.clone(),
            directory: directory.path().to_owned(),
        };
        assert_eq!(
            input.hashes().collect::<Result<Vec<_>, _>>().unwrap(),
            vec![block.hash]
        );
        assert_eq!(
            bincode::serialize(input.get(&block.hash).unwrap().as_ref()).unwrap(),
            bytes
        );
        assert!(Arc::ptr_eq(&archive.blocks, &shared.blocks));
        drop(archive);
        let mut rows = shared.rows();
        assert_eq!(
            rows.next().unwrap().unwrap(),
            (
                format!("checkpoint_history/v1/{}", block.hash),
                bytes.clone()
            )
        );
        assert!(rows.next().is_none());
        assert!(rows.next().is_none());
        assert_eq!(shared.encoded(&block.hash).unwrap(), Some(bytes));
        assert!(shared.encoded(&BlockHash::ZERO).unwrap().is_none());
        let certified = HashSet::from([block.hash]);
        let applied = crate::confirmed_replay::blocks::AppliedHistory::from_certified_input(
            &input,
            &certified,
            Some(directory.path()),
        )
        .unwrap();
        shared.blocks.truncate_for_test();
        assert!(shared.rows().next().unwrap().is_err());
        assert!(shared.encoded(&block.hash).is_err());
        assert!(applied.get(&block.hash).is_err());
        assert!(applied.contains(&block.hash).is_err());
        assert!(applied.encoded(&block.hash).is_err());
        assert!(input.visit_unordered(|_| Ok::<_, StoreError>(())).is_err());
        assert!(input.get(&block.hash).is_err());
        assert!(input.iter().next().unwrap().is_err());
        assert!(crate::confirmed_replay::governance::streamed_dependencies(&input).is_err());
    }
}
