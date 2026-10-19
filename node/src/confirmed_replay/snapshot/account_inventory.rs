use blake2::{digest::consts::U32, Blake2b, Digest};
use burst_store::{account::AccountInfo, StoreError};
use std::io::{self, Write};

/// Local change detection only, never an account commitment or peer authority.
#[derive(Debug, PartialEq, Eq)]
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

impl AccountInventory {
    pub(super) fn read(
        rows: impl IntoIterator<Item = Result<AccountInfo, StoreError>>,
    ) -> Result<Self, StoreError> {
        let mut writer = HashWriter(Blake2b::new());
        writer.0.update(b"BURST/local-account-inventory/v1\0");
        let mut count = 0u64;
        for row in rows {
            let row = row?;
            let length = bincode::serialized_size(&row)
                .map_err(|error| StoreError::Serialization(error.to_string()))?;
            writer.0.update(length.to_be_bytes());
            bincode::serialize_into(&mut writer, &row)
                .map_err(|error| StoreError::Serialization(error.to_string()))?;
            count = count
                .checked_add(1)
                .ok_or_else(|| StoreError::Corruption("account inventory count overflow".into()))?;
        }
        writer.0.update(count.to_be_bytes());
        Ok(Self {
            count,
            digest: writer.0.finalize().into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::WalletAddress;

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
