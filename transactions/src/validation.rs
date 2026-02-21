//! Transaction validation logic.

use crate::error::TransactionError;
use crate::Transaction;
use burst_types::Timestamp;

/// Validate a transaction's basic structure (signature, timestamp, amounts).
///
/// This performs stateless validation only. Stateful checks (balance sufficiency,
/// wallet verification status, etc.) are done by the ledger/node.
pub fn validate_transaction(
    tx: &Transaction,
    now: Timestamp,
    time_tolerance_secs: u64,
) -> Result<(), TransactionError> {
    // Check timestamp is within tolerance
    let tx_timestamp = tx.timestamp();
    let tx_secs = tx_timestamp.as_secs();
    let now_secs = now.as_secs();
    let time_diff = if tx_secs > now_secs {
        tx_secs.saturating_sub(now_secs)
    } else {
        now_secs.saturating_sub(tx_secs)
    };

    if time_diff > time_tolerance_secs {
        return Err(TransactionError::InvalidTimestamp {
            reason: format!(
                "timestamp {} is {} seconds away from now {}, tolerance is {}",
                tx_timestamp, time_diff, now, time_tolerance_secs
            ),
        });
    }

    // Check amounts and call type-specific validators
    match tx {
        Transaction::Burn(burn_tx) => {
            if burn_tx.amount == 0 {
                return Err(TransactionError::ZeroAmount);
            }
            validate_burn(burn_tx, now)?;
        }
        Transaction::Send(send_tx) => {
            if send_tx.amount == 0 {
                return Err(TransactionError::ZeroAmount);
            }
            validate_send(send_tx, now)?;
        }
        Transaction::Split(split_tx) => {
            if split_tx.outputs.is_empty() {
                return Err(TransactionError::ZeroAmount);
            }
            for output in &split_tx.outputs {
                if output.amount == 0 {
                    return Err(TransactionError::ZeroAmount);
                }
            }
            validate_split(split_tx, now)?;
        }
        Transaction::Merge(merge_tx) => {
            validate_merge(merge_tx, now)?;
        }
        Transaction::Challenge(challenge_tx) => {
            if challenge_tx.stake_amount == 0 {
                return Err(TransactionError::ZeroAmount);
            }
        }
        Transaction::Endorse(endorse_tx) => {
            if endorse_tx.burn_amount == 0 {
                return Err(TransactionError::ZeroAmount);
            }
        }
        _ => {}
    }

    Ok(())
}

/// Validate a burn transaction specifically.
pub fn validate_burn(tx: &crate::burn::BurnTx, _now: Timestamp) -> Result<(), TransactionError> {
    if tx.amount == 0 {
        return Err(TransactionError::ZeroAmount);
    }

    // Sender must not equal receiver
    if tx.sender == tx.receiver {
        return Err(TransactionError::Other(
            "burn transaction sender and receiver must be different".into(),
        ));
    }

    Ok(())
}

/// Validate a send transaction specifically.
pub fn validate_send(tx: &crate::send::SendTx, _now: Timestamp) -> Result<(), TransactionError> {
    if tx.amount == 0 {
        return Err(TransactionError::ZeroAmount);
    }

    // Sender must not equal receiver
    if tx.sender == tx.receiver {
        return Err(TransactionError::Other(
            "send transaction sender and receiver must be different".into(),
        ));
    }

    // Origin hash must not be zero
    if tx.origin.is_zero() {
        return Err(TransactionError::Other(
            "send transaction origin hash must not be zero".into(),
        ));
    }

    // Link hash must not be zero
    if tx.link.is_zero() {
        return Err(TransactionError::Other(
            "send transaction link hash must not be zero".into(),
        ));
    }

    Ok(())
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

    // Parent hash must not be zero
    if tx.parent_hash.is_zero() {
        return Err(TransactionError::Other(
            "split transaction parent hash must not be zero".into(),
        ));
    }

    // Origin hash must not be zero
    if tx.origin.is_zero() {
        return Err(TransactionError::Other(
            "split transaction origin hash must not be zero".into(),
        ));
    }

    Ok(())
}

/// Validate a merge transaction.
pub fn validate_merge(tx: &crate::merge::MergeTx, _now: Timestamp) -> Result<(), TransactionError> {
    if tx.source_hashes.len() < 2 {
        return Err(TransactionError::Other(
            "merge requires at least 2 sources".into(),
        ));
    }

    // All source hashes must be unique (no duplicates)
    let mut seen = std::collections::HashSet::new();
    for hash in &tx.source_hashes {
        if !seen.insert(hash) {
            return Err(TransactionError::Other(
                "merge transaction source hashes must be unique".into(),
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{burn::BurnTx, merge::MergeTx, send::SendTx, split::SplitTx};
    use burst_types::{Signature, Timestamp, TxHash, WalletAddress};

    fn dummy_wallet_address() -> WalletAddress {
        WalletAddress::new("brst_1111111111111111111111111111111111111111")
    }

    fn dummy_wallet_address_2() -> WalletAddress {
        WalletAddress::new("brst_2222222222222222222222222222222222222222")
    }

    fn dummy_tx_hash() -> TxHash {
        TxHash::new([1u8; 32])
    }

    fn dummy_tx_hash_2() -> TxHash {
        TxHash::new([2u8; 32])
    }

    fn dummy_signature() -> Signature {
        Signature([0u8; 64])
    }

    #[test]
    fn test_validate_transaction_timestamp_too_old() {
        let now = Timestamp::new(1000);
        let tx = Transaction::Burn(BurnTx {
            hash: dummy_tx_hash(),
            sender: dummy_wallet_address(),
            receiver: dummy_wallet_address_2(),
            amount: 100,
            timestamp: Timestamp::new(500), // 500 seconds in the past
            work: 0,
            signature: dummy_signature(),
        });

        let result = validate_transaction(&tx, now, 100);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            TransactionError::InvalidTimestamp { .. }
        ));
    }

    #[test]
    fn test_validate_transaction_timestamp_too_future() {
        let now = Timestamp::new(1000);
        let tx = Transaction::Burn(BurnTx {
            hash: dummy_tx_hash(),
            sender: dummy_wallet_address(),
            receiver: dummy_wallet_address_2(),
            amount: 100,
            timestamp: Timestamp::new(1200), // 200 seconds in the future
            work: 0,
            signature: dummy_signature(),
        });

        let result = validate_transaction(&tx, now, 100);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            TransactionError::InvalidTimestamp { .. }
        ));
    }

    #[test]
    fn test_validate_transaction_timestamp_within_tolerance() {
        let now = Timestamp::new(1000);
        let tx = Transaction::Burn(BurnTx {
            hash: dummy_tx_hash(),
            sender: dummy_wallet_address(),
            receiver: dummy_wallet_address_2(),
            amount: 100,
            timestamp: Timestamp::new(1050), // 50 seconds in the future, within 100s tolerance
            work: 0,
            signature: dummy_signature(),
        });

        let result = validate_transaction(&tx, now, 100);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_burn_zero_amount() {
        let tx = BurnTx {
            hash: dummy_tx_hash(),
            sender: dummy_wallet_address(),
            receiver: dummy_wallet_address_2(),
            amount: 0,
            timestamp: Timestamp::new(1000),
            work: 0,
            signature: dummy_signature(),
        };

        let result = validate_burn(&tx, Timestamp::new(1000));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TransactionError::ZeroAmount));
    }

    #[test]
    fn test_validate_burn_sender_equals_receiver() {
        let sender = dummy_wallet_address();
        let tx = BurnTx {
            hash: dummy_tx_hash(),
            sender: sender.clone(),
            receiver: sender, // Same as sender
            amount: 100,
            timestamp: Timestamp::new(1000),
            work: 0,
            signature: dummy_signature(),
        };

        let result = validate_burn(&tx, Timestamp::new(1000));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TransactionError::Other(_)));
    }

    #[test]
    fn test_validate_burn_valid() {
        let tx = BurnTx {
            hash: dummy_tx_hash(),
            sender: dummy_wallet_address(),
            receiver: dummy_wallet_address_2(),
            amount: 100,
            timestamp: Timestamp::new(1000),
            work: 0,
            signature: dummy_signature(),
        };

        let result = validate_burn(&tx, Timestamp::new(1000));
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_send_zero_amount() {
        let tx = SendTx {
            hash: dummy_tx_hash(),
            sender: dummy_wallet_address(),
            receiver: dummy_wallet_address_2(),
            amount: 0,
            timestamp: Timestamp::new(1000),
            link: dummy_tx_hash(),
            origin: dummy_tx_hash(),
            work: 0,
            signature: dummy_signature(),
        };

        let result = validate_send(&tx, Timestamp::new(1000));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TransactionError::ZeroAmount));
    }

    #[test]
    fn test_validate_send_sender_equals_receiver() {
        let sender = dummy_wallet_address();
        let tx = SendTx {
            hash: dummy_tx_hash(),
            sender: sender.clone(),
            receiver: sender, // Same as sender
            amount: 100,
            timestamp: Timestamp::new(1000),
            link: dummy_tx_hash(),
            origin: dummy_tx_hash(),
            work: 0,
            signature: dummy_signature(),
        };

        let result = validate_send(&tx, Timestamp::new(1000));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TransactionError::Other(_)));
    }

    #[test]
    fn test_validate_send_zero_origin() {
        let tx = SendTx {
            hash: dummy_tx_hash(),
            sender: dummy_wallet_address(),
            receiver: dummy_wallet_address_2(),
            amount: 100,
            timestamp: Timestamp::new(1000),
            link: dummy_tx_hash(),
            origin: TxHash::ZERO,
            work: 0,
            signature: dummy_signature(),
        };

        let result = validate_send(&tx, Timestamp::new(1000));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TransactionError::Other(_)));
    }

    #[test]
    fn test_validate_send_zero_link() {
        let tx = SendTx {
            hash: dummy_tx_hash(),
            sender: dummy_wallet_address(),
            receiver: dummy_wallet_address_2(),
            amount: 100,
            timestamp: Timestamp::new(1000),
            link: TxHash::ZERO,
            origin: dummy_tx_hash(),
            work: 0,
            signature: dummy_signature(),
        };

        let result = validate_send(&tx, Timestamp::new(1000));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TransactionError::Other(_)));
    }

    #[test]
    fn test_validate_send_valid() {
        let tx = SendTx {
            hash: dummy_tx_hash(),
            sender: dummy_wallet_address(),
            receiver: dummy_wallet_address_2(),
            amount: 100,
            timestamp: Timestamp::new(1000),
            link: dummy_tx_hash(),
            origin: dummy_tx_hash(),
            work: 0,
            signature: dummy_signature(),
        };

        let result = validate_send(&tx, Timestamp::new(1000));
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_split_zero_parent_hash() {
        let tx = SplitTx {
            hash: dummy_tx_hash(),
            sender: dummy_wallet_address(),
            timestamp: Timestamp::new(1000),
            parent_hash: TxHash::ZERO,
            origin: dummy_tx_hash(),
            outputs: vec![crate::split::SplitOutput {
                receiver: dummy_wallet_address_2(),
                amount: 100,
            }],
            work: 0,
            signature: dummy_signature(),
        };

        let result = validate_split(&tx, Timestamp::new(1000));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TransactionError::Other(_)));
    }

    #[test]
    fn test_validate_split_zero_origin() {
        let tx = SplitTx {
            hash: dummy_tx_hash(),
            sender: dummy_wallet_address(),
            timestamp: Timestamp::new(1000),
            parent_hash: dummy_tx_hash(),
            origin: TxHash::ZERO,
            outputs: vec![crate::split::SplitOutput {
                receiver: dummy_wallet_address_2(),
                amount: 100,
            }],
            work: 0,
            signature: dummy_signature(),
        };

        let result = validate_split(&tx, Timestamp::new(1000));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TransactionError::Other(_)));
    }

    #[test]
    fn test_validate_split_valid() {
        let tx = SplitTx {
            hash: dummy_tx_hash(),
            sender: dummy_wallet_address(),
            timestamp: Timestamp::new(1000),
            parent_hash: dummy_tx_hash(),
            origin: dummy_tx_hash(),
            outputs: vec![crate::split::SplitOutput {
                receiver: dummy_wallet_address_2(),
                amount: 100,
            }],
            work: 0,
            signature: dummy_signature(),
        };

        let result = validate_split(&tx, Timestamp::new(1000));
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_merge_duplicate_hashes() {
        let hash1 = dummy_tx_hash();
        let tx = MergeTx {
            hash: dummy_tx_hash(),
            sender: dummy_wallet_address(),
            timestamp: Timestamp::new(1000),
            source_hashes: vec![hash1, hash1], // Duplicate
            work: 0,
            signature: dummy_signature(),
        };

        let result = validate_merge(&tx, Timestamp::new(1000));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TransactionError::Other(_)));
    }

    #[test]
    fn test_validate_merge_valid() {
        let tx = MergeTx {
            hash: dummy_tx_hash(),
            sender: dummy_wallet_address(),
            timestamp: Timestamp::new(1000),
            source_hashes: vec![dummy_tx_hash(), dummy_tx_hash_2()],
            work: 0,
            signature: dummy_signature(),
        };

        let result = validate_merge(&tx, Timestamp::new(1000));
        assert!(result.is_ok());
    }
}
