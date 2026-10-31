//! TRST token representation.

use burst_types::{Timestamp, TrstState, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// A holding with an amount, owner, expiry basis and provenance.
/// Merged holdings reference the merger graph rather than copying their ancestry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrstToken {
    pub basis: burst_types::TrstBasis,
    /// Unique token identity. Some identities are derived by splits or changes
    /// and do not name an on-chain block.
    pub id: TxHash,

    /// Amount of TRST in this token.
    pub amount: u128,

    /// Burn hash for an unmerged holding, or merge hash for a merged holding.
    pub origin: TxHash,

    /// Preceding token identity; it can itself be a derived split/change ID.
    /// For the first send after burn, `link == origin`.
    pub link: TxHash,

    /// The wallet that currently holds this token.
    pub holder: WalletAddress,

    /// Timestamp of the originating burn or merge. Transfers and splits retain
    /// this timestamp even though they create a new token identity.
    pub origin_timestamp: Timestamp,

    /// Effective origin timestamp for expiry computation.
    /// For simple tokens, equals `origin_timestamp`.
    /// For merged tokens, equals `min(constituent effective_origin_timestamps)`,
    /// ensuring the merged token expires at the earliest constituent's expiry.
    pub effective_origin_timestamp: Timestamp,

    /// Current state of this token.
    pub state: TrstState,

    /// The wallet that originally burned BRN to create this TRST.
    /// For merged tokens, the wallet that performed the merge (a merge is a
    /// self-operation; constituent originators live in the merger graph).
    pub origin_wallet: WalletAddress,

    /// Burn origin responsible for this revoked holding; used during restoration.
    #[serde(default)]
    pub revoked_origin: Option<TxHash>,
}

impl TrstToken {
    pub(crate) fn normalized_state(&self, expiry_secs: u64, now: Timestamp) -> TrstState {
        match self.state {
            TrstState::Active | TrstState::Expired => {
                if self.is_expired(now, expiry_secs) { TrstState::Expired } else { TrstState::Active }
            }
            state => state,
        }
    }

    pub(crate) fn eq_at(&self, other: &Self, expiry_secs: u64, now: Timestamp) -> bool {
        // Keep this exhaustive: any new persisted token field must participate.
        let Self { basis, id, amount, origin, link, holder, origin_timestamp,
            effective_origin_timestamp, state: _, origin_wallet, revoked_origin } = self;
        *basis == other.basis && *id == other.id && *amount == other.amount
            && *origin == other.origin && *link == other.link && *holder == other.holder
            && *origin_timestamp == other.origin_timestamp
            && *effective_origin_timestamp == other.effective_origin_timestamp
            && *origin_wallet == other.origin_wallet && *revoked_origin == other.revoked_origin
            && self.normalized_state(expiry_secs, now) == other.normalized_state(expiry_secs, now)
    }

    /// Stream references even for expired/revoked tokens. This does not resolve
    /// derived token identities or authenticate the referenced graph/history.
    pub fn visit_retention_references<'a, B>(
        &'a self,
        mut visit: impl FnMut(burst_types::TrstReference<'a>) -> std::ops::ControlFlow<B>,
    ) -> std::ops::ControlFlow<B> {
        use burst_types::TrstReference::{Account, Graph, Token};
        visit(Account(&self.holder))?;
        visit(Account(&self.origin_wallet))?;
        visit(Token(self.id))?;
        visit(Token(self.link))?;
        visit(Graph(self.origin))?;
        self.basis.visit_retention_references(&mut visit)?;
        if let Some(origin) = self.revoked_origin { visit(Graph(origin))?; }
        std::ops::ControlFlow::Continue(())
    }

    /// Compare age with the supplied expiry period, independently of stored state.
    pub fn is_expired(&self, now: Timestamp, expiry_secs: u64) -> bool {
        self.effective_origin_timestamp
            .has_expired(expiry_secs, now)
    }

    /// Check stored state and age only. Ownership, graph provenance, signatures
    /// and ledger availability must be checked by the caller.
    pub fn is_transferable(&self, now: Timestamp, expiry_secs: u64) -> bool {
        self.state.is_transferable() && !self.is_expired(now, expiry_secs)
    }

    /// Expiry deadline, saturating at the largest representable timestamp.
    pub fn earliest_expiry(&self, expiry_secs: u64) -> Timestamp {
        Timestamp::new(
            self.effective_origin_timestamp
                .as_secs()
                .saturating_add(expiry_secs),
        )
    }

    /// Linear age-based reference value in basis points, not a market price.
    /// The face amount does not decay; expiry separately controls transferability.
    /// This reads stored state and age, not graph invalidations or spendability.
    /// A period of u64::MAX gives full reference value unless already revoked.
    pub fn current_value_bps(&self, now: Timestamp, expiry_secs: u64) -> u64 {
        if expiry_secs == 0 || self.state == TrstState::Revoked {
            return 0;
        }
        if expiry_secs == u64::MAX {
            return 10_000;
        }
        let age_secs = now
            .as_secs()
            .saturating_sub(self.effective_origin_timestamp.as_secs());
        if age_secs >= expiry_secs {
            return 0;
        }
        let remaining = expiry_secs - age_secs;
        ((remaining as u128 * 10_000) / expiry_secs as u128) as u64
    }

    /// Face amount scaled by the age-based reference value.
    pub fn effective_value(&self, now: Timestamp, expiry_secs: u64) -> u128 {
        let bps = self.current_value_bps(now, expiry_secs) as u128;
        // Divide first while retaining the remainder. Multiplying a valid u128
        // raw amount by basis points can overflow even though the result fits.
        (self.amount / 10_000) * bps + ((self.amount % 10_000) * bps) / 10_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_reference_preserves_large_raw_amounts() {
        let token = TrstToken {
            basis: Default::default(),
            id: TxHash::ZERO,
            amount: u128::MAX,
            origin: TxHash::ZERO,
            link: TxHash::ZERO,
            holder: WalletAddress::new(format!("brst_{:0>75}", "test")),
            origin_timestamp: Timestamp::new(100),
            effective_origin_timestamp: Timestamp::new(100),
            state: TrstState::Active,
            origin_wallet: WalletAddress::new(format!("brst_{:0>75}", "origin")),
            revoked_origin: None,
        };
        assert_eq!(token.effective_value(Timestamp::new(100), 100), u128::MAX);
        assert_eq!(
            token.effective_value(Timestamp::new(150), 100),
            u128::MAX / 2
        );
        assert_eq!(token.effective_value(Timestamp::new(200), 100), 0);
        assert_eq!(
            token.current_value_bps(Timestamp::new(u64::MAX), u64::MAX),
            10_000
        );
        assert_eq!(
            token.effective_value(Timestamp::new(u64::MAX), u64::MAX),
            u128::MAX
        );
        assert_eq!(token.current_value_bps(Timestamp::new(99), 100), 10_000);
        assert_eq!(token.current_value_bps(Timestamp::new(150), 100), 5_000);
        assert_eq!(token.current_value_bps(Timestamp::new(150), 0), 0);
        let mut expired = token.clone();
        expired.state = TrstState::Expired;
        assert_eq!(expired.current_value_bps(Timestamp::new(200), 100), 0);
        assert_eq!(expired.current_value_bps(Timestamp::new(200), 200), 5_000);
        expired.state = TrstState::Revoked;
        assert_eq!(expired.current_value_bps(Timestamp::new(200), u64::MAX), 0);
    }
}

#[cfg(test)]
mod retention_tests {
    use super::*;
    use burst_types::{TrstBasis, TrstReference};
    use std::ops::ControlFlow;

    #[test]
    fn retention_keeps_expired_revoked_and_derived_token_references() {
        use TrstReference::{Account, Graph, Revision, Token};
        let h = |n| TxHash::new([n; 32]);
        for state in [TrstState::Active, TrstState::Expired, TrstState::Revoked] {
            for (basis, reference) in [
                (TrstBasis::Entire, None),
                (TrstBasis::Remainder(h(4)), Some(Revision(h(4)))),
                (TrstBasis::Single(h(4)), Some(Graph(h(4)))),
            ] {
                let token = TrstToken {
                    basis, id: h(1), amount: 10, origin: h(3), link: h(2),
                    holder: WalletAddress::new("brst_holder"),
                    origin_timestamp: Timestamp::new(1), effective_origin_timestamp: Timestamp::new(1),
                    state, origin_wallet: WalletAddress::new("brst_origin"), revoked_origin: Some(h(5)),
                };
                let mut expected = vec![Account(&token.holder), Account(&token.origin_wallet),
                    Token(h(1)), Token(h(2)), Graph(h(3))];
                expected.extend(reference);
                expected.push(Graph(h(5)));
                let mut seen = Vec::new();
                assert_eq!(token.visit_retention_references(|r| { seen.push(r); ControlFlow::<()>::Continue(()) }), ControlFlow::Continue(()));
                assert_eq!(seen, expected);
                let mut calls = 0;
                assert_eq!(token.visit_retention_references(|_| { calls += 1; ControlFlow::Break("stop") }), ControlFlow::Break("stop"));
                assert_eq!(calls, 1);
            }
        }
    }
}
