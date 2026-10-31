//! Local confirmation prerequisites written with a validated lifecycle outcome.
//! Remote history replay derives its own panel; these records are not certificates.

use super::ledger::Ledger;
use bincode::Options;
use burst_ledger::{BlockType, StateBlock};
use burst_store::{account::AccountInfo, StoreError};
use burst_store_lmdb::block_undo::{BlockUndo, UndoTable};
use burst_types::{BlockHash, WalletAddress};

pub(crate) mod view;
use view::DependencyView;

const MAX_SOURCES: usize = 131_072;
const MAX_BYTES: u64 = 8 + 32 * MAX_SOURCES as u64;

fn outcome_key(hash: BlockHash) -> String {
    format!("verification_outcome_sources/v1/{hash}")
}

fn transition_key(hash: BlockHash) -> String {
    format!("verification_transition_sources/v1/{hash}")
}

fn identity_key(wallet: &WalletAddress) -> String {
    format!("verification_identity_source/v1/{wallet}")
}

fn identity_binding_key(hash: BlockHash) -> String {
    format!("verification_identity_binding/v1/{hash}")
}

pub(crate) fn record_identity(
    batch: &mut impl Ledger,
    wallet: &WalletAddress,
    source: BlockHash,
) -> Result<(), String> {
    if source.is_zero() {
        return Err("verification identity source is missing".into());
    }
    batch
        .put_meta(&identity_key(wallet), source.as_bytes())
        .map_err(|error| error.to_string())
}

/// Only the first account block after an identity transition needs an extra edge.
/// Later blocks inherit it through their predecessor, including after restart.
pub(crate) fn bind_identity(batch: &mut impl Ledger, block: &StateBlock) -> Result<(), StoreError> {
    let Some(source) = batch.get_meta(&identity_key(&block.account))? else {
        return Ok(());
    };
    let hash = decode_source(&source)?;
    if hash == block.hash {
        return Err(invalid("self-referential identity source"));
    }
    let bound_key = format!("verification_identity_bound/v1/{}", block.account);
    if batch.get_meta(&bound_key)?.as_ref() != Some(&source) {
        batch.put_meta(&identity_binding_key(block.hash), &source)?;
        batch.put_meta(&bound_key, &source)?;
    }
    Ok(())
}

pub(super) fn decode_source(bytes: &[u8]) -> Result<BlockHash, StoreError> {
    let hash = BlockHash::new(
        bytes
            .try_into()
            .map_err(|_| invalid("invalid identity source length"))?,
    );
    if hash.is_zero() {
        return Err(invalid("zero identity source"));
    }
    Ok(hash)
}

fn invalid(message: impl ToString) -> StoreError {
    StoreError::Corruption(message.to_string())
}

pub(super) fn decode(bytes: &[u8]) -> Result<Vec<BlockHash>, StoreError> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("verification dependencies exceed byte limit"));
    }
    let sources: Vec<BlockHash> = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_BYTES)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(invalid)?;
    if sources.is_empty()
        || sources.len() > MAX_SOURCES
        || sources.iter().any(BlockHash::is_zero)
        || sources
            .windows(2)
            .any(|pair| pair[0].as_bytes() >= pair[1].as_bytes())
    {
        return Err(invalid("invalid verification dependency set"));
    }
    Ok(sources)
}

pub(super) fn persist(
    batch: &mut impl Ledger,
    transition: BlockHash,
    outcome: BlockHash,
    sources: &[BlockHash],
) -> Result<(), String> {
    if sources.len() > MAX_SOURCES {
        return Err("verification dependencies require incremental confirmation".into());
    }
    let mut sources = sources.to_vec();
    sources.sort_by_key(|hash| *hash.as_bytes());
    if !sources.contains(&transition) || !sources.contains(&outcome) {
        return Err("verification outcome omits its signed trigger or reward source".into());
    }
    let bytes = bincode::serialize(&sources).map_err(|error| error.to_string())?;
    decode(&bytes).map_err(|error| error.to_string())?;
    for key in [transition_key(transition), outcome_key(outcome)] {
        if batch
            .get_meta(&key)
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Err("verification outcome dependencies already exist".into());
        }
        batch
            .put_meta(&key, &bytes)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn identity_sources(
    batch: &impl DependencyView,
    account: &WalletAddress,
    bytes: &[u8],
) -> Result<Vec<BlockHash>, StoreError> {
    let hash = decode_source(bytes)?;
    let source: StateBlock = bincode::deserialize(&batch.get_block(&hash)?).map_err(invalid)?;
    if source.hash != hash
        || source.compute_hash() != hash
        || crate::block_economics::wallet_from_link(&source.link).as_ref() != Some(account)
    {
        return Err(invalid("verification identity source mismatch"));
    }
    match source.block_type {
        BlockType::Endorse => Ok(vec![hash]),
        BlockType::VerificationVote => {
            let bytes = batch
                .get_meta(&outcome_key(hash))?
                .ok_or_else(|| invalid("identity lacks complete outcome dependencies"))?;
            let outcome = decode(&bytes)?;
            if !outcome.contains(&hash) {
                return Err(invalid("verification identity dependency mismatch"));
            }
            Ok(outcome)
        }
        _ => Err(invalid("unsupported verification identity source")),
    }
}

/// Cached replacements have signed payloads, but no committed block journal.
pub(crate) fn voting_sources(
    batch: &impl DependencyView,
    block: &StateBlock,
) -> Result<Vec<BlockHash>, StoreError> {
    let mut sources = read(batch, block)?;
    match batch.get_block(&block.hash) {
        Ok(_) => {}
        Err(StoreError::NotFound(_)) => {
            if let Some(bytes) = batch.get_meta(&identity_key(&block.account))? {
                sources.extend(identity_sources(batch, &block.account, &bytes)?);
            }
        }
        Err(error) => return Err(error),
    }
    Ok(sources)
}

/// A lifecycle write must not pin another account's provisional branch. These
/// local read dependencies require independent finality, not the outcome's vote.
fn changed_account_sources(
    batch: &impl DependencyView,
    journal: &BlockUndo,
) -> Result<Vec<BlockHash>, StoreError> {
    let mut sources = Vec::new();
    for change in journal.changes.iter().filter(|change| {
        change.table == UndoTable::Accounts && change.key != journal.account.as_str().as_bytes()
    }) {
        let after: AccountInfo = bincode::deserialize(
            change
                .after
                .as_deref()
                .ok_or_else(|| invalid("lifecycle account deletion is unsupported"))?,
        )
        .map_err(invalid)?;
        if change.key != after.address.as_str().as_bytes() {
            return Err(invalid("lifecycle account key mismatch"));
        }
        let before: Option<AccountInfo> = change
            .before
            .as_deref()
            .map(bincode::deserialize)
            .transpose()
            .map_err(invalid)?;
        let Some(before) = before else {
            if !after.head.is_zero() || after.block_count != 0 {
                return Err(invalid("lifecycle cannot create another account's chain"));
            }
            continue;
        };
        if before.address != after.address
            || before.head != after.head
            || before.block_count != after.block_count
        {
            return Err(invalid("lifecycle cannot change another account's chain"));
        }
        if before.head.is_zero() {
            if before.block_count != 0 {
                return Err(invalid("lifecycle account has a missing head"));
            }
        } else {
            if before.block_count == 0
                || batch.block_height(&before.address, &before.head)? != before.block_count
            {
                return Err(invalid("lifecycle account head index mismatch"));
            }
            sources.push(before.head);
        }
    }
    Ok(sources)
}

pub(crate) fn read(
    batch: &impl DependencyView,
    block: &StateBlock,
) -> Result<Vec<BlockHash>, StoreError> {
    let key = transition_key(block.hash);
    let journal = batch.block_undo(&block.hash)?;
    if journal
        .as_ref()
        .is_some_and(|journal| journal.account != block.account)
    {
        return Err(invalid("lifecycle journal account mismatch"));
    }
    let recorded = |key: &str| -> Result<Option<Vec<u8>>, StoreError> {
        let stored = batch.get_meta(key)?;
        if let Some(journal) = &journal {
            let committed = journal
                .changes
                .iter()
                .find(|change| {
                    change.table == burst_store_lmdb::block_undo::UndoTable::Meta
                        && change.key == key.as_bytes()
                })
                .and_then(|change| change.after.as_ref());
            if committed != stored.as_ref() {
                return Err(invalid(
                    "verification dependencies differ from the committed transition",
                ));
            }
        }
        Ok(stored)
    };
    let stored = recorded(&key)?;
    let mut sources = match stored {
        Some(bytes) => {
            let mut sources = decode(&bytes)?;
            if !sources.contains(&block.hash) || block.block_type != BlockType::VerificationVote {
                return Err(invalid("verification transition dependency mismatch"));
            }
            sources.retain(|hash| *hash != block.hash);
            sources
        }
        None => Vec::new(),
    };
    if let Some(journal) = &journal {
        sources.extend(changed_account_sources(batch, journal)?);
    }
    if let Some(bytes) = recorded(&identity_binding_key(block.hash))? {
        sources.extend(identity_sources(batch, &block.account, &bytes)?);
    }
    if matches!(
        block.block_type,
        BlockType::Open | BlockType::Receive | BlockType::RejectReceive
    ) && !block.link.is_zero()
    {
        let source: StateBlock =
            bincode::deserialize(&batch.get_block(&block.link)?).map_err(invalid)?;
        if source.hash != block.link || source.compute_hash() != block.link {
            return Err(invalid("verification reward source hash mismatch"));
        }
        if source.block_type == BlockType::VerificationVote {
            let bytes = batch.get_meta(&outcome_key(block.link))?.ok_or_else(|| {
                invalid("verification reward lacks complete outcome dependencies")
            })?;
            let outcome = decode(&bytes)?;
            if !outcome.contains(&block.link) {
                return Err(invalid("verification reward dependency mismatch"));
            }
            sources.extend(outcome);
        }
    }
    sources.sort_by_key(|hash| *hash.as_bytes());
    sources.dedup();
    Ok(sources)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_outcome_dependencies_fail_closed() {
        for sources in [
            vec![],
            vec![BlockHash::ZERO],
            vec![BlockHash::new([1; 32]); 2],
            vec![BlockHash::new([2; 32]), BlockHash::new([1; 32])],
        ] {
            assert!(decode(&bincode::serialize(&sources).unwrap()).is_err());
        }
        assert!(decode(&u64::MAX.to_le_bytes()).is_err());
        assert!(decode(&vec![0; MAX_BYTES as usize + 1]).is_err());
        let mut bytes = bincode::serialize(&vec![BlockHash::new([1; 32])]).unwrap();
        bytes.push(0);
        assert!(decode(&bytes).is_err());
    }
}
