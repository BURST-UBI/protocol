//! Delegation management — delegate and revoke voting power.
//!
//! Pointer builders return unsigned transaction drafts. Scoped key operations
//! produce signed account blocks: the owner carries grants and revocations;
//! the recipient carries secondary-signed ballots on their own account chain.

use crate::error::WalletError;
pub use burst_governance::signing_delegation::view;
use burst_types::{Signature, WalletAddress};

mod client;
pub(crate) use client::{build_delegated_ballot, build_scoped_operation, require_review};

/// Build and sign the account-block envelope for a scoped operation.
/// Work remains zero. Grant history, scope eligibility and network admission
/// still require node validation.
pub fn build_key_operation(
    state: &crate::transaction_builder::AccountState,
    keys: &burst_types::KeyPair,
    operation: &burst_governance::signing_delegation::operation::DelegationOperation,
    params_hash: burst_types::BlockHash,
    now: burst_types::Timestamp,
) -> Result<burst_ledger::StateBlock, WalletError> {
    use crate::transaction_builder::*;
    use burst_governance::signing_delegation::operation::DelegationOperation;
    let owner = burst_crypto::derive_address(&keys.public);
    if operation
        .identity()
        .is_some_and(|identity| identity.2 != &owner)
        || burst_crypto::public_from_private(&keys.private) != keys.public
        || state.head.is_zero()
        || state.payment_epoch.is_zero()
        || params_hash.is_zero()
    {
        return Err(WalletError::TransactionBuild(
            "delegation owner or signing context mismatch".into(),
        ));
    }
    let tx = burst_transactions::Transaction::ChangeRepresentative(build_change_rep_tx(
        &owner,
        &state.representative,
        now,
    )?);
    let mut block = build_state_block(state, &tx, burst_types::TxHash::ZERO, params_hash)?;
    (block.block_type, block.link) = match operation {
        DelegationOperation::Grant(grant) => (
            burst_ledger::BlockType::Delegate,
            burst_types::BlockHash::new(grant.delegate.0),
        ),
        DelegationOperation::Revoke(_) => (
            burst_ledger::BlockType::RevokeDelegation,
            burst_types::BlockHash::ZERO,
        ),
        DelegationOperation::Ballot { ballot, .. } => {
            let mut choice = [0; 32];
            choice[0] = match ballot.vote {
                burst_transactions::governance::GovernanceVote::Yea => 0,
                burst_transactions::governance::GovernanceVote::Nay => 1,
                burst_transactions::governance::GovernanceVote::Abstain => 2,
            };
            block.transaction = burst_types::TxHash::new(choice);
            (
                burst_ledger::BlockType::GovernanceVote,
                burst_types::BlockHash::new(*ballot.proposal.as_bytes()),
            )
        }
    };
    block.delegation_payload = operation.encode().map_err(WalletError::TransactionBuild)?;
    block
        .validate_format()
        .map_err(|error| WalletError::TransactionBuild(error.into()))?;
    block.hash = block.compute_hash();
    Ok(sign_state_block(block, &keys.private))
}

/// Build an unsigned pointer-delegation draft using the local clock.
/// Rejects self-delegation; it does not validate addresses or ledger eligibility.
pub fn create_delegation(
    delegator: &WalletAddress,
    delegate: &WalletAddress,
) -> Result<burst_transactions::delegate::DelegateTx, WalletError> {
    if delegator == delegate {
        return Err(WalletError::TransactionBuild(
            "cannot delegate to self".to_string(),
        ));
    }
    let now = burst_types::Timestamp::now();
    let tx_bytes = burst_crypto::blake2b_256_multi(&[
        b"delegate",
        delegator.as_str().as_bytes(),
        delegate.as_str().as_bytes(),
        &now.as_secs().to_le_bytes(),
    ]);
    let hash = burst_types::TxHash::new(tx_bytes);

    Ok(burst_transactions::delegate::DelegateTx {
        hash,
        delegator: delegator.clone(),
        delegate: delegate.clone(),
        timestamp: now,
        work: 0,
        signature: Signature([0u8; 64]),
    })
}

/// Build an unsigned pointer-revocation draft using the local clock.
pub fn revoke_delegation(
    delegator: &WalletAddress,
) -> Result<burst_transactions::delegate::RevokeDelegationTx, WalletError> {
    let now = burst_types::Timestamp::now();
    let tx_bytes = burst_crypto::blake2b_256_multi(&[
        b"revoke-delegation",
        delegator.as_str().as_bytes(),
        &now.as_secs().to_le_bytes(),
    ]);
    let hash = burst_types::TxHash::new(tx_bytes);

    Ok(burst_transactions::delegate::RevokeDelegationTx {
        hash,
        delegator: delegator.clone(),
        timestamp: now,
        work: 0,
        signature: Signature([0u8; 64]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_crypto::{derive_address, keypair_from_seed};

    fn addr(seed: u8) -> WalletAddress {
        derive_address(&keypair_from_seed(&[seed; 32]).public)
    }

    #[test]
    fn delegation_points_delegator_at_delegate() {
        let delegator = addr(0xAA);
        let delegate = addr(0xBB);
        let tx = create_delegation(&delegator, &delegate).unwrap();
        assert_eq!(tx.delegator, delegator);
        assert_eq!(tx.delegate, delegate);
        assert!(!tx.hash.is_zero());
    }

    #[test]
    fn cannot_delegate_to_self() {
        let d = addr(0xCC);
        assert!(create_delegation(&d, &d).is_err());
    }

    #[test]
    fn revocation_identifies_delegator() {
        let d = addr(0x33);
        let tx = revoke_delegation(&d).unwrap();
        assert_eq!(tx.delegator, d);
        assert!(!tx.hash.is_zero());
    }
}
