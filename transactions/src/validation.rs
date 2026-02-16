//! Transaction validation logic.

use crate::error::TransactionError;
use crate::Transaction;
use burst_types::Timestamp;

/// Validate a transaction's basic structure (signature, timestamp, amounts).
///
/// This performs stateless validation only. Stateful checks (balance sufficiency,
/// wallet verification status, etc.) are done by the ledger/node.
pub fn validate_transaction(
    _tx: &Transaction,
    _now: Timestamp,
    _time_tolerance_secs: u64,
) -> Result<(), TransactionError> {
    todo!("validate signature, timestamp within tolerance, amounts > 0, etc.")
}

/// Validate a burn transaction specifically.
pub fn validate_burn(tx: &crate::burn::BurnTx, _now: Timestamp) -> Result<(), TransactionError> {
    if tx.amount == 0 {
        return Err(TransactionError::ZeroAmount);
    }
    todo!("verify signature against sender's public key")
}

/// Validate a send transaction specifically.
pub fn validate_send(tx: &crate::send::SendTx, _now: Timestamp) -> Result<(), TransactionError> {
    if tx.amount == 0 {
        return Err(TransactionError::ZeroAmount);
    }
    todo!("verify signature, check origin exists, check link exists")
}

/// Validate a split transaction.
pub fn validate_split(tx: &crate::split::SplitTx, _now: Timestamp) -> Result<(), TransactionError> {
    if tx.outputs.is_empty() {
        return Err(TransactionError::ZeroAmount);
    }
    for output in &tx.outputs {
        if output.amount == 0 {
            return Err(TransactionError::ZeroAmount);
        }
    }
    todo!("verify amounts sum to parent, verify signature")
}

/// Validate a merge transaction.
pub fn validate_merge(tx: &crate::merge::MergeTx, _now: Timestamp) -> Result<(), TransactionError> {
    if tx.source_hashes.is_empty() {
        return Err(TransactionError::Other("merge must have at least one source".into()));
    }
    todo!("verify all sources exist, are transferable, belong to sender")
}
