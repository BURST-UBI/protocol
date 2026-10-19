//! Recover historical references by following hashes back from authenticated
//! snapshot heads. This proves membership, not permission to execute a block.

use super::ReplayError;
use bincode::Options;
use burst_consensus::checkpoint::CheckpointAnchor;
use burst_ledger::{BlockType, StateBlock};
use burst_store::{account::AccountInfo, StoreError};
use burst_store_lmdb::LmdbStore;
use burst_types::{BlockHash, NetworkId, Signature, WalletAddress};

pub const MAX_WITNESS_BLOCKS: usize = 128;
pub const MAX_WITNESS_BYTES: usize = 1024 * 1024;

mod staged;
pub(crate) use staged::HistoricalBlocks;

fn invalid(error: impl ToString) -> ReplayError {
    ReplayError::Invalid(error.to_string())
}

/// Every recovered reference remains bound to the snapshot that authenticated
/// its account head. This context is not itself a serializable trust token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryContext {
    pub network: NetworkId,
    pub genesis: BlockHash,
    pub epoch: BlockHash,
    pub state_hash: BlockHash,
}

/// Signable block contents proven to occur in one snapshot's account prefix.
/// Work is excluded from the block hash (decision 4.9), so membership does not
/// authenticate the received work nonce or recheck historical work thresholds.
/// New admission must still use its normal work and economic checks.
#[derive(Clone)]
pub struct HistoricalBlock {
    context: HistoryContext,
    height: u64,
    block: StateBlock,
}

impl HistoricalBlock {
    pub fn context(&self) -> HistoryContext {
        self.context
    }
    pub fn height(&self) -> u64 {
        self.height
    }
    pub fn block(&self) -> &StateBlock {
        &self.block
    }
    pub(crate) fn into_block(self) -> StateBlock {
        self.block
    }
}

/// An in-memory walk of a single authenticated account prefix. It keeps only
/// the next hash/height, not all previously visited blocks. Returned witnesses
/// may be retained selectively by a future compact replay base. Reopening a
/// cursor must start from an authenticated snapshot, never unsigned peer state.
#[derive(Clone)]
pub struct HistoryCursor {
    context: HistoryContext,
    account: WalletAddress,
    cutoff: u64,
    next: BlockHash,
    height: u64,
}

impl HistoryCursor {
    /// Certificate-bound account prefixes. These prove membership only; callers
    /// must still replay every new block before installing the certified state.
    pub(crate) fn certified_cut(
        parent: &CheckpointAnchor,
        certificate: &burst_consensus::checkpoint::CheckpointCertificate,
        heads: &[crate::confirmed_history::HistoryHead],
    ) -> Result<Vec<Self>, ReplayError> {
        parent.verify_certificate(certificate).map_err(invalid)?;
        let cut = &certificate.checkpoint;
        if crate::confirmed_history::head_commitment(cut.network, cut.genesis, cut.cutoff, heads)
            .map_err(invalid)?
            != cut.ledger_root
        {
            return Err(invalid(
                "transferred heads differ from certified ledger root",
            ));
        }
        heads
            .iter()
            .map(|head| {
                burst_crypto::decode_address(head.account.as_str())
                    .ok_or_else(|| invalid("invalid certified account address"))?;
                Ok(Self {
                    context: HistoryContext {
                        network: cut.network,
                        genesis: cut.genesis,
                        epoch: cut.hash(),
                        state_hash: cut.ledger_root,
                    },
                    account: head.account.clone(),
                    cutoff: cut.cutoff,
                    next: head.hash,
                    height: head.height,
                })
            })
            .collect()
    }

    pub(super) fn seek_proven(&mut self, proof: &HistoricalBlock) -> Result<(), ReplayError> {
        if proof.context != self.context
            || proof.block.account != self.account
            || proof.height == 0
            || proof.height > self.height
        {
            return Err(invalid("historical proof does not belong to this cursor"));
        }
        self.next = proof.block.hash;
        self.height = proof.height;
        Ok(())
    }

    pub(super) fn new(
        anchor: &CheckpointAnchor,
        state_hash: BlockHash,
        address: &WalletAddress,
        account: Option<&AccountInfo>,
    ) -> Result<Option<Self>, ReplayError> {
        let Some(account) = account else {
            return Ok(None);
        };
        if account.address != *address
            || account.confirmation_height != account.block_count
            || account.head.is_zero() != (account.block_count == 0)
        {
            return Err(invalid("snapshot account does not name a certified prefix"));
        }
        if account.block_count == 0 {
            return Ok(None);
        }
        burst_crypto::decode_address(address.as_str())
            .ok_or_else(|| invalid("invalid snapshot account address"))?;
        Ok(Some(Self {
            context: HistoryContext {
                network: anchor.network(),
                genesis: anchor.genesis_hash(),
                epoch: anchor.hash(),
                state_hash,
            },
            account: address.clone(),
            cutoff: anchor.cutoff(),
            next: account.head,
            height: account.block_count,
        }))
    }

    pub fn context(&self) -> HistoryContext {
        self.context
    }

    /// The next request is derived from verified bytes, not an untrusted index.
    pub fn next(&self) -> Option<crate::confirmed_history::HistoryHead> {
        (self.height > 0).then(|| crate::confirmed_history::HistoryHead {
            account: self.account.clone(),
            height: self.height,
            hash: self.next,
        })
    }

    /// Resume from opaque witnesses produced by a copy of this authenticated
    /// cursor. Their private bodies have already passed `accept`; check the
    /// complete position/context sequence before changing this cursor.
    pub(crate) fn resume_suffix(
        &mut self,
        proofs: Vec<HistoricalBlock>,
    ) -> Result<Vec<StateBlock>, ReplayError> {
        if proofs.is_empty() || proofs.len() > MAX_WITNESS_BLOCKS {
            return Err(invalid("invalid suffix resume batch"));
        }
        let mut next = self.next;
        let mut height = self.height;
        for proof in &proofs {
            if height == 0
                || proof.context != self.context
                || proof.height != height
                || proof.block.account != self.account
                || proof.block.hash != next
                || proof.block.timestamp.as_secs() > self.cutoff
            {
                return Err(invalid(
                    "suffix witnesses do not continue authenticated cursor",
                ));
            }
            next = proof.block.previous;
            height -= 1;
        }
        self.next = next;
        self.height = height;
        Ok(proofs.into_iter().map(|proof| proof.block).collect())
    }

    /// Accept newest-to-oldest bytes atomically. A missing, reordered, forged,
    /// oversized or malformed chunk cannot advance the cursor, even partially.
    pub fn accept(&mut self, bytes: &[Vec<u8>]) -> Result<Vec<HistoricalBlock>, ReplayError> {
        if bytes.is_empty() || bytes.len() > MAX_WITNESS_BLOCKS || bytes.len() as u64 > self.height
        {
            return Err(invalid("invalid historical witness block count"));
        }
        let mut size = 0usize;
        for block in bytes {
            size = size
                .checked_add(block.len())
                .filter(|size| *size <= MAX_WITNESS_BYTES)
                .ok_or_else(|| invalid("historical witness exceeds byte budget"))?;
            if block.is_empty() || block.len() > burst_types::MAX_P2P_MESSAGE_BYTES {
                return Err(invalid("historical witness block exceeds wire limit"));
            }
        }
        let mut next = self.next;
        let mut height = self.height;
        let mut accepted = Vec::with_capacity(bytes.len());
        for bytes in bytes {
            let block: StateBlock = bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .reject_trailing_bytes()
                .with_limit(burst_types::MAX_P2P_MESSAGE_BYTES as u64)
                .deserialize(bytes)
                .map_err(invalid)?;
            block.validate_format().map_err(invalid)?;
            if block.hash != next
                || block.compute_hash() != next
                || block.account != self.account
                || block.timestamp.as_secs() > self.cutoff
                || block.previous.is_zero() != (height == 1)
                || (block.block_type == BlockType::Open) != (height == 1)
            {
                return Err(invalid(
                    "historical witness is not the requested certified ancestor",
                ));
            }
            if block.hash == self.context.genesis {
                if height != 1
                    || block.account != crate::genesis_key::genesis_address(self.context.network)
                    || block.signature != Signature([0; 64])
                    || block.work != 0
                {
                    return Err(invalid("invalid genesis witness"));
                }
            } else if block.payment_epoch.is_zero()
                || !burst_crypto::verify_signature(
                    block.hash.as_bytes(),
                    &block.signature,
                    &crate::block_processor::signing_key(
                        &block,
                        &crate::genesis_key::genesis_address(self.context.network),
                    )
                    .map_err(invalid)?,
                )
            {
                return Err(invalid(
                    "invalid historical witness signature or payment context",
                ));
            }
            next = block.previous;
            accepted.push(HistoricalBlock {
                context: self.context,
                height,
                block,
            });
            height -= 1;
        }
        self.next = next;
        self.height = height;
        Ok(accepted)
    }
}

/// Serve a bounded raw ancestry chunk. These bytes are not authenticated by the
/// serving node's indexes; the receiver must pass them through its own cursor.
/// A pruned server may return an available prefix for continuation on another
/// peer. No bytes are returned when the initial hash is unavailable.
pub fn read_chunk(
    store: &LmdbStore,
    start: BlockHash,
    limit: usize,
) -> Result<Vec<Vec<u8>>, ReplayError> {
    if start.is_zero() || limit == 0 || limit > MAX_WITNESS_BLOCKS {
        return Err(invalid("invalid historical witness request"));
    }
    let txn = store.tx_begin_read().map_err(invalid)?;
    let blocks = store.block_store();
    let metadata = store.meta_store();
    let mut hash = start;
    let mut used = 0usize;
    let mut result = Vec::new();
    while !hash.is_zero() && result.len() < limit {
        let bytes = match blocks.get_block_txn(&txn, &hash) {
            Ok(bytes) => Some(bytes),
            Err(StoreError::NotFound(_)) => metadata
                .get_meta_txn(&txn, &format!("checkpoint_history/v1/{hash}"))
                .map_err(invalid)?,
            Err(error) => return Err(invalid(error)),
        };
        let Some(bytes) = bytes else {
            if result.is_empty() {
                return Err(invalid("historical witness is unavailable"));
            }
            break;
        };
        if bytes.is_empty() || bytes.len() > burst_types::MAX_P2P_MESSAGE_BYTES {
            return Err(invalid("stored historical block exceeds wire limit"));
        }
        let next_size = used
            .checked_add(bytes.len())
            .ok_or_else(|| invalid("historical witness size overflow"))?;
        if next_size > MAX_WITNESS_BYTES {
            break;
        }
        let block: StateBlock = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .with_limit(bytes.len() as u64)
            .deserialize(&bytes)
            .map_err(invalid)?;
        if block.hash != hash || block.compute_hash() != hash {
            return Err(invalid("stored historical block hash mismatch"));
        }
        hash = block.previous;
        used = next_size;
        result.push(bytes);
    }
    Ok(result)
}
