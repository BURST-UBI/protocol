use burst_brn::{wallet_storage, BrnError, BrnWalletState};
use burst_store::StoreError;
use burst_types::WalletAddress;
use heed::{Database, RoTxn, RwTxn, types::Bytes};

fn invalid(error: impl ToString) -> StoreError { StoreError::Corruption(error.to_string()) }
fn backend(error: heed::Error) -> StoreError { StoreError::Backend(error.to_string()) }

pub(crate) fn uses_records(txn: &RoTxn<'_>, meta: Database<Bytes, Bytes>) -> Result<bool, StoreError> {
    match meta.get(txn, wallet_storage::FORMAT_KEY).map_err(backend)? {
        Some([1]) => Ok(true),
        Some(_) => Err(invalid("invalid BRN wallet record format")),
        None => {
            if meta.prefix_iter(txn, wallet_storage::ROW_PREFIX).map_err(backend)?.next().transpose().map_err(backend)?.is_some() {
                return Err(invalid("orphan BRN wallet records"));
            }
            Ok(false)
        }
    }
}

pub(crate) fn validate_inventory(
    txn: &RoTxn<'_>, wallets: Database<Bytes, Bytes>, meta: Database<Bytes, Bytes>,
    budget: usize,
) -> Result<bool, StoreError> {
    let normalized = uses_records(txn, meta)?;
    if normalized {
        let mut bytes = 0usize;
        for row in meta.prefix_iter(txn, wallet_storage::ROW_PREFIX).map_err(backend)? {
            let (key, value) = row.map_err(backend)?;
            bytes = bytes.checked_add(key.len()).and_then(|size| size.checked_add(value.len()))
                .filter(|size| *size <= budget).ok_or_else(|| invalid("BRN snapshot exceeds restoration limit"))?;
            let (address, relative) = wallet_storage::split_row_key(key).map_err(invalid)?;
            super::decode_wallet_address(address)?;
            if relative == wallet_storage::HEADER || wallets.get(txn, address).map_err(backend)?.is_none() {
                return Err(invalid("orphan BRN wallet record or misplaced header"));
            }
        }
    }
    Ok(normalized)
}

/// Return decoded state and total stored key/value bytes, excluding LMDB page
/// overhead. Charge each row before decoding. Child rows without a wallet header
/// are corruption.
pub(crate) fn decode_wallet(
    txn: &RoTxn<'_>, meta: Database<Bytes, Bytes>, address: &WalletAddress,
    header: Option<&[u8]>, budget: usize,
) -> Result<(Option<BrnWalletState>, usize), StoreError> {
    let prefix = wallet_storage::row_prefix(address).map_err(invalid)?;
    let mut bytes = header.map_or(0, |header| address.as_str().len() + header.len());
    if bytes > budget { return Err(invalid("BRN snapshot exceeds restoration limit")); }
    let children = meta.prefix_iter(txn, &prefix).map_err(backend)?;
    let first = header.into_iter().map(|header| Ok((wallet_storage::HEADER, header)));
    let children = children.map(|row| {
        let (key, value) = row.map_err(|error| BrnError::Other(error.to_string()))?;
        bytes = bytes.checked_add(key.len()).and_then(|size| size.checked_add(value.len()))
            .filter(|size| *size <= budget).ok_or_else(|| BrnError::Other("BRN snapshot exceeds restoration limit".into()))?;
        Ok((&key[prefix.len()..], value))
    });
    let state = wallet_storage::decode(first.chain(children)).map_err(invalid)?;
    Ok((state, bytes))
}

pub(crate) fn replace_wallet(
    txn: &mut RwTxn<'_>, wallets: Database<Bytes, Bytes>, meta: Database<Bytes, Bytes>,
    address: &WalletAddress, after: Option<&BrnWalletState>,
) -> Result<(), StoreError> {
    let before = decode_wallet(txn, meta, address, wallets.get(txn, address.as_str().as_bytes()).map_err(backend)?, usize::MAX)?.0;
    let prefix = wallet_storage::row_prefix(address).map_err(invalid)?;
    wallet_storage::visit_delta(before.as_ref(), after, |relative, _, value| {
        let (db, key) = if relative == wallet_storage::HEADER {
            (wallets, address.as_str().as_bytes().to_vec())
        } else { (meta, [prefix.as_slice(), relative].concat()) };
        match value {
            Some(value) => db.put(txn, &key, &value).map_err(|error| BrnError::Other(error.to_string()))?,
            None => { db.delete(txn, &key).map_err(|error| BrnError::Other(error.to_string()))?; }
        }
        Ok(())
    }).map_err(invalid)
}
