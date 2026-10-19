use super::*;
use bincode::Options;
use burst_store::StoreError;
use burst_store_lmdb::write_batch::checkpoint_projection::CheckpointBlock;
use std::borrow::Cow;

impl CompactCutCoverage {
    pub(in crate::confirmed_replay::snapshot) fn installation_blocks<'a>(
        &'a self,
        mut get: impl FnMut(&BlockHash) -> Result<Vec<u8>, StoreError> + 'a,
    ) -> impl Iterator<Item = Result<Cow<'static, CheckpointBlock>, StoreError>> + 'a {
        let invalid = |message: &str| StoreError::Corruption(message.into());
        let mut positions: Vec<_> = self.positions.iter().collect();
        positions.sort_by_key(|(_, hash)| *hash.as_bytes());
        let mut used = 0usize;
        positions.into_iter().map(move |((account, height), hash)| {
            let bytes = match self.archive.encoded(hash)? {
                Some(bytes) => bytes,
                None => get(hash)?,
            };
            used = used
                .checked_add(bytes.len())
                .filter(|size| *size <= 64 << 20)
                .ok_or_else(|| invalid("compact installation bodies exceed byte limit"))?;
            let block: burst_ledger::StateBlock = bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .with_limit(bytes.len() as u64)
                .reject_trailing_bytes()
                .deserialize(&bytes)
                .map_err(|error| StoreError::Corruption(error.to_string()))?;
            block
                .validate_format()
                .map_err(|error| StoreError::Corruption(error.to_string()))?;
            if block.hash != *hash || block.compute_hash() != *hash || block.account != *account {
                return Err(invalid(
                    "compact installation body differs from authenticated position",
                ));
            }
            Ok(Cow::Owned(CheckpointBlock {
                hash: *hash,
                account: account.clone(),
                height: *height,
                bytes,
            }))
        })
    }
}
