//! Offline payment construction. Contexts contain public, node-supplied state;
//! they are not proofs of balances or authority. Keep amounts in Rust when
//! serializing contexts: JSON numbers must not round-trip through JavaScript.
use crate::{signed_client::PreparedWork, transaction_builder::*, WalletError};
use burst_ledger::{BlockType, StateBlock};
use burst_transactions::Transaction;
use burst_types::{BlockHash, KeyPair, TxHash, WalletAddress};

mod action;
pub use action::{OfflineAction, OfflineActionContext, SignedOfflineAction};

#[cfg(not(target_arch = "wasm32"))]
mod storage;
#[cfg(not(target_arch = "wasm32"))]
pub use storage::load_action_with_content;
#[cfg(not(target_arch = "wasm32"))]
pub use storage::migrate_account_named_journal;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use storage::record_submission_attempt;
#[cfg(not(target_arch = "wasm32"))]
pub use storage::{archive_payment, load_payment, payment_may_have_been_submitted, save_payment};
#[cfg(not(target_arch = "wasm32"))]
pub use storage::{load_payment_with_ancestry, save_payment_with_ancestry, save_signed_action};

pub(crate) const MAX_ANCESTRY_BYTES: usize = 256 * 1024;
pub(crate) const MAX_ANCESTRY_BLOCKS: usize = 256;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct OfflinePaymentContext {
    #[serde(default)]
    pub(crate) ancestry: Vec<StateBlock>,
    pub(crate) account: WalletAddress,
    pub(crate) state: AccountState,
    pub(crate) params_hash: BlockHash,
    pub(crate) threshold: u64,
    pub(crate) work: PreparedWork,
}

#[derive(serde::Serialize)]
pub struct OfflinePaymentReview {
    pub kind: &'static str,
    pub hash: String,
    pub provider: String,
    pub amount: String,
    pub payment: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct Receipt {
    pub source: TxHash,
    pub origin: TxHash,
    pub amount: u128,
    pub sender: WalletAddress,
}

impl Receipt {
    pub(crate) fn transaction(
        &self,
        account: &WalletAddress,
        timestamp: burst_types::Timestamp,
    ) -> Result<Transaction, WalletError> {
        Ok(Transaction::Receive(build_receive_tx(
            account,
            self.source,
            self.amount,
            timestamp,
        )?))
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct OfflineReceiveContext {
    pub(crate) signing: OfflinePaymentContext,
    pub(crate) receipt: Receipt,
}

impl OfflineReceiveContext {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn save_payment(
        &self,
        path: &std::path::Path,
        block: &StateBlock,
    ) -> Result<(), WalletError> {
        self.signing.save_payment(path, block)
    }
    pub fn sign(
        &self,
        keys: &KeyPair,
        timestamp: burst_types::Timestamp,
    ) -> Result<StateBlock, WalletError> {
        self.signing.sign(
            keys,
            &self.receipt.transaction(&self.signing.account, timestamp)?,
            self.receipt.origin,
        )
    }

    pub fn review(&self) -> OfflinePaymentReview {
        OfflinePaymentReview {
            kind: "receive",
            hash: String::new(),
            provider: self.receipt.sender.to_string(),
            amount: self.receipt.amount.to_string(),
            payment: Some(self.receipt.source.to_string()),
        }
    }
}

fn payment_kind(kind: &BlockType) -> bool {
    matches!(
        kind,
        BlockType::Burn
            | BlockType::Send
            | BlockType::Merge
            | BlockType::Receive
            | BlockType::Open
            | BlockType::RejectReceive
            | BlockType::ChangeRepresentative
    )
}

impl OfflinePaymentContext {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn save_payment(
        &self,
        path: &std::path::Path,
        block: &StateBlock,
    ) -> Result<(), WalletError> {
        if block.account != self.account || block.previous != self.state.head {
            return Err(WalletError::Signing(
                "saved payment differs from its prepared account position".into(),
            ));
        }
        storage::save_payment_with_ancestry(path, block, &self.ancestry)
    }
    /// Sign using the supplied context, without a node client or clock.
    /// The caller reviews the payment and chooses its timestamp. Work is checked
    /// against the context's threshold, not an independently verified network value.
    pub fn sign(
        &self,
        keys: &KeyPair,
        transaction: &Transaction,
        origin: TxHash,
    ) -> Result<StateBlock, WalletError> {
        validate_ancestry(&self.account, self.state.head, &self.ancestry)?;
        if burst_crypto::derive_address(&keys.public) != self.account
            || transaction.sender() != &self.account
        {
            return Err(WalletError::Signing(
                "offline context, transaction and signing key must name the same account".into(),
            ));
        }
        let amount = match transaction {
            Transaction::Burn(tx) => Some(tx.amount),
            Transaction::Send(tx) => Some(tx.amount),
            Transaction::Receive(tx) => Some(tx.amount),
            _ => None,
        };
        if amount == Some(0) {
            return Err(WalletError::TransactionBuild(
                "amount must be positive".into(),
            ));
        }
        let mut block = build_state_block(&self.state, transaction, origin, self.params_hash)?;
        if !payment_kind(&block.block_type) {
            return Err(WalletError::TransactionBuild(
                "this operation needs its own governance or identity signing context".into(),
            ));
        }
        if self.state.head.is_zero() && matches!(transaction, Transaction::Receive(_)) {
            block.block_type = BlockType::Open;
            block.hash = block.compute_hash();
        }
        if !self.work.apply_to(&mut block, self.threshold) {
            return Err(WalletError::TransactionBuild(
                "offline context has invalid work".into(),
            ));
        }
        block
            .validate_format()
            .map_err(|error| WalletError::TransactionBuild(error.into()))?;
        Ok(sign_state_block(block, &keys.private))
    }
}

pub(crate) fn validate_signed_payment(block: &StateBlock) -> Result<(), WalletError> {
    if !payment_kind(&block.block_type) {
        return Err(WalletError::Signing("invalid offline payment kind".into()));
    }
    validate_signed_block(block)
}

pub(crate) fn validate_signed_action(block: &StateBlock) -> Result<(), WalletError> {
    if matches!(
        block.block_type,
        BlockType::Epoch | BlockType::GovernanceActivation
    ) || block.payment_epoch.is_zero()
        || block.params_hash.is_zero()
    {
        return Err(WalletError::Signing(
            "invalid wallet action recovery context".into(),
        ));
    }
    validate_signed_block(block)
}

/// Check version, format, stored hash and account-key signature. Does not check
/// proof of work, ledger eligibility or confirmation.
pub(crate) fn validate_signed_block(block: &StateBlock) -> Result<(), WalletError> {
    let public = burst_crypto::decode_address(block.account.as_str())
        .ok_or_else(|| WalletError::Signing("invalid offline payment account".into()))?;
    if block.version != burst_ledger::CURRENT_BLOCK_VERSION
        || block.validate_format().is_err()
        || block.hash != block.compute_hash()
        || !burst_crypto::verify_signature(
            block.hash.as_bytes(),
            &block.signature,
            &burst_types::PublicKey(public),
        )
    {
        return Err(WalletError::Signing(
            "invalid offline payment signature or format".into(),
        ));
    }
    Ok(())
}

/// Check a bounded chain segment, newest first, beginning at `previous`.
/// Empty or partial ancestry is allowed; this is not a proof of confirmation
/// or a requirement to reach the account's opening block.
pub(crate) fn validate_ancestry(
    account: &WalletAddress,
    mut previous: BlockHash,
    ancestry: &[StateBlock],
) -> Result<(), WalletError> {
    if ancestry.len() > MAX_ANCESTRY_BLOCKS
        || serde_json::to_vec(ancestry)
            .map_err(|e| WalletError::Other(e.to_string()))?
            .len()
            > MAX_ANCESTRY_BYTES
    {
        return Err(WalletError::Signing(
            "offline recovery ancestry exceeds its size limit".into(),
        ));
    }
    for ancestor in ancestry {
        validate_signed_block(ancestor)?;
        if previous.is_zero() || ancestor.hash != previous || &ancestor.account != account {
            return Err(WalletError::Signing(
                "offline recovery ancestry does not match the saved account chain".into(),
            ));
        }
        previous = ancestor.previous;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::Timestamp;

    pub(super) fn context(keys: &KeyPair) -> OfflinePaymentContext {
        let account = burst_crypto::derive_address(&keys.public);
        let head = BlockHash::new([7; 32]);
        OfflinePaymentContext {
            ancestry: Vec::new(),
            account: account.clone(),
            state: AccountState {
                signing_grant: burst_types::BlockHash::ZERO,
                payment_epoch: BlockHash::new([8; 32]),
                release_sources: vec![],
                head,
                block_count: 1,
                representative: account.clone(),
                brn_spent: 11,
                trst_balance: 20,
            },
            params_hash: BlockHash::new([9; 32]),
            threshold: 0,
            work: PreparedWork {
                root: StateBlock::work_root_for(&account, head),
                nonce: 0,
            },
        }
    }

    #[test]
    fn offline_payment_roundtrip_preserves_reviewed_amount_and_signature() {
        let keys = burst_crypto::keypair_from_seed(&[1; 32]);
        let prepared = context(&keys);
        let encoded = serde_json::to_vec(&prepared).unwrap();
        let restored: OfflinePaymentContext = serde_json::from_slice(&encoded).unwrap();
        let recipient =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[2; 32]).public);
        // Choose the transaction after exporting the context; signing needs no node.
        let tx = Transaction::Burn(
            build_burn_tx(&prepared.account, &recipient, 7, Timestamp::new(42)).unwrap(),
        );
        let block = restored.sign(&keys, &tx, TxHash::ZERO).unwrap();
        assert_eq!(block.previous, prepared.state.head);
        assert_eq!(block.brn_spent, 18);
        let signed_json = serde_json::to_value(&block).unwrap();
        assert_eq!(signed_json["brn_balance"], 18);
        assert!(signed_json.get("brn_spent").is_none());
        let context_json = serde_json::to_value(&prepared).unwrap();
        assert_eq!(context_json["state"]["brn_balance"], 11);
        assert!(context_json["state"].get("brn_spent").is_none());
        assert_eq!(block.trst_balance, 20);
        assert_eq!(block.timestamp, Timestamp::new(42));
        assert_eq!(
            block.link.as_bytes(),
            &burst_crypto::decode_address(recipient.as_str()).unwrap()
        );
        validate_signed_payment(&block).unwrap();
        let mut tampered = block;
        tampered.brn_spent += 1;
        assert!(validate_signed_payment(&tampered).is_err());
    }

    #[test]
    fn offline_context_refuses_other_signers_work_roots_and_identity_operations() {
        let keys = burst_crypto::keypair_from_seed(&[1; 32]);
        let mut prepared = context(&keys);
        let tx = Transaction::Burn(
            build_burn_tx(&prepared.account, &prepared.account, 7, Timestamp::new(42)).unwrap(),
        );
        assert!(prepared
            .sign(
                &burst_crypto::keypair_from_seed(&[2; 32]),
                &tx,
                TxHash::ZERO
            )
            .is_err());
        let identity = Transaction::Endorse(
            build_endorse_tx(&prepared.account, &prepared.account, 7, Timestamp::new(42)).unwrap(),
        );
        assert!(prepared.sign(&keys, &identity, TxHash::ZERO).is_err());
        prepared.work.root = BlockHash::new([99; 32]);
        assert!(prepared.sign(&keys, &tx, TxHash::ZERO).is_err());
    }

    #[test]
    fn offline_receive_opens_account_and_send_preserves_origin() {
        let keys = burst_crypto::keypair_from_seed(&[1; 32]);
        let mut prepared = context(&keys);
        let source = TxHash::new([5; 32]);
        let origin = TxHash::new([6; 32]);
        let tx = Transaction::Send(
            build_send_tx(
                &prepared.account,
                &prepared.account,
                7,
                source,
                origin,
                Timestamp::new(42),
            )
            .unwrap(),
        );
        let sent = prepared.sign(&keys, &tx, origin).unwrap();
        assert_eq!(sent.trst_balance, 13);
        assert_eq!(sent.origin, origin);
        validate_signed_payment(&sent).unwrap();

        prepared.state.head = BlockHash::ZERO;
        prepared.state.block_count = 0;
        prepared.state.brn_spent = 0;
        prepared.state.trst_balance = 0;
        prepared.work.root = StateBlock::work_root_for(&prepared.account, BlockHash::ZERO);
        let tx = Transaction::Receive(
            build_receive_tx(&prepared.account, source, 7, Timestamp::new(42)).unwrap(),
        );
        let received = prepared.sign(&keys, &tx, origin).unwrap();
        assert_eq!(received.block_type, BlockType::Open);
        assert_eq!(received.trst_balance, 7);
        assert_eq!(received.origin, origin);
        assert_eq!(received.link.into_tx_hash(), source);
        validate_signed_payment(&received).unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn offline_draft_preserves_maximum_payment_and_refuses_overwrite() {
        let keys = burst_crypto::keypair_from_seed(&[1; 32]);
        let mut prepared = context(&keys);
        prepared.state.brn_spent = u128::MAX;
        prepared.state.trst_balance = u128::MAX;
        prepared.state.release_sources = (1..=burst_ledger::state_block::MAX_RELEASE_SOURCES)
            .map(|index| {
                let mut bytes = [0; 32];
                bytes[..8].copy_from_slice(&(index as u64).to_be_bytes());
                BlockHash::new(bytes)
            })
            .collect();
        let sources = (0..burst_ledger::state_block::MAX_MERGE_INPUTS)
            .map(|index| TxHash::new([index as u8; 32]))
            .collect();
        let tx = Transaction::Merge(
            build_merge_tx(&prepared.account, sources, Timestamp::new(42)).unwrap(),
        );
        let block = prepared.sign(&keys, &tx, TxHash::ZERO).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("payment.json");
        save_payment(&path, &block).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(save_payment(&path, &block).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let restored = load_payment(&path).unwrap();
        assert_eq!(
            serde_json::to_vec(&restored).unwrap(),
            serde_json::to_vec(&block).unwrap()
        );
        assert_eq!(restored.brn_spent, u128::MAX);
        assert_eq!(restored.trst_balance, u128::MAX);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn offline_draft_refuses_corruption_future_versions_and_oversized_files() {
        let keys = burst_crypto::keypair_from_seed(&[1; 32]);
        let prepared = context(&keys);
        let tx = Transaction::Burn(
            build_burn_tx(&prepared.account, &prepared.account, 7, Timestamp::new(42)).unwrap(),
        );
        let block = prepared.sign(&keys, &tx, TxHash::ZERO).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("payment.json");
        save_payment(&path, &block).unwrap();
        let original = std::fs::read(&path).unwrap();
        std::fs::write(&path, &original[..original.len() / 2]).unwrap();
        assert!(load_payment(&path).is_err());
        let mut draft: serde_json::Value = serde_json::from_slice(&original).unwrap();
        draft["version"] = serde_json::json!(5);
        std::fs::write(&path, serde_json::to_vec(&draft).unwrap()).unwrap();
        assert!(load_payment(&path)
            .unwrap_err()
            .to_string()
            .contains("version"));
        draft["version"] = serde_json::json!(1);
        std::fs::write(&path, serde_json::to_vec(&draft).unwrap()).unwrap();
        assert!(
            payment_may_have_been_submitted(&path, &block.hash).unwrap(),
            "legacy drafts have unknown submission history"
        );
        record_submission_attempt(&path, &block.hash).unwrap();
        assert!(payment_may_have_been_submitted(&path, &block.hash).unwrap());
        draft["block"]["brn_balance"] = serde_json::json!(19);
        std::fs::write(&path, serde_json::to_vec(&draft).unwrap()).unwrap();
        assert!(load_payment(&path).is_err());
        std::fs::write(&path, vec![b' '; 4 * 1024 * 1024]).unwrap();
        assert!(load_payment(&path)
            .unwrap_err()
            .to_string()
            .contains("size limit"));
        assert!(load_payment(directory.path()).is_err());
    }
}
