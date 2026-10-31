use super::*;

impl WalletPortfolio {
    /// Check unique IDs, origin-time order, revocation tags and the active-amount
    /// sum. The expiry cache must be present exactly when an active token exists;
    /// this helper does not check its timestamp against an expiry period.
    /// It neither reclassifies tokens by local time nor authenticates provenance.
    pub(super) fn validate_snapshot(&self) -> Result<(), String> {
        self.validate_snapshot_with_poll(&|_| Ok(()))
    }

    pub(super) fn validate_snapshot_with_poll(
        &self,
        poll: &dyn Fn(&'static str) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut ids = HashSet::new();
        self.validate_snapshot_with_ids_with_poll(|_, token| ids.insert(token.id), poll)
    }

    /// The caller's index must return false for a repeated ID in this portfolio.
    /// The callback can retain that index for a later delta. Its updates are not
    /// rolled back if a later token or cache check fails; callers stage them.
    pub(super) fn validate_snapshot_with_ids(
        &self,
        insert_id: impl FnMut(usize, &TrstToken) -> bool,
    ) -> Result<(), String> {
        self.validate_snapshot_with_ids_with_poll(insert_id, &|_| Ok(()))
    }

    fn validate_snapshot_with_ids_with_poll(
        &self,
        mut insert_id: impl FnMut(usize, &TrstToken) -> bool,
        poll: &dyn Fn(&'static str) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut active = 0u128;
        let mut previous = None;
        let mut has_active = false;
        for (position, token) in self.tokens.iter().enumerate() {
            poll("trst:portfolio-token")?;
            if !insert_id(position, token) {
                return Err("duplicate TRST holding".into());
            }
            if previous.is_some_and(|timestamp| timestamp > token.origin_timestamp) {
                return Err("unordered TRST holdings".into());
            }
            previous = Some(token.origin_timestamp);
            if (token.state == TrstState::Revoked) != token.revoked_origin.is_some() {
                return Err("inconsistent TRST revocation state".into());
            }
            if token.state == TrstState::Active {
                has_active = true;
                active = active
                    .checked_add(token.amount)
                    .ok_or("TRST holding balance overflow")?;
            }
        }
        if active != self.cached_transferable {
            return Err("TRST holding balance cache mismatch".into());
        }
        if has_active != self.earliest_expiry.is_some() {
            return Err("TRST holding expiry cache mismatch".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> TrstEngine {
        let mut engine = TrstEngine::with_expiry(100);
        let holder = WalletAddress::new("brst_holder");
        for (id, timestamp) in [(1, 100), (2, 110)] {
            let token = engine
                .mint(
                    TxHash::new([id; 32]),
                    holder.clone(),
                    50,
                    holder.clone(),
                    Timestamp::new(timestamp),
                )
                .unwrap();
            engine.track_token(token);
        }
        engine
    }

    #[test]
    fn checked_basis_validation_can_stop_between_empty_wallets() {
        let mut engine = TrstEngine::with_expiry(100);
        for id in 0..3 {
            engine.wallets.insert(
                WalletAddress::new(format!("brst_empty_{id}")),
                WalletPortfolio::default().into(),
            );
        }
        engine.validate_bases().unwrap();
        let before = engine.save_wallets();
        let visits = std::cell::Cell::new(0);
        let result = engine.validate_bases_with_poll(&|stage| {
            if stage == "trst:basis-wallet" {
                visits.set(visits.get() + 1);
                if visits.get() == 2 {
                    return Err("empty wallet cancellation".into());
                }
            }
            Ok(())
        });
        assert_eq!(result, Err("empty wallet cancellation".into()));
        assert_eq!(visits.get(), 2);
        assert_eq!(engine.save_wallets(), before);
    }

    #[test]
    fn cancelled_index_rebuild_keeps_both_original_indexes() {
        let mut engine = fixture();
        engine.revoke_by_originator(&WalletAddress::new("brst_holder"));
        let origins = engine.origin_holders.clone();
        let revoked = engine.revoked_holders.clone();
        let visits = std::cell::Cell::new(0);
        let result = engine.rebuild_indexes_with_poll(&|stage| {
            if stage == "trst:index-token" {
                visits.set(visits.get() + 1);
                if visits.get() == 2 {
                    return Err("index cancellation".into());
                }
            }
            Ok(())
        });
        assert_eq!(result, Err("index cancellation".into()));
        assert_eq!(visits.get(), 2);
        assert_eq!(engine.origin_holders, origins);
        assert_eq!(engine.revoked_holders, revoked);
        engine.rebuild_indexes();
        assert_eq!(engine.origin_holders, origins);
        assert_eq!(engine.revoked_holders, revoked);
    }

    #[test]
    fn corrupted_portfolio_caches_and_token_state_are_rejected() {
        for mutation in 0..9 {
            let mut engine = fixture();
            let holder = engine.wallets.keys().next().unwrap().clone();
            let portfolio = engine.wallets.get_mut(&holder).unwrap();
            match mutation {
                0 => portfolio.cached_transferable += 1,
                1 => portfolio.earliest_expiry = None,
                2 => portfolio.tokens[1].id = portfolio.tokens[0].id,
                3 => portfolio.tokens.reverse(),
                4 => portfolio.tokens[0].state = TrstState::Revoked,
                5 => portfolio.tokens[0].revoked_origin = Some(TxHash::new([1; 32])),
                6 => portfolio.tokens[0].amount = u128::MAX,
                7 => portfolio.tokens.clear(),
                _ => portfolio.earliest_expiry = Some(Timestamp::new(999)),
            }
            assert!(
                TrstEngine::load_wallets(&engine.save_wallets(), 100).is_err(),
                "mutation {mutation}"
            );
        }
    }

    #[test]
    fn expiry_cache_is_checked_against_the_restored_governed_period() {
        let engine = fixture();
        let bytes = engine.save_wallets();
        assert!(TrstEngine::load_wallets(&bytes, 101).is_err());
        assert_eq!(
            TrstEngine::load_wallets(&bytes, 100)
                .unwrap()
                .save_wallets(),
            bytes
        );
        // Undo validates before/after images without pretending they used the
        // current parameter. The caller then restores the resulting period.
        let mut after = engine.clone();
        after.set_expiry_period(200, Timestamp::new(150)).unwrap();
        let restored = TrstEngine::rewind_wallet_snapshot(
            &bytes,
            &after.save_wallets(),
            &after.save_wallets(),
        )
        .unwrap();
        assert_eq!(restored, bytes);
        assert!(TrstEngine::load_wallets(&restored, 100).is_ok());
        assert!(TrstEngine::load_wallets(&restored, 200).is_err());
    }

    #[test]
    fn expiry_revocation_and_restoration_remain_loadable_without_local_time() {
        let mut engine = fixture();
        let owner = WalletAddress::new("brst_holder");
        for step in 0..5 {
            match step {
                0 => {
                    engine.flush_all_expired(Timestamp::new(205));
                }
                1 => {
                    engine.revoke_by_originator(&owner);
                }
                2 => {
                    engine
                        .restore_by_originator(&owner, Timestamp::new(205))
                        .unwrap();
                }
                3 => {
                    engine.set_expiry_period(200, Timestamp::new(205)).unwrap();
                }
                _ => {
                    engine.set_expiry_period(0, Timestamp::new(205)).unwrap();
                }
            }
            let bytes = engine.save_wallets();
            assert_eq!(
                TrstEngine::load_wallets(&bytes, engine.expiry_secs)
                    .unwrap()
                    .save_wallets(),
                bytes
            );
        }
    }

    #[test]
    fn combined_snapshot_requires_the_original_burn_ownership() {
        let engine = fixture();
        assert!(engine.validate_bases().is_ok());
        for mutation in 0..4 {
            let mut corrupt = engine.clone();
            match mutation {
                0 => corrupt.wallet_origins.clear(),
                1 => {
                    let holder = corrupt.wallets.keys().next().unwrap().clone();
                    corrupt.wallets.get_mut(&holder).unwrap().tokens[0].origin_wallet =
                        WalletAddress::new("brst_wrong_owner");
                }
                2 | 3 => {
                    corrupt.merger_graph.record_merge(MergeNode {
                        merge_tx: TxHash::new([if mutation == 2 { 10 } else { 1 }; 32]),
                        source_origins: vec![MergeSource {
                            origin: TxHash::new([if mutation == 2 { 99 } else { 2 }; 32]),
                            amount: 50,
                        }],
                        total_amount: 50,
                        revoked_contribs: HashMap::new(),
                    });
                }
                _ => unreachable!(),
            }
            assert!(corrupt.validate_bases().is_err(), "mutation {mutation}");
        }
    }
}
