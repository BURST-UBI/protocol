use super::*;
use burst_store::account::AccountInfo;
use burst_types::WalletAddress;
use std::collections::HashSet;

pub(super) fn owns(table: UndoTable, key: &[u8]) -> bool {
    matches!(
        table,
        UndoTable::Accounts
            | UndoTable::Frontiers
            | UndoTable::Pending
            | UndoTable::BrnWallets
            | UndoTable::TrstOrigins
            | UndoTable::TrstExpiry
            | UndoTable::TrstReverse
    ) || (table == UndoTable::BrnMeta && key.starts_with(burst_brn::wallet_storage::ROW_PREFIX))
        || (table == UndoTable::Meta
            && std::str::from_utf8(key).is_ok_and(|key| {
                !crate::verification_storage::is_state_key(key)
                    && burst_store_lmdb::write_batch::checkpoint_projection::is_lifecycle_key(key)
            }))
}

#[cfg(test)]
pub(super) fn visit(
    state: &ConfirmedReplay,
    before: &burst_trst::TrstEngine,
    after: &burst_trst::TrstEngine,
    cache_at: Timestamp,
    cache_holders: &HashSet<WalletAddress>,
    emit: impl FnMut(UndoChange) -> Result<(), ReplayError>,
) -> Result<(), ReplayError> {
    visit_with_wallet_records(state, before, after, cache_at, cache_holders, None, emit)
}

pub(super) fn visit_wallet_delta(
    address: &WalletAddress,
    before: Option<&burst_brn::BrnWalletState>,
    after: Option<&burst_brn::BrnWalletState>,
    mut emit: impl FnMut(UndoChange) -> Result<(), ReplayError>,
) -> Result<(), ReplayError> {
    use burst_brn::wallet_storage;
    let prefix = wallet_storage::row_prefix(address).map_err(replay_error)?;
    wallet_storage::visit_delta(before, after, |relative, before, after| {
        let (table, key) = if relative == wallet_storage::HEADER {
            (UndoTable::BrnWallets, address.as_str().as_bytes().to_vec())
        } else {
            (UndoTable::BrnMeta, [prefix.as_slice(), relative].concat())
        };
        emit(UndoChange {
            table,
            key,
            before,
            after,
        })
        .map_err(|error| burst_brn::BrnError::Other(error.to_string()))
    })
    .map_err(replay_error)
}

pub(super) fn visit_with_wallet_records(
    state: &ConfirmedReplay,
    before: &burst_trst::TrstEngine,
    after: &burst_trst::TrstEngine,
    cache_at: Timestamp,
    cache_holders: &HashSet<WalletAddress>,
    before_wallets: Option<&burst_brn::wallet_states::WalletStates>,
    mut emit: impl FnMut(UndoChange) -> Result<(), ReplayError>,
) -> Result<(), ReplayError> {
    let mutations = state
        .mutations
        .as_ref()
        .ok_or_else(|| crate::confirmed_replay::invalid("recovery omitted transition mutations"))?;
    let mut written_accounts = HashSet::new();
    if let Some(before) = before_wallets {
        state
            .brn
            .wallets
            .visit_changes(before, |address, old, new| {
                visit_wallet_delta(address, old, new, &mut emit).map_err(|error| error.to_string())
            })
            .map_err(replay_error)?;
    } else {
        state
            .brn
            .wallets
            .visit_recorded(|address, before, after| {
                emit(UndoChange {
                    table: UndoTable::BrnWallets,
                    key: address.as_str().as_bytes().to_vec(),
                    before: before.map(<[u8]>::to_vec),
                    after,
                })
                .map_err(|error| error.to_string())
            })
            .map_err(replay_error)?;
    }
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
    // The caller collects these holders from portfolio changes.
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
    fn normalized_brn_deltas_match_projection_without_serialized_wallet_capture() {
        let mut state = crate::confirmed_replay::snapshot::SnapshotContinuation::from_genesis(
            NetworkId::Dev,
            &ProtocolParams::default(),
        )
        .unwrap()
        .state()
        .clone();
        let directory = tempfile::tempdir().unwrap();
        let at = Timestamp::new(100);
        let changed = WalletAddress::new("brst_changed");
        let removed = WalletAddress::new("brst_removed");
        let added = WalletAddress::new("brst_added");
        let mut wallet = burst_brn::BrnWalletState::new(Timestamp::EPOCH);
        for id in 1u64..=1024 {
            let mut hash = [0; 32];
            hash[..8].copy_from_slice(&id.to_be_bytes());
            wallet
                .record_unproven_release(BlockHash::new(hash), 1, Timestamp::new(10))
                .unwrap();
        }
        state.brn.track_wallet(changed.clone(), wallet.clone());
        state.brn.track_wallet(removed.clone(), wallet);
        let before =
            Rows::capture_projection_records(&state, at, directory.path(), true, false, true)
                .unwrap();
        state.mutations = Some(Default::default());
        state.brn.wallets.record_transition(false);
        state.brn.wallets.reset_recorded_transition();
        let wallet = state.brn.get_wallet_mut(&changed).unwrap();
        wallet.total_burned += 1;
        wallet
            .record_unproven_release(BlockHash::new([77; 32]), 2, at)
            .unwrap();
        state.brn.wallets.remove(&removed);
        state
            .brn
            .track_wallet(added, burst_brn::BrnWalletState::new(at));
        assert!(state.brn.wallets.visit_recorded(|_, _, _| Ok(())).is_err());
        let after =
            Rows::capture_projection_records(&state, at, directory.path(), true, false, true)
                .unwrap();
        let partial =
            Rows::capture_projection_records(&state, at, directory.path(), false, false, true)
                .unwrap();
        partial
            .values
            .visit(|table, key, _| {
                assert!(
                    !(table == UndoTable::BrnWallets
                        || table == UndoTable::BrnMeta
                            && key.starts_with(burst_brn::wallet_storage::ROW_PREFIX))
                );
                Ok(())
            })
            .unwrap();
        let mut changes = Vec::new();
        visit_with_wallet_records(
            &state,
            &before.trst,
            &after.trst,
            at,
            &HashSet::new(),
            Some(&before.wallets),
            |row| {
                changes.push(row);
                Ok(())
            },
        )
        .unwrap();
        changes.sort_by(|a, b| (a.table, &a.key).cmp(&(b.table, &b.key)));
        // Changed wallet: scalar/count header and one added release. Removed
        // wallet: header and all 1024 proofs. New wallet: header only.
        assert_eq!(changes.len(), 1028);
        let changed_prefix = burst_brn::wallet_storage::row_prefix(&changed).unwrap();
        assert_eq!(
            changes
                .iter()
                .filter(
                    |row| row.table == UndoTable::BrnMeta && row.key.starts_with(&changed_prefix)
                )
                .count(),
            1
        );
        compare_full_projection(&before, &after, &changes).unwrap();
        let mut missing = changes.clone();
        missing.retain(|row| {
            !(row.table == UndoTable::BrnMeta && row.key.starts_with(&changed_prefix))
        });
        assert!(
            compare_full_projection(&before, &after, &missing).is_err(),
            "the full oracle must include streamed proof records"
        );
    }

    #[test]
    fn minted_index_recording_matches_full_projection_without_old_mints() {
        let mut state = crate::confirmed_replay::snapshot::SnapshotContinuation::from_genesis(
            NetworkId::Dev,
            &ProtocolParams::default(),
        )
        .unwrap()
        .state()
        .clone();
        let directory = tempfile::tempdir().unwrap();
        let at = Timestamp::new(100);
        for n in 0..32 {
            state
                .record_minted_burn(TxHash::new([n; 32]), Timestamp::EPOCH)
                .unwrap();
        }
        let before = Rows::capture(&state, at, directory.path(), false).unwrap();
        state.mutations = Some(Default::default());
        state.brn.wallets.record_transition(true);
        state.record_minted_burn(TxHash::new([67; 32]), at).unwrap();
        let after = Rows::capture(&state, at, directory.path(), false).unwrap();
        let mut changes = Vec::new();
        visit(
            &state,
            &before.trst,
            &after.trst,
            at,
            &HashSet::new(),
            |change| {
                changes.push(change);
                Ok(())
            },
        )
        .unwrap();
        changes.sort_by(|a, b| (a.table, &a.key).cmp(&(b.table, &b.key)));
        assert_eq!(changes.len(), 3);
        assert!(changes.iter().all(|row| row.before.is_none()));
        compare_full_projection(&before, &after, &changes).unwrap();
        Rows::capture_projection(&state, at, directory.path(), false, false)
            .unwrap()
            .values
            .visit(|table, _, _| {
                assert!(!matches!(
                    table,
                    UndoTable::TrstOrigins | UndoTable::TrstExpiry | UndoTable::TrstReverse
                ));
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn brn_recording_matches_full_projection_and_omits_untouched_wallets() {
        let mut state = crate::confirmed_replay::snapshot::SnapshotContinuation::from_genesis(
            NetworkId::Dev,
            &ProtocolParams::default(),
        )
        .unwrap()
        .state()
        .clone();
        let directory = tempfile::tempdir().unwrap();
        let at = Timestamp::new(100);
        let changed = WalletAddress::new("brst_changed");
        let removed = WalletAddress::new("brst_removed");
        for address in [&changed, &removed] {
            state.brn.track_wallet(
                address.clone(),
                burst_brn::BrnWalletState::new(Timestamp::EPOCH),
            );
        }
        for i in 0..100 {
            state.brn.track_wallet(
                WalletAddress::new(format!("brst_untouched_{i}")),
                burst_brn::BrnWalletState::new(Timestamp::EPOCH),
            );
        }
        let before = Rows::capture(&state, at, directory.path(), false).unwrap();
        state.mutations = Some(Default::default());
        state.brn.wallets.record_transition(true);
        let mut wallet = state.brn.wallets.remove(&changed).unwrap();
        state.brn.record_burn(&mut wallet, 1, at).unwrap();
        state.brn.wallets.insert(changed.clone(), wallet);
        state.brn.deactivate_wallet(&changed, at).unwrap();
        state.brn.wallets.remove(&removed);
        let added = WalletAddress::new("brst_added");
        state
            .brn
            .track_wallet(added, burst_brn::BrnWalletState::new(at));
        let after = Rows::capture(&state, at, directory.path(), false).unwrap();
        let partial = Rows::capture_projection(&state, at, directory.path(), false, false).unwrap();
        partial
            .values
            .visit(|table, _, _| {
                assert_ne!(table, UndoTable::BrnWallets);
                Ok(())
            })
            .unwrap();
        let mut changes = Vec::new();
        visit(
            &state,
            &before.trst,
            &after.trst,
            at,
            &HashSet::new(),
            |change| {
                changes.push(change);
                Ok(())
            },
        )
        .unwrap();
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
        let expired = expiry::ExpiryView::new(&trst, 100, at).unwrap().trst;
        let active = expiry::ExpiryView::new(&trst, 10000, at).unwrap().trst;
        let mut revoked = active.clone();
        revoked.revoke_by_originator(&origin);
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
    let physical_verification = changes
        .iter()
        .any(|row| row.table == UndoTable::Meta && row.key.starts_with(b"verification_state/"));
    let physical_governance = changes
        .iter()
        .any(|row| row.table == UndoTable::Meta && row.key.starts_with(b"governance_state/"));
    let included = |table, key: &[u8]| {
        owns(table, key)
            || aggregates::owns(table, key)
            || (table == UndoTable::Meta
                && (key.starts_with(b"verification_state/")
                    || key.starts_with(b"governance_state/")))
    };
    let full = |rows: &Rows| -> Result<std::collections::BTreeMap<_, _>, ReplayError> {
        let mut values = std::collections::BTreeMap::new();
        rows.values
            .visit(|table, key, value| {
                if included(table, &key) && values.insert((table, key), value).is_some() {
                    return Err(invalid("duplicate full projection row"));
                }
                Ok(())
            })
            .map_err(replay_error)?;
        if physical_governance {
            let bytes = values
                .remove(&(UndoTable::BrnMeta, b"governance_engine".to_vec()))
                .ok_or_else(|| {
                    crate::confirmed_replay::invalid("full oracle omitted governance")
                })?;
            let engine: burst_governance::GovernanceEngine =
                bincode::deserialize(&bytes).map_err(replay_error)?;
            engine
                .visit_records(|row| {
                    let (key, value) = row.storage_entry()?;
                    if values
                        .insert((UndoTable::Meta, key.into_bytes()), value)
                        .is_some()
                    {
                        return Err("duplicate full governance row".to_owned());
                    }
                    Ok(())
                })
                .map_err(replay_error)?;
        }
        if physical_verification {
            let bytes = values
                .remove(&(UndoTable::Meta, b"verification_orchestrator".to_vec()))
                .ok_or_else(|| {
                    crate::confirmed_replay::invalid("full oracle omitted verification")
                })?;
            let snapshot =
                burst_verification::OrchestratorSnapshot::decode(&bytes).map_err(replay_error)?;
            let engine = burst_verification::VerificationOrchestrator::restore(snapshot);
            // Independently enumerate complete states rather than using the delta selector.
            burst_verification::orchestrator::records::normalized::visit_state_rows(
                &engine,
                |key, value| {
                    if values
                        .insert((UndoTable::Meta, key.into_bytes()), value)
                        .is_some()
                    {
                        return Err("duplicate full verification row".to_owned());
                    }
                    Ok(())
                },
            )
            .map_err(replay_error)?;
        }
        if rows.wallet_records {
            if !rows.complete_wallets {
                return Err(crate::confirmed_replay::invalid(
                    "full oracle requires complete wallets",
                ));
            }
            for (address, wallet) in &rows.wallets {
                // Compare each complete wallet against an empty wallet.
                visit_wallet_delta(address, None, Some(wallet), |change| {
                    if values
                        .insert(
                            (change.table, change.key),
                            change.after.expect("full wallet row"),
                        )
                        .is_some()
                    {
                        return Err(crate::confirmed_replay::invalid(
                            "duplicate full wallet row",
                        ));
                    }
                    Ok(())
                })?;
            }
        }
        Ok(values)
    };
    let before = full(before)?;
    let after = full(after)?;
    let mut expected = Vec::new();
    for (position, old) in &before {
        let new = after.get(position);
        if new != Some(old) {
            expected.push(UndoChange {
                table: position.0,
                key: position.1.clone(),
                before: Some(old.clone()),
                after: new.cloned(),
            });
        }
    }
    for (position, new) in &after {
        if !before.contains_key(position) {
            expected.push(UndoChange {
                table: position.0,
                key: position.1.clone(),
                before: None,
                after: Some(new.clone()),
            });
        }
    }
    expected.sort_by(|a, b| (a.table, &a.key).cmp(&(b.table, &b.key)));
    let actual: Vec<_> = changes
        .iter()
        .filter(|row| included(row.table, &row.key))
        .cloned()
        .collect();
    if expected != actual {
        return Err(crate::confirmed_replay::invalid(
            "transition mutations differ from full recovery projection",
        ));
    }
    Ok(())
}
