//! Local before/after images for one block's writer transaction.
//!
//! These records are not peer proofs or permission to roll back. Branch
//! installation must check finality, dependency closure and current values,
//! then publish rebuilt engines only after the replacement commits.

use bincode::Options;
use burst_store::StoreError;
use burst_types::{BlockHash, WalletAddress};
use heed::{types::Bytes, Database};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::LmdbEnvironment;

pub(crate) const PREFIX: &[u8] = b"block_undo/v1/";
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_CHANGES: usize = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum UndoTable {
    Blocks,
    Heights,
    BlockHeights,
    Frontiers,
    Accounts,
    Transactions,
    AccountTransactions,
    Pending,
    Meta,
    BrnWallets,
    BrnMeta,
    TrstOrigins,
    TrstExpiry,
    TrstReverse,
}

impl UndoTable {
    pub(crate) fn database(self, env: &LmdbEnvironment) -> Database<Bytes, Bytes> {
        match self {
            Self::Blocks => env.blocks_db,
            Self::Heights => env.height_db,
            Self::BlockHeights => env.block_height_db,
            Self::Frontiers => env.frontiers_db,
            Self::Accounts => env.accounts_db,
            Self::Transactions => env.transactions_db,
            Self::AccountTransactions => env.account_txs_db,
            Self::Pending => env.pending_db,
            Self::Meta => env.meta_db,
            Self::BrnWallets => env.brn_wallets_db,
            Self::BrnMeta => env.brn_meta_db,
            Self::TrstOrigins => env.trst_origin_db,
            Self::TrstExpiry => env.trst_expiry_db,
            Self::TrstReverse => env.trst_reverse_db,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoChange {
    pub table: UndoTable,
    pub key: Vec<u8>,
    pub before: Option<Vec<u8>>,
    pub after: Option<Vec<u8>>,
}

impl UndoChange {
    /// Permit the transition layer to reconcile aggregate economic and lifecycle
    /// snapshots. Primary ledger values and consensus journals never use this
    /// hook. The callback must reject overlapping changes and return the
    /// restored snapshot; an error leaves this change unmodified.
    pub fn match_current_with_snapshots(
        &mut self,
        current: Option<Vec<u8>>,
        reconcile: &mut impl FnMut(&[u8], &[u8], &[u8], &[u8]) -> Result<Vec<u8>, StoreError>,
    ) -> Result<(), StoreError> {
        if current != self.after
            && (matches!(
                (self.table, self.key.as_slice()),
                (
                    UndoTable::Meta,
                    b"trst_engine_wallets"
                        | b"merger_graph"
                        | b"verification_orchestrator"
                        | b"delegation_engine_state"
                        | b"consti_engine"
                ) | (UndoTable::BrnMeta, b"governance_engine")
            ))
        {
            let (Some(before), Some(after), Some(live)) = (&self.before, &self.after, &current)
            else {
                return Err(invalid("undo requires complete economic snapshots"));
            };
            let restored = reconcile(&self.key, before, after, live)?;
            if restored.len() > MAX_BYTES {
                return Err(invalid("restored snapshot exceeds size limit"));
            }
            self.before = Some(restored);
            self.after = current;
            return Ok(());
        }
        self.match_current(current)
    }

    /// Match the writer's current value. Cementation may advance an account's
    /// confirmation height without changing its provisional head; preserve that
    /// advance when restoring an older account image.
    pub fn match_current(&mut self, current: Option<Vec<u8>>) -> Result<(), StoreError> {
        if current == self.after {
            return Ok(());
        }
        if self.table != UndoTable::Accounts {
            return Err(invalid("undo conflicts with current state"));
        }
        let decode =
            |bytes: Option<&[u8]>| -> Result<burst_store::account::AccountInfo, StoreError> {
                bincode::deserialize(bytes.ok_or_else(|| invalid("undo account disappeared"))?)
                    .map_err(|e| StoreError::Serialization(e.to_string()))
            };
        let mut live = decode(current.as_deref())?;
        let expected = decode(self.after.as_deref())?;
        let mut previous = decode(self.before.as_deref())?;
        let confirmed = live.confirmation_height;
        if confirmed < expected.confirmation_height || confirmed > previous.block_count {
            return Err(invalid("undo crosses an advanced confirmation height"));
        }
        live.confirmation_height = expected.confirmation_height;
        if bincode::serialize(&live).map_err(|e| StoreError::Serialization(e.to_string()))?
            != self.after.as_ref().unwrap().as_slice()
        {
            return Err(invalid("undo conflicts with a changed account"));
        }
        previous.confirmation_height = confirmed;
        self.before = Some(
            bincode::serialize(&previous).map_err(|e| StoreError::Serialization(e.to_string()))?,
        );
        self.after = current;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockUndo {
    pub block: BlockHash,
    pub account: WalletAddress,
    pub height: u64,
    pub changes: Vec<UndoChange>,
}

pub(crate) fn key(hash: &BlockHash) -> Vec<u8> {
    let mut key = PREFIX.to_vec();
    key.extend_from_slice(hash.as_bytes());
    key
}

fn invalid(message: &str) -> StoreError {
    StoreError::Corruption(message.into())
}

fn irreversible_meta(key: &[u8]) -> bool {
    [
        b"trst_state_format".as_slice(),
        b"trst_graph_format".as_slice(),
        b"block_undo/".as_slice(),
        b"consensus_final/",
        b"consensus_sequence/",
        b"consensus_fence/",
        b"consensus_time/",
        b"consensus_governance/",
        b"payment_epoch/",
        b"checkpoint_vote/",
        b"checkpoint_round/",
        b"checkpoint_chain/",
        b"checkpoint_record_archive/",
        b"checkpoint_orphan/",
        b"checkpoint_history/",
        b"checkpoint_installed/",
        b"checkpoint_snapshot/",
        b"snapshot_attestation/",
        b"genesis_params/",
        b"pending_confirmation/",
        b"fork_candidate/",
        b"fork_content/",
        b"confirmed_authority/",
        b"confirmed_governance/",
    ]
    .iter()
    .any(|prefix| key.starts_with(prefix))
}

impl BlockUndo {
    pub(crate) fn decode(hash: &BlockHash, bytes: &[u8]) -> Result<Self, StoreError> {
        if bytes.len() > MAX_BYTES {
            return Err(invalid("oversized block undo record"));
        }
        let record: Self = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(MAX_BYTES as u64)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
        if record.block != *hash
            || hash.is_zero()
            || record.height == 0
            || record.changes.is_empty()
            || record.changes.len() > MAX_CHANGES
        {
            return Err(invalid("invalid block undo identity or count"));
        }
        let mut previous = None;
        for change in &record.changes {
            let position = (change.table, change.key.as_slice());
            if previous.is_some_and(|old| old >= position)
                || change.key.is_empty()
                || change.before == change.after
                || (change.table == UndoTable::Meta && irreversible_meta(&change.key))
            {
                return Err(invalid("noncanonical block undo changes"));
            }
            previous = Some(position);
        }
        Ok(record)
    }
}

pub(crate) struct UndoRecorder {
    block: BlockHash,
    account: WalletAddress,
    height: u64,
    changes: BTreeMap<(UndoTable, Vec<u8>), UndoChange>,
    bytes: usize,
}

impl UndoRecorder {
    pub(crate) fn matches(&self, block: &BlockHash, account: &WalletAddress, height: u64) -> bool {
        self.block == *block && self.account == *account && self.height == height
    }

    pub(crate) fn new(block: BlockHash, account: WalletAddress, height: u64) -> Self {
        Self {
            block,
            account,
            height,
            changes: BTreeMap::new(),
            bytes: 0,
        }
    }

    pub(crate) fn record(
        &mut self,
        table: UndoTable,
        key: &[u8],
        before: Option<Vec<u8>>,
        after: Option<&[u8]>,
    ) -> Result<(), StoreError> {
        if table == UndoTable::Meta && irreversible_meta(key) {
            return Err(invalid(
                "undo records cannot modify journals or irreversible consensus state",
            ));
        }
        let position = (table, key.to_vec());
        let change = UndoChange {
            table,
            key: key.to_vec(),
            before: self
                .changes
                .get(&position)
                .map_or(before, |old| old.before.clone()),
            after: after.map(<[u8]>::to_vec),
        };
        let size = |c: &UndoChange| {
            c.key.len()
                + c.before.as_ref().map_or(0, Vec::len)
                + c.after.as_ref().map_or(0, Vec::len)
                + 64
        };
        let old_size = self.changes.get(&position).map_or(0, size);
        let new_size = if change.before == change.after {
            0
        } else {
            size(&change)
        };
        let bytes = self
            .bytes
            .checked_sub(old_size)
            .and_then(|b| b.checked_add(new_size))
            .ok_or_else(|| invalid("block undo size overflow"))?;
        if bytes > MAX_BYTES - 1024
            || (new_size != 0
                && !self.changes.contains_key(&position)
                && self.changes.len() >= MAX_CHANGES)
        {
            return Err(invalid("block undo requires incremental economic storage"));
        }
        self.bytes = bytes;
        if new_size == 0 {
            self.changes.remove(&position);
        } else {
            self.changes.insert(position, change);
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<(Vec<u8>, Vec<u8>), StoreError> {
        let key = key(&self.block);
        let record = BlockUndo {
            block: self.block,
            account: self.account,
            height: self.height,
            changes: self.changes.into_values().collect(),
        };
        let bytes =
            bincode::serialize(&record).map_err(|e| StoreError::Serialization(e.to_string()))?;
        BlockUndo::decode(&record.block, &bytes)?;
        Ok((key, bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_reconciliation_cannot_rewrite_primary_or_consensus_values() {
        for (table, key) in [
            (UndoTable::Accounts, b"trst_engine_wallets".as_slice()),
            (UndoTable::Blocks, b"merger_graph"),
            (UndoTable::BrnMeta, b"verification_orchestrator"),
            (UndoTable::Meta, b"governance_engine"),
            (UndoTable::BrnMeta, b"protocol_params"),
            (UndoTable::BrnMeta, b"consti_engine"),
            (UndoTable::BrnMeta, b"delegation_engine_state"),
            (UndoTable::Meta, b"consensus_final/v1/a"),
            (UndoTable::Meta, b"another_snapshot"),
        ] {
            let mut change = UndoChange {
                table,
                key: key.to_vec(),
                before: Some(vec![1]),
                after: Some(vec![2]),
            };
            let mut called = false;
            assert!(change
                .match_current_with_snapshots(Some(vec![3]), &mut |_, _, _, _| {
                    called = true;
                    Ok(vec![4])
                })
                .is_err());
            assert!(!called);
        }
    }

    #[test]
    fn snapshot_reconciliation_requires_complete_images_and_is_atomic_on_error() {
        let change = UndoChange {
            table: UndoTable::Meta,
            key: b"trst_engine_wallets".to_vec(),
            before: Some(vec![1]),
            after: Some(vec![2]),
        };
        for missing in 0..3 {
            let mut candidate = change.clone();
            if missing == 0 {
                candidate.before = None;
            }
            if missing == 1 {
                candidate.after = None;
            }
            let current = (missing != 2).then_some(vec![3]);
            let original = candidate.clone();
            assert!(candidate
                .match_current_with_snapshots(current, &mut |_, _, _, _| panic!(
                    "incomplete snapshot"
                ))
                .is_err());
            assert_eq!(candidate, original);
        }
        let mut candidate = change.clone();
        assert!(candidate
            .match_current_with_snapshots(Some(vec![3]), &mut |_, _, _, _| Err(invalid("overlap")))
            .is_err());
        assert_eq!(candidate, change);
        candidate
            .match_current_with_snapshots(Some(vec![3]), &mut |key, before, after, live| {
                assert_eq!(
                    (key, before, after, live),
                    (
                        b"trst_engine_wallets".as_slice(),
                        &[1][..],
                        &[2][..],
                        &[3][..]
                    )
                );
                Ok(vec![4])
            })
            .unwrap();
        assert_eq!(candidate.before, Some(vec![4]));
        assert_eq!(candidate.after, Some(vec![3]));
    }

    fn record() -> BlockUndo {
        let mut recorder =
            UndoRecorder::new(BlockHash::new([1; 32]), WalletAddress::new("brst_test"), 1);
        recorder
            .record(UndoTable::Meta, b"engine", Some(vec![1]), Some(&[2]))
            .unwrap();
        recorder
            .record(UndoTable::Blocks, &[1; 32], None, Some(b"block"))
            .unwrap();
        let (_, bytes) = recorder.finish().unwrap();
        BlockUndo::decode(&BlockHash::new([1; 32]), &bytes).unwrap()
    }

    #[test]
    fn rejects_corrupt_ambiguous_and_wrong_block_records() {
        let record = record();
        let bytes = bincode::serialize(&record).unwrap();
        for length in 0..bytes.len() {
            assert!(BlockUndo::decode(&record.block, &bytes[..length]).is_err());
        }
        let mut extra = bytes.clone();
        extra.push(0);
        assert!(BlockUndo::decode(&record.block, &extra).is_err());
        assert!(BlockUndo::decode(&BlockHash::new([2; 32]), &bytes).is_err());
        for variant in 0..6 {
            let mut bad = record.clone();
            match variant {
                0 => bad.height = 0,
                1 => bad.changes.push(bad.changes[0].clone()),
                2 => bad.changes.reverse(),
                3 => bad.changes[0].after = bad.changes[0].before.clone(),
                4 => bad.changes[0].key.clear(),
                _ => {
                    bad.changes[1].table = UndoTable::Meta;
                    bad.changes[1].key = key(&record.block);
                }
            }
            assert!(BlockUndo::decode(&bad.block, &bincode::serialize(&bad).unwrap()).is_err());
        }
    }

    #[test]
    fn recording_limit_and_repeated_keys_preserve_the_first_before_image() {
        let mut recorder =
            UndoRecorder::new(BlockHash::new([1; 32]), WalletAddress::new("brst_test"), 1);
        recorder
            .record(UndoTable::Meta, b"engine", Some(vec![1]), Some(&[2]))
            .unwrap();
        recorder
            .record(UndoTable::Meta, b"engine", Some(vec![2]), Some(&[3]))
            .unwrap();
        let change = recorder.changes.values().next().unwrap();
        assert_eq!(change.before, Some(vec![1]));
        assert_eq!(change.after, Some(vec![3]));
        recorder
            .record(UndoTable::Meta, b"engine", Some(vec![3]), Some(&[1]))
            .unwrap();
        assert!(recorder.changes.is_empty());
        assert_eq!(recorder.bytes, 0);
        recorder.bytes = MAX_BYTES;
        assert!(recorder
            .record(UndoTable::Meta, b"other", None, Some(&[1]))
            .is_err());
        assert!(recorder.changes.is_empty());
    }

    #[test]
    fn consensus_choices_sequences_and_handoffs_are_never_reversible() {
        for key in [
            b"trst_state_format".as_slice(),
            b"trst_graph_format".as_slice(),
            b"consensus_final/v1/a".as_slice(),
            b"consensus_sequence/v1/a",
            b"consensus_fence/v1/a",
            b"consensus_time/v1/a",
            b"consensus_governance/v1/a",
            b"payment_epoch/v1",
            b"payment_epoch/v1/counting_limit",
            b"confirmed_governance/v1",
            b"checkpoint_vote/v1/a",
            b"checkpoint_round/v1/a",
            b"checkpoint_chain/v1/a",
            b"checkpoint_record_archive/v1/a",
            b"snapshot_attestation/v1/dev/genesis/epoch/signer",
            b"genesis_params/v1",
            b"pending_confirmation/v1/a",
            b"block_undo/v1/a",
        ] {
            let mut recorder =
                UndoRecorder::new(BlockHash::new([1; 32]), WalletAddress::new("brst_test"), 1);
            assert!(recorder
                .record(UndoTable::Meta, key, None, Some(&[1]))
                .is_err());
            let mut corrupted = record();
            corrupted.changes[1].key = key.to_vec();
            assert!(
                BlockUndo::decode(&corrupted.block, &bincode::serialize(&corrupted).unwrap())
                    .is_err()
            );
        }
    }
}
