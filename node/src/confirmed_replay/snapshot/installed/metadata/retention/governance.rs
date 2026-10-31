use super::*;
use burst_governance::{engine::GovernanceRecord, record_references::Reference};

/// Keyed governance records from the already authenticated pinned cut.
/// Delegation, revisions and authority schedules are
/// separate roots. Page completion does not authorize history retirement.
pub(crate) struct GovernanceReferenceReader<'scan, 'db> {
    store: &'scan LmdbStore,
    view: &'scan burst_store_lmdb::LmdbReadTransaction<'db>,
    after: Option<String>,
    phase: usize,
    failed: bool,
}

impl<'scan, 'db> GovernanceReferenceReader<'scan, 'db> {
    pub(super) fn new(
        store: &'scan LmdbStore,
        view: &'scan burst_store_lmdb::LmdbReadTransaction<'db>,
    ) -> Result<Self, StoreError> {
        let meta = store.meta_store();
        if meta
            .get_meta_txn(view, "governance_records_format")?
            .as_deref()
            != Some(&[1])
            || store
                .brn_store()
                .contains_meta_txn(view, b"governance_engine")?
        {
            return Err(invalid(
                "retention requires keyed governance state without aggregate storage",
            ));
        }
        Ok(Self {
            store,
            view,
            after: None,
            phase: 0,
            failed: false,
        })
    }

    pub(crate) fn visit_next_page(
        &mut self,
        max_rows: usize,
        max_bytes: usize,
        mut visit: impl FnMut(Reference<'_>) -> Result<(), StoreError>,
    ) -> Result<ReferencePage, StoreError> {
        if self.failed {
            return Err(invalid("governance retention scan already failed"));
        }
        let result = (|| {
            if max_rows == 0
                || max_rows > burst_store_lmdb::retention::MAX_PAGE_ROWS
                || max_bytes == 0
                || max_bytes > burst_store_lmdb::retention::MAX_PAGE_BYTES
            {
                return Err(invalid("invalid governance retention page budget"));
            }
            if self.phase == 2 {
                return Ok(ReferencePage {
                    rows: 0,
                    bytes: 0,
                    done: true,
                });
            }
            let page = self.store.meta_store().visit_prefix_page_txn(
                self.view,
                ["governance_state/", "governance_endorsement/"][self.phase],
                self.after.as_deref(),
                max_rows,
                max_bytes,
                |key, bytes| {
                    if self.phase == 1 {
                        return visit_endorsement(key, bytes, &mut visit);
                    }
                    let mut callback_error = None;
                    let mut forward = |reference: Reference<'_>| {
                        visit(reference).map_err(|error| {
                            callback_error = Some(error);
                            "retention callback stopped".to_string()
                        })
                    };
                    let result = GovernanceRecord::from_storage_entry(key, bytes)
                        .and_then(|record| record.visit_retention_references(&mut forward));
                    result.map_err(|reason| callback_error.unwrap_or_else(|| invalid(reason)))
                },
            )?;
            self.after = page.last_key.or_else(|| self.after.take());
            if page.done {
                self.phase += 1;
                self.after = None;
            }
            Ok(ReferencePage {
                rows: page.rows,
                bytes: page.bytes,
                done: self.phase == 2,
            })
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}

/// Persisted duplicate-burn guard: retain the signed carrier even after proposal
/// withdrawal. The recovered cut authenticates this binding; extraction alone
/// neither verifies the referenced block nor authorizes deleting it.
pub(super) fn visit_endorsement(
    key: &str,
    bytes: &[u8],
    mut visit: impl FnMut(Reference<'_>) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    use burst_types::{BlockHash, WalletAddress};
    let suffix = key
        .strip_prefix("governance_endorsement/")
        .ok_or_else(|| invalid("unsupported governance endorsement key"))?;
    let (proposal, account) = suffix
        .split_once('/')
        .ok_or_else(|| invalid("missing governance endorsement account"))?;
    let mut hash = [0; 32];
    hex::decode_to_slice(proposal, &mut hash).map_err(invalid)?;
    let proposal_hash = BlockHash::new(hash);
    if proposal_hash.is_zero()
        || proposal != proposal_hash.to_string()
        || !account.starts_with("brst_")
        || account.len() <= 5
        || account.contains('/')
    {
        return Err(invalid("noncanonical governance endorsement key"));
    }
    let carrier = BlockHash::new(
        bytes
            .try_into()
            .map_err(|_| invalid("invalid endorsement carrier length"))?,
    );
    if carrier.is_zero() {
        return Err(invalid("zero governance endorsement carrier"));
    }
    let account = WalletAddress::new(account);
    visit(Reference::Proposal(proposal_hash.into_tx_hash()))?;
    visit(Reference::Account(&account))?;
    visit(Reference::Block(carrier))
}

#[cfg(test)]
mod tests;
