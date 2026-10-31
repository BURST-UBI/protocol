use blake2::{digest::consts::U32, Blake2b, Digest};
use burst_store::{account::AccountInfo, StoreError};
use std::io::{self, Write};

/// Order-sensitive fingerprint of serialized account rows for local change
/// detection. Callers must use the same row ordering when comparing captures.
/// This does not establish peer authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AccountInventory {
    count: u64,
    digest: [u8; 32],
}

struct HashWriter(Blake2b<U32>);

impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Owns the iterator and digest until every account has been consumed. A partial
/// or failed scan cannot become a local change-detection fingerprint.
pub(super) struct AccountInventoryCapture<I: Iterator<Item = Result<AccountInfo, StoreError>>> {
    rows: std::iter::Fuse<I>,
    writer: HashWriter,
    count: u64,
    complete: bool,
    failed: bool,
}

impl<I: Iterator<Item = Result<AccountInfo, StoreError>>> AccountInventoryCapture<I> {
    pub(super) fn new(rows: I) -> Self {
        let mut writer = HashWriter(Blake2b::new());
        writer.0.update(b"BURST/local-account-inventory/v1\0");
        Self {
            rows: rows.fuse(),
            writer,
            count: 0,
            complete: false,
            failed: false,
        }
    }

    pub(super) fn advance(&mut self, max_rows: usize) -> Result<bool, StoreError> {
        if self.failed {
            return Err(StoreError::Corruption(
                "account inventory capture already failed".into(),
            ));
        }
        self.failed = true;
        if max_rows == 0 {
            return Err(StoreError::Corruption(
                "zero account inventory page budget".into(),
            ));
        }
        if !self.complete {
            for _ in 0..max_rows {
                let Some(row) = self.rows.next() else {
                    self.complete = true;
                    break;
                };
                let row = row?;
                let length = bincode::serialized_size(&row)
                    .map_err(|error| StoreError::Serialization(error.to_string()))?;
                self.writer.0.update(length.to_be_bytes());
                bincode::serialize_into(&mut self.writer, &row)
                    .map_err(|error| StoreError::Serialization(error.to_string()))?;
                self.count = self.count.checked_add(1).ok_or_else(|| {
                    StoreError::Corruption("account inventory count overflow".into())
                })?;
            }
        }
        self.failed = false;
        Ok(self.complete)
    }

    pub(super) fn finish(mut self) -> Result<AccountInventory, StoreError> {
        if self.failed || !self.complete {
            return Err(StoreError::Corruption(
                "account inventory capture incomplete".into(),
            ));
        }
        self.writer.0.update(self.count.to_be_bytes());
        Ok(AccountInventory {
            count: self.count,
            digest: self.writer.0.finalize().into(),
        })
    }
}

impl AccountInventory {
    pub(super) fn read(
        rows: impl IntoIterator<Item = Result<AccountInfo, StoreError>>,
    ) -> Result<Self, StoreError> {
        let mut capture = AccountInventoryCapture::new(rows.into_iter());
        while !capture.advance(128)? {}
        capture.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::WalletAddress;

    #[test]
    fn paged_inventory_preserves_digest_and_rejects_partial_or_failed_capture() {
        let rows: Vec<_> = (0..301)
            .map(|i| AccountInfo::unopened(WalletAddress::new(format!("brst_account_{i}"))))
            .collect();
        // Compute the expected digest from length-prefixed rows, then count.
        let mut hash = Blake2b::<U32>::new();
        hash.update(b"BURST/local-account-inventory/v1\0");
        for row in &rows {
            let bytes = bincode::serialize(row).unwrap();
            hash.update((bytes.len() as u64).to_be_bytes());
            hash.update(bytes);
        }
        hash.update((rows.len() as u64).to_be_bytes());
        let expected = AccountInventory {
            count: rows.len() as u64,
            digest: hash.finalize().into(),
        };
        for page_size in [1, 3, 128, 301, 512] {
            let mut capture = AccountInventoryCapture::new(rows.clone().into_iter().map(Ok));
            let mut pages = 1;
            while !capture.advance(page_size).unwrap() {
                pages += 1;
            }
            assert_eq!(pages, rows.len() / page_size + 1);
            assert!(capture.advance(page_size).unwrap());
            assert_eq!(capture.finish().unwrap(), expected);
        }
        let mut partial = AccountInventoryCapture::new(rows.clone().into_iter().map(Ok));
        assert!(!partial.advance(1).unwrap());
        assert!(partial.finish().is_err());
        let mut failed = AccountInventoryCapture::new(
            [
                Ok(rows[0].clone()),
                Err(StoreError::Corruption("injected".into())),
                Ok(rows[1].clone()),
            ]
            .into_iter(),
        );
        assert!(!failed.advance(1).unwrap());
        assert!(failed.advance(1).is_err());
        assert!(failed.advance(1).is_err());
        assert!(failed.finish().is_err());
        let mut zero = AccountInventoryCapture::new(std::iter::empty());
        assert!(zero.advance(0).is_err());
        assert!(zero.finish().is_err());
        let mut empty = AccountInventoryCapture::new(std::iter::empty());
        assert!(empty.advance(1).unwrap());
        assert_eq!(empty.finish().unwrap(), AccountInventory::read([]).unwrap());
    }

    #[test]
    fn inventory_binds_order_count_and_account_fields_and_propagates_read_errors() {
        let first = AccountInfo::unopened(WalletAddress::new("brst_first"));
        let second = AccountInfo::unopened(WalletAddress::new("brst_second"));
        let fingerprint =
            |rows: Vec<AccountInfo>| AccountInventory::read(rows.into_iter().map(Ok)).unwrap();
        let expected = fingerprint(vec![first.clone(), second.clone()]);
        assert_eq!(expected, fingerprint(vec![first.clone(), second.clone()]));
        for rows in [
            vec![],
            vec![first.clone()],
            vec![first.clone(), second.clone(), second.clone()],
            vec![second.clone(), first.clone()],
        ] {
            assert_ne!(expected, fingerprint(rows));
        }
        let mut changed = second.clone();
        changed.total_brn_staked = 1;
        assert_ne!(expected, fingerprint(vec![first.clone(), changed]));
        let mut changed = second;
        changed.confirmation_height = 1;
        assert_ne!(expected, fingerprint(vec![first.clone(), changed]));
        assert!(AccountInventory::read([
            Ok(first),
            Err(StoreError::Corruption("injected read failure".into()))
        ])
        .is_err());
    }
}
