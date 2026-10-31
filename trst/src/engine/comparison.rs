use super::*;
use crate::comparison::{maps, ordered, Poll};

impl TrstEngine {
    /// Compare holdings in canonical order, origin ownership and outcome causes
    /// under one supplied expiry period and timestamp. Active/Expired labels are
    /// recalculated for comparison; other states and token fields remain exact.
    /// Stored balance/deadline caches, engine expiry settings and the merger graph
    /// are excluded. This is not snapshot validation.
    ///
    /// Scan both engines for normalized balance overflow before checking equality,
    /// even for shared roots. Portfolios and indexes remain unchanged. Indexed
    /// canonical traversal still performs an ordered-map lookup per token.
    pub fn wallet_snapshot_eq_at_with_poll(&self, other: &Self, expiry_secs: u64,
        now: Timestamp, poll: Poll<'_>,
    ) -> Result<bool, String> {
        for engine in [self, other] {
            for portfolio in engine.wallets.values() {
                poll("trst:comparison-wallet-balance")?;
                let mut balance = 0u128;
                for token in portfolio.iter_tokens() {
                    poll("trst:comparison-balance")?;
                    if token.normalized_state(expiry_secs, now) == TrstState::Active {
                        balance = balance.checked_add(token.amount).ok_or("TRST holding balance overflow")?;
                    }
                }
            }
        }
        if !maps(&self.wallets, &other.wallets, poll, "trst:comparison-wallet", |left, right| {
            let mut left = left.iter_canonical_tokens();
            let mut right = right.iter_canonical_tokens();
            loop {
                poll("trst:comparison-token")?;
                match (left.next(), right.next()) {
                    (None, None) => return Ok(true),
                    (Some(a), Some(b)) if a.eq_at(b, expiry_secs, now) => {},
                    _ => return Ok(false),
                }
            }
        })? { return Ok(false); }
        if !maps(&self.wallet_origins, &other.wallet_origins, poll, "trst:comparison-owner", |a, b| {
            ordered(a.iter(), b.iter(), poll, "trst:comparison-origin")
        })? { return Ok(false); }
        maps(&self.outcome_invalidations, &other.outcome_invalidations, poll, "trst:comparison-outcome", |a, b| {
            let OutcomeInvalidation { causes, wallet_revoked } = a;
            if *wallet_revoked != b.wallet_revoked || causes.len() != b.causes.len() { return Ok(false); }
            for cause in causes {
                poll("trst:comparison-cause")?;
                if !b.causes.contains(cause) { return Ok(false); }
            }
            Ok(true)
        })
    }
}

#[cfg(test)]
mod tests;
