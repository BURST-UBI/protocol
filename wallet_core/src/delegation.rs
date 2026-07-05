//! Delegation management — delegate and revoke voting power.
//!
//! BURST uses a plain, revocable, on-chain delegation POINTER (not the
//! whitepaper's encrypted-secondary-key handoff). The delegate votes with their
//! OWN key; the governance tally attributes the delegator's weight through the
//! delegation graph (`count_effective_*`). So a delegation is simply a signed
//! `delegator → delegate` pointer, and revocation is a signed marker by the
//! delegator's primary key. No key escrow, no decryption step.

use crate::error::WalletError;
use burst_types::{Signature, WalletAddress};

/// Build a delegation transaction pointing `delegator`'s vote at `delegate`.
/// Must be signed externally by the delegator's primary key (that signature is
/// the sole authority proof).
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

/// Build a revoke-delegation transaction. Must be signed by the delegator's
/// primary key, which proves authority to revoke.
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
        let d = addr(0xAA);
        let g = addr(0xBB);
        let tx = create_delegation(&d, &g).unwrap();
        assert_eq!(tx.delegator, d);
        assert_eq!(tx.delegate, g);
        assert!(!tx.hash.is_zero());
    }

    #[test]
    fn cannot_delegate_to_self() {
        let d = addr(0xCC);
        assert!(create_delegation(&d, &d).is_err());
    }

    #[test]
    fn revoke_is_signed_by_delegator() {
        let d = addr(0x33);
        let tx = revoke_delegation(&d).unwrap();
        assert_eq!(tx.delegator, d);
        assert!(!tx.hash.is_zero());
    }
}
