//! Expiry and pending-return state must survive the same durable commit.

use burst_store_lmdb::{LmdbStore, LmdbWriteTransaction};
use burst_trst::TrstEngine;
use burst_types::Timestamp;

fn stage(
    batch: &mut LmdbWriteTransaction<'_>,
    before: &TrstEngine,
    trst: &mut TrstEngine,
    now: Timestamp,
    return_pending: bool,
) -> Result<(), String> {
    trst.flush_all_expired(now);
    let pending = if return_pending {
        batch.all_pending().map_err(|e| e.to_string())?
    } else {
        Vec::new()
    };
    for (destination, hash, mut pending) in pending {
        if pending.return_credit
            || !pending.provenance.first().is_some_and(|p| {
                p.effective_origin_timestamp
                    .has_expired(trst.expiry_secs, now)
            })
        {
            continue;
        }
        let returned = crate::ledger_bridge::create_returned_token(&destination, &hash, &pending);
        crate::ledger_bridge::deliver_pending(trst, &pending, returned, now)?;
        batch
            .delete_pending(&destination, hash.as_bytes())
            .map_err(|e| e.to_string())?;
        match batch.get_pending(&pending.source, &hash) {
            Err(burst_store::StoreError::NotFound(_)) => {}
            Err(error) => return Err(error.to_string()),
            Ok(_) => return Err("expiry return credit already exists".into()),
        }
        pending.return_credit = true;
        let bytes = bincode::serialize(&pending).map_err(|error| error.to_string())?;
        batch
            .put_pending(&pending.source, hash.as_bytes(), &bytes)
            .map_err(|error| error.to_string())?;
    }
    for holder in trst.wallets.keys() {
        crate::ledger_updater::refresh_token_counters(batch, holder, trst, now)?;
    }
    crate::trst_storage::persist_delta(batch, before, trst).map_err(|error| error.to_string())?;
    crate::graph_storage::persist_delta(batch, &before.merger_graph, &trst.merger_graph)
        .map_err(|e| e.to_string())
}

/// The caller holds the live TRST engine lock throughout this operation.
pub(crate) fn apply(
    store: &LmdbStore,
    live: &mut TrstEngine,
    now: Timestamp,
) -> Result<(), String> {
    commit(store, live, now, true)
}

/// Held-token expiry is a time-derived display/spending state. Pending returns
/// change ownership and must wait for an agreed boundary in certified mode.
pub(crate) fn apply_held(
    store: &LmdbStore,
    live: &mut TrstEngine,
    now: Timestamp,
) -> Result<(), String> {
    commit(store, live, now, false)
}

fn commit(
    store: &LmdbStore,
    live: &mut TrstEngine,
    now: Timestamp,
    return_pending: bool,
) -> Result<(), String> {
    let mut candidate = live.clone();
    let mut batch = store.tx_begin_write().map_err(|e| e.to_string())?;
    stage(&mut batch, live, &mut candidate, now, return_pending)?;
    batch.commit().map_err(|e| e.to_string())?;
    *live = candidate;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::{
        account::AccountStore,
        meta::MetaStore,
        pending::{PendingInfo, PendingProvenance, PendingStore},
        trst_index::TrstIndexStore,
    };
    use burst_types::{BlockHash, TrstState, TxHash, WalletAddress};

    #[test]
    fn expired_pending_return_is_atomic_idempotent_and_durable() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let sender = WalletAddress::new("brst_sender");
        let receiver = WalletAddress::new("brst_receiver");
        let hash = TxHash::new([1; 32]);
        let pending = PendingInfo {
            return_credit: false,
            source: sender.clone(),
            amount: 15,
            timestamp: Timestamp::new(10),
            provenance: vec![PendingProvenance {
                basis: Default::default(),
                revoked_amounts: Default::default(),
                amount: 15,
                origin: hash,
                origin_wallet: sender.clone(),
                origin_timestamp: Timestamp::new(1),
                effective_origin_timestamp: Timestamp::new(1),
            }],
        };
        store
            .pending_store()
            .put_pending(&receiver, &hash, &pending)
            .unwrap();
        let mut live = TrstEngine::new();
        live.expiry_secs = 10;
        // An absent account fails after the return was staged; neither live
        // portfolios nor the durable pending entry may change on that error.
        assert!(apply(&store, &mut live, Timestamp::new(20)).is_err());
        assert!(live.wallets.is_empty());
        assert!(store.pending_store().get_pending(&receiver, &hash).is_ok());
        let account =
            crate::genesis_state::account_to_write(None, sender.clone(), BlockHash::ZERO).unwrap();
        store.account_store().put_account(&account).unwrap();
        {
            let mut staged = live.clone();
            let mut batch = store.tx_begin_write().unwrap();
            stage(&mut batch, &live, &mut staged, Timestamp::new(20), true).unwrap();
            // Simulate an interruption before commit.
        }
        assert!(store.pending_store().get_pending(&receiver, &hash).is_ok());
        assert!(live.wallets.is_empty());
        apply_held(&store, &mut live, Timestamp::new(20)).unwrap();
        assert!(live.wallets.is_empty());
        assert!(store.pending_store().get_pending(&receiver, &hash).is_ok());
        apply(&store, &mut live, Timestamp::new(20)).unwrap();
        apply(&store, &mut live, Timestamp::new(21)).unwrap();
        assert!(store.pending_store().get_pending(&receiver, &hash).is_err());
        assert_eq!(
            store
                .account_store()
                .get_account(&sender)
                .unwrap()
                .expired_trst,
            15
        );
        let portfolio = live.get_portfolio(&sender).unwrap();
        assert_eq!(portfolio.tokens.len(), 1);
        assert_eq!(portfolio.tokens[0].origin, hash);
        assert_eq!(portfolio.tokens[0].state, TrstState::Expired);
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let engine = TrstEngine::load_wallets(
            &store.meta_store().get_meta(TrstEngine::meta_key()).unwrap(),
            10,
        )
        .unwrap();
        assert_eq!(engine.get_portfolio(&sender).unwrap().tokens[0].amount, 15);
        assert_eq!(store.pending_store().pending_count().unwrap(), 1);
        assert!(
            store
                .pending_store()
                .get_pending(&sender, &hash)
                .unwrap()
                .return_credit
        );
    }

    #[test]
    fn revoked_pending_return_updates_revoked_counter_without_inventing_a_signed_balance() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let sender = WalletAddress::new("brst_sender");
        let receiver = WalletAddress::new("brst_receiver");
        let hash = TxHash::new([1; 32]);
        let account =
            crate::genesis_state::account_to_write(None, sender.clone(), BlockHash::ZERO).unwrap();
        store.account_store().put_account(&account).unwrap();
        let mut live = TrstEngine::with_expiry(10);
        live.mint(
            hash,
            receiver.clone(),
            15,
            sender.clone(),
            Timestamp::new(1),
        )
        .unwrap();
        let pending = PendingInfo {
            return_credit: false,
            source: sender.clone(),
            amount: 15,
            timestamp: Timestamp::new(1),
            provenance: vec![PendingProvenance {
                basis: Default::default(),
                revoked_amounts: Default::default(),
                amount: 15,
                origin: hash,
                origin_wallet: sender.clone(),
                origin_timestamp: Timestamp::new(1),
                effective_origin_timestamp: Timestamp::new(1),
            }],
        };
        store
            .pending_store()
            .put_pending(&receiver, &hash, &pending)
            .unwrap();
        let (held, effects) = live.revoke_with_pending_effects(&sender);
        assert!(held.is_empty());
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(
            crate::verification_commit::revoke_pending(&mut batch, &effects, &mut live).unwrap(),
            15
        );
        batch.commit().unwrap();
        let index = store.trst_index_store();
        index.put_origin_index(&hash, &hash).unwrap();
        index.put_expiry_index(Timestamp::new(11), &hash).unwrap();
        apply(&store, &mut live, Timestamp::new(20)).unwrap();
        assert_eq!(
            index.clean_expiry_before(Timestamp::new(20), 1024).unwrap(),
            1
        );
        assert_eq!(index.get_by_origin(&hash).unwrap(), vec![hash]);
        let account = store.account_store().get_account(&sender).unwrap();
        assert_eq!(
            (
                account.trst_balance,
                account.expired_trst,
                account.revoked_trst
            ),
            (0, 0, 15)
        );
        assert_eq!(store.pending_store().pending_count().unwrap(), 1);
        assert!(
            store
                .pending_store()
                .get_pending(&sender, &hash)
                .unwrap()
                .return_credit
        );
        live.un_revoke_by_origin(&sender, Timestamp::new(21));
        apply(&store, &mut live, Timestamp::new(21)).unwrap();
        let account = store.account_store().get_account(&sender).unwrap();
        assert_eq!(
            (
                account.trst_balance,
                account.expired_trst,
                account.revoked_trst
            ),
            (0, 15, 0)
        );
        assert_eq!(
            live.transferable_balance(&sender, Timestamp::new(21)),
            Some(0)
        );
        live.set_expiry_period(u64::MAX, Timestamp::new(22));
        apply(&store, &mut live, Timestamp::new(22)).unwrap();
        assert_eq!(
            live.transferable_balance(&sender, Timestamp::new(22)),
            Some(15)
        );
        assert_eq!(index.get_by_origin(&hash).unwrap(), vec![hash]);
        drop(index);
        drop(store);
        let reopened = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let mut restored = TrstEngine::load_wallets(
            &reopened
                .meta_store()
                .get_meta(TrstEngine::meta_key())
                .unwrap(),
            u64::MAX,
        )
        .unwrap();
        assert_eq!(
            restored.transferable_balance(&sender, Timestamp::new(22)),
            Some(15)
        );
        assert_eq!(
            reopened.trst_index_store().get_by_origin(&hash).unwrap(),
            vec![hash]
        );
    }
}
