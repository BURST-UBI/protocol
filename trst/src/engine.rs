//! Core TRST lifecycle engine.

use crate::error::TrstError;
use crate::merger_graph::MergerGraph;
use crate::token::{OriginProportion, TrstToken};
use burst_types::{Timestamp, TrstState, TxHash, WalletAddress};

/// The TRST engine — manages the full token lifecycle.
pub struct TrstEngine {
    /// The merger graph for proactive revocation.
    pub merger_graph: MergerGraph,
}

impl TrstEngine {
    pub fn new() -> Self {
        Self {
            merger_graph: MergerGraph::new(),
        }
    }

    /// Mint fresh TRST from a burn transaction.
    ///
    /// Called when a consumer burns BRN for a provider. The provider receives
    /// newly created TRST with the burn tx as origin.
    pub fn mint(
        &self,
        burn_tx_hash: TxHash,
        receiver: WalletAddress,
        amount: u128,
        origin_wallet: WalletAddress,
        timestamp: Timestamp,
    ) -> TrstToken {
        TrstToken {
            id: burn_tx_hash,
            amount,
            origin: burn_tx_hash,
            link: burn_tx_hash, // For the first token, link == origin
            holder: receiver,
            origin_timestamp: timestamp,
            state: TrstState::Active,
            origin_wallet,
            origin_proportions: Vec::new(), // Non-merged: 100% from this origin
        }
    }

    /// Transfer TRST from one wallet to another.
    ///
    /// Creates a new token for the receiver (with updated link) and
    /// reduces the sender's token. Returns (receiver_token, change_token_if_any).
    pub fn transfer(
        &self,
        token: &TrstToken,
        _sender: &WalletAddress,
        receiver: WalletAddress,
        amount: u128,
        new_tx_hash: TxHash,
        now: Timestamp,
        expiry_secs: u64,
    ) -> Result<(TrstToken, Option<TrstToken>), TrstError> {
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
            id: new_tx_hash,
            amount,
            origin: token.origin,
            link: token.id, // Link to the previous transaction
            holder: receiver,
            origin_timestamp: token.origin_timestamp,
            state: TrstState::Active,
            origin_wallet: token.origin_wallet.clone(),
            origin_proportions: token.origin_proportions.clone(),
        };

        let change = if amount < token.amount {
            Some(TrstToken {
                id: todo!("generate change tx hash"),
                amount: token.amount - amount,
                origin: token.origin,
                link: token.id,
                holder: token.holder.clone(),
                origin_timestamp: token.origin_timestamp,
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
    /// All splits share the same origin and link. Amounts must sum to the parent.
    pub fn split(
        &self,
        token: &TrstToken,
        amounts: &[(WalletAddress, u128)],
        _tx_hashes: &[TxHash],
        now: Timestamp,
        expiry_secs: u64,
    ) -> Result<Vec<TrstToken>, TrstError> {
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

        let splits = amounts
            .iter()
            .zip(_tx_hashes.iter())
            .map(|((receiver, amount), &hash)| TrstToken {
                id: hash,
                amount: *amount,
                origin: token.origin,
                link: token.id,
                holder: receiver.clone(),
                origin_timestamp: token.origin_timestamp,
                state: TrstState::Active,
                origin_wallet: token.origin_wallet.clone(),
                origin_proportions: token.origin_proportions.clone(),
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
        if tokens.is_empty() {
            return Err(TrstError::EmptyMerge);
        }

        // Verify all tokens are transferable.
        for t in tokens {
            if !t.is_transferable(now, expiry_secs) {
                return Err(TrstError::NotTransferable(format!("{:?} ({})", t.state, t.id)));
            }
        }

        let total_amount: u128 = tokens.iter().map(|t| t.amount).sum();

        // Find the earliest origin timestamp (conservative expiry).
        let earliest_origin = tokens
            .iter()
            .map(|t| t.origin_timestamp)
            .min()
            .unwrap();

        // Build origin proportions for the merged token.
        let mut proportions: Vec<OriginProportion> = Vec::new();
        for t in tokens {
            if t.origin_proportions.is_empty() {
                // Simple token: 100% from its origin.
                proportions.push(OriginProportion {
                    origin: t.origin,
                    origin_wallet: t.origin_wallet.clone(),
                    amount: t.amount,
                });
            } else {
                // Already merged: carry forward its proportions.
                proportions.extend(t.origin_proportions.clone());
            }
        }

        // Record in merger graph.
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

        Ok(TrstToken {
            id: merge_tx_hash,
            amount: total_amount,
            origin: merge_tx_hash, // Merged token uses merge tx as its new origin reference
            link: merge_tx_hash,
            holder,
            origin_timestamp: earliest_origin,
            state: TrstState::Active,
            origin_wallet: tokens[0].origin_wallet.clone(), // Primary origin wallet
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
    /// Returns a list of revocation events (wallet, amount) for proportional splitting.
    pub fn revoke_by_origin(
        &self,
        _origin_wallet: &WalletAddress,
        origin_tx: &TxHash,
    ) -> Vec<crate::merger_graph::RevocationEvent> {
        self.merger_graph.propagate_revocation(origin_tx)
    }
}

impl Default for TrstEngine {
    fn default() -> Self {
        Self::new()
    }
}
