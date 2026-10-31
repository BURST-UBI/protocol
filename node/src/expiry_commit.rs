//! Expiry and pending-return state must survive the same durable commit.

use burst_store_lmdb::{LmdbStore, LmdbWriteTransaction};
use burst_trst::TrstEngine;
use burst_types::Timestamp;

#[cfg(test)]
thread_local! {
    // Flush, selection, delivery, remaining return loop, counters, holdings, graph.
    static RETURN_PHASES: std::cell::RefCell<Option<[u128; 7]>> = const {
        std::cell::RefCell::new(None)
    };
}

fn stage(
    batch: &mut LmdbWriteTransaction<'_>,
    before: &TrstEngine,
    trst: &mut TrstEngine,
    now: Timestamp,
    return_pending: bool,
) -> Result<(), String> {
    #[cfg(test)]
    let started = RETURN_PHASES
        .with(|slot| slot.borrow().is_some())
        .then(std::time::Instant::now);
    trst.flush_all_expired(now);
    #[cfg(test)]
    let flushed = started.map_or(0, |start| start.elapsed().as_nanos());
    let pending = if return_pending {
        batch
            .expired_pending(trst.expiry_secs, now)
            .map_err(|e| e.to_string())?
    } else {
        Vec::new()
    };
    #[cfg(test)]
    let selected = started.map_or(0, |start| start.elapsed().as_nanos());
    #[cfg(test)]
    let mut delivery_ns = 0;
    for (destination, hash, mut pending) in pending {
        if pending.return_credit
            || !pending.provenance.first().is_some_and(|p| {
                p.effective_origin_timestamp
                    .has_expired(trst.expiry_secs, now)
            })
        {
            continue;
        }
        #[cfg(test)]
        let delivery_started = started.map(|_| std::time::Instant::now());
        let returned = crate::block_economics::create_returned_token(&destination, &hash, &pending);
        crate::block_economics::deliver_pending(trst, &pending, returned, now)?;
        #[cfg(test)]
        if let Some(start) = delivery_started {
            delivery_ns += start.elapsed().as_nanos();
        }
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
    #[cfg(test)]
    let returned = started.map_or(0, |start| start.elapsed().as_nanos());
    batch
        .refresh_expiry_counters(trst, now)
        .map_err(|e| e.to_string())?;
    #[cfg(test)]
    let counted = started.map_or(0, |start| start.elapsed().as_nanos());
    crate::trst_storage::persist_delta(batch, before, trst).map_err(|error| error.to_string())?;
    #[cfg(test)]
    let persisted = started.map_or(0, |start| start.elapsed().as_nanos());
    let result =
        crate::graph_storage::persist_delta(batch, &before.merger_graph, &trst.merger_graph)
            .map_err(|e| e.to_string());
    #[cfg(test)]
    if let Some(start) = started {
        let finished = start.elapsed().as_nanos();
        RETURN_PHASES.with(|slot| {
            *slot.borrow_mut() = Some([
                flushed,
                selected - flushed,
                delivery_ns,
                returned - selected - delivery_ns,
                counted - returned,
                persisted - counted,
                finished - persisted,
            ])
        });
    }
    result
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
    fn unchanged_counters_do_not_enter_undo_and_backward_time_still_refreshes() {
        use burst_store_lmdb::block_undo::UndoTable;
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let mut live = TrstEngine::with_expiry(100);
        let holder = WalletAddress::new("brst_counter_holder");
        let token = live
            .mint(
                TxHash::new([33; 32]),
                holder.clone(),
                15,
                holder.clone(),
                Timestamp::new(1000),
            )
            .unwrap();
        live.track_token(token);
        let account =
            crate::genesis_state::account_to_write(None, holder.clone(), BlockHash::ZERO).unwrap();
        store.account_store().put_account(&account).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        crate::ledger_updater::persist_economics(&mut batch, &burst_brn::BrnEngine::new(), &live)
            .unwrap();
        batch.commit().unwrap();
        {
            let mut batch = store.tx_begin_write().unwrap();
            crate::ledger_updater::refresh_token_counters(
                &mut batch,
                &holder,
                &live,
                Timestamp::new(1050),
            )
            .unwrap();
            batch.require_clean_checkpoint_boundary().unwrap();
        }
        for (n, now, expected, account_changes) in [
            (1, 1050, 0, 0),
            (2, 1100, 15, 1),
            (3, 1101, 15, 0),
            (4, 1050, 0, 1),
            (5, 1100, 15, 1),
        ] {
            let hash = BlockHash::new([n; 32]);
            let mut candidate = live.clone();
            let mut batch = store.tx_begin_write().unwrap();
            batch
                .begin_block_undo(hash, holder.clone(), n as u64)
                .unwrap();
            // Undo records require a real change even on a no-op expiry tick.
            batch.put_meta("expiry-counter-probe", &[n]).unwrap();
            stage(
                &mut batch,
                &live,
                &mut candidate,
                Timestamp::new(now),
                false,
            )
            .unwrap();
            batch.commit().unwrap();
            live = candidate;
            assert_eq!(
                store
                    .account_store()
                    .get_account(&holder)
                    .unwrap()
                    .expired_trst,
                expected
            );
            let batch = store.tx_begin_write().unwrap();
            let undo = batch.block_undo(&hash).unwrap().unwrap();
            assert_eq!(
                undo.changes
                    .iter()
                    .filter(|change| change.table == UndoTable::Accounts)
                    .count(),
                account_changes
            );
        }
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        apply_held(&store, &mut live, Timestamp::new(1050)).unwrap();
        assert_eq!(
            store
                .account_store()
                .get_account(&holder)
                .unwrap()
                .expired_trst,
            0
        );
    }

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
        live.restore_by_originator(&sender, Timestamp::new(21))
            .unwrap();
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
        live.set_expiry_period(u64::MAX, Timestamp::new(22))
            .unwrap();
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

    #[test]
    #[ignore = "explicit capped-host committed pending expiry burst diagnostic"]
    fn expired_pending_return_burst_scaling() {
        use std::time::Instant;
        for population in [100u64, 1_000, 10_000] {
            let dir = tempfile::tempdir().unwrap();
            let store = LmdbStore::open(dir.path(), 32, 256 << 20).unwrap();
            let sender = WalletAddress::new("brst_burst_sender");
            let receiver = WalletAddress::new("brst_burst_receiver");
            let mut live = TrstEngine::with_expiry(10);
            let mut batch = store.tx_begin_write().unwrap();
            let account =
                crate::genesis_state::account_to_write(None, sender.clone(), BlockHash::ZERO)
                    .unwrap();
            batch.put_account_info(&account, false).unwrap();
            for n in 0..population {
                let mut bytes = [0; 32];
                bytes[..8].copy_from_slice(&(n + 1).to_be_bytes());
                let hash = TxHash::new(bytes);
                // Primary-key order opposes insertion timestamps at the sender.
                let at = Timestamp::new(population - n);
                live.mint(hash, receiver.clone(), 15, sender.clone(), at)
                    .unwrap();
                let pending = PendingInfo {
                    return_credit: false,
                    source: sender.clone(),
                    amount: 15,
                    timestamp: at,
                    provenance: vec![PendingProvenance {
                        basis: Default::default(),
                        revoked_amounts: Default::default(),
                        amount: 15,
                        origin: hash,
                        origin_wallet: sender.clone(),
                        origin_timestamp: at,
                        effective_origin_timestamp: at,
                    }],
                };
                batch
                    .put_pending(
                        &receiver,
                        hash.as_bytes(),
                        &bincode::serialize(&pending).unwrap(),
                    )
                    .unwrap();
            }
            crate::trst_storage::stage_stable_rows_for_test(&mut batch, &live).unwrap();
            batch.commit().unwrap();
            let now = Timestamp::new(population + 11);
            // Certified local held expiry must not return ownership on local time.
            apply_held(&store, &mut live, now).unwrap();
            assert!(live.wallets.is_empty());
            assert_eq!(store.pending_store().pending_count().unwrap(), population);

            let mut candidate = live.clone();
            let mut batch = store.tx_begin_write().unwrap();
            RETURN_PHASES.with(|slot| assert!(slot.borrow_mut().replace([0; 7]).is_none()));
            let started = Instant::now();
            stage(&mut batch, &live, &mut candidate, now, true).unwrap();
            let stage_ns = started.elapsed().as_nanos();
            let phases = RETURN_PHASES.with(|slot| slot.borrow_mut().take().unwrap());
            assert!(phases.iter().sum::<u128>() <= stage_ns);
            assert!(phases[1] > 0 && phases[2] > 0 && phases[5] > 0);
            println!("expiry_return_phases population={population} flush_ns={} select_ns={} delivery_ns={} pending_ns={} counters_ns={} holdings_ns={} graph_ns={}",
                phases[0], phases[1], phases[2], phases[3], phases[4], phases[5], phases[6]);
            let started = Instant::now();
            batch.commit().unwrap();
            let commit_ns = started.elapsed().as_nanos();
            live = candidate;
            let memory: Vec<_> = std::fs::read_to_string("/proc/self/status")
                .unwrap()
                .lines()
                .filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:"))
                .map(str::to_owned)
                .collect();
            println!("expiry_return_burst population={population} stage_ns={stage_ns} commit_ns={commit_ns} memory={memory:?}");

            let expected = live.save_wallets();
            apply(&store, &mut live, now).unwrap();
            assert_eq!(live.save_wallets(), expected);
            assert_eq!(
                store
                    .account_store()
                    .get_account(&sender)
                    .unwrap()
                    .expired_trst,
                u128::from(population) * 15
            );
            let portfolio = live.get_portfolio(&sender).unwrap();
            assert_eq!(portfolio.tokens.len(), population as usize);
            assert!(portfolio
                .tokens
                .iter()
                .all(|token| token.amount == 15 && token.state == TrstState::Expired));
            assert!(portfolio
                .tokens
                .windows(2)
                .all(|pair| pair[0].origin_timestamp <= pair[1].origin_timestamp));
            for (index, token) in portfolio.tokens.iter().enumerate() {
                let mut origin = [0; 32];
                origin[..8].copy_from_slice(&(population - index as u64).to_be_bytes());
                assert_eq!(token.origin, TxHash::new(origin));
                assert_eq!(token.origin_timestamp, Timestamp::new(index as u64 + 1));
            }
            drop(store);
            let reopened = LmdbStore::open(dir.path(), 32, 256 << 20).unwrap();
            let batch = reopened.tx_begin_write().unwrap();
            assert_eq!(batch.read_trst_engine(10).unwrap().save_wallets(), expected);
            let pending = batch.all_pending().unwrap();
            assert_eq!(pending.len(), population as usize);
            assert!(pending
                .iter()
                .all(|(destination, _, row)| *destination == sender
                    && row.return_credit
                    && row.amount == 15));
        }
    }
}
