//! Pending receive storage trait.

use crate::StoreError;
use burst_types::{Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// Information about a pending incoming transfer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingInfo {
    pub source: WalletAddress,
    pub amount: u128,
    pub timestamp: Timestamp,
    /// Provenance from consumed tokens. Deserialization permits an empty list,
    /// but `validate_pending_provenance` rejects it.
    #[serde(default)]
    pub provenance: Vec<PendingProvenance>,
    /// The tokens were already returned to `source` at an expiry boundary.
    /// A Receive acknowledges their nominal signed balance once; it must not
    /// deliver tokens again. This receipt itself neither expires nor revokes.
    pub return_credit: bool,
}

impl PendingInfo {
    /// The pending key names a source block, unlike derived holding IDs. Return
    /// credits and restored chunks retain references too. The visitor can stop
    /// before the next reference; callers own cut validation and traversal bounds.
    pub fn visit_retention_references<'a, B>(
        &'a self,
        destination: &'a WalletAddress,
        source_hash: burst_types::BlockHash,
        mut visit: impl FnMut(burst_types::TrstReference<'a>) -> std::ops::ControlFlow<B>,
    ) -> std::ops::ControlFlow<B> {
        use burst_types::TrstReference::{Account, Block, Graph};
        visit(Block(source_hash))?;
        visit(Account(destination))?;
        visit(Account(&self.source))?;
        for part in &self.provenance {
            visit(Account(&part.origin_wallet))?;
            visit(Graph(part.origin))?;
            part.basis.visit_retention_references(&mut visit)?;
            for origin in part.revoked_amounts.keys() {
                visit(Graph(TxHash::new(*origin)))?;
            }
        }
        std::ops::ControlFlow::Continue(())
    }
}

/// Origin provenance for a consumed token portion, stored in pending entries.
///
/// For merged tokens, `origin` names the merge transaction. `basis` identifies
/// the composition used for this portion; `revoked_amounts` retains its splits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingProvenance {
    pub basis: burst_types::TrstBasis,
    pub amount: u128,
    pub origin: TxHash,
    pub origin_wallet: WalletAddress,
    pub origin_timestamp: Timestamp,
    pub effective_origin_timestamp: Timestamp,
    /// Catch-time splits, retained even after restoration so the restored
    /// origin cannot blend back into the clean remainder. Clean sends start
    /// empty; delivery must not apply historical revocations again.
    pub revoked_amounts: std::collections::BTreeMap<[u8; 32], burst_types::TrstRevocationChunk>,
}

/// Check matching origin metadata, positive amounts and checked totals.
/// Does not resolve graph references, validate bases or authorize a transfer.
pub fn validate_pending_provenance(pending: &PendingInfo) -> Result<(), String> {
    let first = pending
        .provenance
        .first()
        .ok_or("missing pending provenance")?;
    if pending.amount == 0
        || pending.provenance.iter().any(|p| {
            p.amount == 0
                || p.origin != first.origin
                || p.origin_wallet != first.origin_wallet
                || p.origin_timestamp != first.origin_timestamp
                || p.effective_origin_timestamp != first.effective_origin_timestamp
                || p.revoked_amounts
                    .iter()
                    .any(|(origin, chunk)| *origin == [0; 32] || chunk.amount == 0)
                || p.revoked_amounts
                    .values()
                    .try_fold(0u128, |sum, chunk| sum.checked_add(chunk.amount))
                    .is_none_or(|sum| sum > p.amount)
        })
        || pending
            .provenance
            .iter()
            .try_fold(0u128, |sum, p| sum.checked_add(p.amount))
            != Some(pending.amount)
    {
        return Err("inconsistent pending provenance".into());
    }
    Ok(())
}

/// A bounded amount-descending page, with hash order breaking equal-amount ties.
pub struct PendingPage {
    pub entries: Vec<(TxHash, PendingInfo)>,
    pub has_more: bool,
}

/// Sum and count of outgoing transfers, excluding return-credit receipts.
#[derive(Default)]
pub struct PendingSourceTotals {
    pub amount: u128,
    pub count: u64,
}

impl PendingSourceTotals {
    pub fn include(&mut self, source: &WalletAddress, pending: &PendingInfo) -> Result<(), StoreError> {
        if pending.return_credit { return Ok(()); }
        if &pending.source != source {
            return Err(StoreError::Corruption("pending transfer belongs to another sender".into()));
        }
        let amount = self.amount.checked_add(pending.amount)
            .ok_or_else(|| StoreError::Corruption("pending amount overflow".into()))?;
        let count = self.count.checked_add(1)
            .ok_or_else(|| StoreError::Corruption("pending count overflow".into()))?;
        self.amount = amount;
        self.count = count;
        Ok(())
    }
}

/// Captured outgoing transfers for enumeration or aggregation.
pub trait PendingSourceSnapshot {
    /// The default materializes all entries before summing. Backends can override
    /// this method to stream entries from a pinned read transaction.
    fn totals(self: Box<Self>, source: &WalletAddress) -> Result<PendingSourceTotals, StoreError> {
        let mut totals = PendingSourceTotals::default();
        for (_, _, pending) in self.into_entries()? { totals.include(source, &pending)?; }
        Ok(totals)
    }
    fn into_entries(self: Box<Self>) -> Result<Vec<(WalletAddress, TxHash, PendingInfo)>, StoreError>;
}

impl PendingSourceSnapshot for Vec<(WalletAddress, TxHash, PendingInfo)> {
    fn into_entries(self: Box<Self>) -> Result<Vec<(WalletAddress, TxHash, PendingInfo)>, StoreError> {
        Ok(*self)
    }
}

/// Trait for tracking pending receives.
///
/// Keys are `(destination, source_hash)` pairs. Each pending entry represents
/// an incoming transfer that has not yet been pocketed by the destination account.
pub trait PendingStore {
    /// Record a pending receive for the destination account.
    fn put_pending(
        &self,
        destination: &WalletAddress,
        source_hash: &TxHash,
        info: &PendingInfo,
    ) -> Result<(), StoreError>;

    /// Retrieve a specific pending receive.
    fn get_pending(
        &self,
        destination: &WalletAddress,
        source_hash: &TxHash,
    ) -> Result<PendingInfo, StoreError>;

    /// Delete a pending receive (once it has been pocketed).
    fn delete_pending(
        &self,
        destination: &WalletAddress,
        source_hash: &TxHash,
    ) -> Result<(), StoreError>;

    /// Get all pending receives for a given destination account.
    fn get_pending_for_account(
        &self,
        destination: &WalletAddress,
    ) -> Result<Vec<PendingInfo>, StoreError>;

    /// Get all pending receives with their source block hashes (for receive RPC).
    fn get_pending_for_account_with_hashes(
        &self,
        destination: &WalletAddress,
    ) -> Result<Vec<(TxHash, PendingInfo)>, StoreError>;

    /// Amount-ordered pagination. Backends may override this compatibility
    /// implementation with an index to avoid loading the complete backlog.
    fn get_pending_page(
        &self,
        destination: &WalletAddress,
        threshold: u128,
        offset: u64,
        count: u32,
    ) -> Result<PendingPage, StoreError> {
        let mut all = self.get_pending_for_account_with_hashes(destination)?;
        all.sort_by_key(|(hash, info)| (std::cmp::Reverse(info.amount), *hash));
        let mut entries: Vec<_> = all
            .into_iter()
            .filter(|(_, info)| info.amount >= threshold)
            .skip(usize::try_from(offset).unwrap_or(usize::MAX))
            .take(count as usize + 1)
            .collect();
        let has_more = entries.len() > count as usize;
        if has_more {
            entries.pop();
        }
        Ok(PendingPage { entries, has_more })
    }

    /// Continue after an amount/hash key, ordered by amount descending then hash
    /// ascending. The default loads and sorts the full account backlog. Separate
    /// calls do not share a snapshot, so changes can affect later pages.
    fn get_pending_page_after(
        &self, destination: &WalletAddress, threshold: u128,
        after: Option<(u128, TxHash)>, count: u32,
    ) -> Result<PendingPage, StoreError> {
        let mut all = self.get_pending_for_account_with_hashes(destination)?;
        all.sort_by_key(|(hash, info)| (std::cmp::Reverse(info.amount), *hash));
        let mut entries: Vec<_> = all.into_iter().filter(|(hash, info)| {
            info.amount >= threshold && after.is_none_or(|(amount, prior)|
                info.amount < amount || (info.amount == amount && *hash > prior))
        }).take(count as usize + 1).collect();
        let has_more = entries.len() > count as usize;
        if has_more { entries.pop(); }
        Ok(PendingPage { entries, has_more })
    }

    /// Outgoing TRST remains attributed to this sender until receipt or return.
    /// It is not available for another spend.
    fn get_pending_from_account(
        &self,
        source: &WalletAddress,
    ) -> Result<Vec<(WalletAddress, TxHash, PendingInfo)>, StoreError>;

    /// Pin outgoing data now; callers may enumerate after releasing their
    /// publication guard. Backends without snapshots materialize under the guard.
    fn capture_pending_from_account(
        &self, source: &WalletAddress,
    ) -> Result<Box<dyn PendingSourceSnapshot + '_>, StoreError> {
        Ok(Box::new(self.get_pending_from_account(source)?))
    }

    /// Total number of pending receives across all accounts.
    fn pending_count(&self) -> Result<u64, StoreError>;

    /// Get every pending receive across all accounts.
    ///
    /// Materializes the entire pending table. Use account queries or captured
    /// source views when the operation only needs one account.
    fn get_all_pending(&self) -> Result<Vec<(WalletAddress, TxHash, PendingInfo)>, StoreError>;
}

#[cfg(test)]
mod retention_tests {
    use super::*;
    use burst_types::{BlockHash, TrstBasis, TrstReference, TrstRevocationChunk};
    use std::ops::ControlFlow;

    #[test]
    fn retention_keeps_source_block_old_basis_and_return_credit_chunks() {
        use TrstReference::{Account, Block, Graph, Revision};
        let h = |n| TxHash::new([n; 32]);
        for return_credit in [false, true] {
            let pending = PendingInfo {
                source: WalletAddress::new("brst_source"), amount: 10, timestamp: Timestamp::new(1), return_credit,
                provenance: vec![PendingProvenance {
                    basis: TrstBasis::Remainder(h(3)), amount: 10, origin: h(2),
                    origin_wallet: WalletAddress::new("brst_origin"), origin_timestamp: Timestamp::new(1),
                    effective_origin_timestamp: Timestamp::new(1),
                    revoked_amounts: [([4; 32], TrstRevocationChunk { amount: 2, restored: false }),
                        ([5; 32], TrstRevocationChunk { amount: 3, restored: true })].into(),
                }],
            };
            validate_pending_provenance(&pending).unwrap();
            let source = BlockHash::new([1; 32]);
            let destination = WalletAddress::new("brst_destination");
            let expected = [Block(source), Account(&destination), Account(&pending.source),
                Account(&pending.provenance[0].origin_wallet), Graph(h(2)), Revision(h(3)), Graph(h(4)), Graph(h(5))];
            for limit in 0..=expected.len() {
                let mut seen = Vec::new();
                let result = pending.visit_retention_references(&destination, source, |r| {
                    if seen.len() == limit { return ControlFlow::Break("budget"); }
                    seen.push(r);
                    ControlFlow::Continue(())
                });
                assert_eq!(seen, expected[..limit]);
                assert_eq!(result.is_break(), limit < expected.len());
            }
        }
    }
}
