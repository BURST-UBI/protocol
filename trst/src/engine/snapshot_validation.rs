use super::*;

impl WalletPortfolio {
    /// Check cached state without reclassifying tokens against a local clock.
    /// Expiry changes still belong to the agreed economic transition.
    pub(super) fn validate_snapshot(&self) -> Result<(), String> {
        let mut ids = HashSet::new();
        let mut active = 0u128;
        let mut previous = None;
        let mut has_active = false;
        for token in &self.tokens {
            if !ids.insert(token.id) {
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
    fn corrupted_portfolio_caches_and_token_state_are_rejected() {
        for mutation in 0..9 {
            let mut engine = fixture();
            let portfolio = engine.wallets.values_mut().next().unwrap();
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
        after.set_expiry_period(200, Timestamp::new(150));
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
                    engine.revoke_by_origin(&owner);
                }
                2 => {
                    engine.un_revoke_by_origin(&owner, Timestamp::new(205));
                }
                3 => {
                    engine.set_expiry_period(200, Timestamp::new(205));
                }
                _ => {
                    engine.set_expiry_period(0, Timestamp::new(205));
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
                    corrupt.wallets.values_mut().next().unwrap().tokens[0].origin_wallet =
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
