//! Core TRST lifecycle engine.

use std::collections::{HashMap, HashSet};

use crate::error::TrstError;
use crate::merger_graph::MergerGraph;
use crate::token::{OriginProportion, TrstToken};
use burst_types::{Timestamp, TrstState, TxHash, WalletAddress};

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
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ConsumedProvenance {
    pub amount: u128,
    pub origin: TxHash,
    pub origin_wallet: WalletAddress,
    pub origin_timestamp: Timestamp,
    pub effective_origin_timestamp: Timestamp,
    pub origin_proportions: Vec<OriginProportion>,
}

/// Information about a pending token needed for expiry-based return.
#[derive(Clone, Debug)]
pub struct PendingTokenInfo {
    /// The ID of the pending token.
    pub token_id: TxHash,
    /// When the pending send was created.
    pub creation_timestamp: Timestamp,
    /// The original sender who should get the token back if it expires.
    pub sender: WalletAddress,
}

/// Result of returning a single expired pending token.
#[derive(Clone, Debug)]
pub struct PendingReturnResult {
    /// The token that was returned.
    pub token_id: TxHash,
    /// The sender who received the token back.
    pub sender: WalletAddress,
    /// The amount of TRST returned.
    pub amount: u128,
}

/// Per-wallet portfolio with O(1) balance lookups.
///
/// Tokens are kept sorted by `origin_timestamp` (sorted invariant maintained
/// on insert). The `cached_transferable` balance is updated incrementally on
/// every mutation so `transferable_balance()` is O(1).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
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

impl Default for WalletPortfolio {
    fn default() -> Self {
        Self {
            tokens: Vec::new(),
            cached_transferable: 0,
            earliest_expiry: None,
        }
    }
}

impl WalletPortfolio {
    /// Insert a token maintaining the sorted-by-origin_timestamp invariant.
    ///
    /// Fast path O(1): new tokens almost always have the latest timestamp,
    /// so they append at the end without shifting. Slow path O(n): binary
    /// search + insert with shift for out-of-order timestamps.
    fn insert_sorted(&mut self, token: TrstToken) {
        let ts = token.origin_timestamp;
        if self.tokens.last().map_or(true, |last| last.origin_timestamp <= ts) {
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
}

/// The TRST engine — manages the full token lifecycle.
pub struct TrstEngine {
    /// The merger graph for proactive revocation.
    pub merger_graph: MergerGraph,
    /// Per-wallet portfolios with O(1) balance lookups and sorted tokens.
    pub wallets: HashMap<WalletAddress, WalletPortfolio>,
    /// Maps each origin wallet to all burn tx hashes (origins) it produced.
    /// Populated incrementally on mint/track. Used by revocation to find
    /// all real origin hashes belonging to a sybil wallet.
    pub wallet_origins: HashMap<WalletAddress, HashSet<TxHash>>,
    /// Maps each origin wallet to the set of holder wallets that contain
    /// simple (non-merged) tokens originating from it. Enables O(k)
    /// simple-token revocation instead of O(wallets * tokens).
    origin_wallet_holders: HashMap<WalletAddress, HashSet<WalletAddress>>,
    /// Global TRST expiry period in seconds (needed for earliest_expiry recomputation).
    pub expiry_secs: u64,
}

impl TrstEngine {
    pub fn new() -> Self {
        Self {
            merger_graph: MergerGraph::new(),
            wallets: HashMap::new(),
            wallet_origins: HashMap::new(),
            origin_wallet_holders: HashMap::new(),
            expiry_secs: u64::MAX,
        }
    }

    pub fn with_expiry(expiry_secs: u64) -> Self {
        Self {
            merger_graph: MergerGraph::new(),
            wallets: HashMap::new(),
            wallet_origins: HashMap::new(),
            origin_wallet_holders: HashMap::new(),
            expiry_secs,
        }
    }

    /// Track a token in the per-wallet portfolio.
    /// Maintains sorted order and updates cached transferable balance — O(log n) insert.
    /// Also updates the `wallet_origins` index for simple tokens.
    pub fn track_token(&mut self, token: TrstToken) {
        self.index_origin(&token);
        let portfolio = self
            .wallets
            .entry(token.holder.clone())
            .or_default();
        if token.state == TrstState::Active {
            portfolio.cached_transferable += token.amount;
            let tok_expiry = token.earliest_expiry(u64::MAX);
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

    /// Track a token with a known expiry period (updates earliest_expiry correctly).
    /// Also updates the `wallet_origins` index for simple tokens.
    pub fn track_token_with_expiry(&mut self, token: TrstToken, expiry_secs: u64) {
        self.index_origin(&token);
        let portfolio = self
            .wallets
            .entry(token.holder.clone())
            .or_default();
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

    /// Record a token's origin(s) in the `wallet_origins` and
    /// `origin_wallet_holders` indexes.
    fn index_origin(&mut self, token: &TrstToken) {
        if token.origin_proportions.is_empty() {
            if let Some(set) = self.wallet_origins.get_mut(&token.origin_wallet) {
                set.insert(token.origin);
            } else {
                let mut set = HashSet::new();
                set.insert(token.origin);
                self.wallet_origins.insert(token.origin_wallet.clone(), set);
            }
            if let Some(holders) = self.origin_wallet_holders.get_mut(&token.origin_wallet) {
                holders.insert(token.holder.clone());
            } else {
                let mut holders = HashSet::new();
                holders.insert(token.holder.clone());
                self.origin_wallet_holders
                    .insert(token.origin_wallet.clone(), holders);
            }
        } else {
            for p in &token.origin_proportions {
                if let Some(set) = self.wallet_origins.get_mut(&p.origin_wallet) {
                    set.insert(p.origin);
                } else {
                    let mut set = HashSet::new();
                    set.insert(p.origin);
                    self.wallet_origins.insert(p.origin_wallet.clone(), set);
                }
            }
        }
    }

    /// Remove a specific token from a wallet's tracked portfolio.
    pub fn untrack_token(&mut self, wallet: &WalletAddress, token_id: &TxHash) {
        let expiry = self.expiry_secs;
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
            }
        }
    }

    /// Remove multiple tokens from a wallet in a single pass — O(n).
    ///
    /// Much more efficient than calling `untrack_token` in a loop, which
    /// would be O(n*k) due to linear scans + repeated expiry recomputation.
    pub fn bulk_untrack(&mut self, wallet: &WalletAddress, token_ids: &HashSet<TxHash>) {
        let expiry = self.expiry_secs;
        if let Some(portfolio) = self.wallets.get_mut(wallet) {
            let mut removed_amount = 0u128;
            portfolio.tokens.retain(|t| {
                if token_ids.contains(&t.id) {
                    if t.state == TrstState::Active {
                        removed_amount += t.amount;
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
    }

    /// Compute the transferable (non-expired, non-revoked) balance for a wallet — O(1).
    ///
    /// Flushes any newly expired tokens first (amortized, only when `earliest_expiry` passes).
    /// Returns `None` if the wallet has no tracked tokens in memory.
    pub fn transferable_balance(
        &mut self,
        wallet: &WalletAddress,
        now: Timestamp,
        expiry_secs: u64,
    ) -> Option<u128> {
        if let Some(portfolio) = self.wallets.get_mut(wallet) {
            portfolio.flush_expired(now, expiry_secs);
            Some(portfolio.cached_transferable)
        } else {
            None
        }
    }

    /// Read-only transferable balance (does not flush expiry).
    /// Use when you only need a snapshot and can't take `&mut self`.
    pub fn transferable_balance_snapshot(
        &self,
        wallet: &WalletAddress,
    ) -> Option<u128> {
        self.wallets.get(wallet).map(|p| p.cached_transferable)
    }

    /// Returns true if the wallet has tracked tokens in the engine.
    pub fn is_wallet_tracked(&self, wallet: &WalletAddress) -> bool {
        self.wallets.contains_key(wallet)
    }

    /// Debit tokens from a wallet's tracked portfolio after a send.
    ///
    /// Tokens are sorted by `origin_timestamp` (oldest first), so we consume
    /// from the front. Truly O(k) where k = tokens fully consumed — only
    /// drains the consumed prefix instead of rebuilding the entire vec.
    pub fn debit_wallet(&mut self, wallet: &WalletAddress, mut amount: u128) {
        if amount == 0 {
            return;
        }
        let expiry_secs = self.expiry_secs;
        if let Some(portfolio) = self.wallets.get_mut(wallet) {
            portfolio.cached_transferable =
                portfolio.cached_transferable.saturating_sub(amount);

            let mut fully_consumed = 0;
            let mut consumed_earliest = false;
            for t in portfolio.tokens.iter() {
                if amount == 0 {
                    break;
                }
                if t.amount <= amount {
                    if t.state == TrstState::Active
                        && portfolio.earliest_expiry == Some(t.earliest_expiry(expiry_secs))
                    {
                        consumed_earliest = true;
                    }
                    amount -= t.amount;
                    fully_consumed += 1;
                } else {
                    break;
                }
            }
            portfolio.tokens.drain(0..fully_consumed);
            if amount > 0 {
                if let Some(first) = portfolio.tokens.first_mut() {
                    first.amount = first.amount.saturating_sub(amount);
                }
                if portfolio.tokens.first().map_or(false, |t| t.amount == 0) {
                    portfolio.tokens.remove(0);
                }
            }
            if consumed_earliest {
                portfolio.recompute_earliest_expiry(expiry_secs);
            }
        }
    }

    /// Debit tokens from a wallet and return provenance of consumed tokens.
    ///
    /// Same FIFO logic as `debit_wallet`, but returns a list of
    /// `ConsumedProvenance` entries describing what was consumed. Used to
    /// populate pending entries with origin info so receivers get properly
    /// provenanced tokens.
    pub fn debit_wallet_with_provenance(
        &mut self,
        wallet: &WalletAddress,
        mut amount: u128,
    ) -> Vec<ConsumedProvenance> {
        let mut consumed = Vec::new();
        if amount == 0 {
            return consumed;
        }
        let expiry_secs = self.expiry_secs;
        if let Some(portfolio) = self.wallets.get_mut(wallet) {
            portfolio.cached_transferable =
                portfolio.cached_transferable.saturating_sub(amount);

            let mut fully_consumed = 0;
            let mut consumed_earliest = false;
            for t in portfolio.tokens.iter() {
                if amount == 0 {
                    break;
                }
                let take = t.amount.min(amount);
                consumed.push(ConsumedProvenance {
                    amount: take,
                    origin: t.origin,
                    origin_wallet: t.origin_wallet.clone(),
                    origin_timestamp: t.origin_timestamp,
                    effective_origin_timestamp: t.effective_origin_timestamp,
                    origin_proportions: t.origin_proportions.clone(),
                });
                if t.amount <= amount {
                    if t.state == TrstState::Active
                        && portfolio.earliest_expiry == Some(t.earliest_expiry(expiry_secs))
                    {
                        consumed_earliest = true;
                    }
                    amount -= t.amount;
                    fully_consumed += 1;
                } else {
                    break;
                }
            }
            portfolio.tokens.drain(0..fully_consumed);
            if amount > 0 {
                if let Some(first) = portfolio.tokens.first_mut() {
                    first.amount = first.amount.saturating_sub(amount);
                }
                if portfolio.tokens.first().map_or(false, |t| t.amount == 0) {
                    portfolio.tokens.remove(0);
                }
            }
            if consumed_earliest {
                portfolio.recompute_earliest_expiry(expiry_secs);
            }
        }
        consumed
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
            origin_proportions: Vec::new(),
        })
    }

    /// Transfer TRST from one wallet to another.
    ///
    /// Creates a new token for the receiver (with updated link) and
    /// returns the change back to the sender as a new token.
    /// Returns `(receiver_token, change_token_if_any)`.
    ///
    /// In Nano's account-balance model, the sender's balance is simply
    /// decremented. Here we model TRST tokens explicitly for provenance
    /// tracking, so a partial send produces a change token.
    pub fn transfer(
        &self,
        token: &TrstToken,
        sender: &WalletAddress,
        receiver: WalletAddress,
        amount: u128,
        send_tx_hash: TxHash,
        change_tx_hash: TxHash,
        now: Timestamp,
        expiry_secs: u64,
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
        if !token.is_transferable(now, expiry_secs) {
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
            origin_proportions: token.origin_proportions.clone(),
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
                origin_proportions: token.origin_proportions.clone(),
            })
        } else {
            None
        };

        Ok((receiver_token, change))
    }

    /// Split a token into multiple smaller tokens.
    ///
    /// All splits share the same origin and link back to the parent token.
    /// Invariants enforced by this method:
    /// - `child.origin == parent.origin` (provenance preserved, not a new origin)
    /// - `child.link == parent.id` (link to the split source)
    /// - `child.origin_timestamp == parent.origin_timestamp` (expiry base preserved)
    /// Amounts must sum to the parent.
    pub fn split(
        &self,
        token: &TrstToken,
        amounts: &[(WalletAddress, u128)],
        tx_hashes: &[TxHash],
        now: Timestamp,
        expiry_secs: u64,
    ) -> Result<Vec<TrstToken>, TrstError> {
        if amounts.len() != tx_hashes.len() {
            return Err(TrstError::SplitMismatch {
                total: amounts.len() as u128,
                parent: tx_hashes.len() as u128,
            });
        }
        if amounts.len() < 2 {
            return Err(TrstError::Other("split requires at least 2 outputs".into()));
        }
        if !token.is_transferable(now, expiry_secs) {
            return Err(TrstError::NotTransferable(format!("{:?}", token.state)));
        }

        let total: u128 = amounts.iter().map(|(_, a)| a).sum();
        if total != token.amount {
            return Err(TrstError::SplitMismatch {
                total,
                parent: token.amount,
            });
        }

        // Reject zero-amount splits
        for (_, amt) in amounts {
            if *amt == 0 {
                return Err(TrstError::Other("split output amount must be non-zero".into()));
            }
        }

        let splits = amounts
            .iter()
            .zip(tx_hashes.iter())
            .map(|((receiver, amount), &hash)| {
                // Scale origin_proportions proportionally to the split fraction.
                let scaled_proportions = if !token.origin_proportions.is_empty() && token.amount > 0 {
                    token.origin_proportions.iter().map(|p| {
                        let scaled = (p.amount as u128)
                            .saturating_mul(*amount as u128)
                            / (token.amount as u128);
                        OriginProportion {
                            origin: p.origin,
                            origin_wallet: p.origin_wallet.clone(),
                            amount: scaled,
                        }
                    }).collect()
                } else {
                    token.origin_proportions.clone()
                };

                TrstToken {
                    id: hash,
                    amount: *amount,
                    origin: token.origin,
                    link: token.id,
                    holder: receiver.clone(),
                    origin_timestamp: token.origin_timestamp,
                    effective_origin_timestamp: token.effective_origin_timestamp,
                    state: TrstState::Active,
                    origin_wallet: token.origin_wallet.clone(),
                    origin_proportions: scaled_proportions,
                }
            })
            .collect();

        Ok(splits)
    }

    /// Merge multiple tokens into a single token.
    ///
    /// The merged token's expiry is the **earliest** expiry among inputs (conservative).
    /// Updates the merger graph for future revocation propagation.
    pub fn merge(
        &mut self,
        tokens: &[TrstToken],
        holder: WalletAddress,
        merge_tx_hash: TxHash,
        now: Timestamp,
        expiry_secs: u64,
    ) -> Result<TrstToken, TrstError> {
        if tokens.len() < 2 {
            return Err(TrstError::EmptyMerge);
        }

        // Verify all tokens are transferable and held by the same wallet.
        for t in tokens {
            if t.holder != holder {
                return Err(TrstError::NotOwner {
                    expected: holder.clone(),
                    actual: t.holder.clone(),
                });
            }
            if !t.is_transferable(now, expiry_secs) {
                return Err(TrstError::NotTransferable(format!("{:?} ({})", t.state, t.id)));
            }
        }

        let total_amount: u128 = tokens.iter().map(|t| t.amount).sum();

        // The effective origin timestamp for expiry is the earliest among all
        // constituents, ensuring "the merged token's expiry date is the earliest
        // expiry among all merged tokens" (whitepaper).
        let effective_ts = tokens
            .iter()
            .map(|t| t.effective_origin_timestamp)
            .min()
            .unwrap();

        // Build origin proportions for the merged token.
        let mut proportions: Vec<OriginProportion> = Vec::new();
        for t in tokens {
            if t.origin_proportions.is_empty() {
                proportions.push(OriginProportion {
                    origin: t.origin,
                    origin_wallet: t.origin_wallet.clone(),
                    amount: t.amount,
                });
            } else {
                proportions.extend(t.origin_proportions.clone());
            }
        }

        // Validate proportions sum BEFORE mutating the merger graph.
        let proportions_sum: u128 = proportions.iter().map(|p| p.amount).sum();
        if proportions_sum != total_amount {
            return Err(TrstError::SplitMismatch {
                total: proportions_sum,
                parent: total_amount,
            });
        }

        // Record in merger graph (only after all validation passes).
        let merge_sources = proportions
            .iter()
            .map(|p| crate::merger_graph::MergeSource {
                origin: p.origin,
                amount: p.amount,
            })
            .collect();

        self.merger_graph.record_merge(crate::merger_graph::MergeNode {
            merge_tx: merge_tx_hash,
            source_origins: merge_sources,
            total_amount,
            holder: holder.clone(),
        });

        let earliest_origin_wallet = tokens
            .iter()
            .min_by_key(|t| t.origin_timestamp)
            .unwrap()
            .origin_wallet
            .clone();

        Ok(TrstToken {
            id: merge_tx_hash,
            amount: total_amount,
            origin: merge_tx_hash,
            link: tokens[0].id,
            holder,
            origin_timestamp: now,
            effective_origin_timestamp: effective_ts,
            state: TrstState::Active,
            origin_wallet: earliest_origin_wallet,
            origin_proportions: proportions,
        })
    }

    /// Check and update expiry state for a token.
    pub fn check_expiry(&self, token: &mut TrstToken, now: Timestamp, expiry_secs: u64) {
        if token.state == TrstState::Active && token.is_expired(now, expiry_secs) {
            token.state = TrstState::Expired;
        }
    }

    /// Revoke all TRST originating from a fraudulent wallet.
    ///
    /// Looks up all real burn tx hashes (origins) belonging to `origin_wallet`
    /// via the `wallet_origins` index, then:
    /// - Marks each origin as revoked in the merger graph
    /// - Propagates through merged tokens via the merger graph — O(k)
    /// - Revokes simple tokens in tracked wallets matching `origin_wallet` — O(k)
    ///
    /// Returns all revocation events (for merged tokens with proportional amounts).
    pub fn revoke_by_origin(
        &mut self,
        origin_wallet: &WalletAddress,
    ) -> Vec<crate::merger_graph::RevocationEvent> {
        let origin_txs: Vec<TxHash> = self
            .wallet_origins
            .get(origin_wallet)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();

        let mut all_events = Vec::new();

        for origin_tx in &origin_txs {
            self.merger_graph.mark_origin_revoked(*origin_tx);
            let events = self.merger_graph.propagate_revocation(origin_tx);
            for event in &events {
                if let Some(portfolio) = self.wallets.get_mut(&event.holder) {
                    if let Some(t) = portfolio.tokens.iter_mut().find(|t| t.id == event.merge_tx && t.state == TrstState::Active) {
                        t.state = TrstState::Revoked;
                        portfolio.cached_transferable =
                            portfolio.cached_transferable.saturating_sub(t.amount);
                    }
                }
            }
            all_events.extend(events);
        }

        // Revoke simple (non-merged) tokens — only check holder wallets
        // that we know contain tokens from this origin_wallet (via index).
        let holder_addrs: Vec<WalletAddress> = self
            .origin_wallet_holders
            .get(origin_wallet)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        for addr in holder_addrs {
            if let Some(portfolio) = self.wallets.get_mut(&addr) {
                for t in &mut portfolio.tokens {
                    if t.state == TrstState::Active
                        && t.origin_proportions.is_empty()
                        && &t.origin_wallet == origin_wallet
                    {
                        portfolio.cached_transferable =
                            portfolio.cached_transferable.saturating_sub(t.amount);
                        t.state = TrstState::Revoked;
                    }
                }
            }
        }

        all_events
    }

    /// Un-revoke TRST when a previously fraudulent wallet is re-verified.
    ///
    /// Symmetric with `revoke_by_origin`: uses the `wallet_origins` index to
    /// find all real origins, then un-marks them in the merger graph and
    /// restores both merged and simple tokens.
    ///
    /// Returns a list of (token_id, holder, amount) for each restored token.
    pub fn un_revoke_by_origin(
        &mut self,
        origin_wallet: &WalletAddress,
    ) -> Vec<UnRevocationResult> {
        let mut results = Vec::new();

        let origin_txs: Vec<TxHash> = self
            .wallet_origins
            .get(origin_wallet)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();

        // Remove revocation marks for all origins belonging to this wallet.
        for origin_tx in &origin_txs {
            self.merger_graph.mark_origin_unrevoked(origin_tx);
        }

        // Restore merged tokens via the merger graph — O(k).
        for origin_tx in &origin_txs {
            let events = self.merger_graph.propagate_unrevocation(origin_tx);
            for event in events {
                if let Some(portfolio) = self.wallets.get_mut(&event.holder) {
                    if let Some(t) = portfolio.tokens.iter_mut().find(|t| t.id == event.merge_tx && t.state == TrstState::Revoked) {
                        t.state = TrstState::Active;
                        portfolio.cached_transferable += t.amount;
                        results.push(UnRevocationResult {
                            token_id: t.id,
                            holder: event.holder.clone(),
                            amount: t.amount,
                        });
                    }
                }
            }
        }

        // Restore simple (non-merged) tokens — only check holder wallets
        // that we know contain tokens from this origin_wallet (via index).
        let holder_addrs: Vec<WalletAddress> = self
            .origin_wallet_holders
            .get(origin_wallet)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        for addr in holder_addrs {
            if let Some(portfolio) = self.wallets.get_mut(&addr) {
                for t in &mut portfolio.tokens {
                    if t.state == TrstState::Revoked
                        && t.origin_proportions.is_empty()
                        && &t.origin_wallet == origin_wallet
                    {
                        t.state = TrstState::Active;
                        portfolio.cached_transferable += t.amount;
                        results.push(UnRevocationResult {
                            token_id: t.id,
                            holder: addr.clone(),
                            amount: t.amount,
                        });
                    }
                }
            }
        }

        results
    }

    /// Return pending TRST tokens that have expired back to their senders.
    ///
    /// In the block-lattice model, a sent token is Pending until the receiver
    /// publishes a receive block. If the token's expiry elapses while still
    /// pending, it should be returned to the sender rather than lost.
    ///
    /// Takes a list of pending token descriptors (token ID, creation timestamp,
    /// sender address) plus the expiry period. Checks which pending tokens have
    /// expired and returns them to their senders by updating the token state and holder.
    ///
    /// Returns a list of (token_id, sender, amount) for each returned token.
    pub fn return_expired_pending(
        &self,
        pending_info: &[PendingTokenInfo],
        tokens: &mut Vec<TrstToken>,
        expiry_period: u64,
        now: Timestamp,
    ) -> Vec<PendingReturnResult> {
        let mut returns = Vec::new();

        // Build a lookup from token ID to pending info.
        let pending_map: std::collections::HashMap<TxHash, &PendingTokenInfo> = pending_info
            .iter()
            .map(|info| (info.token_id, info))
            .collect();

        for token in tokens.iter_mut() {
            if token.state != TrstState::Pending {
                continue;
            }

            if let Some(info) = pending_map.get(&token.id) {
                // Check if the pending period has expired.
                if info.creation_timestamp.has_expired(expiry_period, now) {
                    // Return token to sender: restore holder and set to Active.
                    let amount = token.amount;
                    token.holder = info.sender.clone();
                    token.state = TrstState::Active;

                    returns.push(PendingReturnResult {
                        token_id: token.id,
                        sender: info.sender.clone(),
                        amount,
                    });
                }
            }
        }

        returns
    }

    /// Compute total effective (demurrage-adjusted) TRST balance across tokens.
    /// Active tokens are valued based on time remaining; expired/revoked = 0.
    pub fn effective_balance(
        &self,
        tokens: &[TrstToken],
        now: Timestamp,
        expiry_secs: u64,
    ) -> u128 {
        tokens
            .iter()
            .filter(|t| t.state == TrstState::Active)
            .map(|t| t.effective_value(now, expiry_secs))
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
    /// Returns a TrstEngine with the restored portfolios and a fresh MergerGraph
    /// (the merger graph is persisted separately).
    pub fn load_wallets(data: &[u8], expiry_secs: u64) -> Self {
        let wallets: HashMap<WalletAddress, WalletPortfolio> =
            bincode::deserialize(data).unwrap_or_default();
        let mut wallet_origins: HashMap<WalletAddress, HashSet<TxHash>> = HashMap::new();
        let mut origin_wallet_holders: HashMap<WalletAddress, HashSet<WalletAddress>> =
            HashMap::new();
        for portfolio in wallets.values() {
            for t in &portfolio.tokens {
                if t.origin_proportions.is_empty() {
                    wallet_origins
                        .entry(t.origin_wallet.clone())
                        .or_default()
                        .insert(t.origin);
                    origin_wallet_holders
                        .entry(t.origin_wallet.clone())
                        .or_default()
                        .insert(t.holder.clone());
                } else {
                    for p in &t.origin_proportions {
                        wallet_origins
                            .entry(p.origin_wallet.clone())
                            .or_default()
                            .insert(p.origin);
                    }
                }
            }
        }
        Self {
            merger_graph: MergerGraph::new(),
            wallets,
            wallet_origins,
            origin_wallet_holders,
            expiry_secs,
        }
    }

    /// The meta-store key used for wallet portfolio persistence.
    pub fn meta_key() -> &'static str {
        TRST_ENGINE_META_KEY
    }

    /// Flush expired tokens across all wallets. Call periodically (e.g. every 30s).
    pub fn flush_all_expired(&mut self, now: Timestamp, expiry_secs: u64) {
        for portfolio in self.wallets.values_mut() {
            portfolio.flush_expired(now, expiry_secs);
        }
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

    fn test_timestamp(secs: u64) -> Timestamp {
        Timestamp::new(secs)
    }

    #[test]
    fn test_minting_creates_token_with_correct_fields() {
        let mut engine = TrstEngine::new();
        let burn_tx_hash = test_hash(1);
        let receiver = test_address(1);
        let origin_wallet = test_address(2);
        let amount = 1000;
        let timestamp = test_timestamp(1000);

        let token = engine.mint(burn_tx_hash, receiver.clone(), amount, origin_wallet.clone(), timestamp).unwrap();

        assert_eq!(token.id, burn_tx_hash);
        assert_eq!(token.amount, amount);
        assert_eq!(token.origin, burn_tx_hash);
        assert_eq!(token.link, burn_tx_hash);
        assert_eq!(token.holder, receiver);
        assert_eq!(token.origin_timestamp, timestamp);
        assert_eq!(token.state, TrstState::Active);
        assert_eq!(token.origin_wallet, origin_wallet);
        assert!(token.origin_proportions.is_empty());

        assert!(engine.wallet_origins.get(&origin_wallet).unwrap().contains(&burn_tx_hash));
    }

    #[test]
    fn test_transfer_creates_receiver_token_and_optional_change_token() {
        let mut engine = TrstEngine::new();
        let origin_tx = test_hash(1);
        let sender = test_address(1);
        let receiver = test_address(2);
        let send_tx = test_hash(2);
        let change_tx = test_hash(3);
        let expiry_secs = 3600;

        let token = engine.mint(origin_tx, sender.clone(), 1000, sender.clone(), test_timestamp(1000)).unwrap();

        let now = test_timestamp(1500);
        let (receiver_token, change_token) = engine
            .transfer(&token, &sender, receiver.clone(), 600, send_tx, change_tx, now, expiry_secs)
            .unwrap();

        assert_eq!(receiver_token.id, send_tx);
        assert_eq!(receiver_token.amount, 600);
        assert_eq!(receiver_token.origin, origin_tx);
        assert_eq!(receiver_token.link, token.id);
        assert_eq!(receiver_token.holder, receiver);
        assert_eq!(receiver_token.state, TrstState::Active);

        assert!(change_token.is_some());
        let change = change_token.unwrap();
        assert_eq!(change.id, change_tx);
        assert_eq!(change.amount, 400);
        assert_eq!(change.origin, origin_tx);
        assert_eq!(change.link, token.id);
        assert_eq!(change.holder, sender);
        assert_eq!(change.state, TrstState::Active);
    }

    #[test]
    fn test_transfer_full_amount_produces_no_change() {
        let mut engine = TrstEngine::new();
        let origin_tx = test_hash(1);
        let sender = test_address(1);
        let receiver = test_address(2);
        let send_tx = test_hash(2);
        let change_tx = test_hash(3);
        let expiry_secs = 3600;

        let token = engine.mint(origin_tx, sender.clone(), 1000, sender.clone(), test_timestamp(1000)).unwrap();

        let now = test_timestamp(1500);
        let (receiver_token, change_token) = engine
            .transfer(&token, &sender, receiver.clone(), 1000, send_tx, change_tx, now, expiry_secs)
            .unwrap();

        assert_eq!(receiver_token.amount, 1000);
        assert_eq!(receiver_token.holder, receiver);

        assert!(change_token.is_none());
    }

    #[test]
    fn test_transfer_more_than_available_returns_error() {
        let mut engine = TrstEngine::new();
        let origin_tx = test_hash(1);
        let sender = test_address(1);
        let receiver = test_address(2);
        let send_tx = test_hash(2);
        let change_tx = test_hash(3);
        let expiry_secs = 3600;

        let token = engine.mint(origin_tx, sender.clone(), 1000, sender.clone(), test_timestamp(1000)).unwrap();

        let now = test_timestamp(1500);
        let result = engine.transfer(&token, &sender, receiver, 1500, send_tx, change_tx, now, expiry_secs);

        assert!(result.is_err());
        match result.unwrap_err() {
            TrstError::InsufficientBalance { needed, available } => {
                assert_eq!(needed, 1500);
                assert_eq!(available, 1000);
            }
            _ => panic!("Expected InsufficientBalance error"),
        }
    }

    #[test]
    fn test_transfer_of_expired_token_returns_error() {
        let mut engine = TrstEngine::new();
        let origin_tx = test_hash(1);
        let sender = test_address(1);
        let receiver = test_address(2);
        let send_tx = test_hash(2);
        let change_tx = test_hash(3);
        let expiry_secs = 3600; // 1 hour

        let origin_time = test_timestamp(1000);
        let token = engine.mint(origin_tx, sender.clone(), 1000, sender.clone(), origin_time).unwrap();

        let now = test_timestamp(5000);
        let result = engine.transfer(&token, &sender, receiver, 500, send_tx, change_tx, now, expiry_secs);

        assert!(result.is_err());
        match result.unwrap_err() {
            TrstError::NotTransferable(_) => {}
            _ => panic!("Expected NotTransferable error"),
        }
    }

    #[test]
    fn test_split_with_correct_amounts_succeeds() {
        let mut engine = TrstEngine::new();
        let origin_tx = test_hash(1);
        let holder = test_address(1);
        let expiry_secs = 3600;

        let token = engine.mint(origin_tx, holder.clone(), 1000, holder.clone(), test_timestamp(1000)).unwrap();

        let amounts = vec![
            (test_address(2), 300),
            (test_address(3), 400),
            (test_address(4), 300),
        ];
        let tx_hashes = vec![test_hash(2), test_hash(3), test_hash(4)];

        let now = test_timestamp(1500);
        let splits = engine.split(&token, &amounts, &tx_hashes, now, expiry_secs).unwrap();

        assert_eq!(splits.len(), 3);
        assert_eq!(splits[0].amount, 300);
        assert_eq!(splits[0].holder, test_address(2));
        assert_eq!(splits[0].id, test_hash(2));
        assert_eq!(splits[1].amount, 400);
        assert_eq!(splits[1].holder, test_address(3));
        assert_eq!(splits[2].amount, 300);
        assert_eq!(splits[2].holder, test_address(4));

        for split in &splits {
            assert_eq!(split.origin, origin_tx);
            assert_eq!(split.link, token.id);
            assert_eq!(split.state, TrstState::Active);
        }
    }

    #[test]
    fn test_split_with_mismatched_amounts_fails() {
        let mut engine = TrstEngine::new();
        let origin_tx = test_hash(1);
        let holder = test_address(1);
        let expiry_secs = 3600;

        let token = engine.mint(origin_tx, holder.clone(), 1000, holder.clone(), test_timestamp(1000)).unwrap();

        let amounts = vec![
            (test_address(2), 300),
            (test_address(3), 400),
            (test_address(4), 200),
        ];
        let tx_hashes = vec![test_hash(2), test_hash(3), test_hash(4)];

        let now = test_timestamp(1500);
        let result = engine.split(&token, &amounts, &tx_hashes, now, expiry_secs);

        assert!(result.is_err());
        match result.unwrap_err() {
            TrstError::SplitMismatch { total, parent } => {
                assert_eq!(total, 900);
                assert_eq!(parent, 1000);
            }
            _ => panic!("Expected SplitMismatch error"),
        }
    }

    #[test]
    fn test_merge_combines_amounts_and_creates_merger_graph_entry() {
        let mut engine = TrstEngine::new();
        let expiry_secs = 3600;

        let origin1 = test_hash(1);
        let origin2 = test_hash(2);
        let holder = test_address(5);
        let token1 = engine.mint(origin1, holder.clone(), 500, test_address(10), test_timestamp(1000)).unwrap();
        let token2 = engine.mint(origin2, holder.clone(), 300, test_address(11), test_timestamp(1100)).unwrap();

        let tokens = vec![token1, token2];
        let merge_tx = test_hash(10);
        let now = test_timestamp(1500);

        let merged = engine.merge(&tokens, holder.clone(), merge_tx, now, expiry_secs).unwrap();

        assert_eq!(merged.amount, 800);
        assert_eq!(merged.holder, holder);
        assert_eq!(merged.id, merge_tx);
        assert_eq!(merged.origin, merge_tx);
        assert_eq!(merged.state, TrstState::Active);
        assert_eq!(merged.origin_timestamp, test_timestamp(1500)); // merge time
        assert_eq!(merged.effective_origin_timestamp, test_timestamp(1000)); // earliest constituent

        assert_eq!(merged.origin_proportions.len(), 2);
        assert!(merged.origin_proportions.iter().any(|p| p.origin == origin1 && p.amount == 500));
        assert!(merged.origin_proportions.iter().any(|p| p.origin == origin2 && p.amount == 300));

        let revocations = engine.revoke_by_origin(&test_address(10));
        assert!(!revocations.is_empty());
    }

    #[test]
    fn test_check_expiry_transitions_active_to_expired() {
        let mut engine = TrstEngine::new();
        let origin_tx = test_hash(1);
        let holder = test_address(1);
        let expiry_secs = 3600; // 1 hour

        let origin_time = test_timestamp(1000);
        let mut token = engine.mint(origin_tx, holder.clone(), 1000, holder.clone(), origin_time).unwrap();

        let now_before = test_timestamp(2000);
        engine.check_expiry(&mut token, now_before, expiry_secs);
        assert_eq!(token.state, TrstState::Active);

        let now_after = test_timestamp(5000);
        engine.check_expiry(&mut token, now_after, expiry_secs);
        assert_eq!(token.state, TrstState::Expired);
    }

    #[test]
    fn test_revoke_by_origin_propagates_through_merger_graph() {
        let mut engine = TrstEngine::new();
        let expiry_secs = 3600;

        let origin1 = test_hash(1);
        let origin2 = test_hash(2);
        let origin_wallet1 = test_address(10);
        let origin_wallet2 = test_address(11);
        let holder1 = test_address(5);
        let holder2 = test_address(6);

        let token1 = engine.mint(origin1, holder1.clone(), 500, origin_wallet1.clone(), test_timestamp(1000)).unwrap();
        let token2 = engine.mint(origin2, holder1.clone(), 300, origin_wallet2.clone(), test_timestamp(1100)).unwrap();
        let token3 = engine.mint(origin1, holder2.clone(), 200, origin_wallet1.clone(), test_timestamp(1200)).unwrap();

        let merge1_tx = test_hash(10);
        let mut merged1 = engine
            .merge(
                &vec![token1, token2],
                holder1.clone(),
                merge1_tx,
                test_timestamp(1500),
                expiry_secs,
            )
            .unwrap();

        merged1.holder = holder2.clone();

        let merge2_tx = test_hash(11);
        let _merged2 = engine
            .merge(
                &vec![merged1, token3],
                holder2.clone(),
                merge2_tx,
                test_timestamp(1600),
                expiry_secs,
            )
            .unwrap();

        let revocations = engine.revoke_by_origin(&origin_wallet1);

        assert!(!revocations.is_empty());
        let total_revoked: u128 = revocations.iter().map(|r| r.revoked_amount).sum();
        assert!(total_revoked > 0);
    }

    // ── Un-revocation tests ─────────────────────────────────────────────

    #[test]
    fn test_un_revoke_simple_tokens_restores_active_state() {
        let mut engine = TrstEngine::new();
        let origin_wallet = test_address(10);
        let other_wallet = test_address(11);

        let token1 = engine.mint(test_hash(1), test_address(1), 500, origin_wallet.clone(), test_timestamp(1000)).unwrap();
        let token2 = engine.mint(test_hash(2), test_address(2), 300, origin_wallet.clone(), test_timestamp(1100)).unwrap();
        let token3 = engine.mint(test_hash(3), test_address(3), 200, other_wallet.clone(), test_timestamp(1200)).unwrap();

        engine.track_token(token1);
        engine.track_token(token2);
        engine.track_token(token3);

        engine.revoke_by_origin(&origin_wallet);

        let p1 = engine.wallets.get(&test_address(1)).unwrap();
        assert_eq!(p1.tokens[0].state, TrstState::Revoked);
        assert_eq!(p1.cached_transferable, 0);
        let p2 = engine.wallets.get(&test_address(2)).unwrap();
        assert_eq!(p2.tokens[0].state, TrstState::Revoked);
        let p3 = engine.wallets.get(&test_address(3)).unwrap();
        assert_eq!(p3.tokens[0].state, TrstState::Active);

        let results = engine.un_revoke_by_origin(&origin_wallet);
        assert_eq!(results.len(), 2);

        let p1 = engine.wallets.get(&test_address(1)).unwrap();
        assert_eq!(p1.tokens[0].state, TrstState::Active);
        assert_eq!(p1.cached_transferable, 500);
        let p2 = engine.wallets.get(&test_address(2)).unwrap();
        assert_eq!(p2.tokens[0].state, TrstState::Active);
        assert_eq!(p2.cached_transferable, 300);
        let p3 = engine.wallets.get(&test_address(3)).unwrap();
        assert_eq!(p3.tokens[0].state, TrstState::Active);
    }

    #[test]
    fn test_un_revoke_skips_non_revoked_tokens() {
        let mut engine = TrstEngine::new();
        let origin_wallet = test_address(10);

        let token1 = engine.mint(test_hash(1), test_address(1), 500, origin_wallet.clone(), test_timestamp(1000)).unwrap();
        let token2 = engine.mint(test_hash(2), test_address(1), 300, origin_wallet.clone(), test_timestamp(1100)).unwrap();
        engine.track_token(token1);
        engine.track_token(token2);

        engine.revoke_by_origin(&origin_wallet);

        let results = engine.un_revoke_by_origin(&origin_wallet);
        assert_eq!(results.len(), 2);

        let p = engine.wallets.get(&test_address(1)).unwrap();
        assert_eq!(p.cached_transferable, 800);
        for t in &p.tokens {
            assert_eq!(t.state, TrstState::Active);
        }
    }

    #[test]
    fn test_un_revoke_returns_correct_amounts_and_holders() {
        let mut engine = TrstEngine::new();
        let origin_wallet = test_address(10);
        let holder_a = test_address(1);
        let holder_b = test_address(2);

        let token_a = engine.mint(test_hash(1), holder_a.clone(), 750, origin_wallet.clone(), test_timestamp(1000)).unwrap();
        let token_b = engine.mint(test_hash(2), holder_b.clone(), 250, origin_wallet.clone(), test_timestamp(1100)).unwrap();
        engine.track_token(token_a);
        engine.track_token(token_b);

        engine.revoke_by_origin(&origin_wallet);
        let results = engine.un_revoke_by_origin(&origin_wallet);

        assert_eq!(results.len(), 2);
        let total: u128 = results.iter().map(|r| r.amount).sum();
        assert_eq!(total, 1000);
    }

    #[test]
    fn test_un_revoke_merged_token_all_origins_clean() {
        let mut engine = TrstEngine::new();
        let expiry_secs = 3600;
        let origin_wallet1 = test_address(10);
        let origin_wallet2 = test_address(11);
        let holder = test_address(5);

        let token1 = engine.mint(test_hash(1), holder.clone(), 500, origin_wallet1.clone(), test_timestamp(1000)).unwrap();
        let token2 = engine.mint(test_hash(2), holder.clone(), 300, origin_wallet2.clone(), test_timestamp(1100)).unwrap();

        let merge_tx = test_hash(10);
        let merged = engine
            .merge(&[token1, token2], holder.clone(), merge_tx, test_timestamp(1500), expiry_secs)
            .unwrap();
        engine.track_token(merged);

        engine.revoke_by_origin(&origin_wallet1);

        let p = engine.wallets.get(&holder).unwrap();
        assert_eq!(p.tokens[0].state, TrstState::Revoked);

        let results = engine.un_revoke_by_origin(&origin_wallet1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].amount, 800);

        let p = engine.wallets.get(&holder).unwrap();
        assert_eq!(p.tokens[0].state, TrstState::Active);
    }

    #[test]
    fn test_un_revoke_merged_token_other_origin_still_revoked() {
        let mut engine = TrstEngine::new();
        let expiry_secs = 3600;
        let origin_wallet1 = test_address(10);
        let origin_wallet2 = test_address(11);
        let holder = test_address(5);

        let token1 = engine.mint(test_hash(1), holder.clone(), 500, origin_wallet1.clone(), test_timestamp(1000)).unwrap();
        let token2 = engine.mint(test_hash(2), holder.clone(), 300, origin_wallet2.clone(), test_timestamp(1100)).unwrap();

        let merge_tx = test_hash(10);
        let merged = engine
            .merge(&[token1, token2], holder.clone(), merge_tx, test_timestamp(1500), expiry_secs)
            .unwrap();
        engine.track_token(merged);

        engine.revoke_by_origin(&origin_wallet1);
        engine.revoke_by_origin(&origin_wallet2);

        let results = engine.un_revoke_by_origin(&origin_wallet1);
        assert_eq!(results.len(), 0);

        let p = engine.wallets.get(&holder).unwrap();
        assert_eq!(p.tokens[0].state, TrstState::Revoked);
    }

    #[test]
    fn test_un_revoke_with_no_revoked_tokens_returns_empty() {
        let mut engine = TrstEngine::new();
        let origin_wallet = test_address(10);

        let token = engine.mint(test_hash(1), test_address(1), 500, origin_wallet.clone(), test_timestamp(1000)).unwrap();
        engine.track_token(token);

        let results = engine.un_revoke_by_origin(&origin_wallet);
        assert!(results.is_empty());

        let p = engine.wallets.get(&test_address(1)).unwrap();
        assert_eq!(p.tokens[0].state, TrstState::Active);
    }

    #[test]
    fn test_un_revoke_updates_merger_graph_revocation_tracking() {
        let mut engine = TrstEngine::new();
        let origin_wallet = test_address(10);

        let token = engine.mint(test_hash(1), test_address(1), 500, origin_wallet.clone(), test_timestamp(1000)).unwrap();
        engine.track_token(token);

        engine.revoke_by_origin(&origin_wallet);
        assert!(engine.merger_graph.is_origin_revoked(&test_hash(1)));

        engine.un_revoke_by_origin(&origin_wallet);
        assert!(!engine.merger_graph.is_origin_revoked(&test_hash(1)));
    }

    // ── Pending expiry return tests ─────────────────────────────────────

    #[test]
    fn test_return_expired_pending_returns_expired_tokens_to_sender() {
        let mut engine = TrstEngine::new();
        let sender = test_address(1);
        let receiver = test_address(2);
        let expiry_period = 3600; // 1 hour

        let mut token = engine.mint(test_hash(1), receiver.clone(), 500, sender.clone(), test_timestamp(1000)).unwrap();
        token.state = TrstState::Pending;

        let pending_info = vec![PendingTokenInfo {
            token_id: test_hash(1),
            creation_timestamp: test_timestamp(1000),
            sender: sender.clone(),
        }];

        let now = test_timestamp(5000);
        let mut tokens = vec![token];
        let returns = engine.return_expired_pending(&pending_info, &mut tokens, expiry_period, now);

        assert_eq!(returns.len(), 1);
        assert_eq!(returns[0].token_id, test_hash(1));
        assert_eq!(returns[0].sender, sender);
        assert_eq!(returns[0].amount, 500);

        assert_eq!(tokens[0].holder, sender);
        assert_eq!(tokens[0].state, TrstState::Active);
    }

    #[test]
    fn test_return_expired_pending_does_not_return_unexpired_tokens() {
        let mut engine = TrstEngine::new();
        let sender = test_address(1);
        let receiver = test_address(2);
        let expiry_period = 3600;

        let mut token = engine.mint(test_hash(1), receiver.clone(), 500, sender.clone(), test_timestamp(1000)).unwrap();
        token.state = TrstState::Pending;

        let pending_info = vec![PendingTokenInfo {
            token_id: test_hash(1),
            creation_timestamp: test_timestamp(1000),
            sender: sender.clone(),
        }];

        let now = test_timestamp(2000);
        let mut tokens = vec![token];
        let returns = engine.return_expired_pending(&pending_info, &mut tokens, expiry_period, now);

        assert!(returns.is_empty());
        assert_eq!(tokens[0].holder, receiver);
        assert_eq!(tokens[0].state, TrstState::Pending);
    }

    #[test]
    fn test_return_expired_pending_handles_mixed_states() {
        let mut engine = TrstEngine::new();
        let sender = test_address(1);
        let receiver = test_address(2);
        let expiry_period = 3600;

        let mut token1 = engine.mint(test_hash(1), receiver.clone(), 500, sender.clone(), test_timestamp(1000)).unwrap();
        token1.state = TrstState::Pending;

        let mut token2 = engine.mint(test_hash(2), receiver.clone(), 300, sender.clone(), test_timestamp(4000)).unwrap();
        token2.state = TrstState::Pending;

        let token3 = engine.mint(test_hash(3), receiver.clone(), 200, sender.clone(), test_timestamp(1000)).unwrap();

        let pending_info = vec![
            PendingTokenInfo {
                token_id: test_hash(1),
                creation_timestamp: test_timestamp(1000),
                sender: sender.clone(),
            },
            PendingTokenInfo {
                token_id: test_hash(2),
                creation_timestamp: test_timestamp(4000),
                sender: sender.clone(),
            },
        ];

        let now = test_timestamp(5000);
        let mut tokens = vec![token1, token2, token3];
        let returns = engine.return_expired_pending(&pending_info, &mut tokens, expiry_period, now);

        assert_eq!(returns.len(), 1);
        assert_eq!(returns[0].token_id, test_hash(1));
        assert_eq!(returns[0].amount, 500);

        assert_eq!(tokens[0].state, TrstState::Active);
        assert_eq!(tokens[0].holder, sender);
        assert_eq!(tokens[1].state, TrstState::Pending);
        assert_eq!(tokens[1].holder, receiver);
        assert_eq!(tokens[2].state, TrstState::Active);
        assert_eq!(tokens[2].holder, receiver);
    }

    #[test]
    fn test_return_expired_pending_with_no_pending_tokens() {
        let mut engine = TrstEngine::new();
        let sender = test_address(1);

        let token = engine.mint(test_hash(1), test_address(2), 500, sender.clone(), test_timestamp(1000)).unwrap();

        let pending_info = vec![];
        let now = test_timestamp(5000);
        let mut tokens = vec![token];
        let returns = engine.return_expired_pending(&pending_info, &mut tokens, 3600, now);

        assert!(returns.is_empty());
        assert_eq!(tokens[0].state, TrstState::Active);
    }

    #[test]
    fn test_return_expired_pending_boundary_exact_expiry() {
        let mut engine = TrstEngine::new();
        let sender = test_address(1);
        let receiver = test_address(2);
        let expiry_period = 3600;

        let mut token = engine.mint(test_hash(1), receiver.clone(), 500, sender.clone(), test_timestamp(1000)).unwrap();
        token.state = TrstState::Pending;

        let pending_info = vec![PendingTokenInfo {
            token_id: test_hash(1),
            creation_timestamp: test_timestamp(1000),
            sender: sender.clone(),
        }];

        let now = test_timestamp(4600);
        let mut tokens = vec![token];
        let returns = engine.return_expired_pending(&pending_info, &mut tokens, expiry_period, now);

        assert_eq!(returns.len(), 1);
        assert_eq!(tokens[0].state, TrstState::Active);
        assert_eq!(tokens[0].holder, sender);
    }

    #[test]
    fn test_return_expired_pending_multiple_senders() {
        let mut engine = TrstEngine::new();
        let sender_a = test_address(1);
        let sender_b = test_address(2);
        let receiver = test_address(3);
        let expiry_period = 3600;

        let mut token1 = engine.mint(test_hash(1), receiver.clone(), 500, sender_a.clone(), test_timestamp(1000)).unwrap();
        token1.state = TrstState::Pending;

        let mut token2 = engine.mint(test_hash(2), receiver.clone(), 300, sender_b.clone(), test_timestamp(1000)).unwrap();
        token2.state = TrstState::Pending;

        let pending_info = vec![
            PendingTokenInfo {
                token_id: test_hash(1),
                creation_timestamp: test_timestamp(1000),
                sender: sender_a.clone(),
            },
            PendingTokenInfo {
                token_id: test_hash(2),
                creation_timestamp: test_timestamp(1000),
                sender: sender_b.clone(),
            },
        ];

        let now = test_timestamp(5000);
        let mut tokens = vec![token1, token2];
        let returns = engine.return_expired_pending(&pending_info, &mut tokens, expiry_period, now);

        assert_eq!(returns.len(), 2);
        assert_eq!(returns[0].sender, sender_a);
        assert_eq!(returns[0].amount, 500);
        assert_eq!(returns[1].sender, sender_b);
        assert_eq!(returns[1].amount, 300);

        assert_eq!(tokens[0].holder, sender_a);
        assert_eq!(tokens[1].holder, sender_b);
    }

    #[test]
    fn test_split_preserves_origin_and_link_from_parent() {
        let mut engine = TrstEngine::new();
        let original_burn_tx = test_hash(1);
        let holder = test_address(1);
        let expiry_secs = 3600;

        let token = engine
            .mint(original_burn_tx, holder.clone(), 1000, holder.clone(), test_timestamp(500))
            .unwrap();

        let amounts = vec![
            (test_address(2), 600),
            (test_address(3), 400),
        ];
        let tx_hashes = vec![test_hash(10), test_hash(11)];

        let splits = engine
            .split(&token, &amounts, &tx_hashes, test_timestamp(1500), expiry_secs)
            .unwrap();

        for split in &splits {
            assert_eq!(split.origin, original_burn_tx, "split must preserve parent's origin");
            assert_eq!(split.link, token.id, "split must link to parent token");
            assert_eq!(split.origin_timestamp, test_timestamp(500), "split must preserve origin_timestamp");
            assert_eq!(split.origin_wallet, holder, "split must preserve origin_wallet");
        }
    }

    #[test]
    fn test_split_of_transferred_token_preserves_original_origin() {
        let mut engine = TrstEngine::new();
        let original_burn_tx = test_hash(1);
        let minter = test_address(1);
        let recipient = test_address(2);
        let expiry_secs = 7200;

        let token = engine
            .mint(original_burn_tx, minter.clone(), 1000, minter.clone(), test_timestamp(100))
            .unwrap();

        let (transferred, _) = engine
            .transfer(&token, &minter, recipient.clone(), 1000, test_hash(5), test_hash(6), test_timestamp(200), expiry_secs)
            .unwrap();

        assert_eq!(transferred.origin, original_burn_tx);

        let amounts = vec![
            (test_address(3), 700),
            (test_address(4), 300),
        ];
        let tx_hashes = vec![test_hash(20), test_hash(21)];

        let splits = engine
            .split(&transferred, &amounts, &tx_hashes, test_timestamp(300), expiry_secs)
            .unwrap();

        for split in &splits {
            assert_eq!(split.origin, original_burn_tx, "split of transferred token must trace back to original burn");
            assert_eq!(split.link, transferred.id, "split must link to the transferred token");
            assert_eq!(split.origin_timestamp, test_timestamp(100), "origin_timestamp must be from original mint");
        }
    }

    #[test]
    fn test_cached_transferable_stays_consistent_with_full_recompute() {
        let mut engine = TrstEngine::new();
        let expiry_secs = 3600;
        let holder = test_address(1);
        let origin_wallet = test_address(10);

        let t1 = engine.mint(test_hash(1), holder.clone(), 1000, origin_wallet.clone(), test_timestamp(100)).unwrap();
        engine.track_token_with_expiry(t1.clone(), expiry_secs);
        let t2 = engine.mint(test_hash(2), holder.clone(), 500, origin_wallet.clone(), test_timestamp(200)).unwrap();
        engine.track_token_with_expiry(t2.clone(), expiry_secs);
        let t3 = engine.mint(test_hash(3), holder.clone(), 300, origin_wallet.clone(), test_timestamp(300)).unwrap();
        engine.track_token_with_expiry(t3, expiry_secs);

        let now = test_timestamp(400);
        let p = engine.wallets.get_mut(&holder).unwrap();
        let cached = p.cached_transferable;
        p.recompute_transferable(now, expiry_secs);
        assert_eq!(p.cached_transferable, cached, "after track_token, recompute must match incremental");
        assert_eq!(cached, 1800);

        engine.debit_wallet(&holder, 400);
        let p = engine.wallets.get_mut(&holder).unwrap();
        let cached = p.cached_transferable;
        p.recompute_transferable(now, expiry_secs);
        assert_eq!(p.cached_transferable, cached, "after debit, recompute must match incremental");

        let merged = engine.merge(&[t1, t2], holder.clone(), test_hash(20), now, expiry_secs).unwrap();
        engine.track_token_with_expiry(merged.clone(), expiry_secs);
        let p = engine.wallets.get_mut(&holder).unwrap();
        let cached = p.cached_transferable;
        p.recompute_transferable(now, expiry_secs);
        assert_eq!(p.cached_transferable, cached, "after merge+track, recompute must match incremental");

        let far_future = test_timestamp(100 + expiry_secs + 1);
        let p = engine.wallets.get_mut(&holder).unwrap();
        p.flush_expired(far_future, expiry_secs);
        let cached = p.cached_transferable;
        p.recompute_transferable(far_future, expiry_secs);
        assert_eq!(p.cached_transferable, cached, "after expiry flush, recompute must match incremental");

        let _ = merged;
    }
}
