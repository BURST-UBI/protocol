//! Stateless payload checks used before transaction-to-block conversion.

use crate::error::TransactionError;
use crate::Transaction;
use burst_types::Timestamp;

/// Validate a transaction's timestamp and basic structure.
///
/// Timestamp tolerance is symmetric and inclusive. Variant checks cover only
/// the cases below; this does not validate every field, signature, hash or work
/// nonce. Node admission checks state, eligibility and the resulting block.
pub fn validate_transaction_structure(
    tx: &Transaction,
    now: Timestamp,
    time_tolerance_secs: u64,
) -> Result<(), TransactionError> {
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
        Transaction::Merge(merge_tx) => {
            validate_merge(merge_tx, now)?;
        }
        Transaction::Challenge(challenge_tx) if challenge_tx.stake_amount == 0 => {
            return Err(TransactionError::ZeroAmount);
        }
        Transaction::Challenge(_) => {}
        Transaction::Endorse(endorse_tx) if endorse_tx.burn_amount == 0 => {
            return Err(TransactionError::ZeroAmount);
        }
        Transaction::Endorse(_) => {}
        _ => {}
    }

    Ok(())
}

/// Require a positive amount and different sender and recipient.
/// The time argument is unused; timestamp checks belong to the outer validator.
pub fn validate_burn(tx: &crate::burn::BurnTx, _now: Timestamp) -> Result<(), TransactionError> {
    if tx.amount == 0 {
        return Err(TransactionError::ZeroAmount);
    }

    if tx.sender == tx.receiver {
        return Err(TransactionError::Other(
            "burn transaction sender and receiver must be different".into(),
        ));
    }

    Ok(())
}

/// Require a positive amount and nonzero provenance references. Self-sends are
/// allowed. This does not resolve those references or check available TRST.
pub fn validate_send(tx: &crate::send::SendTx, _now: Timestamp) -> Result<(), TransactionError> {
    if tx.amount == 0 {
        return Err(TransactionError::ZeroAmount);
    }

    if tx.origin.is_zero() {
        return Err(TransactionError::Other(
            "send transaction origin hash must not be zero".into(),
        ));
    }

    if tx.link.is_zero() {
        return Err(TransactionError::Other(
            "send transaction link hash must not be zero".into(),
        ));
    }

    Ok(())
}

/// Require 2–MAX_MERGE_SOURCES distinct references. This does not resolve them,
/// reject a zero reference or check ownership, expiry and spendability.
pub fn validate_merge(tx: &crate::merge::MergeTx, _now: Timestamp) -> Result<(), TransactionError> {
    if tx.source_hashes.len() < 2 {
        return Err(TransactionError::Other(
            "merge requires at least 2 sources".into(),
        ));
    }

    if tx.source_hashes.len() > burst_trst::MAX_MERGE_SOURCES {
        return Err(TransactionError::Other(format!(
            "merge cannot have more than {} sources, got {}",
            burst_trst::MAX_MERGE_SOURCES,
            tx.source_hashes.len()
        )));
    }

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
    use crate::{burn::BurnTx, merge::MergeTx, send::SendTx};
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

        let result = validate_transaction_structure(&tx, now, 100);
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

        let result = validate_transaction_structure(&tx, now, 100);
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

        let result = validate_transaction_structure(&tx, now, 100);
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
    fn test_validate_send_to_self() {
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
        assert!(result.is_ok(), "decision 6.7 permits TRST self-sends");
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
