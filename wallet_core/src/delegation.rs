//! Delegation management — delegate and revoke voting power.

use burst_types::WalletAddress;
use crate::error::WalletError;

/// Delegate voting power to a representative.
///
/// Generates a delegation key pair, encrypts the private key for the delegate,
/// and builds the delegation transaction.
pub fn create_delegation(
    _delegator: &WalletAddress,
    _delegate: &WalletAddress,
) -> Result<burst_transactions::delegate::DelegateTx, WalletError> {
    todo!("generate delegation key pair, encrypt private key, build DelegateTx")
}

/// Revoke a delegation.
pub fn revoke_delegation(
    _delegator: &WalletAddress,
) -> Result<burst_transactions::delegate::RevokeDelegationTx, WalletError> {
    todo!("generate new delegation public key, build RevokeDelegationTx")
}
