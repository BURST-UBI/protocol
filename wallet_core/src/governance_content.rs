//! Locally signed full-content proposal construction. Transport and admission
//! are separate; constructing this block does not submit or activate it.

use crate::{error::WalletError, transaction_builder::*};
use burst_governance::{content::EncodedContent, ProposalContent};
use burst_ledger::{BlockType, StateBlock};
use burst_types::{BlockHash, KeyPair, Timestamp, TxHash};

/// Return the signed commitment block together with its encoded proposal content.
/// The caller must retain and transmit both; the block alone cannot recover the
/// proposal text. Proof of work remains zero.
pub fn build_signed_proposal(
    state: &AccountState,
    keys: &KeyPair,
    content: &ProposalContent,
    params_hash: BlockHash,
    now: Timestamp,
) -> Result<(StateBlock, EncodedContent), WalletError> {
    if state.head.is_zero()
        || state.block_count == 0
        || state.payment_epoch.is_zero()
        || params_hash.is_zero()
    {
        return Err(WalletError::TransactionBuild(
            "proposal requires an open account and signing context".into(),
        ));
    }
    if burst_crypto::public_from_private(&keys.private) != keys.public {
        return Err(WalletError::TransactionBuild(
            "proposal keypair mismatch".into(),
        ));
    }
    let encoded = EncodedContent::new(content)
        .map_err(|error| WalletError::TransactionBuild(error.to_string()))?;
    let owner = burst_crypto::derive_address(&keys.public);
    let tx = burst_transactions::Transaction::ChangeRepresentative(build_change_rep_tx(
        &owner,
        &state.representative,
        now,
    )?);
    let mut block = build_state_block(state, &tx, TxHash::ZERO, params_hash)?;
    block.block_type = BlockType::GovernanceProposal;
    (block.link, block.origin, block.transaction) = encoded
        .manifest()
        .block_fields()
        .map_err(|error| WalletError::TransactionBuild(error.to_string()))?;
    block
        .validate_format()
        .map_err(|error| WalletError::TransactionBuild(error.into()))?;
    block.hash = block.compute_hash();
    Ok((sign_state_block(block, &keys.private), encoded))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposal_builder_rejects_missing_context_and_mismatched_keys() {
        let keys = burst_crypto::keypair_from_seed(&[1; 32]);
        let content = ProposalContent::ParameterChange {
            param: burst_governance::GovernableParam::BrnRate,
            new_value: 0,
        };
        for missing in 0..5 {
            let mut state = AccountState {
                signing_grant: burst_types::BlockHash::ZERO,
                head: BlockHash::new([1; 32]),
                block_count: 1,
                payment_epoch: BlockHash::new([2; 32]),
                representative: burst_crypto::derive_address(&keys.public),
                brn_spent: 7,
                trst_balance: 9,
                release_sources: Vec::new(),
            };
            let mut params = BlockHash::new([3; 32]);
            match missing {
                0 => state.head = BlockHash::ZERO,
                1 => state.block_count = 0,
                2 => state.payment_epoch = BlockHash::ZERO,
                3 => params = BlockHash::ZERO,
                _ => (),
            }
            let mut signer = burst_crypto::keypair_from_seed(&[1; 32]);
            if missing == 4 {
                signer.public = burst_crypto::keypair_from_seed(&[2; 32]).public;
            }
            assert!(
                build_signed_proposal(&state, &signer, &content, params, Timestamp::new(10))
                    .is_err()
            );
        }
    }
}
