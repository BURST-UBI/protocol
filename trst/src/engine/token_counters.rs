//! Expired and revoked face-amount queries with a bounded vector-scan cache.
//! The cache keeps weak portfolio identities and at most 256 wallet entries.
//! Eviction follows insertion order; cache hits do not refresh that order.
use super::*;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Weak;

const CAPACITY: usize = 256;

struct Entry {
    portfolio: Weak<WalletPortfolio>,
    period: u64,
    from: Timestamp,
    until: Option<Timestamp>,
    totals: (u128, u128),
}

#[derive(Default)]
pub(super) struct CounterCache {
    entries: BTreeMap<WalletAddress, Entry>,
    order: VecDeque<WalletAddress>,
}

impl TrstEngine {
    /// Return (expired nonrevoked amount, revoked amount) for the current holdings.
    /// Revoked holdings are counted only in the second total, regardless of age.
    /// Other holdings are classified by timestamp, not their stored expiry state.
    /// This query does not mutate holdings or reconstruct past ledger ownership.
    pub fn token_counters(
        &self,
        holder: &WalletAddress,
        now: Timestamp,
    ) -> Result<(u128, u128), String> {
        self.token_counter_window(holder, now).map(|(totals, _)| totals)
    }

    /// Counter totals and the next timestamp where they can change without a
    /// portfolio mutation, assuming the expiry period remains unchanged.
    /// Indexed portfolios query prefix sums; a vector-cache miss scans all holdings.
    /// Cache reuse requires the same portfolio identity and period, and a timestamp
    /// in the saved half-open interval. Missing wallets and sum overflow are errors.
    pub fn token_counter_window(
        &self,
        holder: &WalletAddress,
        now: Timestamp,
    ) -> Result<((u128, u128), Option<Timestamp>), String> {
        let portfolio = self.wallets.get(holder).ok_or("affected holder portfolio missing")?;
        if let Some(window) = portfolio.indexed()
            .filter(|root| root.matches_context(holder, self.expiry_secs))
            .and_then(|root| root.counter_window(now))
        {
            return Ok(window);
        }
        {
            let cache = self.counter_cache.lock().unwrap();
            if let Some(entry) = cache.entries.get(holder) {
                if entry.portfolio.ptr_eq(&portfolio.downgrade())
                    && entry.period == self.expiry_secs
                    && now >= entry.from
                    && entry.until.is_none_or(|until| now < until)
                {
                    return Ok((entry.totals, entry.until));
                }
            }
        }
        let mut expired = 0u128;
        let mut revoked = 0u128;
        let mut until = None;
        for token in &portfolio.tokens {
            if token.state == TrstState::Revoked {
                revoked = revoked.checked_add(token.amount).ok_or("revoked balance overflow")?;
            } else if token.is_expired(now, self.expiry_secs) {
                expired = expired.checked_add(token.amount).ok_or("expired balance overflow")?;
            } else {
                let expiry = token.earliest_expiry(self.expiry_secs);
                until = Some(until.map_or(expiry, |old: Timestamp| old.min(expiry)));
            }
        }
        let entry = Entry {
            portfolio: portfolio.downgrade(), period: self.expiry_secs,
            from: now, until, totals: (expired, revoked),
        };
        let mut cache = self.counter_cache.lock().unwrap();
        if !cache.entries.contains_key(holder) {
            if cache.entries.len() == CAPACITY {
                let oldest = cache.order.pop_front().expect("counter cache eviction order");
                cache.entries.remove(&oldest);
            }
            cache.order.push_back(holder.clone());
        }
        cache.entries.insert(holder.clone(), entry);
        Ok(((expired, revoked), until))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holder(n: usize) -> WalletAddress {
        WalletAddress::new(format!("brst_{n:060}"))
    }

    fn fixture() -> TrstEngine {
        let mut engine = TrstEngine::with_expiry(100);
        for n in 1..=4u8 {
            let mut token = engine.mint(TxHash::new([n; 32]), holder(1),
                n as u128 * 10, holder(2), Timestamp::new(n as u64 * 10)).unwrap();
            if n == 2 { token.state = TrstState::Expired; }
            if n == 3 { token.state = TrstState::Revoked; token.revoked_origin = Some(token.origin); }
            engine.track_token(token);
        }
        engine
    }

    fn reference(engine: &TrstEngine, now: u64) -> (u128, u128) {
        let mut totals = (0, 0);
        for token in &engine.wallets[&holder(1)].tokens {
            if token.state == TrstState::Revoked { totals.1 += token.amount; }
            else if token.is_expired(Timestamp::new(now), engine.expiry_secs) {
                totals.0 += token.amount;
            }
        }
        totals
    }

    #[test]
    fn counters_match_scan_across_time_rules_and_snapshot_restore() {
        let mut engine = fixture();
        for period in [100, 0, u64::MAX, 50, 200] {
            engine.set_expiry_period(period, Timestamp::new(150)).unwrap();
            for now in [0, 109, 110, 119, 120, 140, 141, u64::MAX, 100, 150] {
                assert_eq!(engine.token_counters(&holder(1), Timestamp::new(now)).unwrap(),
                    reference(&engine, now));
            }
        }
        let bytes = engine.save_wallets();
        let restored = TrstEngine::load_wallets(&bytes, engine.expiry_secs).unwrap();
        assert!(restored.counter_cache.lock().unwrap().entries.is_empty());
        assert_eq!(restored.token_counters(&holder(1), Timestamp::new(150)).unwrap(), reference(&engine, 150));
        assert_eq!(bytes, engine.save_wallets());
        assert_eq!(engine.token_counters(&holder(999), Timestamp::new(150)).unwrap_err(),
            "affected holder portfolio missing");
    }

    #[test]
    fn warm_counter_reuse_expires_and_candidate_mutation_is_isolated() {
        let engine = fixture();
        assert_eq!(engine.token_counters(&holder(1), Timestamp::new(100)).unwrap(), (0, 30));
        engine.token_counters(&holder(1), Timestamp::new(109)).unwrap();
        assert_eq!(engine.counter_cache.lock().unwrap().entries[&holder(1)].from, Timestamp::new(100));
        assert_eq!(engine.token_counters(&holder(1), Timestamp::new(110)).unwrap(), (10, 30));
        assert_eq!(engine.counter_cache.lock().unwrap().entries[&holder(1)].from, Timestamp::new(110));
        let mut candidate = engine.clone();
        candidate.wallets.get_mut(&holder(1)).unwrap().tokens[0].amount = 70;
        assert_eq!(candidate.token_counters(&holder(1), Timestamp::new(110)).unwrap(), (70, 30));
        assert_eq!(engine.token_counters(&holder(1), Timestamp::new(110)).unwrap(), (10, 30));
        assert_eq!(engine.token_counters(&holder(1), Timestamp::new(109)).unwrap(), (0, 30));
        // A unique root with only weak cache observers must also invalidate.
        drop(candidate);
        let mut engine = engine;
        let weak = engine.wallets[&holder(1)].downgrade();
        engine.wallets.get_mut(&holder(1)).unwrap().tokens[0].amount = 80;
        assert!(weak.upgrade().is_none());
        assert_eq!(engine.token_counters(&holder(1), Timestamp::new(110)).unwrap(), (80, 30));
    }

    #[test]
    fn counter_cache_is_bounded_does_not_retain_tokens_or_cache_overflow() {
        let mut engine = fixture();
        for n in 0..CAPACITY + 10 {
            let address = holder(n + 100);
            engine.wallets.insert(address.clone(), WalletPortfolio::default().into());
            engine.token_counters(&address, Timestamp::new(0)).unwrap();
            engine.token_counters(&address, Timestamp::new(1)).unwrap();
        }
        {
            let cache = engine.counter_cache.lock().unwrap();
            assert_eq!(cache.entries.len(), CAPACITY);
            assert_eq!(cache.order.len(), CAPACITY);
            assert!(!cache.entries.contains_key(&holder(100)));
        }
        engine.token_counters(&holder(1), Timestamp::new(150)).unwrap();
        let weak = engine.wallets[&holder(1)].downgrade();
        engine.wallets.remove(&holder(1));
        assert!(weak.upgrade().is_none());
        engine = fixture();
        engine.wallets.get_mut(&holder(1)).unwrap().tokens[0].amount = u128::MAX;
        assert_eq!(engine.token_counters(&holder(1), Timestamp::new(150)).unwrap_err(), "expired balance overflow");
        assert!(engine.counter_cache.lock().unwrap().entries.is_empty());
        for token in &mut engine.wallets.get_mut(&holder(1)).unwrap().tokens {
            token.state = TrstState::Revoked;
        }
        assert_eq!(engine.token_counters(&holder(1), Timestamp::new(0)).unwrap_err(), "revoked balance overflow");
        assert!(engine.counter_cache.lock().unwrap().entries.is_empty());
    }
}
