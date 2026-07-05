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

use std::collections::{HashMap, HashSet};

use crate::error::TrstError;
use crate::merger_graph::{ceil_proportion, MergeNode, MergeSource, MergerGraph};
use crate::token::TrstToken;
use burst_types::{Timestamp, TrstState, TxHash, WalletAddress};

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
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
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
        let needs_flush = match self.earliest_expiry {
            Some(exp) => now.as_secs() >= exp.as_secs(),
            None => false,
        };
        if !needs_flush {
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
pub struct TrstEngine {
    /// The merger graph for proactive revocation.
    pub merger_graph: MergerGraph,
    /// Per-wallet portfolios with O(1) balance lookups and sorted tokens.
    pub wallets: HashMap<WalletAddress, WalletPortfolio>,
    /// Maps each origin wallet to all burn tx hashes (origins) it produced.
    /// Used by revocation to find all origins belonging to a sybil wallet (7.1a).
    pub wallet_origins: HashMap<WalletAddress, HashSet<TxHash>>,
    /// Maps an origin tx (burn or merge) to the wallets CURRENTLY holding live
    /// tokens with that origin. Maintained on every track/untrack/debit so
    /// revocation touches only actual holders, never historical ones.
    origin_holders: HashMap<TxHash, HashSet<WalletAddress>>,
    /// Maps a revoked burn origin to the wallets holding tokens tagged with it.
    /// Makes un-revocation (6.15b) O(k) in the number of affected tokens.
    revoked_holders: HashMap<TxHash, HashSet<WalletAddress>>,
    /// Global TRST expiry period in seconds (governance parameter, 6.9).
    pub expiry_secs: u64,
}

impl TrstEngine {
    pub fn new() -> Self {
        Self::with_expiry(u64::MAX)
    }

    pub fn with_expiry(expiry_secs: u64) -> Self {
        Self {
            merger_graph: MergerGraph::new(),
            wallets: HashMap::new(),
            wallet_origins: HashMap::new(),
            origin_holders: HashMap::new(),
            revoked_holders: HashMap::new(),
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

    /// Track an incoming (received or returned) token, first applying any
    /// revocations that happened while it was in flight.
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
                    // Apply outstanding revocations in deterministic (byte) order.
                    let mut contribs: Vec<(TxHash, u128)> = node
                        .revoked_contribs
                        .iter()
                        .map(|(o, a)| (*o, *a))
                        .collect();
                    contribs.sort_by_key(|(o, _)| *o.as_bytes());

                    let mut denom = node.total_amount;
                    let mut remaining = token.amount;
                    let mut chunks: Vec<TrstToken> = Vec::new();
                    for (origin, contrib) in contribs {
                        let cut = ceil_proportion(remaining, contrib, denom);
                        denom = denom.saturating_sub(contrib);
                        if cut == 0 {
                            continue;
                        }
                        events.push(RevocationEvent {
                            holder: token.holder.clone(),
                            token_id: token.id,
                            revoked_origin: origin,
                            revoked_amount: cut,
                            total_amount: token.amount,
                        });
                        remaining -= cut;
                        chunks.push(TrstToken {
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
            }
        }

        self.track_token(token);
        events
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

    /// Rebuild all holder indexes from the portfolios and merger graph.
    /// Call after restoring both `wallets` and `merger_graph` from disk.
    pub fn rebuild_indexes(&mut self) {
        self.origin_holders.clear();
        self.revoked_holders.clear();
        self.wallet_origins.clear();
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
                if !self.merger_graph.contains_merge(&t.origin) {
                    self.wallet_origins
                        .entry(t.origin_wallet.clone())
                        .or_default()
                        .insert(t.origin);
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
        let expiry_secs = self.expiry_secs;
        if let Some(portfolio) = self.wallets.get_mut(wallet) {
            portfolio.flush_expired(now, expiry_secs);
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
    /// Tokens sharing an origin are provenance-identical (same burn or merge,
    /// same expiry), so the debit may span several of them without blending
    /// provenance. It never crosses origin boundaries — if `amount` exceeds
    /// what the origin's tokens hold, only what exists is debited (validation
    /// must reject such sends beforehand via `origin_transferable`).
    pub fn debit_wallet(&mut self, wallet: &WalletAddress, token_origin: &TxHash, amount: u128) {
        let _ = self.debit_wallet_with_provenance(wallet, token_origin, amount);
    }

    /// Debit like [`debit_wallet`] and return the consumed provenance.
    ///
    /// Returns at most one entry — all consumed tokens share the origin, so
    /// the receiver gets a single clean provenance pointer (6.17b).
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
        let mut provenance: Option<ConsumedProvenance> = None;
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
                    match &mut provenance {
                        Some(p) => p.amount = p.amount.saturating_add(take),
                        None => {
                            provenance = Some(ConsumedProvenance {
                                amount: take,
                                origin: t.origin,
                                origin_wallet: t.origin_wallet.clone(),
                                origin_timestamp: t.origin_timestamp,
                                effective_origin_timestamp: t.effective_origin_timestamp,
                            })
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
        provenance.into_iter().collect()
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
        if sender == &receiver {
            return Err(TrstError::Other("cannot transfer to self".into()));
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

        let total_amount: u128 = tokens.iter().map(|t| t.amount).sum();

        // The effective origin timestamp for expiry is the earliest among all
        // constituents: "the merged token's expiry date is the earliest expiry
        // among all merged tokens" (whitepaper).
        let effective_ts = tokens
            .iter()
            .map(|t| t.effective_origin_timestamp)
            .min()
            .unwrap();

        // Record the merge's immediate inputs. Tokens sharing an origin are
        // provenance-identical, so their amounts combine into one source.
        let mut source_amounts: HashMap<TxHash, u128> = HashMap::new();
        let mut source_order: Vec<TxHash> = Vec::new();
        for t in tokens {
            match source_amounts.get_mut(&t.origin) {
                Some(a) => *a = a.saturating_add(t.amount),
                None => {
                    source_amounts.insert(t.origin, t.amount);
                    source_order.push(t.origin);
                }
            }
        }
        let merge_sources: Vec<MergeSource> = source_order
            .into_iter()
            .map(|origin| MergeSource {
                origin,
                amount: source_amounts[&origin],
            })
            .collect();

        self.merger_graph.record_merge(MergeNode {
            merge_tx: merge_tx_hash,
            source_origins: merge_sources,
            total_amount,
            revoked_contribs: HashMap::new(),
        });

        let most_recent_input = tokens.iter().max_by_key(|t| t.origin_timestamp).unwrap().id;
        let state = if Timestamp::new(effective_ts.as_secs()).has_expired(self.expiry_secs, now) {
            TrstState::Expired
        } else {
            TrstState::Active
        };

        Ok(TrstToken {
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
        let mut origin_txs: Vec<TxHash> = self
            .wallet_origins
            .get(origin_wallet)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        // Deterministic application order — every node must compute the
        // same sequential proportional splits.
        origin_txs.sort_by_key(|t| *t.as_bytes());

        let mut events = Vec::new();
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
                let denom = taint.total_amount.saturating_sub(taint.prior_revoked);
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
                        denom,
                        &mut events,
                    );
                }
            }
        }
        events
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
        denom: u128,
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
                let cut = ceil_proportion(t.amount, tainted, denom);
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
                } else {
                    t.amount -= cut;
                    chunks.push(TrstToken {
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
        for portfolio in self.wallets.values_mut() {
            for t in &mut portfolio.tokens {
                match t.state {
                    TrstState::Active if t.is_expired(now, new_expiry_secs) => {
                        t.state = TrstState::Expired;
                    }
                    TrstState::Expired if !t.is_expired(now, new_expiry_secs) => {
                        t.state = TrstState::Active;
                    }
                    _ => {}
                }
            }
            portfolio.recompute_transferable(now, new_expiry_secs);
            portfolio.recompute_earliest_expiry(new_expiry_secs);
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

impl TrstEngine {
    /// Serialize the per-wallet token portfolios to bytes for LMDB persistence.
    pub fn save_wallets(&self) -> Vec<u8> {
        bincode::serialize(&self.wallets).unwrap_or_default()
    }

    /// Restore per-wallet token portfolios from serialized bytes.
    ///
    /// Returns a TrstEngine with the restored portfolios and a fresh
    /// MergerGraph — the merger graph is persisted separately. Call
    /// [`rebuild_indexes`](Self::rebuild_indexes) again after restoring the
    /// merger graph so merged origins are classified correctly.
    pub fn load_wallets(data: &[u8], expiry_secs: u64) -> Self {
        let wallets: HashMap<WalletAddress, WalletPortfolio> =
            bincode::deserialize(data).unwrap_or_default();
        let mut engine = Self {
            merger_graph: MergerGraph::new(),
            wallets,
            wallet_origins: HashMap::new(),
            origin_holders: HashMap::new(),
            revoked_holders: HashMap::new(),
            expiry_secs,
        };
        engine.rebuild_indexes();
        engine
    }

    /// The meta-store key used for wallet portfolio persistence.
    pub fn meta_key() -> &'static str {
        TRST_ENGINE_META_KEY
    }

    /// Flush expired tokens across all wallets. Call periodically (e.g. every 30s).
    pub fn flush_all_expired(&mut self, now: Timestamp) {
        let expiry_secs = self.expiry_secs;
        for portfolio in self.wallets.values_mut() {
            portfolio.flush_expired(now, expiry_secs);
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
        for (addr, portfolio) in self.wallets.iter_mut() {
            let flushed = portfolio.flush_expired(now, expiry_secs);
            if flushed > 0 {
                out.push((addr.clone(), flushed));
            }
        }
        out
    }

    /// Get a portfolio for a wallet (immutable).
    pub fn get_portfolio(&self, wallet: &WalletAddress) -> Option<&WalletPortfolio> {
        self.wallets.get(wallet)
    }

    /// Get a portfolio for a wallet (mutable).
    pub fn get_portfolio_mut(&mut self, wallet: &WalletAddress) -> Option<&mut WalletPortfolio> {
        self.wallets.get_mut(wallet)
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
    fn merge_rejects_too_many_sources() {
        let mut engine = TrstEngine::with_expiry(u64::MAX);
        let holder = test_address(1);
        let mut tokens = Vec::new();
        for i in 0..(MAX_MERGE_SOURCES + 1) {
            let mut h = [0u8; 32];
            h[0] = (i % 256) as u8;
            h[1] = (i / 256) as u8;
            h[2] = 7;
            tokens.push(TrstToken {
                id: TxHash::new(h),
                amount: 1,
                origin: TxHash::new(h),
                link: TxHash::new(h),
                holder: holder.clone(),
                origin_timestamp: ts(1000),
                effective_origin_timestamp: ts(1000),
                state: TrstState::Active,
                origin_wallet: holder.clone(),
                revoked_origin: None,
            });
        }
        let result = engine.merge(&tokens, holder, test_hash(99), ts(1500));
        assert!(result.is_err());
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
    fn save_and_load_wallets_rebuilds_indexes() {
        let mut engine = TrstEngine::with_expiry(u64::MAX);
        let ow = test_address(10);
        let holder = test_address(1);
        let token = engine
            .mint(test_hash(1), holder.clone(), 500, ow.clone(), ts(1000))
            .unwrap();
        engine.track_token(token);

        let bytes = engine.save_wallets();
        let mut restored = TrstEngine::load_wallets(&bytes, u64::MAX);
        assert_eq!(
            restored.get_portfolio(&holder).unwrap().cached_transferable,
            500
        );
        // Revocation works after restore (indexes rebuilt).
        let events = restored.revoke_by_origin(&ow);
        assert_eq!(events.len(), 1);
    }
}
