use super::*;
use burst_store::account::AccountInfo;
use burst_types::WalletAddress;
use std::collections::HashSet;

pub(super) fn owns(table: UndoTable, key: &[u8]) -> bool {
    matches!(
        table,
        UndoTable::Accounts | UndoTable::Frontiers | UndoTable::Pending | UndoTable::BrnWallets
            | UndoTable::TrstOrigins | UndoTable::TrstExpiry | UndoTable::TrstReverse
    ) || (table == UndoTable::Meta
        && std::str::from_utf8(key).is_ok_and(|key| {
            key != crate::endorsement_commit::SNAPSHOT_KEY
                && burst_store_lmdb::write_batch::checkpoint_projection::is_lifecycle_key(key)
        }))
}

pub(super) fn visit(
    state: &ConfirmedReplay,
    before: &burst_trst::TrstEngine,
    after: &burst_trst::TrstEngine,
    cache_at: Timestamp,
    cache_holders: &HashSet<WalletAddress>,
    mut emit: impl FnMut(UndoChange) -> Result<(), ReplayError>,
) -> Result<(), ReplayError> {
    let mutations = state
        .mutations
        .as_ref()
        .ok_or_else(|| crate::confirmed_replay::invalid("recovery omitted transition mutations"))?;
    let mut written_accounts = HashSet::new();
    state.brn.wallets.visit_recorded(|address, before, after| {
        emit(UndoChange {
            table: UndoTable::BrnWallets,
            key: address.as_str().as_bytes().to_vec(),
            before: before.map(<[u8]>::to_vec),
            after,
        }).map_err(|error| error.to_string())
    }).map_err(replay_error)?;
    for change in mutations.changes() {
        if change.table == UndoTable::Meta
            && change.key == crate::endorsement_commit::SNAPSHOT_KEY.as_bytes()
        {
            continue;
        }
        if !owns(change.table, &change.key) || change.table == UndoTable::Frontiers {
            return Err(crate::confirmed_replay::invalid(
                "unexpected recorded table or key",
            ));
        }
        let mut change = change.clone();
        if change.table == UndoTable::Accounts {
            written_accounts.insert(change.key.clone());
            let old = normalize(&change.key, &mut change.before, before, cache_at)?;
            let new = normalize(&change.key, &mut change.after, after, cache_at)?;
            emit(UndoChange {
                table: UndoTable::Frontiers,
                key: change.key.clone(),
                before: old,
                after: new,
            })?;
        }
        emit(change)?;
    }
    // Portfolio changes can alter account caches without a signed account write.
    // The existing token delta visitor supplies these holders, not block fields.
    for holder in cache_holders {
        let key = holder.as_str().as_bytes();
        if written_accounts.contains(key) {
            continue;
        }
        let Some(account) = state.account(holder) else {
            continue;
        };
        let old = expiry::normalized_account(account, before, cache_at)?;
        let new = expiry::normalized_account(account, after, cache_at)?;
        if old.expired_trst == new.expired_trst && old.revoked_trst == new.revoked_trst {
            continue;
        }
        emit(UndoChange {
            table: UndoTable::Accounts,
            key: key.to_vec(),
            before: Some(bincode::serialize(&old).map_err(replay_error)?),
            after: Some(bincode::serialize(&new).map_err(replay_error)?),
        })?;
    }
    Ok(())
}

fn normalize(
    key: &[u8],
    bytes: &mut Option<Vec<u8>>,
    trst: &burst_trst::TrstEngine,
    at: Timestamp,
) -> Result<Option<Vec<u8>>, ReplayError> {
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let account: AccountInfo = bincode::deserialize(bytes).map_err(replay_error)?;
    if key != account.address.as_str().as_bytes() {
        return Err(crate::confirmed_replay::invalid(
            "recorded account key mismatch",
        ));
    }
    let account = expiry::normalized_account(&account, trst, at)?;
    *bytes = bincode::serialize(&account).map_err(replay_error)?;
    Ok((account.block_count > 0).then(|| account.head.as_bytes().to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::{NetworkId, ProtocolParams, TxHash};

    #[test]
    fn minted_index_recording_matches_full_projection_without_old_mints() {
        let mut state = crate::confirmed_replay::snapshot::SnapshotContinuation::from_genesis(
            NetworkId::Dev, &ProtocolParams::default(),
        ).unwrap().state().clone();
        let directory = tempfile::tempdir().unwrap();
        let at = Timestamp::new(100);
        for n in 0..32 {
            state.record_minted_burn(TxHash::new([n; 32]), Timestamp::EPOCH).unwrap();
        }
        let before = Rows::capture(&state, at, directory.path()).unwrap();
        state.mutations = Some(Default::default());
        state.brn.wallets.record_transition(true);
        state.record_minted_burn(TxHash::new([67; 32]), at).unwrap();
        let after = Rows::capture(&state, at, directory.path()).unwrap();
        let mut changes = Vec::new();
        visit(&state, &before.trst, &after.trst, at, &HashSet::new(), |change| {
            changes.push(change);
            Ok(())
        }).unwrap();
        changes.sort_by(|a, b| (a.table, &a.key).cmp(&(b.table, &b.key)));
        assert_eq!(changes.len(), 3);
        assert!(changes.iter().all(|row| row.before.is_none()));
        compare_full_projection(&before, &after, &changes).unwrap();
        Rows::capture_projection(&state, at, directory.path(), false).unwrap().values
            .visit(|table, _, _| {
                assert!(!matches!(table, UndoTable::TrstOrigins | UndoTable::TrstExpiry | UndoTable::TrstReverse));
                Ok(())
            }).unwrap();
    }

    #[test]
    fn brn_recording_matches_full_projection_and_omits_untouched_wallets() {
        let mut state = crate::confirmed_replay::snapshot::SnapshotContinuation::from_genesis(
            NetworkId::Dev, &ProtocolParams::default(),
        ).unwrap().state().clone();
        let directory = tempfile::tempdir().unwrap();
        let at = Timestamp::new(100);
        let changed = WalletAddress::new("brst_changed");
        let removed = WalletAddress::new("brst_removed");
        for address in [&changed, &removed] {
            state.brn.track_wallet(address.clone(), burst_brn::BrnWalletState::new(Timestamp::EPOCH));
        }
        for i in 0..100 {
            state.brn.track_wallet(WalletAddress::new(format!("brst_untouched_{i}")),
                burst_brn::BrnWalletState::new(Timestamp::EPOCH));
        }
        let before = Rows::capture(&state, at, directory.path()).unwrap();
        state.mutations = Some(Default::default());
        state.brn.wallets.record_transition(true);
        let mut wallet = state.brn.wallets.remove(&changed).unwrap();
        state.brn.record_burn(&mut wallet, 1, at).unwrap();
        state.brn.wallets.insert(changed.clone(), wallet);
        state.brn.deactivate_wallet(&changed, at).unwrap();
        state.brn.wallets.remove(&removed);
        let added = WalletAddress::new("brst_added");
        state.brn.track_wallet(added, burst_brn::BrnWalletState::new(at));
        let after = Rows::capture(&state, at, directory.path()).unwrap();
        let partial = Rows::capture_projection(&state, at, directory.path(), false).unwrap();
        partial.values.visit(|table, _, _| {
            assert_ne!(table, UndoTable::BrnWallets);
            Ok(())
        }).unwrap();
        let mut changes = Vec::new();
        visit(&state, &before.trst, &after.trst, at, &HashSet::new(), |change| {
            changes.push(change);
            Ok(())
        }).unwrap();
        changes.sort_by(|a, b| (a.table, &a.key).cmp(&(b.table, &b.key)));
        assert_eq!(changes.len(), 3);
        compare_full_projection(&before, &after, &changes).unwrap();
    }

    #[test]
    fn portfolio_only_changes_preserve_signed_balances_and_update_account_caches() {
        let mut state = crate::confirmed_replay::snapshot::SnapshotContinuation::from_genesis(
            NetworkId::Dev,
            &ProtocolParams::default(),
        )
        .unwrap()
        .state()
        .clone();
        state.mutations = Some(Default::default());
        state.brn.wallets.record_transition(true);
        let holder = WalletAddress::new("brst_cache_holder");
        let origin = WalletAddress::new("brst_cache_origin");
        let mut account = AccountInfo::unopened(holder.clone());
        account.trst_balance = 500;
        state.accounts.insert(holder.clone(), account.clone());
        let mut trst = burst_trst::TrstEngine::with_expiry(100);
        let token = trst
            .mint(
                TxHash::new([72; 32]),
                holder.clone(),
                500,
                origin.clone(),
                Timestamp::new(1000),
            )
            .unwrap();
        trst.track_token(token);
        let at = Timestamp::new(1200);
        let expired = expiry::ExpiryView::new(&trst, 100, at).trst;
        let active = expiry::ExpiryView::new(&trst, 10000, at).trst;
        let mut revoked = active.clone();
        revoked.revoke_by_origin(&origin);
        let empty = burst_trst::TrstEngine::with_expiry(10000);
        for (before, after) in [(&expired, &active), (&active, &revoked), (&revoked, &empty)] {
            let mut holders = HashSet::new();
            after
                .visit_wallet_record_delta(before, |old, new| {
                    for row in old.iter().chain(new.iter()) {
                        if let burst_trst::engine::WalletRecord::Holding { holder, .. } = row {
                            holders.insert(holder.clone());
                        }
                    }
                    Ok::<_, String>(())
                })
                .unwrap();
            assert!(holders.contains(&holder));
            let mut changes = Vec::new();
            visit(&state, before, after, at, &holders, |row| {
                changes.push(row);
                Ok(())
            })
            .unwrap();
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].table, UndoTable::Accounts);
            let old: AccountInfo =
                bincode::deserialize(changes[0].before.as_ref().unwrap()).unwrap();
            let new: AccountInfo =
                bincode::deserialize(changes[0].after.as_ref().unwrap()).unwrap();
            assert_eq!(old.trst_balance, 500);
            assert_eq!(new.trst_balance, 500);
            assert_eq!(
                changes[0].before.as_ref().unwrap(),
                &bincode::serialize(&expiry::normalized_account(&account, before, at).unwrap())
                    .unwrap()
            );
            assert_eq!(
                changes[0].after.as_ref().unwrap(),
                &bincode::serialize(&expiry::normalized_account(&account, after, at).unwrap())
                    .unwrap()
            );
            assert_ne!(
                (old.expired_trst, old.revoked_trst),
                (new.expired_trst, new.revoked_trst)
            );
        }
        assert_eq!(state.account(&holder).unwrap().expired_trst, 0);
        assert_eq!(state.account(&holder).unwrap().revoked_trst, 0);
    }
}

#[cfg(test)]
pub(super) fn compare_full_projection(
    before: &Rows,
    after: &Rows,
    changes: &[UndoChange],
) -> Result<(), ReplayError> {
    let mut expected = Vec::new();
    before
        .values
        .visit(|table, key, old| {
            if !owns(table, &key) {
                return Ok(());
            }
            let new = after.values.get(table, &key)?;
            if new.as_ref() != Some(&old) {
                expected.push(UndoChange {
                    table,
                    key,
                    before: Some(old),
                    after: new,
                });
            }
            Ok(())
        })
        .map_err(replay_error)?;
    after
        .values
        .visit(|table, key, new| {
            if owns(table, &key) && before.values.get(table, &key)?.is_none() {
                expected.push(UndoChange {
                    table,
                    key,
                    before: None,
                    after: Some(new),
                });
            }
            Ok(())
        })
        .map_err(replay_error)?;
    expected.sort_by(|a, b| (a.table, &a.key).cmp(&(b.table, &b.key)));
    let actual: Vec<_> = changes
        .iter()
        .filter(|row| owns(row.table, &row.key))
        .cloned()
        .collect();
    if expected != actual {
        return Err(crate::confirmed_replay::invalid(
            "transition mutations differ from full recovery projection",
        ));
    }
    Ok(())
}
