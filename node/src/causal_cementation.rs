//! Cement a dependency-closed ledger cut in one caller-owned transaction.

use std::collections::HashSet;

use burst_ledger::StateBlock;
use burst_store::StoreError;
use burst_store_lmdb::{LmdbStore, LmdbWriteTransaction};
use burst_types::BlockHash;

const MAX_DEPENDENCIES: usize = 131_072;
const MAX_CEMENTED_PER_BATCH: usize = 4096;

#[derive(Debug)]
pub(crate) struct CementedBatch {
    pub hashes: Vec<BlockHash>,
    pub complete: bool,
}

fn load(
    batch: &LmdbWriteTransaction<'_>,
    hash: &BlockHash,
) -> Result<(StateBlock, u64, u64), StoreError> {
    let bytes = batch.get_block(hash)?;
    let block: StateBlock = bincode::deserialize(&bytes)
        .map_err(|error| StoreError::Serialization(error.to_string()))?;
    if block.hash != *hash || block.compute_hash() != *hash {
        return Err(StoreError::Corruption(
            "cementation candidate hash mismatch".into(),
        ));
    }
    let height = batch.block_height(&block.account, hash)?;
    let account = batch.get_account(&block.account)?;
    if account.address != block.account || height == 0 || height > account.block_count {
        return Err(StoreError::Corruption(
            "cementation account index mismatch".into(),
        ));
    }
    Ok((block, height, account.confirmation_height))
}

pub(crate) fn dependencies_confirmed(
    batch: &LmdbWriteTransaction<'_>,
    block: &StateBlock,
) -> Result<bool, StoreError> {
    let mut dependencies: HashSet<_> = block.dependencies().collect();
    dependencies.extend(crate::verification_commit::dependencies::voting_sources(
        batch, block,
    )?);
    let mut ready = true;
    for dependency in dependencies {
        let (_, height, confirmed) = load(batch, &dependency)?;
        ready &= height <= confirmed;
    }
    Ok(ready)
}

pub(crate) fn handoff(
    batch: &mut LmdbWriteTransaction<'_>,
    target: BlockHash,
) -> Result<bool, StoreError> {
    let (block, _, _) = load(batch, &target)?;
    if !dependencies_confirmed(batch, &block)? {
        return Ok(false);
    }
    batch.handoff_confirmation(&target)?;
    Ok(true)
}

pub(crate) fn cement(store: &LmdbStore, target: BlockHash) -> Result<CementedBatch, StoreError> {
    let mut batch = store.tx_begin_write()?;
    let mut authority = crate::confirmed_authority::ConfirmedAuthority::load(&batch)?;
    let mut governance = None;
    let mut stack = vec![(target, false)];
    let mut visiting = HashSet::new();
    let mut hashes = Vec::new();
    let mut examined = 0;
    while let Some((hash, dependencies_visited)) = stack.pop() {
        let (block, height, confirmed) = load(&batch, &hash)?;
        if height <= confirmed {
            visiting.remove(&hash);
            continue;
        }
        if dependencies_visited {
            // No holes, including when a dependency shares this account chain.
            if confirmed.checked_add(1) != Some(height) {
                return Err(StoreError::Corruption("non-contiguous cementation".into()));
            }
            authority.promote(&batch, hash)?;
            if crate::confirmed_governance::ConfirmedGovernance::applies_to(&block.block_type) {
                if governance.is_none() {
                    governance = crate::confirmed_governance::ConfirmedGovernance::load(&batch)?;
                }
                if let Some(governance) = &mut governance {
                    governance.promote(&batch, &block)?;
                }
            }
            batch.advance_confirmation_height(&block.account, height)?;
            batch.prune_cemented_undo(&hash)?;
            visiting.remove(&hash);
            hashes.push(hash);
            if hashes.len() == MAX_CEMENTED_PER_BATCH {
                break;
            }
            continue;
        }
        examined += 1;
        if examined > MAX_DEPENDENCIES || !visiting.insert(hash) {
            return Err(StoreError::Corruption(
                "cyclic or oversized cementation dependency graph".into(),
            ));
        }
        if height == 1 {
            if !block.previous.is_zero() {
                return Err(StoreError::Corruption(
                    "open position has a predecessor".into(),
                ));
            }
        } else {
            let (previous, previous_height, _) = load(&batch, &block.previous)?;
            if previous.account != block.account || previous_height.checked_add(1) != Some(height) {
                return Err(StoreError::Corruption(
                    "cementation predecessor mismatch".into(),
                ));
            }
        }
        stack.push((hash, true));
        let mut dependencies: Vec<_> = block.dependencies().collect();
        let implicit = crate::verification_commit::dependencies::read(&batch, &block)?;
        let mut waiting_for_quorum = false;
        for dependency in &implicit {
            if dependencies.contains(dependency) {
                continue;
            }
            let (_, height, confirmed) = load(&batch, dependency)?;
            // The target's signature does not commit local-only dependencies.
            // Its quorum cannot choose a fork for those independent accounts.
            if height > confirmed && !batch.confirmation_pending(dependency)? {
                waiting_for_quorum = true;
            }
        }
        if waiting_for_quorum {
            return Ok(CementedBatch {
                hashes: Vec::new(),
                complete: false,
            });
        }
        dependencies.extend(implicit);
        for dependency in dependencies.into_iter().rev() {
            stack.push((dependency, false));
        }
    }
    let (_, target_height, confirmed) = load(&batch, &target)?;
    let complete = target_height <= confirmed;
    if complete {
        batch.acknowledge_confirmation(&target)?;
    }
    authority.persist(&mut batch)?;
    if let Some(governance) = governance {
        governance.persist(&mut batch)?;
    }
    batch.commit()?;
    Ok(CementedBatch { hashes, complete })
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_ledger::BlockType;
    use burst_store::{account::AccountStore, block::BlockStore};
    use burst_types::{Signature, Timestamp, TxHash, WalletAddress};

    fn block(account: &str, kind: BlockType, previous: BlockHash, link: BlockHash) -> StateBlock {
        let account = WalletAddress::new(format!("brst_{account}"));
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: burst_ledger::CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: kind,
            account: account.clone(),
            representative: account,
            previous,
            brn_balance: 0,
            trst_balance: 0,
            link,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: vec![],
            release_sources: Vec::new(),
            timestamp: Timestamp::new(100),
            params_hash: BlockHash::ZERO,
            work: 0,
            signature: Signature([0; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block
    }

    fn save(store: &LmdbStore, block: &StateBlock, height: u64) {
        let first = store.block_store().block_count().unwrap() == 0;
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .begin_block_undo(block.hash, block.account.clone(), height)
            .unwrap();
        let mut account =
            crate::genesis_state::account_to_write(None, block.account.clone(), block.hash)
                .unwrap();
        account.block_count = height;
        batch.put_account_info(&account, false).unwrap();
        batch
            .put_block_with_account(
                &block.hash,
                &bincode::serialize(block).unwrap(),
                &block.account,
                height,
            )
            .unwrap();
        batch.commit().unwrap();
        if first {
            let mut batch = store.tx_begin_write().unwrap();
            crate::confirmed_authority::ConfirmedAuthority::initialize(&mut batch, &account)
                .unwrap();
            batch.commit().unwrap();
        }
    }

    #[test]
    fn handoff_waits_for_dependency_finality_without_pinning_an_unconfirmed_parent() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 32 * 1024 * 1024).unwrap();
        let open = block("alice", BlockType::Open, BlockHash::ZERO, BlockHash::ZERO);
        let send = block("alice", BlockType::Send, open.hash, BlockHash::new([9; 32]));
        save(&store, &open, 1);
        save(&store, &send, 2);
        for queue_parent in [false, true] {
            if queue_parent {
                store.meta_store().enqueue_confirmation(&open.hash).unwrap();
            }
            let mut batch = store.tx_begin_write().unwrap();
            assert!(!handoff(&mut batch, send.hash).unwrap());
            assert!(!batch.confirmation_pending(&send.hash).unwrap());
            batch.commit().unwrap();
        }
        cement(&store, open.hash).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert!(handoff(&mut batch, send.hash).unwrap());
        batch.commit().unwrap();
        assert_eq!(
            store.meta_store().pending_confirmations().unwrap(),
            vec![send.hash]
        );
        assert!(cement(&store, send.hash).unwrap().complete);
    }

    #[test]
    fn undo_cannot_be_pruned_before_finality_or_by_an_aborted_cementation() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 32 * 1024 * 1024).unwrap();
        let open = block("alice", BlockType::Open, BlockHash::ZERO, BlockHash::ZERO);
        save(&store, &open, 1);
        let mut batch = store.tx_begin_write().unwrap();
        assert!(batch.prune_cemented_undo(&open.hash).is_err());
        assert!(batch.commit().is_err());
        {
            let mut batch = store.tx_begin_write().unwrap();
            batch.advance_confirmation_height(&open.account, 1).unwrap();
            batch.prune_cemented_undo(&open.hash).unwrap();
        }
        let batch = store.tx_begin_write().unwrap();
        assert_eq!(
            batch
                .get_account(&open.account)
                .unwrap()
                .confirmation_height,
            0
        );
        assert!(batch.block_undo(&open.hash).unwrap().is_some());
        drop(batch);
        cement(&store, open.hash).unwrap();
        assert!(store
            .tx_begin_write()
            .unwrap()
            .block_undo(&open.hash)
            .unwrap()
            .is_none());
    }

    #[test]
    fn verification_reward_cements_the_complete_outcome_even_if_source_was_already_confirmed() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 32 * 1024 * 1024).unwrap();
        let mut votes = Vec::new();
        for name in ["alice", "bob", "charlie"] {
            let open = block(name, BlockType::Open, BlockHash::ZERO, BlockHash::ZERO);
            save(&store, &open, 1);
            let vote = block(
                name,
                BlockType::VerificationVote,
                open.hash,
                BlockHash::new([9; 32]),
            );
            votes.push(vote);
        }
        for vote in &votes[..2] {
            save(&store, vote, 2);
        }
        let mut sources: Vec<_> = votes.iter().map(|vote| vote.hash).collect();
        sources.sort_by_key(|hash| *hash.as_bytes());
        let bytes = bincode::serialize(&sources).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let previous = batch.get_account(&votes[2].account).unwrap();
        crate::ledger_updater::stage_block(&mut batch, &votes[2], Some(&previous), 0, None, None)
            .unwrap();
        batch
            .put_meta(
                &format!("verification_outcome_sources/v1/{}", votes[0].hash),
                &bytes,
            )
            .unwrap();
        batch
            .put_meta(
                &format!("verification_transition_sources/v1/{}", votes[2].hash),
                &bytes,
            )
            .unwrap();
        batch.commit().unwrap();
        cement(&store, votes[0].hash).unwrap();
        assert_eq!(
            store
                .account_store()
                .get_account(&votes[1].account)
                .unwrap()
                .confirmation_height,
            0
        );
        let claim = block("alice", BlockType::Receive, votes[0].hash, votes[0].hash);
        save(&store, &claim, 3);
        let waiting = cement(&store, claim.hash).unwrap();
        assert!(!waiting.complete);
        assert!(waiting.hashes.is_empty());
        let missing = store.block_store().get_block(&votes[1].hash).unwrap();
        store.block_store().delete_block(&votes[1].hash).unwrap();
        assert!(cement(&store, claim.hash).is_err());
        assert_eq!(
            store
                .account_store()
                .get_account(&claim.account)
                .unwrap()
                .confirmation_height,
            2
        );
        assert_eq!(
            store
                .account_store()
                .get_account(&votes[2].account)
                .unwrap()
                .confirmation_height,
            0
        );
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_block_with_account(&votes[1].hash, &missing, &votes[1].account, 2)
            .unwrap();
        batch.commit().unwrap();
        for vote in &votes[1..] {
            store.meta_store().enqueue_confirmation(&vote.hash).unwrap();
        }
        let outcome = cement(&store, claim.hash).unwrap();
        assert!(outcome.complete);
        assert!(outcome.hashes.contains(&votes[1].hash));
        assert!(outcome.hashes.contains(&votes[2].hash));
        assert_eq!(outcome.hashes.last(), Some(&claim.hash));
        for vote in votes {
            assert!(
                store
                    .account_store()
                    .get_account(&vote.account)
                    .unwrap()
                    .confirmation_height
                    >= 2
            );
        }
    }

    #[test]
    fn endorsement_cements_target_history_before_identity_admission() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 32 * 1024 * 1024).unwrap();
        let open = block("alice", BlockType::Open, BlockHash::ZERO, BlockHash::ZERO);
        let send = block("alice", BlockType::Send, open.hash, BlockHash::new([9; 32]));
        let receive = block("bob", BlockType::Open, BlockHash::ZERO, send.hash);
        let choice = block(
            "bob",
            BlockType::ChangeRepresentative,
            receive.hash,
            BlockHash::ZERO,
        );
        let mut endorse = block(
            "alice",
            BlockType::Endorse,
            send.hash,
            BlockHash::new([9; 32]),
        );
        endorse.target_head = choice.hash;
        endorse.hash = endorse.compute_hash();
        for (block, height) in [
            (&open, 1),
            (&send, 2),
            (&receive, 1),
            (&choice, 2),
            (&endorse, 3),
        ] {
            save(&store, block, height);
        }
        store
            .meta_store()
            .enqueue_confirmation(&endorse.hash)
            .unwrap();
        let result = cement(&store, endorse.hash).unwrap();
        assert!(result.complete);
        assert_eq!(
            result.hashes,
            vec![
                open.hash,
                send.hash,
                receive.hash,
                choice.hash,
                endorse.hash
            ]
        );
        assert_eq!(
            store
                .account_store()
                .get_account(&choice.account)
                .unwrap()
                .confirmation_height,
            2
        );
    }

    #[test]
    fn receive_cements_source_and_ancestors_but_not_descendants_and_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 32 * 1024 * 1024).unwrap();
        let open = block("alice", BlockType::Open, BlockHash::ZERO, BlockHash::ZERO);
        let send = block("alice", BlockType::Send, open.hash, BlockHash::new([9; 32]));
        let next = block(
            "alice",
            BlockType::ChangeRepresentative,
            send.hash,
            BlockHash::ZERO,
        );
        let receive = block("bob", BlockType::Open, BlockHash::ZERO, send.hash);
        for (block, height) in [(&open, 1), (&send, 2), (&next, 3), (&receive, 1)] {
            save(&store, block, height);
        }
        store
            .meta_store()
            .enqueue_confirmation(&receive.hash)
            .unwrap();
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 32 * 1024 * 1024).unwrap();
        assert_eq!(
            store.meta_store().pending_confirmations().unwrap(),
            vec![receive.hash]
        );
        let result = cement(&store, receive.hash).unwrap();
        assert_eq!(result.hashes, vec![open.hash, send.hash, receive.hash]);
        assert!(result.complete);
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 32 * 1024 * 1024).unwrap();
        assert!(store
            .meta_store()
            .pending_confirmations()
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .account_store()
                .get_account(&open.account)
                .unwrap()
                .confirmation_height,
            2
        );
        assert_eq!(
            store
                .account_store()
                .get_account(&receive.account)
                .unwrap()
                .confirmation_height,
            1
        );
        assert!(cement(&store, receive.hash).unwrap().hashes.is_empty());
        assert!(store.block_store().get_block(&next.hash).is_ok());
        let batch = store.tx_begin_write().unwrap();
        for hash in [open.hash, send.hash, receive.hash] {
            assert!(batch.block_undo(&hash).unwrap().is_none());
        }
        assert!(batch.block_undo(&next.hash).unwrap().is_some());
    }

    #[test]
    fn missing_source_aborts_all_staged_confirmation_heights() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 32 * 1024 * 1024).unwrap();
        let open = block("bob", BlockType::Open, BlockHash::ZERO, BlockHash::ZERO);
        let receive = block(
            "bob",
            BlockType::Receive,
            open.hash,
            BlockHash::new([8; 32]),
        );
        save(&store, &open, 1);
        save(&store, &receive, 2);
        store
            .meta_store()
            .enqueue_confirmation(&receive.hash)
            .unwrap();
        assert!(cement(&store, receive.hash).is_err());
        assert_eq!(
            store.meta_store().pending_confirmations().unwrap(),
            vec![receive.hash]
        );
        let batch = store.tx_begin_write().unwrap();
        assert!(batch.block_undo(&open.hash).unwrap().is_some());
        assert!(batch.block_undo(&receive.hash).unwrap().is_some());
        assert_eq!(
            store
                .account_store()
                .get_account(&open.account)
                .unwrap()
                .confirmation_height,
            0
        );
    }

    #[test]
    fn mismatched_height_and_corrupt_hash_cannot_be_cemented() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 32 * 1024 * 1024).unwrap();
        let open = block("alice", BlockType::Open, BlockHash::ZERO, BlockHash::ZERO);
        let next = block("alice", BlockType::Send, open.hash, BlockHash::ZERO);
        save(&store, &open, 1);
        save(&store, &next, 3);
        assert!(cement(&store, next.hash).is_err());
        let mut corrupt = open.clone();
        corrupt.trst_balance = 99;
        store
            .block_store()
            .put_block(&open.hash, &bincode::serialize(&corrupt).unwrap())
            .unwrap();
        assert!(cement(&store, open.hash).is_err());
        assert_eq!(
            store
                .account_store()
                .get_account(&open.account)
                .unwrap()
                .confirmation_height,
            0
        );
    }

    #[test]
    fn long_chain_resumes_after_bounded_cementation_batch() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 32 * 1024 * 1024).unwrap();
        let open = block("alice", BlockType::Open, BlockHash::ZERO, BlockHash::ZERO);
        let mut blocks = vec![open];
        for _ in 0..MAX_CEMENTED_PER_BATCH {
            let previous = blocks.last().unwrap().hash;
            blocks.push(block(
                "alice",
                BlockType::ChangeRepresentative,
                previous,
                BlockHash::ZERO,
            ));
        }
        let last = blocks.last().unwrap();
        for (index, block) in blocks.iter().enumerate() {
            save(&store, block, index as u64 + 1);
        }
        store.meta_store().enqueue_confirmation(&last.hash).unwrap();
        let first = cement(&store, last.hash).unwrap();
        assert_eq!(first.hashes.len(), MAX_CEMENTED_PER_BATCH);
        assert!(!first.complete);
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 32 * 1024 * 1024).unwrap();
        assert_eq!(
            store.meta_store().pending_confirmations().unwrap(),
            vec![last.hash]
        );
        let second = cement(&store, last.hash).unwrap();
        assert_eq!(second.hashes, vec![last.hash]);
        assert!(store
            .meta_store()
            .pending_confirmations()
            .unwrap()
            .is_empty());
        assert!(second.complete);
    }
}
