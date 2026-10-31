use super::*;
use burst_brn::{retention::Reference, wallet_storage};

/// Normalized wallet headers, pauses and releases from the same pinned view as
/// the parent TRST scan. This does not include stakes, rate rows or lifecycle
/// receipts, and does not validate complete per-wallet counts or row order.
pub struct BrnWalletReferenceReader<'view, 'db> {
    source: &'view TrstReferenceReader<'db>,
    phase: usize,
    after: Option<Vec<u8>>,
    failed: bool,
}

impl<'db> TrstReferenceReader<'db> {
    pub fn brn_wallet_references(&self) -> Result<BrnWalletReferenceReader<'_, 'db>, StoreError> {
        if self.failed {
            return Err(invalid("retention scan already failed"));
        }
        if !crate::brn::records::uses_records(&self.view.txn, self.store.brn_meta_db)? {
            return Err(invalid(
                "retention scan requires normalized BRN wallet records",
            ));
        }
        Ok(BrnWalletReferenceReader {
            source: self,
            phase: 0,
            after: None,
            failed: false,
        })
    }
}

impl BrnWalletReferenceReader<'_, '_> {
    pub fn visit_next_page(
        &mut self,
        max_rows: usize,
        max_bytes: usize,
        mut visit: impl FnMut(Reference<'_>) -> Result<(), StoreError>,
    ) -> Result<ReferencePage, StoreError> {
        if self.failed {
            return Err(invalid("BRN retention scan already failed"));
        }
        let result = self.visit_page(max_rows, max_bytes, &mut visit);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn visit_page(
        &mut self,
        max_rows: usize,
        max_bytes: usize,
        visit: &mut impl FnMut(Reference<'_>) -> Result<(), StoreError>,
    ) -> Result<ReferencePage, StoreError> {
        if max_rows == 0 || max_rows > MAX_PAGE_ROWS || max_bytes == 0 || max_bytes > MAX_PAGE_BYTES
        {
            return Err(invalid("invalid retention page budget"));
        }
        let view = &self.source.view.txn;
        let store = self.source.store;
        let mut page = ReferencePage {
            rows: 0,
            bytes: 0,
            done: false,
        };
        while self.phase < 2 {
            let (db, prefix) = if self.phase == 0 {
                (store.brn_wallets_db, &b""[..])
            } else {
                (store.brn_meta_db, wallet_storage::ROW_PREFIX)
            };
            let start = self.after.clone();
            let lower = match start.as_deref() {
                Some(key) => Bound::Excluded(key),
                None if prefix.is_empty() => Bound::Unbounded,
                None => Bound::Included(prefix),
            };
            let mut rows = db
                .range(view, &(lower, Bound::<&[u8]>::Unbounded))
                .map_err(LmdbError::from)?;
            loop {
                if page.rows == max_rows {
                    return Ok(page);
                }
                let Some(row) = rows.next() else {
                    break;
                };
                let (key, bytes) = row.map_err(LmdbError::from)?;
                if !key.starts_with(prefix) {
                    break;
                }
                let size = key
                    .len()
                    .checked_add(bytes.len())
                    .ok_or_else(|| invalid("retention row size overflow"))?;
                if size > max_bytes {
                    return Err(invalid("retention row exceeds byte budget"));
                }
                if size > max_bytes - page.bytes {
                    return Ok(page);
                }
                let (address, relative) = if self.phase == 0 {
                    (key, wallet_storage::HEADER)
                } else {
                    let (address, relative) =
                        wallet_storage::split_row_key(key).map_err(invalid)?;
                    if relative == wallet_storage::HEADER
                        || store
                            .brn_wallets_db
                            .get(view, address)
                            .map_err(LmdbError::from)?
                            .is_none()
                    {
                        return Err(invalid("orphan BRN wallet record or misplaced header"));
                    }
                    (address, relative)
                };
                let owner = crate::brn::decode_wallet_address(address)?;
                let mut callback_error = None;
                let result =
                    wallet_storage::visit_retention_references(&owner, relative, bytes, |r| {
                        visit(r).map_err(|error| {
                            callback_error = Some(error);
                            burst_brn::BrnError::Other("retention callback stopped".into())
                        })
                    });
                if let Some(error) = callback_error {
                    return Err(error);
                }
                result.map_err(invalid)?;
                self.after = Some(key.to_vec());
                page.rows += 1;
                page.bytes += size;
            }
            self.phase += 1;
            self.after = None;
        }
        page.done = true;
        Ok(page)
    }
}

#[cfg(test)]
mod tests;
