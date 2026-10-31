use super::*;
use std::collections::BinaryHeap;

impl WalletPortfolio {
    /// Clone holdings in requested ID order, using indexed lookups or one vector
    /// scan. Duplicate requests stay duplicated; missing IDs return an error.
    /// This selects inputs without checking merge count, state or provenance.
    pub fn select_merge_inputs(&self, ids: &[TxHash]) -> Result<Vec<TrstToken>, String> {
        if let Some(root) = self.indexed() {
            return ids
                .iter()
                .map(|id| {
                    root.get(id)
                        .cloned()
                        .ok_or_else(|| format!("merge input {id} is not held by this wallet"))
                })
                .collect();
        }
        let mut selected: HashMap<_, Option<&TrstToken>> =
            ids.iter().map(|id| (*id, None)).collect();
        let mut missing = selected.len();
        for token in &self.tokens {
            if missing == 0 {
                break;
            }
            if let Some(slot) = selected.get_mut(&token.id) {
                if slot.is_none() {
                    *slot = Some(token);
                    missing -= 1;
                }
            }
        }
        ids.iter()
            .map(|id| {
                selected[id]
                    .cloned()
                    .ok_or_else(|| format!("merge input {id} is not held by this wallet"))
            })
            .collect()
    }

    /// Select at most MAX_MERGE_SOURCES active, unexpired, untagged holdings.
    /// Their effective origin times must fall within expiry_secs / 10 of the
    /// oldest eligible holding. Order is timestamp, token ID, then vector position
    /// for duplicate IDs. The result may have fewer than two inputs.
    ///
    /// Indexed portfolios use the active-time range. The vector path scans all
    /// n holdings with a bounded heap: O(n log m) work and O(m) space, where
    /// m = MAX_MERGE_SOURCES. Neither path executes or authorizes a merge.
    pub fn select_expiry_merge_group(&self, now: Timestamp, expiry_secs: u64) -> Vec<TrstToken> {
        if let Some(root) = self.indexed() {
            return root.select_expiry_merge_group(now, expiry_secs);
        }
        let mut oldest = BinaryHeap::with_capacity(MAX_MERGE_SOURCES);
        for (position, token) in self.tokens.iter().enumerate() {
            if token.state != TrstState::Active
                || token.revoked_origin.is_some()
                || token.is_expired(now, expiry_secs)
            {
                continue;
            }
            let key = (
                token.effective_origin_timestamp,
                *token.id.as_bytes(),
                position,
            );
            if oldest.len() < MAX_MERGE_SOURCES {
                oldest.push(key);
            } else if oldest.peek().is_some_and(|last| key < *last) {
                *oldest.peek_mut().expect("full selection heap") = key;
            }
        }
        let oldest = oldest.into_sorted_vec();
        let Some((timestamp, _, _)) = oldest.first() else {
            return Vec::new();
        };
        let cutoff = timestamp.as_secs().saturating_add(expiry_secs / 10);
        oldest
            .into_iter()
            .take_while(|(timestamp, _, _)| timestamp.as_secs() <= cutoff)
            .map(|(_, _, position)| self.tokens[position].clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn portfolio(count: usize) -> WalletPortfolio {
        let mut engine = TrstEngine::new();
        let base = engine
            .mint(
                TxHash::new([1; 32]),
                WalletAddress::new("brst_holder"),
                1,
                WalletAddress::new("brst_origin"),
                Timestamp::new(100),
            )
            .unwrap();
        CanonicalPortfolio {
            tokens: (0..count)
                .map(|index| {
                    let mut id = [0; 32];
                    id[..8].copy_from_slice(&(index as u64).to_be_bytes());
                    TrstToken {
                        id: TxHash::new(id),
                        effective_origin_timestamp: Timestamp::new(100 + (index as u64 * 37 % 251)),
                        state: match index % 11 {
                            0 => TrstState::Expired,
                            1 => TrstState::Revoked,
                            _ => TrstState::Active,
                        },
                        revoked_origin: (index % 13 == 0).then_some(base.origin),
                        ..base.clone()
                    }
                })
                .collect(),
            ..Default::default()
        }
        .into()
    }

    #[test]
    fn bounded_selection_matches_full_stable_sort() {
        for count in [0, 1, 255, 256, 257, 4096] {
            let portfolio = portfolio(count);
            for expiry in [0, 1, 100, 1000, u64::MAX] {
                for now in [
                    Timestamp::EPOCH,
                    Timestamp::new(250),
                    Timestamp::new(10_000),
                ] {
                    let mut expected: Vec<_> = portfolio
                        .tokens
                        .iter()
                        .filter(|token| {
                            token.state == TrstState::Active
                                && token.revoked_origin.is_none()
                                && !token.is_expired(now, expiry)
                        })
                        .cloned()
                        .collect();
                    expected.sort_by_key(|token| {
                        (token.effective_origin_timestamp, *token.id.as_bytes())
                    });
                    if let Some(first) = expected.first() {
                        let cutoff = first
                            .effective_origin_timestamp
                            .as_secs()
                            .saturating_add(expiry / 10);
                        expected
                            .retain(|token| token.effective_origin_timestamp.as_secs() <= cutoff);
                    }
                    expected.truncate(MAX_MERGE_SOURCES);
                    assert_eq!(portfolio.select_expiry_merge_group(now, expiry), expected);
                }
            }
        }
    }

    #[test]
    fn bulk_removal_indexes_match_rebuild_with_shared_origins_and_revoked_tags() {
        let mut engine = TrstEngine::new();
        let holder = WalletAddress::new("brst_holder");
        let other = WalletAddress::new("brst_other");
        for (index, mut token) in portfolio(512)
            .into_canonical()
            .tokens
            .into_iter()
            .enumerate()
        {
            token.origin = TxHash::new([(index % 128) as u8; 32]);
            token.revoked_origin =
                (index % 3 == 0).then_some(TxHash::new([(index % 17) as u8; 32]));
            engine.track_token(token.clone());
            if index % 7 == 0 {
                token.holder = other.clone();
                engine.track_token(token);
            }
        }
        for divisor in [2, 3, 1] {
            let removed = engine
                .get_portfolio(&holder)
                .unwrap()
                .tokens
                .iter()
                .enumerate()
                .filter(|(index, _)| index % divisor == 0)
                .map(|(_, token)| token.id)
                .collect();
            engine.bulk_untrack(&holder, &removed);
            let mut rebuilt = engine.clone();
            rebuilt.rebuild_indexes();
            assert_eq!(engine.origin_holders, rebuilt.origin_holders);
            assert_eq!(engine.revoked_holders, rebuilt.revoked_holders);
            assert!(!engine.get_portfolio(&other).unwrap().tokens.is_empty());
        }
        assert!(engine.get_portfolio(&holder).unwrap().tokens.is_empty());
    }

    #[test]
    fn explicit_selection_preserves_order_duplicates_and_missing_input_error() {
        let mut portfolio = portfolio(4096);
        let ids = [
            portfolio.tokens[4000].id,
            portfolio.tokens[2].id,
            portfolio.tokens[4000].id,
        ];
        assert_eq!(
            portfolio.select_merge_inputs(&ids).unwrap(),
            vec![
                portfolio.tokens[4000].clone(),
                portfolio.tokens[2].clone(),
                portfolio.tokens[4000].clone(),
            ]
        );
        assert!(portfolio.select_merge_inputs(&[]).unwrap().is_empty());
        let absent = TxHash::new([255; 32]);
        assert_eq!(
            portfolio
                .select_merge_inputs(&[ids[0], absent])
                .unwrap_err(),
            format!("merge input {absent} is not held by this wallet")
        );
        // A malformed vector with duplicate IDs returns the first matching holding.
        let first = portfolio.tokens[2].clone();
        portfolio.tokens.push(TrstToken {
            amount: 99,
            ..first.clone()
        });
        assert_eq!(
            portfolio.select_merge_inputs(&[first.id]).unwrap(),
            vec![first]
        );
    }
}
