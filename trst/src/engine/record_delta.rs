use super::*;

impl TrstEngine {
    /// Visit changed persisted records without encoding untouched portfolios.
    /// Shared maps/portfolios skip comparison; detached maps still require a
    /// key scan. This is not a parallel preparation or dependency-lock protocol.
    pub fn visit_wallet_record_delta<E>(
        &self,
        before: &Self,
        mut visit: impl FnMut(Option<WalletRecord>, Option<WalletRecord>) -> Result<(), E>,
    ) -> Result<(), E> {
        if !self.wallets.shares_storage_with(&before.wallets) {
            for holder in before.wallets.keys().chain(
                self.wallets
                    .keys()
                    .filter(|key| !before.wallets.contains_key(*key)),
            ) {
                let old = before.wallets.get(holder);
                let new = self.wallets.get(holder);
                if old.zip(new).is_some_and(|(a, b)| a.shares_storage_with(b)) {
                    continue;
                }
                let header = |portfolio: &Shared<WalletPortfolio>| WalletRecord::Portfolio {
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
            }
        }
        if !self
            .wallet_origins
            .shares_storage_with(&before.wallet_origins)
        {
            let mut old_origins = std::collections::BTreeMap::new();
            let mut new_origins = std::collections::BTreeMap::new();
            for owner in before.wallet_origins.keys().chain(
                self.wallet_origins
                    .keys()
                    .filter(|key| !before.wallet_origins.contains_key(*key)),
            ) {
                let a = before.wallet_origins.get(owner);
                let b = self.wallet_origins.get(owner);
                if a == b {
                    continue;
                }
                if a.is_some() != b.is_some() {
                    visit(
                        a.map(|_| WalletRecord::OriginOwner(owner.clone())),
                        b.map(|_| WalletRecord::OriginOwner(owner.clone())),
                    )?;
                }
                for origin in a.into_iter().flatten() {
                    if !b.is_some_and(|set| set.contains(origin)) {
                        old_origins.insert(*origin.as_bytes(), (owner.clone(), *origin));
                    }
                }
                for origin in b.into_iter().flatten() {
                    if !a.is_some_and(|set| set.contains(origin)) {
                        new_origins.insert(*origin.as_bytes(), (owner.clone(), *origin));
                    }
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
            for origin in before.outcome_invalidations.keys().chain(
                self.outcome_invalidations
                    .keys()
                    .filter(|key| !before.outcome_invalidations.contains_key(*key)),
            ) {
                let a = before.outcome_invalidations.get(origin);
                let b = self.outcome_invalidations.get(origin);
                let header = |entry: &OutcomeInvalidation| WalletRecord::Invalidation {
                    origin: *origin,
                    wallet_revoked: entry.wallet_revoked,
                };
                if a.map(|entry| entry.wallet_revoked) != b.map(|entry| entry.wallet_revoked) {
                    visit(a.map(header), b.map(header))?;
                }
                for cause in a.into_iter().flat_map(|entry| &entry.causes) {
                    if !b.is_some_and(|entry| entry.causes.contains(cause)) {
                        visit(
                            Some(WalletRecord::Cause {
                                origin: *origin,
                                cause: *cause,
                            }),
                            None,
                        )?;
                    }
                }
                for cause in b.into_iter().flat_map(|entry| &entry.causes) {
                    if !a.is_some_and(|entry| entry.causes.contains(cause)) {
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

    fn compare(before: &TrstEngine, after: &TrstEngine) -> usize {
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
                    state.set_expiry_period(200, Timestamp::new(201));
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
    fn record_deltas_preserve_empty_entries_and_origin_ownership_moves() {
        let before = TrstEngine::with_expiry(100);
        let mut after = before.clone();
        let first = WalletAddress::new("brst_first");
        let second = WalletAddress::new("brst_second");
        after.wallets.insert(first.clone(), Shared::default());
        after.wallet_origins.insert(first.clone(), HashSet::new());
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
        state.revoke_by_origin(&owners[0]);
        compare(&before, &state);
        assert_eq!(state.wallets[&holder].cached_transferable, 40);
        let before = state.clone();
        state.un_revoke_by_origin(&owners[0], Timestamp::new(160));
        compare(&before, &state);
        assert_eq!(state.wallets[&holder].cached_transferable, 100);
    }
}
