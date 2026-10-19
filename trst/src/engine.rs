//! Core TRST lifecycle engine.
//!
//! Implements the whitepaper's TRST lifecycle with the decisions from
//! IMPLEMENTATION_DECISIONS.md:
//! - merged tokens use the merge tx hash as their origin; provenance is
//!   discovered by following the merger graph, never flattened (6.17b)
//! - revocation of merged TRST is a proportional split — the tainted portion
//!   becomes a separate revoked token, the clean remainder stays live (7.2c),
//!   rounding against the holder (6.18b)
//! - revocation is reversible when the originator is re-verified (6.15b)
//! - expiry is always computed from the effective origin timestamp and the
//!   CURRENT governance period, so a governance change can resurrect
//!   previously-expired TRST (6.9)

use bincode::Options;
use std::collections::{HashMap, HashSet};

use crate::error::TrstError;
use crate::merger_graph::{ceil_proportion, MergeNode, MergeSource, MergerGraph};
use crate::shared::Shared;
use crate::token::TrstToken;
use burst_types::{
    BlockHash, Timestamp, TrstBasis, TrstRevocationChunk, TrstState, TxHash, WalletAddress,
};

mod outcome_revocation;
mod record_delta;
mod records;
mod snapshot_validation;
pub use records::{WalletRecord, WalletRecordReader};

#[derive(Clone, PartialEq, Eq)]
struct OutcomeInvalidation {
    causes: HashSet<BlockHash>,
    wallet_revoked: bool,
}

/// Maximum number of source tokens in a single merge (6.12b).
pub const MAX_MERGE_SOURCES: usize = 256;

/// A revocation applied to one live token.
#[derive(Clone, Debug)]
pub struct RevocationEvent {
    /// The wallet holding the affected token.
    pub holder: WalletAddress,
    /// The live token that was revoked or split.
    pub token_id: TxHash,
    /// The burn origin whose revocation caused this.
    pub revoked_origin: TxHash,
    /// Amount of TRST revoked from this token.
    pub revoked_amount: u128,
    /// Token amount before the split (for computing proportions).
    pub total_amount: u128,
}

pub struct PendingRevocation {
    pub origin: TxHash,
    taints: HashMap<TxHash, (u128, u128)>,
}

impl PendingRevocation {
    /// Apply this catch-time event to the unsplit remainder of a pending portion.
    pub fn cut_basis(
        &self,
        graph: &mut MergerGraph,
        origin: &TxHash,
        basis: &mut TrstBasis,
        amount: u128,
    ) -> Result<u128, String> {
        if amount == 0 {
            return Ok(0);
        }
        if !graph.valid_basis(origin, *basis) {
            return Err("invalid pending composition basis".into());
        }
        let tainted = self.taints.get(origin).map_or(0, |(value, _)| *value);
        let cut = graph.basis_cut(origin, *basis, amount, &self.origin, tainted);
        if cut > 0 && cut < amount {
            *basis = graph.reduced_basis(origin, *basis, self.origin, tainted);
        }
        Ok(cut)
    }

    pub fn cut(&self, origin: &TxHash, amount: u128) -> u128 {
        if *origin == self.origin {
            return amount;
        }
        self.taints.get(origin).map_or(0, |(tainted, denominator)| {
            ceil_proportion(amount, *tainted, *denominator)
        })
    }
}

/// Result of un-revoking a single token.
#[derive(Clone, Debug)]
pub struct UnRevocationResult {
    /// The token that was restored.
    pub token_id: TxHash,
    /// The wallet holding the restored token.
    pub holder: WalletAddress,
    /// The amount of TRST restored.
    pub amount: u128,
}

/// Provenance info from a consumed token portion during debit.
///
/// Per 6.17(b) this carries only the single origin pointer — constituent
/// origins of merged tokens are found by following the merger graph.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ConsumedProvenance {
    pub basis: TrstBasis,
    pub amount: u128,
    pub origin: TxHash,
    pub origin_wallet: WalletAddress,
    pub origin_timestamp: Timestamp,
    pub effective_origin_timestamp: Timestamp,
}

/// Per-wallet portfolio with O(1) balance lookups.
///
/// Tokens are kept sorted by `origin_timestamp` (sorted invariant maintained
/// on insert). The `cached_transferable` balance is updated incrementally on
/// every mutation so `transferable_balance()` is O(1).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WalletPortfolio {
    /// Tokens sorted by `origin_timestamp` (oldest first).
    pub tokens: Vec<TrstToken>,
    /// Pre-computed transferable balance (sum of amounts for Active, non-expired tokens).
    /// Updated incrementally on mint/send/receive/revocation/expiry.
    pub cached_transferable: u128,
    /// The earliest absolute expiry timestamp across all active tokens.
    /// `None` if there are no active tokens.
    pub earliest_expiry: Option<Timestamp>,
}

impl WalletPortfolio {
    /// Insert a token maintaining the sorted-by-origin_timestamp invariant.
    ///
    /// Fast path O(1): new tokens almost always have the latest timestamp,
    /// so they append at the end without shifting. Slow path O(n): binary
    /// search + insert with shift for out-of-order timestamps.
    fn insert_sorted(&mut self, token: TrstToken) {
        let ts = token.origin_timestamp;
        if self
            .tokens
            .last()
            .is_none_or(|last| last.origin_timestamp <= ts)
        {
            self.tokens.push(token);
        } else {
            let pos = self.tokens.partition_point(|t| t.origin_timestamp <= ts);
            self.tokens.insert(pos, token);
        }
    }

    /// Recompute `earliest_expiry` from scratch (only needed after bulk mutations).
    fn recompute_earliest_expiry(&mut self, expiry_secs: u64) {
        self.earliest_expiry = self
            .tokens
            .iter()
            .filter(|t| t.state == TrstState::Active)
            .map(|t| t.earliest_expiry(expiry_secs))
            .min();
    }

    /// Recompute `cached_transferable` from scratch.
    /// Useful for consistency checks — verifies the incremental cache matches reality.
    pub fn recompute_transferable(&mut self, now: Timestamp, expiry_secs: u64) {
        self.cached_transferable = self
            .tokens
            .iter()
            .filter(|t| t.is_transferable(now, expiry_secs))
            .map(|t| t.amount)
            .sum();
    }

    /// Flush expired tokens: if `now >= earliest_expiry`, mark expired tokens
    /// and adjust the cached balance. Returns the total amount that expired.
    fn flush_expired(&mut self, now: Timestamp, expiry_secs: u64) -> u128 {
        if !self.needs_expiry_flush(now) {
            return 0;
        }
        let mut expired_amount = 0u128;
        for t in &mut self.tokens {
            if t.state == TrstState::Active && t.is_expired(now, expiry_secs) {
                expired_amount = expired_amount.saturating_add(t.amount);
                t.state = TrstState::Expired;
            }
        }
        self.cached_transferable = self.cached_transferable.saturating_sub(expired_amount);
        self.recompute_earliest_expiry(expiry_secs);
        expired_amount
    }

    fn needs_expiry_flush(&self, now: Timestamp) -> bool {
        self.earliest_expiry.is_some_and(|expiry| now >= expiry)
    }

    /// Whether any live (non-revoked) token with this origin remains.
    fn has_live_origin(&self, origin: &TxHash) -> bool {
        self.tokens
            .iter()
            .any(|t| t.origin == *origin && t.revoked_origin.is_none())
    }

    /// Whether any token tagged with this revoked origin remains.
    fn has_revoked_tag(&self, tag: &TxHash) -> bool {
        self.tokens.iter().any(|t| t.revoked_origin == Some(*tag))
    }
}

/// Deterministic id for the revoked chunk split off a token — every node must
/// derive the same id without a corresponding on-chain transaction.
fn split_token_id(token_id: &TxHash, revoked_origin: &TxHash) -> TxHash {
    TxHash::new(burst_crypto::blake2b_256_multi(&[
        token_id.as_bytes(),
        revoked_origin.as_bytes(),
        b"trst-revocation-split",
    ]))
}

/// The TRST engine — manages the full token lifecycle.
#[derive(Clone)]
pub struct TrstEngine {
    /// The merger graph for proactive revocation.
    pub merger_graph: MergerGraph,
    /// Per-wallet portfolios with O(1) balance lookups and sorted tokens.
    pub wallets: Shared<HashMap<WalletAddress, Shared<WalletPortfolio>>>,
    /// Maps each origin wallet to all burn tx hashes (origins) it produced.
    /// Used by revocation to find all origins belonging to a sybil wallet (7.1a).
    pub wallet_origins: Shared<HashMap<WalletAddress, HashSet<TxHash>>>,
    /// Maps an origin tx (burn or merge) to the wallets CURRENTLY holding live
    /// tokens with that origin. Maintained on every track/untrack/debit so
    /// revocation touches only actual holders, never historical ones.
    origin_holders: Shared<HashMap<TxHash, HashSet<WalletAddress>>>,
    /// Maps a revoked burn origin to the wallets holding tokens tagged with it.
    /// Makes un-revocation (6.15b) O(k) in the number of affected tokens.
    revoked_holders: Shared<HashMap<TxHash, HashSet<WalletAddress>>>,
    /// Outcome-specific reasons remain independent of the originator's identity.
    outcome_invalidations: Shared<HashMap<TxHash, OutcomeInvalidation>>,
    /// Global TRST expiry period in seconds (governance parameter, 6.9).
    pub expiry_secs: u64,
}

impl TrstEngine {
    /// Call after attaching the graph to a restored holdings snapshot.
    pub fn validate_bases(&self) -> Result<(), String> {
        let mut owners = HashMap::new();
        for (owner, origins) in &self.wallet_origins {
            for origin in origins {
                if owners.insert(*origin, owner).is_some() {
                    return Err("duplicate TRST origin ownership".into());
                }
            }
        }
        for node in self.merger_graph.iter_merges() {
            if owners.contains_key(&node.merge_tx) {
                return Err("TRST origin is registered as both burn and merge".into());
            }
            for source in &node.source_origins {
                if !self.merger_graph.contains_merge(&source.origin)
                    && !owners.contains_key(&source.origin)
                {
                    return Err("merger graph source lacks burn ownership".into());
                }
            }
        }
        if self
            .merger_graph
            .revoked_origins()
            .iter()
            .any(|origin| !owners.contains_key(origin))
        {
            return Err("revoked TRST origin lacks burn ownership".into());
        }
        for origin in self.outcome_invalidations.keys() {
            if !self.merger_graph.is_origin_revoked(origin) {
                return Err("outcome-invalidated origin is active in merger graph".into());
            }
        }
        for token in self
            .wallets
            .values()
            .flat_map(|portfolio| &portfolio.tokens)
        {
            if !self.merger_graph.contains_merge(&token.origin)
                && owners.get(&token.origin).copied() != Some(&token.origin_wallet)
            {
                return Err("TRST holding differs from its burn ownership".into());
            }
            if let Some(origin) = token.revoked_origin {
                if !owners.contains_key(&origin) || !self.merger_graph.is_origin_revoked(&origin) {
                    return Err("TRST revoked holding lacks its revoked burn origin".into());
                }
            }
            if !self.merger_graph.valid_basis(&token.origin, token.basis) {
                return Err("TRST holding references an invalid composition basis".into());
            }
        }
        Ok(())
    }
    pub fn new() -> Self {
        Self::with_expiry(u64::MAX)
    }

    pub fn with_expiry(expiry_secs: u64) -> Self {
        Self {
            merger_graph: MergerGraph::new(),
            wallets: Shared::default(),
            wallet_origins: Shared::default(),
            origin_holders: Shared::default(),
            revoked_holders: Shared::default(),
            outcome_invalidations: Shared::default(),
            expiry_secs,
        }
    }

    /// Track a token in the per-wallet portfolio.
    ///
    /// Maintains sorted order, the cached transferable balance, the earliest
    /// expiry (using the engine's governance expiry period), and the
    /// origin/revocation holder indexes.
    pub fn track_token(&mut self, token: TrstToken) {
        self.index_token(&token);
        let expiry_secs = self.expiry_secs;
        let portfolio = self.wallets.entry(token.holder.clone()).or_default();
        if token.state == TrstState::Active {
            portfolio.cached_transferable += token.amount;
            let tok_expiry = token.earliest_expiry(expiry_secs);
            match portfolio.earliest_expiry {
                Some(existing) if tok_expiry.as_secs() < existing.as_secs() => {
                    portfolio.earliest_expiry = Some(tok_expiry);
                }
                None => {
                    portfolio.earliest_expiry = Some(tok_expiry);
                }
                _ => {}
            }
        }
        portfolio.insert_sorted(token);
    }

    /// Track an incoming token whose amount has not yet had any revocation
    /// applied. Ledger transfers use `receive_settled_token` instead: applying
    /// all historical cuts to an already-clean remainder would revoke it twice.
    ///
    /// A send that was pending when its origin was revoked escapes the
    /// portfolio sweep — this closes that hole at receive time with the same
    /// O(1)/O(k) graph lookup the whitepaper describes. Returns the revocation
    /// events applied, if any.
    pub fn receive_token(&mut self, mut token: TrstToken, now: Timestamp) -> Vec<RevocationEvent> {
        let mut events = Vec::new();

        // Normalize expiry state against the current governance period.
        if token.state == TrstState::Active && token.is_expired(now, self.expiry_secs) {
            token.state = TrstState::Expired;
        }

        if token.revoked_origin.is_none()
            && matches!(token.state, TrstState::Active | TrstState::Expired)
        {
            if let Some(node) = self.merger_graph.get_merge(&token.origin) {
                if !node.revoked_contribs.is_empty() {
                    let mut remaining = token.amount;
                    let mut chunks: Vec<TrstToken> = Vec::new();
                    for (origin, cut) in self.revoked_portions(&token.origin, token.amount) {
                        events.push(RevocationEvent {
                            holder: token.holder.clone(),
                            token_id: token.id,
                            revoked_origin: origin,
                            revoked_amount: cut,
                            total_amount: token.amount,
                        });
                        remaining -= cut;
                        chunks.push(TrstToken {
                            basis: TrstBasis::Single(origin),
                            id: split_token_id(&token.id, &origin),
                            amount: cut,
                            origin: token.origin,
                            link: token.id,
                            holder: token.holder.clone(),
                            origin_timestamp: token.origin_timestamp,
                            effective_origin_timestamp: token.effective_origin_timestamp,
                            state: TrstState::Revoked,
                            origin_wallet: token.origin_wallet.clone(),
                            revoked_origin: Some(origin),
                        });
                    }
                    for chunk in chunks {
                        self.track_token(chunk);
                    }
                    if remaining == 0 {
                        return events;
                    }
                    token.amount = remaining;
                    token.basis = self.merger_graph.current_basis(&token.origin);
                }
            } else if self.merger_graph.is_origin_revoked(&token.origin) {
                // Simple token from a revoked burn — fully tainted.
                events.push(RevocationEvent {
                    holder: token.holder.clone(),
                    token_id: token.id,
                    revoked_origin: token.origin,
                    revoked_amount: token.amount,
                    total_amount: token.amount,
                });
                token.revoked_origin = Some(token.origin);
                token.state = TrstState::Revoked;
                token.basis = TrstBasis::Single(token.origin);
            }
        }

        self.track_token(token);
        events
    }

    /// Deliver already-settled chunks without reapplying graph proportions to
    /// the clean remainder. Only the composition references are checked here.
    pub fn receive_settled_token(
        &mut self,
        mut token: TrstToken,
        revoked: &std::collections::BTreeMap<[u8; 32], TrstRevocationChunk>,
        now: Timestamp,
    ) -> Result<(), TrstError> {
        let total = revoked
            .values()
            .try_fold(0u128, |sum, chunk| {
                if chunk.amount == 0 {
                    None
                } else {
                    sum.checked_add(chunk.amount)
                }
            })
            .filter(|total| *total <= token.amount)
            .ok_or_else(|| TrstError::Other("invalid pending revocation amounts".into()))?;
        if token.state == TrstState::Revoked
            || token.revoked_origin.is_some()
            || revoked.iter().any(|(origin, chunk)| {
                *origin == [0; 32]
                    || chunk.restored == self.merger_graph.is_origin_revoked(&TxHash::new(*origin))
            })
            || (token.amount > total && !self.merger_graph.valid_basis(&token.origin, token.basis))
        {
            return Err(TrstError::Other(
                "invalid pending revocation provenance".into(),
            ));
        }
        if token.amount > total
            && revoked.keys().any(|origin| {
                !self
                    .merger_graph
                    .basis_excludes(token.basis, &TxHash::new(*origin))
            })
        {
            return Err(TrstError::Other(
                "pending clean basis includes settled chunks".into(),
            ));
        }
        self.check_expiry(&mut token, now);
        for (origin, settled) in revoked {
            let origin = TxHash::new(*origin);
            let mut chunk = token.clone();
            chunk.id = split_token_id(&token.id, &origin);
            chunk.link = token.id;
            chunk.amount = settled.amount;
            chunk.basis = TrstBasis::Single(origin);
            if !settled.restored {
                chunk.state = TrstState::Revoked;
                chunk.revoked_origin = Some(origin);
            }
            self.track_token(chunk);
        }
        token.amount -= total;
        if token.amount > 0 {
            self.track_token(token);
        }
        Ok(())
    }

    /// Outstanding revocation applied if a pending amount were received now.
    /// Shared with delivery so bounty accounting uses the same integer rounding.
    pub fn pending_revoked_amount(&self, origin: &TxHash, amount: u128) -> u128 {
        self.revoked_portions(origin, amount)
            .iter()
            .map(|(_, amount)| amount)
            .sum()
    }

    fn revoked_portions(&self, origin: &TxHash, amount: u128) -> Vec<(TxHash, u128)> {
        let Some(node) = self.merger_graph.get_merge(origin) else {
            return if self.merger_graph.is_origin_revoked(origin) && amount > 0 {
                vec![(*origin, amount)]
            } else {
                Vec::new()
            };
        };
        let mut contributions: Vec<_> = node.revoked_contribs.iter().collect();
        contributions.sort_by_key(|(origin, _)| *origin.as_bytes());
        let mut remaining = amount;
        let mut denominator = node.total_amount;
        let mut portions = Vec::new();
        for (origin, contribution) in contributions {
            let cut = ceil_proportion(remaining, *contribution, denominator);
            denominator = denominator.saturating_sub(*contribution);
            remaining -= cut;
            if cut > 0 {
                portions.push((*origin, cut));
            }
        }
        portions
    }

    /// Record a token in the holder/origin indexes.
    fn index_token(&mut self, token: &TrstToken) {
        match token.revoked_origin {
            Some(tag) => {
                self.revoked_holders
                    .entry(tag)
                    .or_default()
                    .insert(token.holder.clone());
            }
            None => {
                self.origin_holders
                    .entry(token.origin)
                    .or_default()
                    .insert(token.holder.clone());
            }
        }
        // Only burn origins belong in wallet_origins — a merge is a
        // self-operation, not TRST originated by the merging wallet (7.1a).
        if !self.merger_graph.contains_merge(&token.origin) {
            self.wallet_origins
                .entry(token.origin_wallet.clone())
                .or_default()
                .insert(token.origin);
        }
    }

    /// Drop `wallet` from the holder indexes for any of `origins` / `tags`
    /// it no longer holds tokens for.
    fn deindex_wallet_origins(
        &mut self,
        wallet: &WalletAddress,
        origins: &HashSet<TxHash>,
        tags: &HashSet<TxHash>,
    ) {
        let portfolio = self.wallets.get(wallet);
        for origin in origins {
            let still_held = portfolio.is_some_and(|p| p.has_live_origin(origin));
            if !still_held {
                if let Some(set) = self.origin_holders.get_mut(origin) {
                    set.remove(wallet);
                    if set.is_empty() {
                        self.origin_holders.remove(origin);
                    }
                }
            }
        }
        for tag in tags {
            let still_held = portfolio.is_some_and(|p| p.has_revoked_tag(tag));
            if !still_held {
                if let Some(set) = self.revoked_holders.get_mut(tag) {
                    set.remove(wallet);
                    if set.is_empty() {
                        self.revoked_holders.remove(tag);
                    }
                }
            }
        }
    }

    /// Rebuild current holder indexes from portfolios. Origin ownership is
    /// historical state: consumed and pending origins must remain registered.
    pub fn rebuild_indexes(&mut self) {
        self.origin_holders.clear();
        self.revoked_holders.clear();
        for (addr, portfolio) in &self.wallets {
            for t in &portfolio.tokens {
                match t.revoked_origin {
                    Some(tag) => {
                        self.revoked_holders
                            .entry(tag)
                            .or_default()
                            .insert(addr.clone());
                    }
                    None => {
                        self.origin_holders
                            .entry(t.origin)
                            .or_default()
                            .insert(addr.clone());
                    }
                }
            }
        }
    }

    /// Remove a specific token from a wallet's tracked portfolio.
    pub fn untrack_token(&mut self, wallet: &WalletAddress, token_id: &TxHash) {
        let expiry = self.expiry_secs;
        let mut removed_key: Option<(TxHash, Option<TxHash>)> = None;
        if let Some(portfolio) = self.wallets.get_mut(wallet) {
            if let Some(pos) = portfolio.tokens.iter().position(|t| t.id == *token_id) {
                let removed = portfolio.tokens.remove(pos);
                if removed.state == TrstState::Active {
                    portfolio.cached_transferable =
                        portfolio.cached_transferable.saturating_sub(removed.amount);
                    let removed_expiry = removed.earliest_expiry(expiry);
                    if portfolio.earliest_expiry == Some(removed_expiry) {
                        portfolio.recompute_earliest_expiry(expiry);
                    }
                }
                removed_key = Some((removed.origin, removed.revoked_origin));
            }
        }
        if let Some((origin, tag)) = removed_key {
            let origins: HashSet<TxHash> = tag.is_none().then_some(origin).into_iter().collect();
            let tags: HashSet<TxHash> = tag.into_iter().collect();
            self.deindex_wallet_origins(wallet, &origins, &tags);
        }
    }

    /// Remove multiple tokens from a wallet in a single pass — O(n).
    ///
    /// Much more efficient than calling `untrack_token` in a loop, which
    /// would be O(n*k) due to linear scans + repeated expiry recomputation.
    pub fn bulk_untrack(&mut self, wallet: &WalletAddress, token_ids: &HashSet<TxHash>) {
        let expiry = self.expiry_secs;
        let mut removed_origins: HashSet<TxHash> = HashSet::new();
        let mut removed_tags: HashSet<TxHash> = HashSet::new();
        if let Some(portfolio) = self.wallets.get_mut(wallet) {
            let mut removed_amount = 0u128;
            portfolio.tokens.retain(|t| {
                if token_ids.contains(&t.id) {
                    if t.state == TrstState::Active {
                        removed_amount += t.amount;
                    }
                    match t.revoked_origin {
                        Some(tag) => {
                            removed_tags.insert(tag);
                        }
                        None => {
                            removed_origins.insert(t.origin);
                        }
                    }
                    false
                } else {
                    true
                }
            });
            if removed_amount > 0 {
                portfolio.cached_transferable =
                    portfolio.cached_transferable.saturating_sub(removed_amount);
                portfolio.recompute_earliest_expiry(expiry);
            }
        }
        if !removed_origins.is_empty() || !removed_tags.is_empty() {
            self.deindex_wallet_origins(wallet, &removed_origins, &removed_tags);
        }
    }

    /// Compute the transferable (non-expired, non-revoked) balance for a wallet — O(1).
    ///
    /// Flushes any newly expired tokens first (amortized, only when `earliest_expiry` passes).
    /// Returns `None` if the wallet has no tracked tokens in memory.
    pub fn transferable_balance(&mut self, wallet: &WalletAddress, now: Timestamp) -> Option<u128> {
        let expiry_secs = self.expiry_secs;
        let portfolio = self.wallets.get(wallet)?;
        if !portfolio.needs_expiry_flush(now) {
            return Some(portfolio.cached_transferable);
        }
        if let Some(portfolio) = self.wallets.get_mut(wallet) {
            portfolio.flush_expired(now, expiry_secs);
            Some(portfolio.cached_transferable)
        } else {
            None
        }
    }

    /// Read-only transferable balance (does not flush expiry).
    /// Use when you only need a snapshot and can't take `&mut self`.
    pub fn transferable_balance_snapshot(&self, wallet: &WalletAddress) -> Option<u128> {
        self.wallets.get(wallet).map(|p| p.cached_transferable)
    }

    /// Evaluate an uncommitted candidate's timestamp on an isolated portfolio.
    /// Validation must not expire live tokens before signature/time checks pass.
    pub fn candidate_balances(
        &self,
        wallet: &WalletAddress,
        origin: &TxHash,
        now: Timestamp,
    ) -> Option<(u128, u128)> {
        let mut portfolio = self.wallets.get(wallet)?.clone();
        if portfolio.needs_expiry_flush(now) {
            portfolio.flush_expired(now, self.expiry_secs);
        }
        let origin_balance = portfolio
            .tokens
            .iter()
            .filter(|t| {
                t.origin == *origin && t.state == TrstState::Active && t.revoked_origin.is_none()
            })
            .map(|t| t.amount)
            .sum();
        Some((portfolio.cached_transferable, origin_balance))
    }

    /// Transferable amount held by `wallet` within tokens of a single origin.
    ///
    /// Send must never cross origin boundaries — the wallet must merge first
    /// (whitepaper §Merging). Use this to validate that a send of `amount`
    /// against `origin` is actually coverable.
    pub fn origin_transferable(
        &mut self,
        wallet: &WalletAddress,
        origin: &TxHash,
        now: Timestamp,
    ) -> u128 {
        self.transferable_balance(wallet, now);
        if let Some(portfolio) = self.wallets.get(wallet) {
            portfolio
                .tokens
                .iter()
                .filter(|t| {
                    t.origin == *origin
                        && t.state == TrstState::Active
                        && t.revoked_origin.is_none()
                })
                .map(|t| t.amount)
                .sum()
        } else {
            0
        }
    }

    /// Returns true if the wallet has tracked tokens in the engine.
    pub fn is_wallet_tracked(&self, wallet: &WalletAddress) -> bool {
        self.wallets.contains_key(wallet)
    }

    /// Debit `amount` from a wallet's tokens of a single origin after a send.
    ///
    /// A debit can span different composition portions of the same origin,
    /// retaining each portion separately. It never crosses origin boundaries — if `amount` exceeds
    /// what the origin's tokens hold, only what exists is debited (validation
    /// must reject such sends beforehand via `origin_transferable`).
    pub fn debit_wallet(&mut self, wallet: &WalletAddress, token_origin: &TxHash, amount: u128) {
        let _ = self.debit_wallet_with_provenance(wallet, token_origin, amount);
    }

    /// Debit like [`debit_wallet`] and return the consumed provenance.
    ///
    /// Returns one entry per composition basis, all pointing to the same burn
    /// or merge (6.17b). A restored split must not blend with its old remainder.
    pub fn debit_wallet_with_provenance(
        &mut self,
        wallet: &WalletAddress,
        token_origin: &TxHash,
        amount: u128,
    ) -> Vec<ConsumedProvenance> {
        if amount == 0 {
            return Vec::new();
        }
        let expiry_secs = self.expiry_secs;
        let mut provenance: HashMap<TrstBasis, ConsumedProvenance> = HashMap::new();
        let mut origin_exhausted = false;

        if let Some(portfolio) = self.wallets.get_mut(wallet) {
            let mut remaining = amount;
            let mut consumed_total = 0u128;
            let mut consumed_earliest = false;
            let mut i = 0;
            while i < portfolio.tokens.len() && remaining > 0 {
                let matches = {
                    let t = &portfolio.tokens[i];
                    t.origin == *token_origin
                        && t.state == TrstState::Active
                        && t.revoked_origin.is_none()
                };
                if !matches {
                    i += 1;
                    continue;
                }
                let take = portfolio.tokens[i].amount.min(remaining);
                {
                    let t = &portfolio.tokens[i];
                    if portfolio.earliest_expiry == Some(t.earliest_expiry(expiry_secs)) {
                        consumed_earliest = true;
                    }
                    match provenance.get_mut(&t.basis) {
                        Some(p) => p.amount = p.amount.saturating_add(take),
                        None => {
                            provenance.insert(
                                t.basis,
                                ConsumedProvenance {
                                    basis: t.basis,
                                    amount: take,
                                    origin: t.origin,
                                    origin_wallet: t.origin_wallet.clone(),
                                    origin_timestamp: t.origin_timestamp,
                                    effective_origin_timestamp: t.effective_origin_timestamp,
                                },
                            );
                        }
                    }
                }
                remaining -= take;
                consumed_total = consumed_total.saturating_add(take);
                if take == portfolio.tokens[i].amount {
                    portfolio.tokens.remove(i);
                    // don't advance i — next token shifted into this slot
                } else {
                    portfolio.tokens[i].amount -= take;
                    i += 1;
                }
            }
            portfolio.cached_transferable =
                portfolio.cached_transferable.saturating_sub(consumed_total);
            if consumed_earliest {
                portfolio.recompute_earliest_expiry(expiry_secs);
            }
            origin_exhausted = !portfolio.has_live_origin(token_origin);
        }

        if origin_exhausted {
            let origins: HashSet<TxHash> = std::iter::once(*token_origin).collect();
            self.deindex_wallet_origins(wallet, &origins, &HashSet::new());
        }
        let mut portions: Vec<_> = provenance.into_values().collect();
        portions.sort_by_key(|portion| portion.basis.key());
        portions
    }

    /// Mint fresh TRST from a burn transaction.
    ///
    /// Called when a consumer burns BRN for a provider. The provider receives
    /// newly created TRST with the burn tx as origin.
    pub fn mint(
        &mut self,
        burn_tx_hash: TxHash,
        receiver: WalletAddress,
        amount: u128,
        origin_wallet: WalletAddress,
        timestamp: Timestamp,
    ) -> Result<TrstToken, TrstError> {
        if amount == 0 {
            return Err(TrstError::Other("mint amount must be non-zero".into()));
        }
        self.wallet_origins
            .entry(origin_wallet.clone())
            .or_default()
            .insert(burn_tx_hash);
        Ok(TrstToken {
            basis: TrstBasis::Entire,
            id: burn_tx_hash,
            amount,
            origin: burn_tx_hash,
            link: burn_tx_hash,
            holder: receiver,
            origin_timestamp: timestamp,
            effective_origin_timestamp: timestamp,
            state: TrstState::Active,
            origin_wallet,
            revoked_origin: None,
        })
    }

    /// Transfer TRST from one wallet to another.
    ///
    /// Creates a new token for the receiver (with updated link) and
    /// returns the change back to the sender as a new token.
    /// Returns `(receiver_token, change_token_if_any)`.
    #[allow(clippy::too_many_arguments)]
    pub fn transfer(
        &self,
        token: &TrstToken,
        sender: &WalletAddress,
        receiver: WalletAddress,
        amount: u128,
        send_tx_hash: TxHash,
        change_tx_hash: TxHash,
        now: Timestamp,
    ) -> Result<(TrstToken, Option<TrstToken>), TrstError> {
        if &token.holder != sender {
            return Err(TrstError::NotOwner {
                expected: token.holder.clone(),
                actual: sender.clone(),
            });
        }
        if amount == 0 {
            return Err(TrstError::Other("transfer amount must be non-zero".into()));
        }
        if !token.is_transferable(now, self.expiry_secs) {
            return Err(TrstError::NotTransferable(format!("{:?}", token.state)));
        }
        if amount > token.amount {
            return Err(TrstError::InsufficientBalance {
                needed: amount,
                available: token.amount,
            });
        }

        let receiver_token = TrstToken {
            basis: token.basis,
            id: send_tx_hash,
            amount,
            origin: token.origin,
            link: token.id,
            holder: receiver,
            origin_timestamp: token.origin_timestamp,
            effective_origin_timestamp: token.effective_origin_timestamp,
            state: TrstState::Active,
            origin_wallet: token.origin_wallet.clone(),
            revoked_origin: None,
        };

        let change = if amount < token.amount {
            Some(TrstToken {
                basis: token.basis,
                id: change_tx_hash,
                amount: token.amount - amount,
                origin: token.origin,
                link: token.id,
                holder: token.holder.clone(),
                origin_timestamp: token.origin_timestamp,
                effective_origin_timestamp: token.effective_origin_timestamp,
                state: TrstState::Active,
                origin_wallet: token.origin_wallet.clone(),
                revoked_origin: None,
            })
        } else {
            None
        };

        Ok((receiver_token, change))
    }

    /// Merge multiple tokens into a single token.
    ///
    /// - The merged token's origin is the merge tx hash (whitepaper §Merging).
    /// - Its expiry is the **earliest** expiry among inputs (conservative).
    /// - The merger graph records the merge's IMMEDIATE inputs (6.17b),
    ///   forming the multi-level graph when a merged token is merged again.
    /// - Expired tokens may be consolidated (6.8b); the result of merging any
    ///   expired input is itself expired (earliest-expiry rule).
    /// - At most [`MAX_MERGE_SOURCES`] inputs (6.12b).
    pub fn merge(
        &mut self,
        tokens: &[TrstToken],
        holder: WalletAddress,
        merge_tx_hash: TxHash,
        now: Timestamp,
    ) -> Result<TrstToken, TrstError> {
        if tokens.len() < 2 {
            return Err(TrstError::EmptyMerge);
        }
        if tokens.len() > MAX_MERGE_SOURCES {
            return Err(TrstError::Other(format!(
                "merge cannot consume more than {MAX_MERGE_SOURCES} tokens, got {}",
                tokens.len()
            )));
        }

        let mut seen_ids = HashSet::with_capacity(tokens.len());
        let mut any_live = false;
        let mut any_expired = false;
        for t in tokens {
            if t.holder != holder {
                return Err(TrstError::NotOwner {
                    expected: holder.clone(),
                    actual: t.holder.clone(),
                });
            }
            if !seen_ids.insert(t.id) {
                return Err(TrstError::Other(format!(
                    "duplicate token {} in merge",
                    t.id
                )));
            }
            // 6.8(b): Active and Expired tokens can be merged (consolidation);
            // Revoked and Pending cannot.
            if t.revoked_origin.is_some()
                || !matches!(t.state, TrstState::Active | TrstState::Expired)
            {
                return Err(TrstError::NotTransferable(format!(
                    "{:?} ({})",
                    t.state, t.id
                )));
            }
            if t.is_expired(now, self.expiry_secs) {
                any_expired = true;
            } else {
                any_live = true;
            }
        }
        // Never mix live and expired inputs: the earliest-expiry floor rule
        // would silently expire the live value. Consolidation of expired
        // tokens (6.8b) is expired-with-expired only.
        if any_live && any_expired {
            return Err(TrstError::Other(
                "cannot merge live and expired tokens — the floor rule would expire the live value"
                    .into(),
            ));
        }

        let total_amount = tokens
            .iter()
            .try_fold(0u128, |total, token| total.checked_add(token.amount))
            .ok_or_else(|| TrstError::Other("merged amount exceeds u128".into()))?;

        // The effective origin timestamp for expiry is the earliest among all
        // constituents: "the merged token's expiry date is the earliest expiry
        // among all merged tokens" (whitepaper).
        let effective_ts = tokens
            .iter()
            .map(|t| t.effective_origin_timestamp)
            .min()
            .unwrap();

        // Combine only inputs with the same origin AND composition. Restored
        // splits and their clean remainders remain distinct immediate edges.
        let mut source_amounts: HashMap<(TxHash, TrstBasis), u128> = HashMap::new();
        let mut source_order = Vec::new();
        for t in tokens {
            if !self.merger_graph.valid_basis(&t.origin, t.basis) {
                return Err(TrstError::Other("invalid merge input basis".into()));
            }
            let source = (t.origin, t.basis);
            match source_amounts.get_mut(&source) {
                Some(a) => *a = a.saturating_add(t.amount),
                None => {
                    source_amounts.insert(source, t.amount);
                    source_order.push(source);
                }
            }
        }
        let bases = source_order.iter().map(|(_, basis)| *basis).collect();
        let merge_sources: Vec<MergeSource> = source_order
            .into_iter()
            .map(|source| MergeSource {
                origin: source.0,
                amount: source_amounts[&source],
            })
            .collect();

        self.merger_graph.record_merge_with_bases(
            MergeNode {
                merge_tx: merge_tx_hash,
                source_origins: merge_sources,
                total_amount,
                revoked_contribs: HashMap::new(),
            },
            bases,
        );

        let most_recent_input = tokens.iter().max_by_key(|t| t.origin_timestamp).unwrap().id;
        let state = if Timestamp::new(effective_ts.as_secs()).has_expired(self.expiry_secs, now) {
            TrstState::Expired
        } else {
            TrstState::Active
        };

        Ok(TrstToken {
            basis: TrstBasis::Entire,
            id: merge_tx_hash,
            amount: total_amount,
            // Whitepaper: "Future transactions from the merged token use the
            // merge transaction's hash as the new origin."
            origin: merge_tx_hash,
            link: most_recent_input,
            holder: holder.clone(),
            origin_timestamp: now,
            effective_origin_timestamp: effective_ts,
            state,
            // A merge is a self-operation (6.7) — constituent originators are
            // found through the merger graph.
            origin_wallet: holder,
            revoked_origin: None,
        })
    }

    /// Check and update expiry state for a token.
    pub fn check_expiry(&self, token: &mut TrstToken, now: Timestamp) {
        if token.state == TrstState::Active && token.is_expired(now, self.expiry_secs) {
            token.state = TrstState::Expired;
        }
    }

    /// Revoke all TRST originating from a fraudulent wallet (7.1a).
    ///
    /// For every burn origin of the wallet:
    /// - simple tokens are fully revoked (100% tainted)
    /// - merged tokens are proportionally split (7.2c): the tainted portion
    ///   becomes a separate token in Revoked state tagged with the origin,
    ///   the clean remainder stays live; rounding is against the holder (6.18b)
    ///
    /// Only CURRENT holders are touched, via the origin-holders index — cost
    /// scales with affected balances, not transaction history.
    pub fn revoke_by_origin(&mut self, origin_wallet: &WalletAddress) -> Vec<RevocationEvent> {
        self.revoke_with_pending_effects(origin_wallet).0
    }

    /// Ordered cuts also apply to transfers still in flight, at settlement time.
    pub fn revoke_with_pending_effects(
        &mut self,
        origin_wallet: &WalletAddress,
    ) -> (Vec<RevocationEvent>, Vec<PendingRevocation>) {
        let mut origin_txs: Vec<TxHash> = self
            .wallet_origins
            .get(origin_wallet)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        // Deterministic application order — every node must compute the
        // same sequential proportional splits.
        origin_txs.sort_by_key(|t| *t.as_bytes());
        for origin in &origin_txs {
            if let Some(invalidation) = self.outcome_invalidations.get_mut(origin) {
                invalidation.wallet_revoked = true;
            }
        }
        self.revoke_origins(origin_txs)
    }

    fn revoke_origins(
        &mut self,
        origin_txs: Vec<TxHash>,
    ) -> (Vec<RevocationEvent>, Vec<PendingRevocation>) {
        let mut events = Vec::new();
        let mut pending = Vec::new();
        for origin in origin_txs {
            if self.merger_graph.is_origin_revoked(&origin) {
                continue;
            }
            let taints = self.merger_graph.apply_revocation(origin);

            // Simple tokens: every live token with this burn origin.
            let mut holders: Vec<WalletAddress> = self
                .origin_holders
                .get(&origin)
                .map(|s| s.iter().cloned().collect())
                .unwrap_or_default();
            holders.sort_by(|a, b| a.as_str().cmp(b.as_str()));
            for holder in holders {
                self.revoke_whole_tokens(&holder, &origin, &mut events);
            }

            // Merged tokens: proportional split per affected merge.
            for taint in &taints {
                let mut holders: Vec<WalletAddress> = self
                    .origin_holders
                    .get(&taint.merge_tx)
                    .map(|s| s.iter().cloned().collect())
                    .unwrap_or_default();
                holders.sort_by(|a, b| a.as_str().cmp(b.as_str()));
                for holder in holders {
                    self.split_revoke_tokens(
                        &holder,
                        &taint.merge_tx,
                        origin,
                        taint.tainted_amount,
                        &mut events,
                    );
                }
            }
            pending.push(PendingRevocation {
                origin,
                taints: taints
                    .into_iter()
                    .map(|taint| {
                        (
                            taint.merge_tx,
                            (
                                taint.tainted_amount,
                                taint.total_amount.saturating_sub(taint.prior_revoked),
                            ),
                        )
                    })
                    .collect(),
            });
        }
        (events, pending)
    }

    /// Fully revoke every live token with `origin` held by `holder`.
    fn revoke_whole_tokens(
        &mut self,
        holder: &WalletAddress,
        origin: &TxHash,
        events: &mut Vec<RevocationEvent>,
    ) {
        let expiry_secs = self.expiry_secs;
        let mut touched = false;
        if let Some(portfolio) = self.wallets.get_mut(holder) {
            let mut active_revoked = 0u128;
            for t in &mut portfolio.tokens {
                if t.origin == *origin
                    && t.revoked_origin.is_none()
                    && matches!(t.state, TrstState::Active | TrstState::Expired)
                {
                    if t.state == TrstState::Active {
                        active_revoked = active_revoked.saturating_add(t.amount);
                    }
                    t.state = TrstState::Revoked;
                    t.revoked_origin = Some(*origin);
                    events.push(RevocationEvent {
                        holder: holder.clone(),
                        token_id: t.id,
                        revoked_origin: *origin,
                        revoked_amount: t.amount,
                        total_amount: t.amount,
                    });
                    touched = true;
                }
            }
            if active_revoked > 0 {
                portfolio.cached_transferable =
                    portfolio.cached_transferable.saturating_sub(active_revoked);
                portfolio.recompute_earliest_expiry(expiry_secs);
            }
        }
        if touched {
            self.revoked_holders
                .entry(*origin)
                .or_default()
                .insert(holder.clone());
            let origins: HashSet<TxHash> = std::iter::once(*origin).collect();
            self.deindex_wallet_origins(holder, &origins, &HashSet::new());
        }
    }

    /// Proportionally split every live token with origin `merge_tx` held by
    /// `holder`: `ceil(amount * tainted / denom)` is cut into a Revoked token
    /// tagged with `revoked_origin`; the remainder stays live.
    fn split_revoke_tokens(
        &mut self,
        holder: &WalletAddress,
        merge_tx: &TxHash,
        revoked_origin: TxHash,
        tainted: u128,
        events: &mut Vec<RevocationEvent>,
    ) {
        let expiry_secs = self.expiry_secs;
        let mut touched = false;
        if let Some(portfolio) = self.wallets.get_mut(holder) {
            let mut active_revoked = 0u128;
            let mut chunks: Vec<TrstToken> = Vec::new();
            for t in &mut portfolio.tokens {
                if t.origin != *merge_tx
                    || t.revoked_origin.is_some()
                    || !matches!(t.state, TrstState::Active | TrstState::Expired)
                {
                    continue;
                }
                let cut = self.merger_graph.basis_cut(
                    &t.origin,
                    t.basis,
                    t.amount,
                    &revoked_origin,
                    tainted,
                );
                if cut == 0 {
                    continue;
                }
                if t.state == TrstState::Active {
                    active_revoked = active_revoked.saturating_add(cut);
                }
                events.push(RevocationEvent {
                    holder: holder.clone(),
                    token_id: t.id,
                    revoked_origin,
                    revoked_amount: cut,
                    total_amount: t.amount,
                });
                touched = true;
                if cut >= t.amount {
                    t.state = TrstState::Revoked;
                    t.revoked_origin = Some(revoked_origin);
                    t.basis = TrstBasis::Single(revoked_origin);
                } else {
                    t.amount -= cut;
                    t.basis = self.merger_graph.reduced_basis(
                        &t.origin,
                        t.basis,
                        revoked_origin,
                        tainted,
                    );
                    chunks.push(TrstToken {
                        basis: TrstBasis::Single(revoked_origin),
                        id: split_token_id(&t.id, &revoked_origin),
                        amount: cut,
                        origin: *merge_tx,
                        link: t.id,
                        holder: holder.clone(),
                        origin_timestamp: t.origin_timestamp,
                        effective_origin_timestamp: t.effective_origin_timestamp,
                        state: TrstState::Revoked,
                        origin_wallet: t.origin_wallet.clone(),
                        revoked_origin: Some(revoked_origin),
                    });
                }
            }
            for chunk in chunks {
                portfolio.insert_sorted(chunk);
            }
            if active_revoked > 0 {
                portfolio.cached_transferable =
                    portfolio.cached_transferable.saturating_sub(active_revoked);
                portfolio.recompute_earliest_expiry(expiry_secs);
            }
        }
        if touched {
            self.revoked_holders
                .entry(revoked_origin)
                .or_default()
                .insert(holder.clone());
            let origins: HashSet<TxHash> = std::iter::once(*merge_tx).collect();
            self.deindex_wallet_origins(holder, &origins, &HashSet::new());
        }
    }

    /// Un-revoke TRST when a previously fraudulent wallet is re-verified (6.15b).
    ///
    /// Symmetric with `revoke_by_origin`: un-marks every origin in the merger
    /// graph, then restores exactly the tokens tagged with those origins via
    /// the revoked-holders index — O(k) in affected tokens. Restored tokens
    /// re-enter Active or Expired state depending on the current expiry period.
    pub fn un_revoke_by_origin(
        &mut self,
        origin_wallet: &WalletAddress,
        now: Timestamp,
    ) -> Vec<UnRevocationResult> {
        let mut origin_txs: Vec<TxHash> = self
            .wallet_origins
            .get(origin_wallet)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        origin_txs.sort_by_key(|t| *t.as_bytes());
        origin_txs.retain(|origin| {
            if let Some(invalidation) = self.outcome_invalidations.get_mut(origin) {
                invalidation.wallet_revoked = false;
                false
            } else {
                true
            }
        });
        self.restore_origins(origin_txs, now)
    }

    fn restore_origins(
        &mut self,
        origin_txs: Vec<TxHash>,
        now: Timestamp,
    ) -> Vec<UnRevocationResult> {
        let expiry_secs = self.expiry_secs;
        let mut results = Vec::new();
        for origin in origin_txs {
            if !self.merger_graph.is_origin_revoked(&origin) {
                continue;
            }
            self.merger_graph.apply_unrevocation(&origin);

            let mut holders: Vec<WalletAddress> = self
                .revoked_holders
                .remove(&origin)
                .map(|s| s.into_iter().collect())
                .unwrap_or_default();
            holders.sort_by(|a, b| a.as_str().cmp(b.as_str()));

            for holder in holders {
                let mut restored_origins: HashSet<TxHash> = HashSet::new();
                if let Some(portfolio) = self.wallets.get_mut(&holder) {
                    let mut restored_active = 0u128;
                    for t in &mut portfolio.tokens {
                        if t.revoked_origin != Some(origin) {
                            continue;
                        }
                        t.revoked_origin = None;
                        t.state = if t.is_expired(now, expiry_secs) {
                            TrstState::Expired
                        } else {
                            TrstState::Active
                        };
                        if t.state == TrstState::Active {
                            restored_active = restored_active.saturating_add(t.amount);
                        }
                        restored_origins.insert(t.origin);
                        results.push(UnRevocationResult {
                            token_id: t.id,
                            holder: holder.clone(),
                            amount: t.amount,
                        });
                    }
                    if restored_active > 0 {
                        portfolio.cached_transferable = portfolio
                            .cached_transferable
                            .saturating_add(restored_active);
                    }
                    portfolio.recompute_earliest_expiry(expiry_secs);
                }
                for o in restored_origins {
                    self.origin_holders
                        .entry(o)
                        .or_default()
                        .insert(holder.clone());
                }
            }
        }
        results
    }

    /// Apply a governance change of the TRST expiry period (6.9).
    ///
    /// Expiry is computed from each token's inception and the CURRENT period,
    /// so extending the period makes previously-expired TRST transferable
    /// again, and shortening it expires tokens immediately. O(total tokens),
    /// but governance changes are rare by construction.
    pub fn set_expiry_period(&mut self, new_expiry_secs: u64, now: Timestamp) {
        self.expiry_secs = new_expiry_secs;
        let state_at = |token: &TrstToken| match token.state {
            TrstState::Active | TrstState::Expired => {
                if token.is_expired(now, new_expiry_secs) {
                    TrstState::Expired
                } else {
                    TrstState::Active
                }
            }
            state => state,
        };
        // Read through the shared snapshot; detach only portfolios whose states
        // or caches actually change. Recovery normalizes at a fixed clock.
        let previous = self.wallets.clone();
        for (holder, portfolio) in &previous {
            let mut changed = false;
            let mut transferable = 0u128;
            let mut earliest = None;
            for token in &portfolio.tokens {
                let state = state_at(token);
                changed |= state != token.state;
                if state == TrstState::Active {
                    transferable += token.amount;
                    let expiry = token.earliest_expiry(new_expiry_secs);
                    earliest = Some(earliest.map_or(expiry, |old: Timestamp| old.min(expiry)));
                }
            }
            if !changed
                && transferable == portfolio.cached_transferable
                && earliest == portfolio.earliest_expiry
            {
                continue;
            }
            let portfolio = self.wallets.get_mut(holder).expect("existing portfolio");
            for token in &mut portfolio.tokens {
                token.state = state_at(token);
            }
            portfolio.cached_transferable = transferable;
            portfolio.earliest_expiry = earliest;
        }
    }

    /// Compute total effective (demurrage-adjusted) TRST balance across tokens.
    /// Active tokens are valued based on time remaining; expired/revoked = 0.
    pub fn effective_balance(&self, tokens: &[TrstToken], now: Timestamp) -> u128 {
        tokens
            .iter()
            .filter(|t| t.state == TrstState::Active)
            .map(|t| t.effective_value(now, self.expiry_secs))
            .sum()
    }
}

// Meta-store key used for persisting the TRST engine's token portfolios.
const TRST_ENGINE_META_KEY: &str = "trst_engine_wallets";
const SNAPSHOT_MAGIC: [u8; 8] = *b"TRSTv003";
const OUTCOME_SNAPSHOT_MAGIC: [u8; 8] = *b"TRSTv004";
const MAX_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(serde::Deserialize)]
struct WalletSnapshot {
    magic: [u8; 8],
    wallets: Vec<(WalletAddress, WalletPortfolio)>,
    origins: Vec<(WalletAddress, Vec<TxHash>)>,
}

#[derive(serde::Deserialize)]
struct OutcomeWalletSnapshot {
    magic: [u8; 8],
    wallets: Vec<(WalletAddress, WalletPortfolio)>,
    origins: Vec<(WalletAddress, Vec<TxHash>)>,
    invalidations: Vec<(TxHash, bool, Vec<BlockHash>)>,
}

impl TrstEngine {
    /// True only when all fields encoded by `save_wallets` still share storage.
    /// Derived holder indexes, expiry rules and the separately persisted graph
    /// are not part of that snapshot. False may also mean an equal-value write.
    pub fn shares_wallet_snapshot_with(&self, other: &Self) -> bool {
        self.wallets.shares_storage_with(&other.wallets)
            && self
                .wallet_origins
                .shares_storage_with(&other.wallet_origins)
            && self
                .outcome_invalidations
                .shares_storage_with(&other.outcome_invalidations)
    }

    /// Compare exactly the persisted wallet fields without encoding either body.
    /// The separately persisted graph and derived holder/expiry indexes are excluded.
    pub fn wallet_snapshot_eq(&self, other: &Self) -> bool {
        self.wallets == other.wallets
            && self.wallet_origins == other.wallet_origins
            && self.outcome_invalidations == other.outcome_invalidations
    }

    /// Persist holdings and every mint's owner, including origins now entirely
    /// pending or consumed by merges. Sort maps and sets for deterministic bytes.
    pub fn save_wallets(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        self.write_wallets(&mut bytes)
            .expect("TRST snapshot contains only serializable data");
        bytes
    }

    /// Write the same deterministic wallet snapshot without an encoded-body buffer.
    pub fn write_wallets(&self, writer: impl std::io::Write) -> Result<(), bincode::Error> {
        let mut wallets: Vec<_> = self.wallets.iter().collect();
        wallets.sort_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));
        let mut origins: Vec<_> = self
            .wallet_origins
            .iter()
            .map(|(address, origins)| {
                let mut origins: Vec<_> = origins.iter().collect();
                origins.sort_by_key(|origin| *origin.as_bytes());
                (address, origins)
            })
            .collect();
        origins.sort_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));
        if self.outcome_invalidations.is_empty() {
            bincode::serialize_into(writer, &(SNAPSHOT_MAGIC, wallets, origins))
        } else {
            let mut invalidations: Vec<_> = self
                .outcome_invalidations
                .iter()
                .map(|(origin, entry)| {
                    let mut causes: Vec<_> = entry.causes.iter().copied().collect();
                    causes.sort_unstable();
                    (*origin, entry.wallet_revoked, causes)
                })
                .collect();
            invalidations.sort_unstable_by_key(|(origin, _, _)| *origin.as_bytes());
            bincode::serialize_into(
                writer,
                &(OUTCOME_SNAPSHOT_MAGIC, wallets, origins, invalidations),
            )
        }
    }

    /// Restore holdings and the historical origin registry. Old portfolio-only
    /// snapshots cannot reconstruct missing pending or merged origins and need
    /// explicit ledger replay, never automatic empty-registry recovery.
    ///
    /// Returns a TrstEngine with the restored portfolios and a fresh
    /// MergerGraph — the merger graph is persisted separately. Call
    /// [`rebuild_indexes`](Self::rebuild_indexes) reconstructs only holder indexes.
    pub fn load_wallets(data: &[u8], expiry_secs: u64) -> Result<Self, bincode::Error> {
        let engine = Self::decode_wallets(data, expiry_secs)?;
        for portfolio in engine.wallets.values() {
            let expected = portfolio
                .tokens
                .iter()
                .filter(|token| token.state == TrstState::Active)
                .map(|token| token.earliest_expiry(expiry_secs))
                .min();
            if portfolio.earliest_expiry != expected {
                return Err(Box::new(bincode::ErrorKind::Custom(
                    "TRST holding expiry cache differs from governed period".into(),
                )));
            }
        }
        Ok(engine)
    }

    // Undo images can span different governed expiry periods. Validate their
    // structure here; the final restored engine must pass `load_wallets` with
    // the period from the same restored transaction.
    fn decode_wallets(data: &[u8], expiry_secs: u64) -> Result<Self, bincode::Error> {
        let invalid = |reason: &str| Box::new(bincode::ErrorKind::Custom(reason.into()));
        if !data.starts_with(&SNAPSHOT_MAGIC) && !data.starts_with(&OUTCOME_SNAPSHOT_MAGIC) {
            return Err(invalid(
                "TRST snapshot lacks durable origin ownership; explicit ledger replay required",
            ));
        }
        if data.len() as u64 > MAX_SNAPSHOT_BYTES {
            return Err(invalid("TRST snapshot exceeds size limit"));
        }
        let options = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(MAX_SNAPSHOT_BYTES)
            .reject_trailing_bytes();
        let (snapshot, invalidations) = if data.starts_with(&OUTCOME_SNAPSHOT_MAGIC) {
            let expanded: OutcomeWalletSnapshot = options.deserialize(data)?;
            (
                WalletSnapshot {
                    magic: expanded.magic,
                    wallets: expanded.wallets,
                    origins: expanded.origins,
                },
                expanded.invalidations,
            )
        } else {
            (options.deserialize::<WalletSnapshot>(data)?, Vec::new())
        };
        if snapshot.magic != SNAPSHOT_MAGIC && snapshot.magic != OUTCOME_SNAPSHOT_MAGIC {
            return Err(invalid("invalid TRST snapshot version"));
        }
        let mut records = WalletRecordReader::new(expiry_secs);
        for (address, portfolio) in snapshot.wallets {
            records
                .push(WalletRecord::Portfolio {
                    holder: address.clone(),
                    cached_transferable: portfolio.cached_transferable,
                    earliest_expiry: portfolio.earliest_expiry,
                })
                .map_err(|reason| invalid(&reason))?;
            for (position, token) in portfolio.tokens.into_iter().enumerate() {
                if token.holder != address {
                    return Err(invalid("invalid TRST snapshot holder"));
                }
                records
                    .push(WalletRecord::Holding {
                        holder: address.clone(),
                        position: position as u64,
                        token: Box::new(token),
                    })
                    .map_err(|reason| invalid(&reason))?;
            }
        }
        for (address, origins) in snapshot.origins {
            records
                .push(WalletRecord::OriginOwner(address.clone()))
                .map_err(|reason| invalid(&reason))?;
            for origin in origins {
                records
                    .push(WalletRecord::Origin {
                        owner: address.clone(),
                        origin,
                    })
                    .map_err(|reason| invalid(&reason))?;
            }
        }
        for (origin, wallet_revoked, causes) in invalidations {
            records
                .push(WalletRecord::Invalidation {
                    origin,
                    wallet_revoked,
                })
                .map_err(|reason| invalid(&reason))?;
            for cause in causes {
                records
                    .push(WalletRecord::Cause { origin, cause })
                    .map_err(|reason| invalid(&reason))?;
            }
        }
        records
            .finish_checked(false)
            .map_err(|reason| invalid(&reason))
    }

    /// The meta-store key used for wallet portfolio persistence.
    pub fn meta_key() -> &'static str {
        TRST_ENGINE_META_KEY
    }

    /// Convert a legacy storage/undo image without reclassifying expiry under
    /// today's governed period. This does not return a publishable engine or
    /// authenticate the image. Live restoration must still validate its period.
    pub fn visit_wallet_snapshot_records(
        bytes: &[u8],
        visit: impl FnMut(WalletRecord) -> Result<(), String>,
    ) -> Result<(), String> {
        Self::decode_wallets(bytes, 0)
            .map_err(|error| error.to_string())?
            .visit_wallet_records(visit)
    }

    /// Undo a local snapshot delta without replacing unrelated wallets or
    /// revocation causes. This does not authorize rollback: the caller must
    /// enforce finality and the complete signed-block dependency closure.
    /// Origin ownership is reconciled per origin, not per creator's whole set.
    /// Later changes to a touched holding or origin membership still conflict.
    pub fn rewind_wallet_snapshot(
        before: &[u8],
        after: &[u8],
        current: &[u8],
    ) -> Result<Vec<u8>, String> {
        Self::rewind_wallet_snapshot_inner(before, after, current, None)
    }

    /// Reconcile the same delta after local expiry caches have moved. The
    /// caller supplies the authenticated governed period and one comparison
    /// clock, and must publish matching account counters atomically. Only
    /// derived expiry/cache fields are normalized; ownership, amounts,
    /// provenance and revocation changes still conflict.
    pub fn rewind_wallet_snapshot_at(
        before: &[u8],
        after: &[u8],
        current: &[u8],
        expiry_secs: u64,
        now: Timestamp,
    ) -> Result<Vec<u8>, String> {
        Self::rewind_wallet_snapshot_inner(before, after, current, Some((expiry_secs, now)))
    }

    fn rewind_wallet_snapshot_inner(
        before: &[u8],
        after: &[u8],
        current: &[u8],
        expiry: Option<(u64, Timestamp)>,
    ) -> Result<Vec<u8>, String> {
        let decode = |bytes: &[u8]| {
            let engine =
                Self::decode_wallets(bytes, u64::MAX).map_err(|error| error.to_string())?;
            if !crate::snapshot_encoding::matches(bytes, |writer| engine.write_wallets(writer))
                .map_err(|error| error.to_string())?
            {
                return Err("noncanonical wallet undo snapshot".to_string());
            }
            Ok(engine)
        };
        let mut before = decode(before)?;
        let mut after = decode(after)?;
        let mut current = decode(current)?;
        if let Some((period, at)) = expiry {
            before.set_expiry_period(period, at);
            after.set_expiry_period(period, at);
            current.set_expiry_period(period, at);
        }
        crate::snapshot_undo::rewind_map(&before.wallets, &after.wallets, &mut current.wallets)?;
        crate::snapshot_undo::rewind_owned_sets(
            &before.wallet_origins,
            &after.wallet_origins,
            &mut current.wallet_origins,
        )?;
        crate::snapshot_undo::rewind_map(
            &before.outcome_invalidations,
            &after.outcome_invalidations,
            &mut current.outcome_invalidations,
        )?;
        let bytes = current.save_wallets();
        // Recheck global origin ownership and invalidation references after combining.
        decode(&bytes)?;
        Ok(bytes)
    }

    /// Flush expired tokens across all wallets. Call periodically (e.g. every 30s).
    pub fn flush_all_expired(&mut self, now: Timestamp) {
        let expiry_secs = self.expiry_secs;
        if !self.wallets.values().any(|p| p.needs_expiry_flush(now)) {
            return;
        }
        for portfolio in self.wallets.values_mut() {
            if portfolio.needs_expiry_flush(now) {
                portfolio.flush_expired(now, expiry_secs);
            }
        }
    }

    /// Like [`flush_all_expired`](Self::flush_all_expired) but returns the amount
    /// NEWLY expired this call, per wallet (only wallets with a non-zero flush).
    /// Lets the node surface expiry into account-level `expired_trst` counters
    /// (whitepaper §Expiry: the total stays as "virtue points"; only the
    /// transferable portion shrinks). Amounts are newly-expired, so callers can
    /// accumulate them without double-counting.
    pub fn flush_all_expired_by_wallet(&mut self, now: Timestamp) -> Vec<(WalletAddress, u128)> {
        let expiry_secs = self.expiry_secs;
        let mut out = Vec::new();
        if !self.wallets.values().any(|p| p.needs_expiry_flush(now)) {
            return out;
        }
        for (addr, portfolio) in self.wallets.iter_mut() {
            if !portfolio.needs_expiry_flush(now) {
                continue;
            }
            let flushed = portfolio.flush_expired(now, expiry_secs);
            if flushed > 0 {
                out.push((addr.clone(), flushed));
            }
        }
        out
    }

    /// Get a portfolio for a wallet (immutable).
    pub fn get_portfolio(&self, wallet: &WalletAddress) -> Option<&WalletPortfolio> {
        self.wallets.get(wallet).map(|portfolio| &**portfolio)
    }

    /// Get a portfolio for a wallet (mutable).
    pub fn get_portfolio_mut(&mut self, wallet: &WalletAddress) -> Option<&mut WalletPortfolio> {
        self.wallets
            .get_mut(wallet)
            .map(|portfolio| &mut **portfolio)
    }
}

impl Default for TrstEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::TrstState;

    fn test_address(n: u8) -> WalletAddress {
        WalletAddress::new(format!("brst_{:0>60}", n))
    }

    fn test_hash(n: u8) -> TxHash {
        TxHash::new([n; 32])
    }

    fn ts(secs: u64) -> Timestamp {
        Timestamp::new(secs)
    }

    #[test]
    fn minting_creates_token_with_correct_fields() {
        let mut engine = TrstEngine::new();
        let burn_tx = test_hash(1);
        let receiver = test_address(1);
        let origin_wallet = test_address(2);

        let token = engine
            .mint(
                burn_tx,
                receiver.clone(),
                1000,
                origin_wallet.clone(),
                ts(1000),
            )
            .unwrap();

        assert_eq!(token.id, burn_tx);
        assert_eq!(token.amount, 1000);
        assert_eq!(token.origin, burn_tx);
        assert_eq!(token.link, burn_tx);
        assert_eq!(token.holder, receiver);
        assert_eq!(token.state, TrstState::Active);
        assert_eq!(token.origin_wallet, origin_wallet);
        assert!(token.revoked_origin.is_none());
        assert!(engine.wallet_origins[&origin_wallet].contains(&burn_tx));
    }

    #[test]
    fn transfer_creates_receiver_and_change_tokens() {
        let mut engine = TrstEngine::with_expiry(3600);
        let sender = test_address(1);
        let receiver = test_address(2);
        let token = engine
            .mint(test_hash(1), sender.clone(), 1000, sender.clone(), ts(1000))
            .unwrap();

        let (recv, change) = engine
            .transfer(
                &token,
                &sender,
                receiver.clone(),
                600,
                test_hash(2),
                test_hash(3),
                ts(1500),
            )
            .unwrap();
        assert_eq!(recv.amount, 600);
        assert_eq!(recv.origin, test_hash(1));
        assert_eq!(recv.link, token.id);
        assert_eq!(recv.holder, receiver);
        let change = change.unwrap();
        assert_eq!(change.amount, 400);
        assert_eq!(change.holder, sender);
    }

    #[test]
    fn self_transfer_preserves_value_and_does_not_bypass_spending_rules() {
        let mut engine = TrstEngine::with_expiry(3600);
        let owner = test_address(1);
        let token = engine.mint(test_hash(1), owner.clone(), 1000, owner.clone(), ts(1000)).unwrap();
        for amount in [1, 600, 1000] {
            let (received, change) = engine.transfer(&token, &owner, owner.clone(), amount,
                test_hash(2), test_hash(3), ts(1500)).unwrap();
            assert_eq!(received.amount + change.as_ref().map_or(0, |t| t.amount), 1000);
            assert_eq!(change.is_none(), amount == 1000);
            for output in std::iter::once(&received).chain(change.as_ref()) {
                assert_eq!(output.holder, owner);
                assert_eq!(output.origin, token.origin);
                assert_eq!(output.link, token.id);
                assert_eq!(output.effective_origin_timestamp, token.effective_origin_timestamp);
                assert_eq!(output.basis, token.basis);
            }
        }
        for amount in [0, 1001] {
            assert!(engine.transfer(&token, &owner, owner.clone(), amount,
                test_hash(2), test_hash(3), ts(1500)).is_err());
        }
        assert!(matches!(engine.transfer(&token, &owner, owner.clone(), 500,
            test_hash(2), test_hash(3), ts(5000)), Err(TrstError::NotTransferable(_))));
        let mut revoked = token;
        revoked.state = TrstState::Revoked;
        assert!(matches!(engine.transfer(&revoked, &owner, owner.clone(), 500,
            test_hash(2), test_hash(3), ts(1500)), Err(TrstError::NotTransferable(_))));
    }

    #[test]
    fn transfer_of_expired_token_fails() {
        let mut engine = TrstEngine::with_expiry(3600);
        let sender = test_address(1);
        let token = engine
            .mint(test_hash(1), sender.clone(), 1000, sender.clone(), ts(1000))
            .unwrap();
        let result = engine.transfer(
            &token,
            &sender,
            test_address(2),
            500,
            test_hash(2),
            test_hash(3),
            ts(5000),
        );
        assert!(matches!(result, Err(TrstError::NotTransferable(_))));
    }

    #[test]
    fn merged_token_uses_merge_tx_as_origin() {
        let mut engine = TrstEngine::with_expiry(3600);
        let holder = test_address(5);
        let t1 = engine
            .mint(
                test_hash(1),
                holder.clone(),
                500,
                test_address(10),
                ts(1000),
            )
            .unwrap();
        let t2 = engine
            .mint(
                test_hash(2),
                holder.clone(),
                300,
                test_address(11),
                ts(1100),
            )
            .unwrap();

        let merge_tx = test_hash(10);
        let merged = engine
            .merge(&[t1, t2], holder.clone(), merge_tx, ts(1500))
            .unwrap();

        assert_eq!(merged.amount, 800);
        // Whitepaper: merged token's origin is the merge tx hash.
        assert_eq!(merged.origin, merge_tx);
        assert_eq!(merged.id, merge_tx);
        assert_eq!(merged.effective_origin_timestamp, ts(1000)); // earliest constituent
        assert_eq!(merged.origin_timestamp, ts(1500));
        assert_eq!(merged.state, TrstState::Active);
        // origin_wallet is the merging wallet (self-operation).
        assert_eq!(merged.origin_wallet, holder);

        // Graph records IMMEDIATE inputs.
        let node = engine.merger_graph.get_merge(&merge_tx).unwrap();
        assert_eq!(node.source_origins.len(), 2);
        assert_eq!(node.total_amount, 800);
    }

    #[test]
    fn merge_overflow_does_not_record_a_partial_graph() {
        let mut engine = TrstEngine::with_expiry(u64::MAX);
        let holder = test_address(5);
        let first = engine
            .mint(
                test_hash(1),
                holder.clone(),
                u128::MAX,
                test_address(10),
                ts(1000),
            )
            .unwrap();
        let second = engine
            .mint(test_hash(2), holder.clone(), 1, test_address(11), ts(1000))
            .unwrap();
        let before = engine.merger_graph.to_bytes();
        let error = engine
            .merge(&[first, second], holder, test_hash(3), ts(1001))
            .unwrap_err();
        assert!(error.to_string().contains("exceeds u128"));
        assert_eq!(engine.merger_graph.to_bytes(), before);
    }

    #[test]
    fn merge_of_merged_token_links_downstream() {
        let mut engine = TrstEngine::with_expiry(u64::MAX);
        let holder = test_address(5);
        let t1 = engine
            .mint(
                test_hash(1),
                holder.clone(),
                500,
                test_address(10),
                ts(1000),
            )
            .unwrap();
        let t2 = engine
            .mint(
                test_hash(2),
                holder.clone(),
                300,
                test_address(11),
                ts(1100),
            )
            .unwrap();
        let merged1 = engine
            .merge(&[t1, t2], holder.clone(), test_hash(20), ts(1500))
            .unwrap();

        let t3 = engine
            .mint(
                test_hash(3),
                holder.clone(),
                200,
                test_address(12),
                ts(1600),
            )
            .unwrap();
        let merged2 = engine
            .merge(&[merged1, t3], holder.clone(), test_hash(21), ts(1700))
            .unwrap();

        assert_eq!(merged2.origin, test_hash(21));
        // The second merge's node lists merge1's tx as an immediate source.
        let node = engine.merger_graph.get_merge(&test_hash(21)).unwrap();
        assert!(node
            .source_origins
            .iter()
            .any(|s| s.origin == test_hash(20) && s.amount == 800));
    }

    #[test]
    fn merge_accepts_maximum_sources_and_rejects_one_more() {
        let mut engine = TrstEngine::with_expiry(u64::MAX);
        let holder = test_address(1);
        let mut tokens = Vec::new();
        for i in 0..(MAX_MERGE_SOURCES + 1) {
            let mut h = [0u8; 32];
            h[0] = (i % 256) as u8;
            h[1] = (i / 256) as u8;
            h[2] = 7;
            tokens.push(engine.mint(TxHash::new(h), holder.clone(), 1,
                holder.clone(), ts(1000)).unwrap());
        }
        let result = engine.merge(&tokens, holder.clone(), test_hash(99), ts(1500));
        assert!(result.is_err());
        assert!(engine.merger_graph.get_merge(&test_hash(99)).is_none());
        tokens.pop();
        let merged = engine.merge(&tokens, holder.clone(), test_hash(99), ts(1500)).unwrap();
        assert_eq!(merged.amount, MAX_MERGE_SOURCES as u128);
        assert_eq!(merged.holder, holder);
        assert_eq!(merged.effective_origin_timestamp, ts(1000));
        assert_eq!(engine.merger_graph.get_merge(&test_hash(99)).unwrap().source_origins.len(), MAX_MERGE_SOURCES);
    }

    #[test]
    fn expired_tokens_can_be_consolidated_by_merge() {
        // 6.8(b): merge-only consolidation of expired TRST.
        let mut engine = TrstEngine::with_expiry(100);
        let holder = test_address(1);
        let t1 = engine
            .mint(
                test_hash(1),
                holder.clone(),
                500,
                test_address(10),
                ts(1000),
            )
            .unwrap();
        let t2 = engine
            .mint(
                test_hash(2),
                holder.clone(),
                300,
                test_address(10),
                ts(1010),
            )
            .unwrap();
        // Both expired by now.
        let merged = engine
            .merge(&[t1, t2], holder.clone(), test_hash(10), ts(5000))
            .unwrap();
        assert_eq!(merged.state, TrstState::Expired);
        assert_eq!(merged.amount, 800);
    }

    #[test]
    fn merge_rejects_mixed_live_and_expired_tokens() {
        // The earliest-expiry floor rule would silently expire the live
        // value — consolidation (6.8b) is expired-with-expired only.
        let mut engine = TrstEngine::with_expiry(100);
        let holder = test_address(1);
        let expired = engine
            .mint(
                test_hash(1),
                holder.clone(),
                500,
                test_address(10),
                ts(1000),
            )
            .unwrap();
        let live = engine
            .mint(
                test_hash(2),
                holder.clone(),
                300,
                test_address(10),
                ts(4990),
            )
            .unwrap();
        // At t=5000: token 1 (born 1000, expiry 100s) is expired; token 2 is live.
        let result = engine.merge(&[expired, live], holder, test_hash(10), ts(5000));
        assert!(result.is_err());
    }

    #[test]
    fn merge_rejects_revoked_tokens() {
        let mut engine = TrstEngine::with_expiry(u64::MAX);
        let holder = test_address(1);
        let t1 = engine
            .mint(
                test_hash(1),
                holder.clone(),
                500,
                test_address(10),
                ts(1000),
            )
            .unwrap();
        let mut t2 = engine
            .mint(
                test_hash(2),
                holder.clone(),
                300,
                test_address(10),
                ts(1010),
            )
            .unwrap();
        t2.state = TrstState::Revoked;
        t2.revoked_origin = Some(t2.origin);
        let result = engine.merge(&[t1, t2], holder, test_hash(10), ts(1500));
        assert!(matches!(result, Err(TrstError::NotTransferable(_))));
    }

    #[test]
    fn track_token_uses_engine_expiry_for_earliest_expiry() {
        // Regression: track_token previously computed expiry with u64::MAX,
        // which disabled lazy expiry flushing entirely.
        let mut engine = TrstEngine::with_expiry(100);
        let holder = test_address(1);
        let token = engine
            .mint(
                test_hash(1),
                holder.clone(),
                500,
                test_address(10),
                ts(1000),
            )
            .unwrap();
        engine.track_token(token);

        let p = engine.get_portfolio(&holder).unwrap();
        assert_eq!(p.earliest_expiry, Some(ts(1100)));
        assert_eq!(p.cached_transferable, 500);

        // After expiry passes, the balance flushes to zero.
        assert_eq!(engine.transferable_balance(&holder, ts(1200)), Some(0));
        let p = engine.get_portfolio(&holder).unwrap();
        assert_eq!(p.tokens[0].state, TrstState::Expired);
    }

    #[test]
    fn simple_revocation_revokes_current_holders_only() {
        let mut engine = TrstEngine::with_expiry(u64::MAX);
        let origin_wallet = test_address(10);
        let a = test_address(1);
        let b = test_address(2);

        let token = engine
            .mint(
                test_hash(1),
                a.clone(),
                500,
                origin_wallet.clone(),
                ts(1000),
            )
            .unwrap();
        engine.track_token(token);

        // a sends the whole token to b: debit from a, track under b.
        let prov = engine.debit_wallet_with_provenance(&a, &test_hash(1), 500);
        assert_eq!(prov.len(), 1);
        let received = TrstToken {
            basis: TrstBasis::Entire,
            id: test_hash(50),
            amount: 500,
            origin: prov[0].origin,
            link: test_hash(50),
            holder: b.clone(),
            origin_timestamp: prov[0].origin_timestamp,
            effective_origin_timestamp: prov[0].effective_origin_timestamp,
            state: TrstState::Active,
            origin_wallet: prov[0].origin_wallet.clone(),
            revoked_origin: None,
        };
        engine.receive_token(received, ts(1100));

        let events = engine.revoke_by_origin(&origin_wallet);
        // Only b (current holder) is affected; a holds nothing.
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].holder, b);
        assert_eq!(events[0].revoked_amount, 500);

        let pb = engine.get_portfolio(&b).unwrap();
        assert_eq!(pb.tokens[0].state, TrstState::Revoked);
        assert_eq!(pb.tokens[0].revoked_origin, Some(test_hash(1)));
        assert_eq!(pb.cached_transferable, 0);
    }

    #[test]
    fn merged_revocation_splits_proportionally() {
        // 7.2(c): the tainted portion becomes a separate revoked token,
        // the clean remainder stays live. 6.18(b): round up the cut.
        let mut engine = TrstEngine::with_expiry(u64::MAX);
        let ow1 = test_address(10);
        let ow2 = test_address(11);
        let holder = test_address(5);

        let t1 = engine
            .mint(test_hash(1), holder.clone(), 600, ow1.clone(), ts(1000))
            .unwrap();
        let t2 = engine
            .mint(test_hash(2), holder.clone(), 400, ow2.clone(), ts(1100))
            .unwrap();
        engine.track_token(t1.clone());
        engine.track_token(t2.clone());

        let merged = engine
            .merge(&[t1, t2], holder.clone(), test_hash(10), ts(1500))
            .unwrap();
        let ids: HashSet<TxHash> = [test_hash(1), test_hash(2)].into_iter().collect();
        engine.bulk_untrack(&holder, &ids);
        engine.track_token(merged);

        let events = engine.revoke_by_origin(&ow1);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].revoked_amount, 600);
        assert_eq!(events[0].total_amount, 1000);

        let p = engine.get_portfolio(&holder).unwrap();
        // Two tokens now: the clean remainder (400, Active) and the revoked chunk (600).
        assert_eq!(p.tokens.len(), 2);
        let live = p
            .tokens
            .iter()
            .find(|t| t.state == TrstState::Active)
            .unwrap();
        let dead = p
            .tokens
            .iter()
            .find(|t| t.state == TrstState::Revoked)
            .unwrap();
        assert_eq!(live.amount, 400);
        assert_eq!(live.origin, test_hash(10));
        assert_eq!(dead.amount, 600);
        assert_eq!(dead.revoked_origin, Some(test_hash(1)));
        assert_eq!(p.cached_transferable, 400);
    }

    #[test]
    fn revocation_propagates_through_multi_level_merges() {
        let mut engine = TrstEngine::with_expiry(u64::MAX);
        let ow1 = test_address(10);
        let holder = test_address(5);

        let t1 = engine
            .mint(test_hash(1), holder.clone(), 50, ow1.clone(), ts(1000))
            .unwrap();
        let t2 = engine
            .mint(test_hash(2), holder.clone(), 50, test_address(11), ts(1100))
            .unwrap();
        engine.track_token(t1.clone());
        engine.track_token(t2.clone());
        let m1 = engine
            .merge(&[t1, t2], holder.clone(), test_hash(20), ts(1500))
            .unwrap();
        let ids: HashSet<TxHash> = [test_hash(1), test_hash(2)].into_iter().collect();
        engine.bulk_untrack(&holder, &ids);
        engine.track_token(m1.clone());

        let t3 = engine
            .mint(
                test_hash(3),
                holder.clone(),
                100,
                test_address(12),
                ts(1600),
            )
            .unwrap();
        engine.track_token(t3.clone());
        let m2 = engine
            .merge(&[m1, t3], holder.clone(), test_hash(21), ts(1700))
            .unwrap();
        let ids: HashSet<TxHash> = [test_hash(20), test_hash(3)].into_iter().collect();
        engine.bulk_untrack(&holder, &ids);
        engine.track_token(m2);

        let events = engine.revoke_by_origin(&ow1);
        // Only the live m2 token is split (m1's token was consumed):
        // m1 tainted 50/100 → m2 consumed 100 of m1 → 50 tainted of m2's 200.
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].revoked_amount, 50);

        let p = engine.get_portfolio(&holder).unwrap();
        assert_eq!(p.cached_transferable, 150);
    }

    #[test]
    fn sequential_revocations_of_different_origins() {
        let mut engine = TrstEngine::with_expiry(u64::MAX);
        let ow1 = test_address(10);
        let ow2 = test_address(11);
        let holder = test_address(5);

        let t1 = engine
            .mint(test_hash(1), holder.clone(), 600, ow1.clone(), ts(1000))
            .unwrap();
        let t2 = engine
            .mint(test_hash(2), holder.clone(), 400, ow2.clone(), ts(1100))
            .unwrap();
        engine.track_token(t1.clone());
        engine.track_token(t2.clone());
        let merged = engine
            .merge(&[t1, t2], holder.clone(), test_hash(10), ts(1500))
            .unwrap();
        let ids: HashSet<TxHash> = [test_hash(1), test_hash(2)].into_iter().collect();
        engine.bulk_untrack(&holder, &ids);
        engine.track_token(merged);

        engine.revoke_by_origin(&ow1);
        let events2 = engine.revoke_by_origin(&ow2);
        // Remainder was 400, ow2's share of the unrevoked total is 400/400 → all.
        assert_eq!(events2.len(), 1);
        assert_eq!(events2[0].revoked_amount, 400);

        let p = engine.get_portfolio(&holder).unwrap();
        assert_eq!(p.cached_transferable, 0);
        assert!(p.tokens.iter().all(|t| t.state == TrstState::Revoked));
    }

    #[test]
    fn revocation_is_idempotent() {
        let mut engine = TrstEngine::with_expiry(u64::MAX);
        let ow = test_address(10);
        let holder = test_address(1);
        let t = engine
            .mint(test_hash(1), holder.clone(), 500, ow.clone(), ts(1000))
            .unwrap();
        engine.track_token(t);
        assert_eq!(engine.revoke_by_origin(&ow).len(), 1);
        assert!(engine.revoke_by_origin(&ow).is_empty());
    }

    #[test]
    fn un_revoke_restores_simple_tokens() {
        let mut engine = TrstEngine::with_expiry(u64::MAX);
        let ow = test_address(10);
        let a = test_address(1);
        let b = test_address(2);
        let t1 = engine
            .mint(test_hash(1), a.clone(), 500, ow.clone(), ts(1000))
            .unwrap();
        let t2 = engine
            .mint(test_hash(2), b.clone(), 300, ow.clone(), ts(1100))
            .unwrap();
        engine.track_token(t1);
        engine.track_token(t2);

        engine.revoke_by_origin(&ow);
        assert_eq!(engine.get_portfolio(&a).unwrap().cached_transferable, 0);

        let results = engine.un_revoke_by_origin(&ow, ts(2000));
        assert_eq!(results.len(), 2);
        let pa = engine.get_portfolio(&a).unwrap();
        assert_eq!(pa.tokens[0].state, TrstState::Active);
        assert!(pa.tokens[0].revoked_origin.is_none());
        assert_eq!(pa.cached_transferable, 500);
        let pb = engine.get_portfolio(&b).unwrap();
        assert_eq!(pb.cached_transferable, 300);
        assert!(!engine.merger_graph.is_origin_revoked(&test_hash(1)));
    }

    #[test]
    fn un_revoke_restores_split_chunks_of_merged_tokens() {
        let mut engine = TrstEngine::with_expiry(u64::MAX);
        let ow1 = test_address(10);
        let ow2 = test_address(11);
        let holder = test_address(5);

        let t1 = engine
            .mint(test_hash(1), holder.clone(), 600, ow1.clone(), ts(1000))
            .unwrap();
        let t2 = engine
            .mint(test_hash(2), holder.clone(), 400, ow2.clone(), ts(1100))
            .unwrap();
        engine.track_token(t1.clone());
        engine.track_token(t2.clone());
        let merged = engine
            .merge(&[t1, t2], holder.clone(), test_hash(10), ts(1500))
            .unwrap();
        let ids: HashSet<TxHash> = [test_hash(1), test_hash(2)].into_iter().collect();
        engine.bulk_untrack(&holder, &ids);
        engine.track_token(merged);

        engine.revoke_by_origin(&ow1);
        let results = engine.un_revoke_by_origin(&ow1, ts(2000));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].amount, 600);

        let p = engine.get_portfolio(&holder).unwrap();
        assert_eq!(p.cached_transferable, 1000);
        assert!(p.tokens.iter().all(|t| t.state == TrstState::Active));
        assert!(engine
            .merger_graph
            .get_merge(&test_hash(10))
            .unwrap()
            .revoked_contribs
            .is_empty());
    }

    #[test]
    fn un_revoke_of_one_origin_leaves_other_revoked() {
        let mut engine = TrstEngine::with_expiry(u64::MAX);
        let ow1 = test_address(10);
        let ow2 = test_address(11);
        let holder = test_address(5);

        let t1 = engine
            .mint(test_hash(1), holder.clone(), 600, ow1.clone(), ts(1000))
            .unwrap();
        let t2 = engine
            .mint(test_hash(2), holder.clone(), 400, ow2.clone(), ts(1100))
            .unwrap();
        engine.track_token(t1.clone());
        engine.track_token(t2.clone());
        let merged = engine
            .merge(&[t1, t2], holder.clone(), test_hash(10), ts(1500))
            .unwrap();
        let ids: HashSet<TxHash> = [test_hash(1), test_hash(2)].into_iter().collect();
        engine.bulk_untrack(&holder, &ids);
        engine.track_token(merged);

        engine.revoke_by_origin(&ow1);
        engine.revoke_by_origin(&ow2);
        // Restore only ow1: its 600 chunk comes back, ow2's 400 stays revoked.
        let results = engine.un_revoke_by_origin(&ow1, ts(2000));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].amount, 600);
        let p = engine.get_portfolio(&holder).unwrap();
        assert_eq!(p.cached_transferable, 600);
        assert!(p
            .tokens
            .iter()
            .any(|t| t.state == TrstState::Revoked && t.revoked_origin == Some(test_hash(2))));
    }

    #[test]
    fn debit_spans_multiple_tokens_of_same_origin() {
        let mut engine = TrstEngine::with_expiry(u64::MAX);
        let holder = test_address(1);
        let origin = test_hash(1);
        // Two tokens with the same origin (e.g. change + a receive).
        let base = engine
            .mint(origin, holder.clone(), 300, test_address(10), ts(1000))
            .unwrap();
        engine.track_token(base.clone());
        let second = TrstToken {
            id: test_hash(2),
            amount: 200,
            ..base.clone()
        };
        engine.track_token(second);

        let prov = engine.debit_wallet_with_provenance(&holder, &origin, 450);
        assert_eq!(prov.len(), 1);
        assert_eq!(prov[0].amount, 450);

        let p = engine.get_portfolio(&holder).unwrap();
        assert_eq!(p.cached_transferable, 50);
        assert_eq!(p.tokens.len(), 1);
        assert_eq!(p.tokens[0].amount, 50);
    }

    #[test]
    fn debit_only_subtracts_what_was_consumed() {
        // Regression: the cache used to be decremented by the requested
        // amount even when the origin's tokens held less.
        let mut engine = TrstEngine::with_expiry(u64::MAX);
        let holder = test_address(1);
        let t1 = engine
            .mint(
                test_hash(1),
                holder.clone(),
                300,
                test_address(10),
                ts(1000),
            )
            .unwrap();
        let t2 = engine
            .mint(
                test_hash(2),
                holder.clone(),
                500,
                test_address(10),
                ts(1100),
            )
            .unwrap();
        engine.track_token(t1);
        engine.track_token(t2);

        engine.debit_wallet(&holder, &test_hash(1), 400); // only 300 exists
        let p = engine.get_portfolio(&holder).unwrap();
        assert_eq!(p.cached_transferable, 500);
    }

    #[test]
    fn origin_transferable_reports_per_origin_balance() {
        let mut engine = TrstEngine::with_expiry(u64::MAX);
        let holder = test_address(1);
        let t1 = engine
            .mint(
                test_hash(1),
                holder.clone(),
                300,
                test_address(10),
                ts(1000),
            )
            .unwrap();
        let t2 = engine
            .mint(
                test_hash(2),
                holder.clone(),
                500,
                test_address(10),
                ts(1100),
            )
            .unwrap();
        engine.track_token(t1);
        engine.track_token(t2);
        assert_eq!(
            engine.origin_transferable(&holder, &test_hash(1), ts(1200)),
            300
        );
        assert_eq!(
            engine.origin_transferable(&holder, &test_hash(2), ts(1200)),
            500
        );
        assert_eq!(
            engine.origin_transferable(&holder, &test_hash(9), ts(1200)),
            0
        );
    }

    #[test]
    fn candidate_validation_cannot_expire_live_portfolio() {
        let mut engine = TrstEngine::with_expiry(100);
        let holder = test_address(1);
        let origin = test_hash(1);
        let token = engine
            .mint(origin, holder.clone(), 300, test_address(10), ts(1000))
            .unwrap();
        engine.track_token(token);
        let before = engine.save_wallets();
        assert_eq!(
            engine.candidate_balances(&holder, &origin, ts(1200)),
            Some((0, 0))
        );
        assert_eq!(engine.save_wallets(), before);
        assert_eq!(
            engine.candidate_balances(&holder, &origin, ts(1050)),
            Some((300, 300))
        );
        assert_eq!(engine.save_wallets(), before);
    }

    #[test]
    fn receive_token_applies_outstanding_simple_revocation() {
        let mut engine = TrstEngine::with_expiry(u64::MAX);
        let ow = test_address(10);
        let receiver = test_address(2);
        // The burn origin is revoked while the send is pending.
        let sender_token = engine
            .mint(test_hash(1), test_address(1), 500, ow.clone(), ts(1000))
            .unwrap();
        engine.track_token(sender_token);
        engine.debit_wallet(&test_address(1), &test_hash(1), 500);
        engine.revoke_by_origin(&ow);

        let incoming = TrstToken {
            basis: TrstBasis::Entire,
            id: test_hash(50),
            amount: 500,
            origin: test_hash(1),
            link: test_hash(50),
            holder: receiver.clone(),
            origin_timestamp: ts(1000),
            effective_origin_timestamp: ts(1000),
            state: TrstState::Active,
            origin_wallet: ow.clone(),
            revoked_origin: None,
        };
        let events = engine.receive_token(incoming, ts(1200));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].revoked_amount, 500);

        let p = engine.get_portfolio(&receiver).unwrap();
        assert_eq!(p.cached_transferable, 0);
        assert_eq!(p.tokens[0].state, TrstState::Revoked);

        // And un-revocation later restores the received token too.
        let restored = engine.un_revoke_by_origin(&ow, ts(1300));
        assert!(restored
            .iter()
            .any(|r| r.holder == receiver && r.amount == 500));
    }

    #[test]
    fn receive_token_applies_outstanding_merged_revocation() {
        let mut engine = TrstEngine::with_expiry(u64::MAX);
        let ow1 = test_address(10);
        let holder = test_address(5);
        let receiver = test_address(6);

        let t1 = engine
            .mint(test_hash(1), holder.clone(), 600, ow1.clone(), ts(1000))
            .unwrap();
        let t2 = engine
            .mint(
                test_hash(2),
                holder.clone(),
                400,
                test_address(11),
                ts(1100),
            )
            .unwrap();
        engine.track_token(t1.clone());
        engine.track_token(t2.clone());
        let merged = engine
            .merge(&[t1, t2], holder.clone(), test_hash(10), ts(1500))
            .unwrap();
        let ids: HashSet<TxHash> = [test_hash(1), test_hash(2)].into_iter().collect();
        engine.bulk_untrack(&holder, &ids);
        engine.track_token(merged);

        // Holder sends half of the merged token; the send is in flight
        // when the revocation lands.
        engine.debit_wallet(&holder, &test_hash(10), 500);
        engine.revoke_by_origin(&ow1);

        let incoming = TrstToken {
            basis: TrstBasis::Entire,
            id: test_hash(60),
            amount: 500,
            origin: test_hash(10),
            link: test_hash(60),
            holder: receiver.clone(),
            origin_timestamp: ts(1500),
            effective_origin_timestamp: ts(1000),
            state: TrstState::Active,
            origin_wallet: holder.clone(),
            revoked_origin: None,
        };
        let events = engine.receive_token(incoming, ts(1600));
        // 60% of the merge is tainted → ceil(500 * 600/1000) = 300 revoked.
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].revoked_amount, 300);
        let p = engine.get_portfolio(&receiver).unwrap();
        assert_eq!(p.cached_transferable, 200);
    }

    #[test]
    fn set_expiry_period_preserves_unchanged_shared_portfolios() {
        let mut engine = TrstEngine::with_expiry(100);
        for (id, at) in [(1, 1000), (2, 1100)] {
            let token = engine
                .mint(
                    test_hash(id),
                    test_address(id),
                    500,
                    test_address(10),
                    ts(at),
                )
                .unwrap();
            engine.track_token(token);
        }
        let original = engine.clone();
        engine.set_expiry_period(100, ts(1050));
        assert!(engine.wallets.shares_storage_with(&original.wallets));
        engine.set_expiry_period(100, ts(1100));
        assert!(!engine.wallets.shares_storage_with(&original.wallets));
        assert!(!engine.wallets[&test_address(1)]
            .shares_storage_with(&original.wallets[&test_address(1)]));
        assert!(engine.wallets[&test_address(2)]
            .shares_storage_with(&original.wallets[&test_address(2)]));
        assert_eq!(
            original.wallets[&test_address(1)].tokens[0].state,
            TrstState::Active
        );
        assert_eq!(
            engine.wallets[&test_address(1)].tokens[0].state,
            TrstState::Expired
        );
        let normalized = engine.clone();
        engine.set_expiry_period(100, ts(1100));
        assert!(engine.wallets.shares_storage_with(&normalized.wallets));
    }

    #[test]
    fn set_expiry_period_matches_full_normalization_for_all_states() {
        let mut original = TrstEngine::with_expiry(100);
        for (index, state) in [
            TrstState::Active,
            TrstState::Expired,
            TrstState::Pending,
            TrstState::Revoked,
        ]
        .into_iter()
        .enumerate()
        {
            let id = index as u8 + 1;
            let mut token = original
                .mint(
                    test_hash(id),
                    test_address(id),
                    500,
                    test_address(10),
                    ts(1000),
                )
                .unwrap();
            token.state = state;
            original.track_token(token);
            // Normalization must repair caches even without a state transition.
            let portfolio = original.wallets.get_mut(&test_address(id)).unwrap();
            portfolio.cached_transferable = 123;
            portfolio.earliest_expiry = Some(ts(7));
        }
        for period in [0, 50, 100, 10_000, u64::MAX] {
            for at in [999, 1050, 1100, 1200, u64::MAX] {
                let mut expected = original.clone();
                expected.expiry_secs = period;
                for portfolio in expected.wallets.values_mut() {
                    for token in &mut portfolio.tokens {
                        match token.state {
                            TrstState::Active if token.is_expired(ts(at), period) => {
                                token.state = TrstState::Expired
                            }
                            TrstState::Expired if !token.is_expired(ts(at), period) => {
                                token.state = TrstState::Active
                            }
                            _ => {}
                        }
                    }
                    portfolio.recompute_transferable(ts(at), period);
                    portfolio.recompute_earliest_expiry(period);
                }
                let mut actual = original.clone();
                actual.set_expiry_period(period, ts(at));
                assert_eq!(
                    actual.save_wallets(),
                    expected.save_wallets(),
                    "period={period}, at={at}"
                );
                assert_eq!(
                    actual.merger_graph.to_bytes(),
                    original.merger_graph.to_bytes()
                );
            }
        }
    }

    #[test]
    fn set_expiry_period_resurrects_expired_tokens() {
        // 6.9: "untransferrable trst can become transferrable again if the
        // governance expiry period allows it"
        let mut engine = TrstEngine::with_expiry(100);
        let holder = test_address(1);
        let token = engine
            .mint(
                test_hash(1),
                holder.clone(),
                500,
                test_address(10),
                ts(1000),
            )
            .unwrap();
        engine.track_token(token);

        // Expire it.
        engine.flush_all_expired(ts(1200));
        assert_eq!(
            engine.get_portfolio(&holder).unwrap().cached_transferable,
            0
        );
        assert_eq!(
            engine.get_portfolio(&holder).unwrap().tokens[0].state,
            TrstState::Expired
        );

        // Governance extends the period — the token comes back.
        engine.set_expiry_period(10_000, ts(1200));
        let p = engine.get_portfolio(&holder).unwrap();
        assert_eq!(p.tokens[0].state, TrstState::Active);
        assert_eq!(p.cached_transferable, 500);

        // Governance shortens it again — the token expires immediately.
        engine.set_expiry_period(50, ts(1200));
        let p = engine.get_portfolio(&holder).unwrap();
        assert_eq!(p.tokens[0].state, TrstState::Expired);
        assert_eq!(p.cached_transferable, 0);
    }

    #[test]
    fn set_expiry_period_does_not_resurrect_revoked_tokens() {
        let mut engine = TrstEngine::with_expiry(100);
        let ow = test_address(10);
        let holder = test_address(1);
        let token = engine
            .mint(test_hash(1), holder.clone(), 500, ow.clone(), ts(1000))
            .unwrap();
        engine.track_token(token);
        engine.revoke_by_origin(&ow);

        engine.set_expiry_period(10_000, ts(1200));
        let p = engine.get_portfolio(&holder).unwrap();
        assert_eq!(p.tokens[0].state, TrstState::Revoked);
        assert_eq!(p.cached_transferable, 0);
    }

    #[test]
    fn cached_transferable_stays_consistent_with_full_recompute() {
        let mut engine = TrstEngine::with_expiry(3600);
        let holder = test_address(1);
        let origin_wallet = test_address(10);

        let t1 = engine
            .mint(
                test_hash(1),
                holder.clone(),
                1000,
                origin_wallet.clone(),
                ts(100),
            )
            .unwrap();
        engine.track_token(t1.clone());
        let t2 = engine
            .mint(
                test_hash(2),
                holder.clone(),
                500,
                origin_wallet.clone(),
                ts(200),
            )
            .unwrap();
        engine.track_token(t2.clone());
        let t3 = engine
            .mint(
                test_hash(3),
                holder.clone(),
                300,
                origin_wallet.clone(),
                ts(300),
            )
            .unwrap();
        engine.track_token(t3);

        let now = ts(400);
        let p = engine.wallets.get_mut(&holder).unwrap();
        let cached = p.cached_transferable;
        p.recompute_transferable(now, 3600);
        assert_eq!(p.cached_transferable, cached);
        assert_eq!(cached, 1800);

        engine.debit_wallet(&holder, &test_hash(1), 400);
        let p = engine.wallets.get_mut(&holder).unwrap();
        let cached = p.cached_transferable;
        p.recompute_transferable(now, 3600);
        assert_eq!(p.cached_transferable, cached);

        let remaining_t1 = engine
            .get_portfolio(&holder)
            .unwrap()
            .tokens
            .iter()
            .find(|t| t.origin == test_hash(1))
            .cloned()
            .unwrap();
        let merged = engine
            .merge(
                &[remaining_t1.clone(), t2.clone()],
                holder.clone(),
                test_hash(20),
                now,
            )
            .unwrap();
        let ids: HashSet<TxHash> = [remaining_t1.id, t2.id].into_iter().collect();
        engine.bulk_untrack(&holder, &ids);
        engine.track_token(merged);
        let p = engine.wallets.get_mut(&holder).unwrap();
        let cached = p.cached_transferable;
        p.recompute_transferable(now, 3600);
        assert_eq!(p.cached_transferable, cached);

        let far_future = ts(100 + 3600 + 1);
        let p = engine.wallets.get_mut(&holder).unwrap();
        p.flush_expired(far_future, 3600);
        let cached = p.cached_transferable;
        p.recompute_transferable(far_future, 3600);
        assert_eq!(p.cached_transferable, cached);
    }

    #[test]
    fn wallet_snapshot_bytes_survive_rehashed_roundtrips() {
        let mut engine = TrstEngine::with_expiry(u64::MAX);
        for id in 1..=32 {
            let token = engine
                .mint(
                    test_hash(id),
                    test_address(id),
                    500,
                    test_address(99),
                    ts(1000),
                )
                .unwrap();
            engine.track_token(token);
        }
        let bytes = engine.save_wallets();
        for _ in 0..16 {
            engine = TrstEngine::load_wallets(&bytes, u64::MAX).unwrap();
            assert_eq!(engine.save_wallets(), bytes);
        }
    }

    #[test]
    fn save_and_load_wallets_rebuilds_indexes() {
        let mut engine = TrstEngine::with_expiry(u64::MAX);
        let ow = test_address(10);
        let holder = test_address(1);
        let token = engine
            .mint(test_hash(1), holder.clone(), 500, ow.clone(), ts(1000))
            .unwrap();
        engine.track_token(token);

        let bytes = engine.save_wallets();
        let mut restored = TrstEngine::load_wallets(&bytes, u64::MAX).unwrap();
        assert!(TrstEngine::load_wallets(b"corrupt", u64::MAX).is_err());
        assert_eq!(
            restored.get_portfolio(&holder).unwrap().cached_transferable,
            500
        );
        // Revocation works after restore (indexes rebuilt).
        let events = restored.revoke_by_origin(&ow);
        assert_eq!(events.len(), 1);
    }

    fn restart(engine: &TrstEngine) -> TrstEngine {
        let mut restored =
            TrstEngine::load_wallets(&engine.save_wallets(), engine.expiry_secs).unwrap();
        restored.merger_graph = MergerGraph::from_bytes(&engine.merger_graph.to_bytes()).unwrap();
        restored.rebuild_indexes();
        restored
    }

    #[test]
    fn pending_origin_revocation_and_reverification_survive_restarts() {
        for already_received in [false, true] {
            let mut engine = TrstEngine::with_expiry(10_000);
            let owner = test_address(10);
            let holder = test_address(1);
            let token = engine
                .mint(test_hash(1), holder.clone(), 500, owner.clone(), ts(1000))
                .unwrap();
            if already_received {
                engine.track_token(token.clone());
                let debited =
                    engine.debit_wallet_with_provenance(&holder, &token.origin, token.amount);
                assert_eq!(debited.iter().map(|p| p.amount).sum::<u128>(), 500);
            }
            let mut engine = restart(&engine);
            assert!(engine.wallet_origins[&owner].contains(&token.origin));
            assert!(engine.revoke_by_origin(&owner).is_empty());
            assert!(engine.merger_graph.is_origin_revoked(&token.origin));

            // Re-verification before delivery must also find the absent origin.
            let mut reverified = restart(&engine);
            assert!(reverified.un_revoke_by_origin(&owner, ts(2000)).is_empty());
            let mut reverified = restart(&reverified);
            assert!(reverified.receive_token(token.clone(), ts(2001)).is_empty());
            assert_eq!(
                reverified.transferable_balance(&holder, ts(2001)),
                Some(500)
            );

            // Delivery while still revoked cannot restore spendability.
            let mut engine = restart(&engine);
            let events = engine.receive_token(token, ts(2001));
            assert_eq!(events.iter().map(|e| e.revoked_amount).sum::<u128>(), 500);
            assert_eq!(engine.transferable_balance(&holder, ts(2001)), Some(0));
            let mut engine = restart(&engine);
            engine.un_revoke_by_origin(&owner, ts(2100));
            assert_eq!(engine.transferable_balance(&holder, ts(2100)), Some(500));
        }
    }

    #[test]
    fn consumed_merge_origins_keep_their_original_owners_after_restart() {
        let mut engine = TrstEngine::with_expiry(10_000);
        let holder = test_address(5);
        let first = engine
            .mint(
                test_hash(1),
                holder.clone(),
                600,
                test_address(10),
                ts(1000),
            )
            .unwrap();
        let second = engine
            .mint(
                test_hash(2),
                holder.clone(),
                400,
                test_address(11),
                ts(1100),
            )
            .unwrap();
        engine.track_token(first.clone());
        engine.track_token(second.clone());
        let merged = engine
            .merge(&[first, second], holder.clone(), test_hash(20), ts(1200))
            .unwrap();
        engine.bulk_untrack(&holder, &[test_hash(1), test_hash(2)].into_iter().collect());
        engine.track_token(merged);
        let mut engine = restart(&engine);
        assert!(!engine.wallet_origins.contains_key(&holder));
        assert!(engine.revoke_by_origin(&holder).is_empty());
        let events = engine.revoke_by_origin(&test_address(10));
        assert_eq!(events.iter().map(|e| e.revoked_amount).sum::<u128>(), 600);
        assert_eq!(engine.transferable_balance(&holder, ts(1300)), Some(400));
        let mut engine = restart(&engine);
        engine.un_revoke_by_origin(&test_address(10), ts(1400));
        assert_eq!(engine.transferable_balance(&holder, ts(1400)), Some(1000));
        let mut engine = restart(&engine);
        let events = engine.revoke_by_origin(&test_address(11));
        assert_eq!(events.iter().map(|e| e.revoked_amount).sum::<u128>(), 400);
        assert_eq!(engine.transferable_balance(&holder, ts(1500)), Some(600));
    }

    #[test]
    fn snapshot_rejects_missing_or_duplicate_ownership_and_trailing_data() {
        let engine = TrstEngine::new();
        let legacy = bincode::serialize(&engine.wallets).unwrap();
        assert!(TrstEngine::load_wallets(&legacy, u64::MAX)
            .err()
            .unwrap()
            .to_string()
            .contains("explicit ledger replay"));
        let mut trailing = engine.save_wallets();
        trailing.push(0);
        assert!(TrstEngine::load_wallets(&trailing, u64::MAX).is_err());
        let wallets: Vec<(WalletAddress, WalletPortfolio)> = vec![];
        let origins = vec![
            (test_address(1), vec![test_hash(1)]),
            (test_address(2), vec![test_hash(1)]),
        ];
        let duplicate = bincode::serialize(&(SNAPSHOT_MAGIC, wallets, origins)).unwrap();
        assert!(TrstEngine::load_wallets(&duplicate, u64::MAX).is_err());
    }

    #[test]
    fn pending_merged_revocation_quote_matches_delivery_rounding() {
        let mut engine = TrstEngine::with_expiry(u64::MAX);
        let holder = test_address(5);
        let first = engine
            .mint(test_hash(1), holder.clone(), 31, test_address(10), ts(1000))
            .unwrap();
        let second = engine
            .mint(test_hash(2), holder.clone(), 69, test_address(11), ts(1000))
            .unwrap();
        let merged = engine
            .merge(&[first, second], holder.clone(), test_hash(20), ts(1100))
            .unwrap();
        engine.revoke_by_origin(&test_address(10));
        for both_revoked in [false, true] {
            if both_revoked {
                engine.revoke_by_origin(&test_address(11));
            }
            for amount in 1..=100 {
                let mut candidate = restart(&engine);
                let quote = candidate.pending_revoked_amount(&merged.origin, amount);
                let mut pending = merged.clone();
                pending.amount = amount;
                let events = candidate.receive_token(pending, ts(1200));
                assert_eq!(
                    quote,
                    events
                        .iter()
                        .map(|event| event.revoked_amount)
                        .sum::<u128>()
                );
                assert_eq!(
                    quote,
                    if both_revoked {
                        amount
                    } else {
                        (amount * 31).div_ceil(100)
                    }
                );
            }
        }
    }

    #[test]
    fn settled_delivery_preserves_clean_value_and_rejects_bad_chunks_before_writing() {
        let mut engine = TrstEngine::with_expiry(10_000);
        let holder = test_address(5);
        let first = engine
            .mint(test_hash(1), holder.clone(), 31, test_address(10), ts(1000))
            .unwrap();
        let second = engine
            .mint(test_hash(2), holder.clone(), 69, test_address(11), ts(1000))
            .unwrap();
        let mut merged = engine
            .merge(&[first, second], holder.clone(), test_hash(20), ts(1100))
            .unwrap();
        let (_, effects) = engine.revoke_with_pending_effects(&test_address(10));
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].cut(&merged.origin, 100), 31);
        assert_eq!(effects[0].cut(&merged.origin, 1), 1);
        assert_eq!(effects[0].cut(&test_hash(99), 100), 0);
        let revoked = [(
            *test_hash(1).as_bytes(),
            TrstRevocationChunk {
                amount: 31,
                restored: false,
            },
        )]
        .into_iter()
        .collect();
        merged.basis = engine.merger_graph.current_basis(&merged.origin);
        engine
            .receive_settled_token(merged.clone(), &revoked, ts(1200))
            .unwrap();
        assert_eq!(engine.transferable_balance(&holder, ts(1200)), Some(69));
        let mut clean = merged;
        clean.id = test_hash(21);
        clean.holder = test_address(6);
        clean.amount = 69;
        let mut engine = restart(&engine);
        let consumed = engine.debit_wallet_with_provenance(&holder, &clean.origin, 69);
        assert_eq!(
            consumed.iter().map(|portion| portion.amount).sum::<u128>(),
            69
        );
        engine
            .receive_settled_token(clean.clone(), &Default::default(), ts(1300))
            .unwrap();
        assert_eq!(
            engine.transferable_balance(&clean.holder, ts(1300)),
            Some(69)
        );
        for (origin, amount) in [([0; 32], 1), ([1; 32], 0), ([1; 32], 70)] {
            let before = engine.save_wallets();
            let invalid = [(
                origin,
                TrstRevocationChunk {
                    amount,
                    restored: false,
                },
            )]
            .into_iter()
            .collect();
            assert!(engine
                .receive_settled_token(clean.clone(), &invalid, ts(1300))
                .is_err());
            assert_eq!(engine.save_wallets(), before);
        }
        let before = engine.save_wallets();
        let false_restoration = [(
            *test_hash(1).as_bytes(),
            TrstRevocationChunk {
                amount: 1,
                restored: true,
            },
        )]
        .into_iter()
        .collect();
        assert!(engine
            .receive_settled_token(clean.clone(), &false_restoration, ts(1300))
            .is_err());
        assert_eq!(engine.save_wallets(), before);
        let (_, next) = engine.revoke_with_pending_effects(&test_address(11));
        assert_eq!(next[0].cut(&clean.origin, 69), 69);
    }

    #[test]
    fn remerge_of_clean_remainder_preserves_its_remaining_proportions() {
        let mut engine = TrstEngine::with_expiry(10_000);
        let holder = test_address(5);
        let first = engine
            .mint(test_hash(1), holder.clone(), 40, test_address(10), ts(1000))
            .unwrap();
        let second = engine
            .mint(test_hash(2), holder.clone(), 60, test_address(11), ts(1000))
            .unwrap();
        let merged = engine
            .merge(&[first, second], holder.clone(), test_hash(20), ts(1100))
            .unwrap();
        engine.track_token(merged);
        engine.revoke_by_origin(&test_address(10));
        let clean = engine
            .get_portfolio(&holder)
            .unwrap()
            .tokens
            .iter()
            .find(|token| token.state == TrstState::Active)
            .unwrap()
            .clone();
        assert_eq!(clean.amount, 60);
        let third = engine
            .mint(test_hash(3), holder.clone(), 40, test_address(12), ts(1200))
            .unwrap();
        let remerged = engine
            .merge(&[clean, third], holder.clone(), test_hash(30), ts(1300))
            .unwrap();
        engine.bulk_untrack(&holder, &[test_hash(20)].into_iter().collect());
        engine.track_token(remerged);
        let mut engine = restart(&engine);
        let cuts = engine.revoke_by_origin(&test_address(11));
        assert_eq!(cuts.iter().map(|cut| cut.revoked_amount).sum::<u128>(), 60);
        assert_eq!(engine.transferable_balance(&holder, ts(1400)), Some(40));
        engine.un_revoke_by_origin(&test_address(10), ts(1500));
        let mut engine = restart(&engine);
        let cuts = engine.revoke_by_origin(&test_address(10));
        assert_eq!(cuts.iter().map(|cut| cut.revoked_amount).sum::<u128>(), 40);
        assert!(cuts.iter().all(|cut| cut.token_id != test_hash(30)));
        assert_eq!(engine.transferable_balance(&holder, ts(1600)), Some(40));
    }

    #[test]
    fn restored_chunk_cannot_make_a_moved_clean_remainder_tainted_again() {
        let mut engine = TrstEngine::with_expiry(10_000);
        let holder = test_address(5);
        let receiver = test_address(6);
        let first = engine
            .mint(test_hash(1), holder.clone(), 40, test_address(10), ts(1000))
            .unwrap();
        let second = engine
            .mint(test_hash(2), holder.clone(), 30, test_address(11), ts(1000))
            .unwrap();
        let third = engine
            .mint(test_hash(3), holder.clone(), 30, test_address(12), ts(1000))
            .unwrap();
        let merged = engine
            .merge(
                &[first, second, third],
                holder.clone(),
                test_hash(20),
                ts(1100),
            )
            .unwrap();
        engine.track_token(merged);
        engine.revoke_by_origin(&test_address(10));
        let clean = engine
            .get_portfolio(&holder)
            .unwrap()
            .tokens
            .iter()
            .find(|token| token.state == TrstState::Active)
            .unwrap()
            .clone();
        let (sent, change) = engine
            .transfer(
                &clean,
                &holder,
                receiver.clone(),
                60,
                test_hash(30),
                test_hash(31),
                ts(1200),
            )
            .unwrap();
        assert!(change.is_none());
        engine.debit_wallet(&holder, &clean.origin, 60);
        engine
            .receive_settled_token(sent, &Default::default(), ts(1201))
            .unwrap();
        engine.un_revoke_by_origin(&test_address(10), ts(1300));
        let mut engine = restart(&engine);
        engine.revoke_by_origin(&test_address(11));
        assert_eq!(engine.transferable_balance(&holder, ts(1400)), Some(40));
        assert_eq!(engine.transferable_balance(&receiver, ts(1400)), Some(30));
        engine.revoke_by_origin(&test_address(10));
        assert_eq!(engine.transferable_balance(&holder, ts(1500)), Some(0));
        assert_eq!(engine.transferable_balance(&receiver, ts(1500)), Some(30));
    }

    #[test]
    fn merging_restored_splits_with_their_remainder_keeps_distinct_source_edges() {
        let mut engine = TrstEngine::with_expiry(10_000);
        let holder = test_address(5);
        let mut inputs = Vec::new();
        for (seed, amount) in [(10, 40), (11, 30), (12, 30)] {
            inputs.push(
                engine
                    .mint(
                        test_hash(seed),
                        holder.clone(),
                        amount,
                        test_address(seed),
                        ts(1000),
                    )
                    .unwrap(),
            );
        }
        let first = engine
            .merge(&inputs, holder.clone(), test_hash(20), ts(1100))
            .unwrap();
        engine.track_token(first);
        engine.revoke_by_origin(&test_address(10));
        engine.un_revoke_by_origin(&test_address(10), ts(1200));
        let mut engine = restart(&engine);
        let portions = engine.get_portfolio(&holder).unwrap().tokens.clone();
        assert_eq!(portions.len(), 2);
        let merged = engine
            .merge(&portions, holder.clone(), test_hash(21), ts(1300))
            .unwrap();
        let sources = &engine
            .merger_graph
            .get_merge(&merged.origin)
            .unwrap()
            .source_origins;
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].origin, sources[1].origin);
        engine.bulk_untrack(&holder, &portions.iter().map(|token| token.id).collect());
        engine.track_token(merged);
        let mut engine = restart(&engine);
        for (seed, expected, balance) in [(11, 30, 70), (10, 40, 30), (12, 30, 0)] {
            let events = engine.revoke_by_origin(&test_address(seed));
            assert_eq!(
                events
                    .iter()
                    .map(|event| event.revoked_amount)
                    .sum::<u128>(),
                expected
            );
            assert_eq!(
                engine.transferable_balance(&holder, ts(1400)),
                Some(balance)
            );
            engine = restart(&engine);
        }
    }

    #[test]
    fn shared_wallet_snapshot_checks_all_persisted_fields() {
        let mut live = TrstEngine::new();
        let token = live
            .mint(
                test_hash(1),
                test_address(1),
                100,
                test_address(1),
                ts(1000),
            )
            .unwrap();
        live.track_token(token);
        let bytes = live.save_wallets();
        let decoded = TrstEngine::load_wallets(&bytes, live.expiry_secs).unwrap();
        assert!(!live.shares_wallet_snapshot_with(&decoded));
        assert!(live.wallet_snapshot_eq(&decoded));
        for field in 0..3 {
            let mut candidate = live.clone();
            assert!(live.shares_wallet_snapshot_with(&candidate));
            match field {
                0 => {
                    candidate
                        .get_portfolio_mut(&test_address(1))
                        .unwrap()
                        .cached_transferable -= 1
                }
                1 => {
                    candidate
                        .wallet_origins
                        .entry(test_address(2))
                        .or_default()
                        .insert(test_hash(2));
                }
                _ => {
                    candidate.outcome_invalidations.insert(
                        test_hash(1),
                        OutcomeInvalidation {
                            causes: HashSet::from([BlockHash::new([3; 32])]),
                            wallet_revoked: false,
                        },
                    );
                }
            }
            assert!(!live.shares_wallet_snapshot_with(&candidate));
            assert!(!live.wallet_snapshot_eq(&candidate));
            assert_ne!(candidate.save_wallets(), bytes);
            assert_eq!(live.save_wallets(), bytes);
        }
        let mut derived_only = live.clone();
        derived_only.expiry_secs = 1;
        derived_only.origin_holders.clear();
        assert!(live.wallet_snapshot_eq(&derived_only));
        assert_eq!(derived_only.save_wallets(), bytes);
    }

    #[test]
    fn candidate_copy_detaches_only_the_changed_wallet() {
        let mut live = TrstEngine::with_expiry(10_000);
        for seed in 1..=3 {
            let token = live
                .mint(
                    test_hash(seed),
                    test_address(seed),
                    100,
                    test_address(seed),
                    ts(1000),
                )
                .unwrap();
            live.track_token(token);
        }
        let snapshot = live.save_wallets();
        let graph = live.merger_graph.to_bytes();
        let mut candidate = live.clone();
        assert_eq!(
            candidate.transferable_balance(&test_address(1), ts(1001)),
            Some(100)
        );
        assert_eq!(
            candidate.origin_transferable(&test_address(1), &test_hash(1), ts(1001)),
            100
        );
        assert_eq!(
            candidate.candidate_balances(&test_address(1), &test_hash(1), ts(1001)),
            Some((100, 100))
        );
        candidate.flush_all_expired(ts(1001));
        assert!(candidate.flush_all_expired_by_wallet(ts(1001)).is_empty());
        assert!(live.wallets.shares_storage_with(&candidate.wallets));
        assert!(live
            .wallet_origins
            .shares_storage_with(&candidate.wallet_origins));
        assert!(live
            .origin_holders
            .shares_storage_with(&candidate.origin_holders));
        assert!(live
            .revoked_holders
            .shares_storage_with(&candidate.revoked_holders));
        assert!(live
            .outcome_invalidations
            .shares_storage_with(&candidate.outcome_invalidations));
        for seed in 1..=3 {
            assert!(std::ptr::eq(
                live.get_portfolio(&test_address(seed)).unwrap(),
                candidate.get_portfolio(&test_address(seed)).unwrap()
            ));
        }
        candidate.debit_wallet_with_provenance(&test_address(1), &test_hash(1), 40);
        assert!(!std::ptr::eq(
            live.get_portfolio(&test_address(1)).unwrap(),
            candidate.get_portfolio(&test_address(1)).unwrap()
        ));
        for seed in 2..=3 {
            assert!(std::ptr::eq(
                live.get_portfolio(&test_address(seed)).unwrap(),
                candidate.get_portfolio(&test_address(seed)).unwrap()
            ));
        }
        assert_eq!(
            candidate.transferable_balance_snapshot(&test_address(1)),
            Some(60)
        );
        assert_eq!(live.save_wallets(), snapshot);
        assert_eq!(live.merger_graph.to_bytes(), graph);
        drop(candidate);
        assert_eq!(live.save_wallets(), snapshot);
    }

    #[test]
    fn candidate_revocation_and_expiry_leave_live_wallets_and_indexes_unchanged() {
        let mut live = TrstEngine::with_expiry(100);
        for seed in 1..=3 {
            let token = live
                .mint(
                    test_hash(seed),
                    test_address(seed),
                    100,
                    test_address(seed),
                    ts(1000),
                )
                .unwrap();
            live.track_token(token);
        }
        let bytes = live.save_wallets();
        let graph = live.merger_graph.to_bytes();
        let mut candidate = live.clone();
        candidate.revoke_by_origin(&test_address(1));
        candidate.set_expiry_period(10, ts(1050));
        assert_eq!(
            candidate.get_portfolio(&test_address(1)).unwrap().tokens[0].state,
            TrstState::Revoked
        );
        assert_eq!(
            candidate.get_portfolio(&test_address(2)).unwrap().tokens[0].state,
            TrstState::Expired
        );
        assert_eq!(live.save_wallets(), bytes);
        assert_eq!(live.merger_graph.to_bytes(), graph);
        assert!(live.revoked_holders.is_empty());
        assert!(live.origin_holders[&test_hash(1)].contains(&test_address(1)));
        assert_eq!(live.expiry_secs, 100);
        assert_eq!(live.revoke_by_origin(&test_address(1)).len(), 1);
        assert_eq!(
            live.get_portfolio(&test_address(2)).unwrap().tokens[0].state,
            TrstState::Active
        );
    }

    #[test]
    fn shared_wallet_snapshots_keep_the_plain_value_encoding() {
        let mut engine = TrstEngine::with_expiry(100);
        for seed in [3, 1, 2] {
            let token = engine
                .mint(
                    test_hash(seed),
                    test_address(seed),
                    100,
                    test_address(seed),
                    ts(1000),
                )
                .unwrap();
            engine.track_token(token);
        }
        let mut wallets: Vec<_> = engine
            .wallets
            .iter()
            .map(|(address, portfolio)| (address, &**portfolio))
            .collect();
        wallets.sort_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));
        let mut origins: Vec<_> = engine
            .wallet_origins
            .iter()
            .map(|(address, origins)| {
                let mut origins: Vec<_> = origins.iter().collect();
                origins.sort_by_key(|origin| *origin.as_bytes());
                (address, origins)
            })
            .collect();
        origins.sort_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));
        let plain = bincode::serialize(&(SNAPSHOT_MAGIC, wallets, origins)).unwrap();
        assert_eq!(engine.save_wallets(), plain);
        assert_eq!(
            TrstEngine::load_wallets(&plain, 100)
                .unwrap()
                .save_wallets(),
            plain
        );
    }

    #[test]
    #[ignore = "manual snapshot-copy measurement, not transaction throughput"]
    fn shared_wallet_candidate_copy_cost() {
        use std::{hint::black_box, time::Instant};
        for count in [100u64, 1_000, 10_000] {
            let mut engine = TrstEngine::new();
            for n in 0..count {
                let address = WalletAddress::new(format!("brst_copy_fixture_{n}"));
                let id = TxHash::new(burst_crypto::blake2b_256(&n.to_be_bytes()));
                let token = engine
                    .mint(id, address.clone(), 100, address, ts(1000))
                    .unwrap();
                engine.track_token(token);
            }
            let start = Instant::now();
            for _ in 0..20 {
                let copied: HashMap<_, _> = engine
                    .wallets
                    .iter()
                    .map(|(address, portfolio)| (address.clone(), (**portfolio).clone()))
                    .collect();
                black_box(copied);
            }
            let deep = start.elapsed();
            let start = Instant::now();
            for _ in 0..20 {
                black_box(engine.clone());
            }
            eprintln!("{count} wallets, 20 copies: plain portfolios {deep:?}, shared complete engine {:?}", start.elapsed());
        }
    }

    #[test]
    fn missing_composition_revision_is_rejected_after_snapshot_restore() {
        let mut engine = TrstEngine::with_expiry(10_000);
        let mut token = engine
            .mint(
                test_hash(1),
                test_address(5),
                40,
                test_address(10),
                ts(1000),
            )
            .unwrap();
        token.basis = TrstBasis::Remainder(test_hash(99));
        engine.track_token(token.clone());
        let restored = restart(&engine);
        assert!(restored.validate_bases().is_err());
        let mut candidate = TrstEngine::with_expiry(10_000);
        assert!(candidate
            .receive_settled_token(token, &Default::default(), ts(1100))
            .is_err());
        assert!(candidate.wallets.is_empty());
    }
}
