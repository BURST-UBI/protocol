//! Full proposal content bound to an account signature. This authenticates
//! bytes, not governance eligibility or consensus. The admission transaction
//! must independently check account state, epoch, time, work and balances.

use burst_governance::{content::ContentManifest, ProposalContent};
use burst_ledger::{BlockType, StateBlock};
use burst_store_lmdb::LmdbWriteTransaction;
use burst_types::{BlockHash, PublicKey};
pub mod transfer;

pub(crate) fn enabled(network: burst_types::NetworkId) -> bool {
    cfg!(test) || crate::release_policy::governance_content(network)
}

pub(crate) fn is_full(block: &StateBlock) -> bool {
    block.block_type == BlockType::GovernanceProposal
        && block.origin.as_bytes() == &burst_governance::content::FULL_CONTENT_MARKER
}

pub(crate) fn key(hash: BlockHash) -> String {
    format!("governance_content/v1/{hash}")
}

/// Authenticate the block and extract its content commitment. Call
/// `decode_verified` on the returned manifest to validate the content bytes.
pub fn authenticate(block: &StateBlock) -> Result<ContentManifest, String> {
    block.validate_format().map_err(str::to_owned)?;
    if block.block_type != BlockType::GovernanceProposal
        || block.previous.is_zero()
        || block.payment_epoch.is_zero()
        || block.params_hash.is_zero()
        || block.hash != block.compute_hash()
    {
        return Err("invalid full-content proposal block".into());
    }
    let public =
        burst_crypto::decode_address(block.account.as_str()).ok_or("invalid proposal signer")?;
    if !burst_crypto::verify_signature(block.hash.as_bytes(), &block.signature, &PublicKey(public))
    {
        return Err("invalid proposal content signature".into());
    }
    ContentManifest::from_block_fields(block.link, block.origin, block.transaction)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "proposal has no full-content commitment".into())
}

/// Called inside the same writer as the account block and governance state.
/// Never persist unbound chunks through this entry point. A block-specific key
/// avoids rollback of one proposal deleting content needed by another proposal.
/// This stages writes only; the caller commits or aborts the transaction.
pub fn stage(
    batch: &mut LmdbWriteTransaction<'_>,
    block: &StateBlock,
    bytes: &[u8],
) -> Result<(), String> {
    authenticate(block)?
        .decode_verified(bytes)
        .map_err(|error| error.to_string())?;
    let account = batch
        .get_account(&block.account)
        .map_err(|error| error.to_string())?;
    if batch.payment_epoch().map_err(|error| error.to_string())? != block.payment_epoch
        || account.head != block.hash
        || batch
            .get_block(&block.hash)
            .map_err(|error| error.to_string())?
            != bincode::serialize(block).map_err(|error| error.to_string())?
    {
        return Err("proposal content must share its block admission transaction".into());
    }
    batch
        .put_block_meta(
            &block.hash,
            &block.account,
            account.block_count,
            &key(block.hash),
            bytes,
        )
        .map_err(|error| error.to_string())
}

pub(crate) fn load(
    batch: &LmdbWriteTransaction<'_>,
    block: &StateBlock,
) -> Result<ProposalContent, String> {
        load_view(&batch.read_view(), block)
    }

    pub(crate) fn load_view(
    batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    block: &StateBlock,
) -> Result<ProposalContent, String> {
    let manifest = authenticate(block)?;
    let bytes = match batch
        .get_meta(&key(block.hash))
        .map_err(|error| error.to_string())?
    {
        Some(bytes) => bytes,
        None => batch
            .get_meta(&crate::confirmed_history::proposal_archive_key(block.hash))
            .map_err(|error| error.to_string())?
            .ok_or("full proposal content is missing")?,
    };
    manifest
        .decode_verified(&bytes)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests;
