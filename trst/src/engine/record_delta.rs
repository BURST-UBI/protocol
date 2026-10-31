use super::*;

impl TrstEngine {
    /// Visit changed persisted records without encoding untouched portfolios.
    /// Persistent map diffs skip shared subtrees. Changed portfolios materialize
    /// canonical vectors and compare positional rows; inserting near the front
    /// can change every later row. This is not the stable holding-ID adapter.
    /// Callback errors stop emission without undoing earlier effects. Callers
    /// validate both states and apply the complete delta transactionally.
    pub fn visit_wallet_record_delta<E>(
        &self,
        before: &Self,
        mut visit: impl FnMut(Option<WalletRecord>, Option<WalletRecord>) -> Result<(), E>,
    ) -> Result<(), E> {
        self.visit_portfolio_changes(before, |holder, old, new| {
            let header = |portfolio: &WalletPortfolio| WalletRecord::Portfolio {
                holder: holder.clone(),
                cached_transferable: portfolio.cached_transferable,
                earliest_expiry: portfolio.earliest_expiry,
            };
            if old.map(|p| (p.cached_transferable, p.earliest_expiry))
                != new.map(|p| (p.cached_transferable, p.earliest_expiry))
            {
                visit(old.map(header), new.map(header))?;
            }
            let count = old
                .map_or(0, |p| p.tokens.len())
                .max(new.map_or(0, |p| p.tokens.len()));
            for position in 0..count {
                let a = old.and_then(|p| p.tokens.get(position));
                let b = new.and_then(|p| p.tokens.get(position));
                if a != b {
                    let holding = |token: &TrstToken| WalletRecord::Holding {
                        holder: holder.clone(),
                        position: position as u64,
                        token: Box::new(token.clone()),
                    };
                    visit(a.map(holding), b.map(holding))?;
                }
            }
            Ok::<_, E>(())
        })?;
        self.visit_wallet_metadata_delta(before, visit)
    }

    /// Visit changed portfolios without cloning tokens or generating positional
    /// rows. Storage adapters choose their physical representation inside the
    /// callback. Shared persistent subtrees and portfolio roots are skipped.
    /// The callback can see equal portfolios with different storage identities;
    /// it must decide which physical rows changed. Errors do not undo callbacks.
    pub fn visit_portfolio_changes<E>(
        &self,
        before: &Self,
        mut visit: impl FnMut(
            &WalletAddress,
            Option<&WalletPortfolio>,
            Option<&WalletPortfolio>,
        ) -> Result<(), E>,
    ) -> Result<(), E> {
        if !self.wallets.shares_storage_with(&before.wallets) {
            for holder in self.wallets.changed_keys(&before.wallets) {
                let old = before.wallets.get(holder);
                let new = self.wallets.get(holder);
                if old.zip(new).is_some_and(|(a, b)| a.shares_storage_with(b)) {
                    continue;
                }
                visit(holder, old.map(|p| &**p), new.map(|p| &**p))?;
            }
        }
        Ok(())
    }

    /// Origin ownership and invalidation records keep their canonical keys even
    /// when a local storage adapter uses a different portfolio representation.
    /// This visitor never enumerates or clones portfolio tokens. Ownership moves
    /// pair old and new records by burn ID, assuming each state has unique burn
    /// ownership. Invalid duplicate ownership is not detected here. Cause records
    /// have no guaranteed visitation order; this is not a canonical byte stream.
    pub fn visit_wallet_metadata_delta<E>(
        &self,
        before: &Self,
        mut visit: impl FnMut(Option<WalletRecord>, Option<WalletRecord>) -> Result<(), E>,
    ) -> Result<(), E> {
        if !self
            .wallet_origins
            .shares_storage_with(&before.wallet_origins)
        {
            let mut old_origins = std::collections::BTreeMap::new();
            let mut new_origins = std::collections::BTreeMap::new();
            for owner in self.wallet_origins.changed_keys(&before.wallet_origins) {
                let old = before.wallet_origins.get(owner);
                let new = self.wallet_origins.get(owner);
                if old == new {
                    continue;
                }
                if old.is_some() != new.is_some() {
                    visit(
                        old.map(|_| WalletRecord::OriginOwner(owner.clone())),
                        new.map(|_| WalletRecord::OriginOwner(owner.clone())),
                    )?;
                }
                match (old, new) {
                    (Some(old), Some(new)) => {
                        for origin in new.changed_keys(old) {
                            if old.contains(origin) {
                                old_origins.insert(*origin.as_bytes(), (owner.clone(), *origin));
                            } else {
                                new_origins.insert(*origin.as_bytes(), (owner.clone(), *origin));
                            }
                        }
                    }
                    (Some(old), None) => {
                        for origin in old {
                            old_origins.insert(*origin.as_bytes(), (owner.clone(), *origin));
                        }
                    }
                    (None, Some(new)) => {
                        for origin in new {
                            new_origins.insert(*origin.as_bytes(), (owner.clone(), *origin));
                        }
                    }
                    (None, None) => {}
                }
            }
            let record = |(owner, origin)| WalletRecord::Origin { owner, origin };
            for (key, value) in old_origins {
                visit(Some(record(value)), new_origins.remove(&key).map(record))?;
            }
            for (_, value) in new_origins {
                visit(None, Some(record(value)))?;
            }
        }
        if !self
            .outcome_invalidations
            .shares_storage_with(&before.outcome_invalidations)
        {
            for origin in self
                .outcome_invalidations
                .changed_keys(&before.outcome_invalidations)
            {
                let old = before.outcome_invalidations.get(origin);
                let new = self.outcome_invalidations.get(origin);
                let header = |entry: &OutcomeInvalidation| WalletRecord::Invalidation {
                    origin: *origin,
                    wallet_revoked: entry.wallet_revoked,
                };
                if old.map(|entry| entry.wallet_revoked) != new.map(|entry| entry.wallet_revoked) {
                    visit(old.map(header), new.map(header))?;
                }
                for cause in old.into_iter().flat_map(|entry| &entry.causes) {
                    if !new.is_some_and(|entry| entry.causes.contains(cause)) {
                        visit(
                            Some(WalletRecord::Cause {
                                origin: *origin,
                                cause: *cause,
                            }),
                            None,
                        )?;
                    }
                }
                for cause in new.into_iter().flat_map(|entry| &entry.causes) {
                    if !old.is_some_and(|entry| entry.causes.contains(cause)) {
                        visit(
                            None,
                            Some(WalletRecord::Cause {
                                origin: *origin,
                                cause: *cause,
                            }),
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn rows(engine: &TrstEngine) -> BTreeMap<String, Vec<u8>> {
        let mut rows = BTreeMap::new();
        engine
            .visit_wallet_records(|record| {
                let (key, value) = record.storage_entry()?;
                assert!(rows.insert(key, value).is_none());
                Ok::<_, String>(())
            })
            .unwrap();
        rows
    }

    fn compare_stable_adapter(before: &TrstEngine, after: &TrstEngine) {
        use super::holding_rows;
        let mut portfolios = before.wallets.clone();
        after
            .visit_portfolio_changes(before, |holder, old, new| {
                let mut entries: BTreeMap<_, _> = holding_rows::delta(holder, None, old)?
                    .into_iter()
                    .map(|(_, row)| row.unwrap().storage_entry())
                    .collect::<Result<_, _>>()?;
                for (old, new) in holding_rows::delta(holder, old, new)? {
                    let old = old.map(|row| row.storage_entry()).transpose()?;
                    let new = new.map(|row| row.storage_entry()).transpose()?;
                    let key = new.as_ref().or(old.as_ref()).unwrap().0.clone();
                    if let (Some(old), Some(new)) = (&old, &new) {
                        assert_eq!(old.0, new.0);
                    }
                    assert_eq!(entries.get(&key), old.as_ref().map(|(_, value)| value));
                    if let Some((key, value)) = new {
                        entries.insert(key, value);
                    } else {
                        entries.remove(&key);
                    }
                }
                if new.is_some() {
                    let key = holding_rows::portfolio_keys(holder).0;
                    let header = holding_rows::Row::from_storage_entry(
                        &key,
                        &entries.remove(&key).unwrap(),
                    )?;
                    let restored = holding_rows::restore(
                        holder,
                        header,
                        entries
                            .iter()
                            .map(|(key, value)| holding_rows::Row::from_storage_entry(key, value)),
                        after.expiry_secs,
                    )?;
                    assert_eq!(Some(&restored), new);
                    portfolios.insert(holder.clone(), Shared::from(restored));
                } else {
                    assert!(entries.is_empty());
                    portfolios.remove(holder);
                }
                Ok::<_, String>(())
            })
            .unwrap();
        assert_eq!(portfolios, after.wallets);

        let metadata = |engine: &TrstEngine| {
            rows(engine)
                .into_iter()
                .filter(|(key, _)| {
                    !key.starts_with(&format!("{}0/", WalletRecord::STORAGE_PREFIX))
                        && !key.starts_with(&format!("{}1/", WalletRecord::STORAGE_PREFIX))
                })
                .collect::<BTreeMap<_, _>>()
        };
        let mut actual = metadata(before);
        after
            .visit_wallet_metadata_delta(before, |old, new| {
                for row in old.iter().chain(&new) {
                    assert!(!matches!(
                        row,
                        WalletRecord::Portfolio { .. } | WalletRecord::Holding { .. }
                    ));
                }
                let old = old.map(|row| row.storage_entry()).transpose()?;
                let new = new.map(|row| row.storage_entry()).transpose()?;
                let key = new.as_ref().or(old.as_ref()).unwrap().0.clone();
                if let (Some(old), Some(new)) = (&old, &new) {
                    assert_eq!(old.0, new.0);
                }
                assert_eq!(actual.get(&key), old.as_ref().map(|(_, value)| value));
                if let Some((key, value)) = new {
                    actual.insert(key, value);
                } else {
                    actual.remove(&key);
                }
                Ok::<_, String>(())
            })
            .unwrap();
        assert_eq!(actual, metadata(after));
    }

    fn compare(before: &TrstEngine, after: &TrstEngine) -> usize {
        compare_stable_adapter(before, after);
        let mut actual = rows(before);
        let mut count = 0;
        after
            .visit_wallet_record_delta(before, |old, new| {
                count += 1;
                let old = old.map(|record| record.storage_entry().unwrap());
                let new = new.map(|record| record.storage_entry().unwrap());
                let key = new.as_ref().or(old.as_ref()).unwrap().0.clone();
                assert_eq!(actual.get(&key), old.as_ref().map(|(_, bytes)| bytes));
                if let Some((_, value)) = new {
                    actual.insert(key, value);
                } else {
                    actual.remove(&key);
                }
                Ok::<_, String>(())
            })
            .unwrap();
        assert_eq!(actual, rows(after));
        let mut restored = TrstEngine::from_wallet_records(
            actual
                .iter()
                .map(|(key, value)| WalletRecord::from_storage_entry(key, value)),
            after.expiry_secs,
        )
        .unwrap();
        restored.merger_graph = after.merger_graph.clone();
        restored.validate_bases().unwrap();
        assert_eq!(restored.save_wallets(), after.save_wallets());
        count
    }

    #[test]
    fn record_deltas_match_full_state_across_expiry_and_appeals() {
        let mut state = TrstEngine::with_expiry(100);
        for seed in 1..=32 {
            let owner = WalletAddress::new(format!("brst_owner_{seed}"));
            let token = state
                .mint(
                    TxHash::new([seed; 32]),
                    owner.clone(),
                    50,
                    owner,
                    Timestamp::new(100),
                )
                .unwrap();
            state.track_token(token);
        }
        assert_eq!(compare(&state, &state.clone()), 0);
        let owner = WalletAddress::new("brst_owner_1");
        let origins = [(owner.clone(), TxHash::new([1; 32]))];
        let cause = BlockHash::new([90; 32]);
        for step in 0..7 {
            let before = state.clone();
            match step {
                0 => {
                    state.invalidate_outcome_origins(cause, &origins).unwrap();
                }
                1 => {
                    state
                        .invalidate_outcome_origins(BlockHash::new([91; 32]), &origins)
                        .unwrap();
                }
                2 => {
                    state
                        .restore_outcome_origins(cause, &origins, Timestamp::new(150))
                        .unwrap();
                }
                3 => {
                    state
                        .restore_outcome_origins(
                            BlockHash::new([91; 32]),
                            &origins,
                            Timestamp::new(150),
                        )
                        .unwrap();
                }
                4 => {
                    state.flush_all_expired(Timestamp::new(201));
                }
                5 => {
                    state.set_expiry_period(200, Timestamp::new(201)).unwrap();
                }
                _ => {
                    state.wallets.remove(&owner);
                    state.wallet_origins.remove(&owner);
                }
            }
            let changed = compare(&before, &state);
            assert!(changed > 0);
            if step == 1 || step == 2 {
                assert_eq!(changed, 1, "only one independent cause changed");
            }
        }
        compare(&TrstEngine::with_expiry(200), &state);
    }

    #[test]
    fn creator_history_extension_emits_only_the_new_origin() {
        let owner = WalletAddress::new("brst_history_owner");
        let mut before = TrstEngine::new();
        let origins: SharedSet<_> = (0u64..10_000)
            .map(|n| {
                let mut bytes = [0u8; 32];
                bytes[..8].copy_from_slice(&n.to_be_bytes());
                TxHash::new(bytes)
            })
            .collect();
        before.wallet_origins.insert(owner.clone(), origins);
        let mut after = before.clone();
        let added = TxHash::new([255; 32]);
        after
            .mint(
                added,
                WalletAddress::new("brst_receiver"),
                1,
                owner.clone(),
                Timestamp::new(1),
            )
            .unwrap();
        assert!(!before.wallet_origins[&owner].contains(&added));
        assert_eq!(
            after.wallet_origins[&owner]
                .changed_keys(&before.wallet_origins[&owner])
                .count(),
            1
        );
        assert_eq!(compare(&before, &after), 1);
        let restored = TrstEngine::load_wallets(&after.save_wallets(), after.expiry_secs).unwrap();
        assert_eq!(restored.save_wallets(), after.save_wallets());
    }

    #[test]
    #[ignore = "explicit capped-host creator-history scaling probe"]
    fn creator_history_mutation_and_delta_scaling() {
        use std::time::Instant;
        let owner = WalletAddress::new("brst_history_owner");
        for population in [100u64, 1_000, 10_000, 100_000] {
            let mut before = TrstEngine::new();
            let origins: SharedSet<_> = (0..population)
                .map(|n| {
                    let mut bytes = [0u8; 32];
                    bytes[..8].copy_from_slice(&n.to_be_bytes());
                    TxHash::new(bytes)
                })
                .collect();
            before.wallet_origins.insert(owner.clone(), origins);
            for replace in [false, true] {
                for sample in 0..5 {
                    let added = TxHash::new([255; 32]);
                    let start = Instant::now();
                    let mut after = before.clone();
                    if replace {
                        after
                            .wallet_origins
                            .get_mut(&owner)
                            .unwrap()
                            .remove(&TxHash::ZERO);
                    }
                    after
                        .mint(
                            added,
                            WalletAddress::new("brst_receiver"),
                            1,
                            owner.clone(),
                            Timestamp::new(1),
                        )
                        .unwrap();
                    let mutation_ns = start.elapsed().as_nanos();
                    let mut changes = Vec::new();
                    let start = Instant::now();
                    after
                        .visit_wallet_record_delta(&before, |old, new| {
                            changes.push((old, new));
                            Ok::<_, ()>(())
                        })
                        .unwrap();
                    let delta_ns = start.elapsed().as_nanos();
                    assert_eq!(changes.len(), if replace { 2 } else { 1 });
                    assert!(!before.wallet_origins[&owner].contains(&added));
                    assert!(before.wallet_origins[&owner].contains(&TxHash::ZERO));
                    let encoded_bytes: usize = changes
                        .iter()
                        .flat_map(|(a, b)| a.iter().chain(b.iter()))
                        .map(|record| {
                            let (key, value) = record.storage_entry().unwrap();
                            key.len() + value.len()
                        })
                        .sum();
                    println!("creator_history_scale population={population} replace={replace} sample={sample} mutation_ns={mutation_ns} delta_ns={delta_ns} changed_records={} encoded_bytes={encoded_bytes}", changes.len());
                }
            }
        }
    }

    #[test]
    fn record_deltas_preserve_empty_entries_and_origin_ownership_moves() {
        let before = TrstEngine::with_expiry(100);
        let mut after = before.clone();
        let first = WalletAddress::new("brst_first");
        let second = WalletAddress::new("brst_second");
        after.wallets.insert(first.clone(), Shared::default());
        after
            .wallet_origins
            .insert(first.clone(), SharedSet::default());
        assert_eq!(compare(&before, &after), 2);
        let before = after.clone();
        after
            .wallet_origins
            .get_mut(&first)
            .unwrap()
            .insert(TxHash::new([1; 32]));
        compare(&before, &after);
        let before = after.clone();
        let origins = after.wallet_origins.remove(&first).unwrap();
        after.wallet_origins.insert(second, origins);
        compare(&before, &after);
        compare(&after, &TrstEngine::with_expiry(100));
        let mut visited = 0;
        assert!(after
            .visit_wallet_record_delta(&TrstEngine::with_expiry(100), |_, _| {
                visited += 1;
                Err::<(), _>("stop")
            })
            .is_err());
        assert_eq!(visited, 1);
    }

    #[test]
    fn positional_holding_rows_expose_shift_amplification() {
        fn id(n: u64) -> TxHash {
            let mut bytes = [0u8; 32];
            bytes[..8].copy_from_slice(&n.to_be_bytes());
            TxHash::new(bytes)
        }
        fn holding_changes(before: &TrstEngine, after: &TrstEngine) -> (usize, usize) {
            let mut count = 0;
            let mut bytes = 0;
            after
                .visit_wallet_record_delta(before, |old, new| {
                    if old
                        .as_ref()
                        .or(new.as_ref())
                        .is_some_and(|row| matches!(row, WalletRecord::Holding { .. }))
                    {
                        count += 1;
                        for row in old.iter().chain(new.iter()) {
                            let (key, value) = row.storage_entry().unwrap();
                            bytes += key.len() + value.len();
                        }
                    }
                    Ok::<_, ()>(())
                })
                .unwrap();
            // Check reconstruction as well as the number of positional rows changed.
            compare(before, after);
            (count, bytes)
        }
        let holder = WalletAddress::new("brst_row_cost_holder");
        for population in [32u64, 256, 1024] {
            let mut state = TrstEngine::with_expiry(u64::MAX);
            for n in 1..=population {
                let token = state
                    .mint(
                        id(n),
                        holder.clone(),
                        1,
                        holder.clone(),
                        Timestamp::new(100 + n),
                    )
                    .unwrap();
                state.track_token(token);
            }
            for (operation, timestamp, expected) in [
                ("append", 101 + population, 1),
                ("front_insert", 1, population + 1),
                // An insertion tied with the current oldest token goes after it.
                ("oldest_tie_insert", 101, population),
            ] {
                let before = state.clone();
                let mut after = before.clone();
                let token = after
                    .mint(
                        id(population + 1),
                        holder.clone(),
                        1,
                        holder.clone(),
                        Timestamp::new(timestamp),
                    )
                    .unwrap();
                after.track_token(token);
                let (rows, encoded_bytes) = holding_changes(&before, &after);
                assert_eq!(
                    rows as u64, expected,
                    "{operation}, population {population}"
                );
                println!("holding_row_cost population={population} operation={operation} rows={rows} old_plus_new_encoded_bytes={encoded_bytes}");
            }
            let before = state.clone();
            let mut after = before.clone();
            after.bulk_untrack(&holder, &[id(1)].into_iter().collect());
            let (rows, encoded_bytes) = holding_changes(&before, &after);
            assert_eq!(rows as u64, population);
            println!("holding_row_cost population={population} operation=front_delete rows={rows} old_plus_new_encoded_bytes={encoded_bytes}");
        }
    }

    #[test]
    fn record_deltas_preserve_merge_splits_and_restoration() {
        let mut state = TrstEngine::with_expiry(u64::MAX);
        let holder = WalletAddress::new("brst_holder");
        let owners = [
            WalletAddress::new("brst_first"),
            WalletAddress::new("brst_second"),
        ];
        let mut tokens = Vec::new();
        // Receive an older origin after a newer one, shifting holding positions.
        for (id, at, amount) in [(1u8, 110, 60), (2, 100, 40)] {
            let before = state.clone();
            let token = state
                .mint(
                    TxHash::new([id; 32]),
                    holder.clone(),
                    amount,
                    owners[(id - 1) as usize].clone(),
                    Timestamp::new(at),
                )
                .unwrap();
            state.track_token(token.clone());
            tokens.push(token);
            compare(&before, &state);
        }
        let before = state.clone();
        let merged = state
            .merge(
                &tokens,
                holder.clone(),
                TxHash::new([3; 32]),
                Timestamp::new(150),
            )
            .unwrap();
        state.bulk_untrack(&holder, &tokens.iter().map(|token| token.id).collect());
        state.track_token(merged);
        compare(&before, &state);
        let before = state.clone();
        state.revoke_by_originator(&owners[0]);
        compare(&before, &state);
        assert_eq!(state.wallets[&holder].cached_transferable, 40);
        let before = state.clone();
        state
            .restore_by_originator(&owners[0], Timestamp::new(160))
            .unwrap();
        compare(&before, &state);
        assert_eq!(state.wallets[&holder].cached_transferable, 100);
    }
}
