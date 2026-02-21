//! Transaction building helpers.

use burst_ledger::state_block::{BlockType, StateBlock, CURRENT_BLOCK_VERSION};
use burst_types::{BlockHash, Signature, Timestamp, TxHash, WalletAddress};

use crate::error::WalletError;

/// Encode a wallet address as a 32-byte public key for the `link` field.
///
/// The `link` field in `StateBlock` is `BlockHash` (32 bytes), but BURST
/// addresses are 65-character strings. This helper decodes the address to
/// its underlying 32-byte public key so it fits in the link field and can
/// be losslessly reconstructed via `burst_crypto::derive_address`.
fn address_to_link(address: &WalletAddress) -> Result<BlockHash, WalletError> {
    burst_crypto::decode_address(address.as_str())
        .map(BlockHash::new)
        .ok_or_else(|| WalletError::InvalidAddress(address.as_str().to_string()))
}

// ---------------------------------------------------------------------------
// Existing builders
// ---------------------------------------------------------------------------

/// Build a burn transaction (BRN -> TRST).
pub fn build_burn_tx(
    sender: &WalletAddress,
    receiver: &WalletAddress,
    amount: u128,
    now: Timestamp,
) -> Result<burst_transactions::burn::BurnTx, WalletError> {
    let hash_data = format!("burn:{}:{}:{}:{}", sender, receiver, amount, now);
    let hash = burst_crypto::hash_transaction(hash_data.as_bytes());
    Ok(burst_transactions::burn::BurnTx {
        hash,
        sender: sender.clone(),
        receiver: receiver.clone(),
        amount,
        timestamp: now,
        work: 0,
        signature: Signature([0u8; 64]),
    })
}

/// Build a send transaction (transfer TRST).
pub fn build_send_tx(
    sender: &WalletAddress,
    receiver: &WalletAddress,
    amount: u128,
    link: TxHash,
    origin: TxHash,
    now: Timestamp,
) -> Result<burst_transactions::send::SendTx, WalletError> {
    let hash_data = format!(
        "send:{}:{}:{}:{}:{}:{}",
        sender, receiver, amount, link, origin, now
    );
    let hash = burst_crypto::hash_transaction(hash_data.as_bytes());
    Ok(burst_transactions::send::SendTx {
        hash,
        sender: sender.clone(),
        receiver: receiver.clone(),
        amount,
        timestamp: now,
        link,
        origin,
        work: 0,
        signature: Signature([0u8; 64]),
    })
}

/// Build an endorsement transaction.
pub fn build_endorse_tx(
    endorser: &WalletAddress,
    target: &WalletAddress,
    burn_amount: u128,
    now: Timestamp,
) -> Result<burst_transactions::endorse::EndorseTx, WalletError> {
    let hash_data = format!("endorse:{}:{}:{}:{}", endorser, target, burn_amount, now);
    let hash = burst_crypto::hash_transaction(hash_data.as_bytes());
    Ok(burst_transactions::endorse::EndorseTx {
        hash,
        endorser: endorser.clone(),
        target: target.clone(),
        burn_amount,
        timestamp: now,
        work: 0,
        signature: Signature([0u8; 64]),
    })
}

/// Build a governance vote transaction.
pub fn build_governance_vote_tx(
    voter: &WalletAddress,
    proposal_hash: TxHash,
    vote: burst_transactions::governance::GovernanceVote,
    now: Timestamp,
) -> Result<burst_transactions::governance::GovernanceVoteTx, WalletError> {
    let vote_str = match vote {
        burst_transactions::governance::GovernanceVote::Yea => "yea",
        burst_transactions::governance::GovernanceVote::Nay => "nay",
        burst_transactions::governance::GovernanceVote::Abstain => "abstain",
    };
    let hash_data = format!(
        "governance_vote:{}:{}:{}:{}",
        voter, proposal_hash, vote_str, now
    );
    let hash = burst_crypto::hash_transaction(hash_data.as_bytes());
    Ok(burst_transactions::governance::GovernanceVoteTx {
        hash,
        voter: voter.clone(),
        proposal_hash,
        vote,
        timestamp: now,
        work: 0,
        signature: Signature([0u8; 64]),
    })
}

// ---------------------------------------------------------------------------
// New builders (Items 1.7, 1.8)
// ---------------------------------------------------------------------------

/// Build a receive transaction to pocket a pending send.
///
/// The `send_hash` is the TxHash of the corresponding send transaction.
pub fn build_receive_tx(
    receiver: &WalletAddress,
    send_hash: TxHash,
    amount: u128,
    now: Timestamp,
) -> Result<burst_transactions::receive::ReceiveTx, WalletError> {
    let hash_data = format!("receive:{}:{}:{}:{}", receiver, send_hash, amount, now);
    let hash = burst_crypto::hash_transaction(hash_data.as_bytes());
    Ok(burst_transactions::receive::ReceiveTx {
        hash,
        receiver: receiver.clone(),
        send_block_hash: send_hash,
        amount,
        timestamp: now,
        work: 0,
        signature: Signature([0u8; 64]),
    })
}

/// Build a change representative transaction for ORV consensus weight delegation.
pub fn build_change_rep_tx(
    account: &WalletAddress,
    new_representative: &WalletAddress,
    now: Timestamp,
) -> Result<burst_transactions::representative::ChangeRepresentativeTx, WalletError> {
    let hash_data = format!("change_rep:{}:{}:{}", account, new_representative, now);
    let hash = burst_crypto::hash_transaction(hash_data.as_bytes());
    Ok(burst_transactions::representative::ChangeRepresentativeTx {
        hash,
        account: account.clone(),
        new_representative: new_representative.clone(),
        timestamp: now,
        work: 0,
        signature: Signature([0u8; 64]),
    })
}

/// Build a split transaction to divide a TRST token into multiple outputs.
///
/// The `parent_hash` references the token being split. The `origin` is copied
/// from the parent token. All outputs share the same origin.
/// The sum of output amounts must equal the parent token's full amount.
pub fn build_split_tx(
    sender: &WalletAddress,
    parent_hash: TxHash,
    origin: TxHash,
    outputs: Vec<burst_transactions::split::SplitOutput>,
    now: Timestamp,
) -> Result<burst_transactions::split::SplitTx, WalletError> {
    if outputs.is_empty() {
        return Err(WalletError::TransactionBuild(
            "split must have at least one output".to_string(),
        ));
    }

    let outputs_str: String = outputs
        .iter()
        .map(|o| format!("{}:{}", o.receiver, o.amount))
        .collect::<Vec<_>>()
        .join(",");
    let hash_data = format!(
        "split:{}:{}:{}:[{}]:{}",
        sender, parent_hash, origin, outputs_str, now
    );
    let hash = burst_crypto::hash_transaction(hash_data.as_bytes());
    Ok(burst_transactions::split::SplitTx {
        hash,
        sender: sender.clone(),
        timestamp: now,
        parent_hash,
        origin,
        outputs,
        work: 0,
        signature: Signature([0u8; 64]),
    })
}

/// Build a merge transaction to combine multiple TRST tokens into one.
///
/// The merged token's expiry is the earliest among all inputs.
/// All source tokens must be owned by the sender.
pub fn build_merge_tx(
    sender: &WalletAddress,
    source_hashes: Vec<TxHash>,
    now: Timestamp,
) -> Result<burst_transactions::merge::MergeTx, WalletError> {
    if source_hashes.len() < 2 {
        return Err(WalletError::TransactionBuild(
            "merge requires at least two source tokens".to_string(),
        ));
    }

    let sources_str: String = source_hashes
        .iter()
        .map(|h| format!("{}", h))
        .collect::<Vec<_>>()
        .join(",");
    let hash_data = format!("merge:{}:[{}]:{}", sender, sources_str, now);
    let hash = burst_crypto::hash_transaction(hash_data.as_bytes());
    Ok(burst_transactions::merge::MergeTx {
        hash,
        sender: sender.clone(),
        timestamp: now,
        source_hashes,
        work: 0,
        signature: Signature([0u8; 64]),
    })
}

/// Build a challenge transaction to contest another wallet's verification.
///
/// The challenger stakes BRN which is lost if the challenge fails.
pub fn build_challenge_tx(
    challenger: &WalletAddress,
    target: &WalletAddress,
    stake_amount: u128,
    now: Timestamp,
) -> Result<burst_transactions::challenge::ChallengeTx, WalletError> {
    if stake_amount == 0 {
        return Err(WalletError::TransactionBuild(
            "challenge stake amount must be greater than zero".to_string(),
        ));
    }

    let hash_data = format!(
        "challenge:{}:{}:{}:{}",
        challenger, target, stake_amount, now
    );
    let hash = burst_crypto::hash_transaction(hash_data.as_bytes());
    Ok(burst_transactions::challenge::ChallengeTx {
        hash,
        challenger: challenger.clone(),
        target: target.clone(),
        stake_amount,
        timestamp: now,
        work: 0,
        signature: Signature([0u8; 64]),
    })
}

/// Build a governance proposal transaction.
pub fn build_governance_proposal_tx(
    proposer: &WalletAddress,
    proposal: burst_transactions::governance::ProposalContent,
    now: Timestamp,
) -> Result<burst_transactions::governance::GovernanceProposalTx, WalletError> {
    let hash_data = format!("governance_proposal:{}:{:?}:{}", proposer, proposal, now);
    let hash = burst_crypto::hash_transaction(hash_data.as_bytes());
    Ok(burst_transactions::governance::GovernanceProposalTx {
        hash,
        proposer: proposer.clone(),
        timestamp: now,
        proposal,
        work: 0,
        signature: Signature([0u8; 64]),
    })
}

/// Build a verification vote transaction.
///
/// The voter stakes BRN and casts a vote on a target wallet's humanity.
/// Vote values: 1 = Legitimate, 2 = Illegitimate, 3 = Neither.
pub fn build_verification_vote_tx(
    voter: &WalletAddress,
    target_wallet: &WalletAddress,
    vote: u8,
    stake_amount: u128,
    now: Timestamp,
) -> Result<burst_transactions::verification_vote::VerificationVoteTx, WalletError> {
    if !(1..=3).contains(&vote) {
        return Err(WalletError::TransactionBuild(
            "vote must be 1 (Legitimate), 2 (Illegitimate), or 3 (Neither)".to_string(),
        ));
    }
    let hash_data = format!(
        "verification_vote:{}:{}:{}:{}:{}",
        voter, target_wallet, vote, stake_amount, now
    );
    let hash = burst_crypto::hash_transaction(hash_data.as_bytes());
    Ok(burst_transactions::verification_vote::VerificationVoteTx {
        hash,
        voter: voter.clone(),
        target_wallet: target_wallet.clone(),
        vote,
        stake_amount,
        timestamp: now,
        work: 0,
        signature: Signature([0u8; 64]),
    })
}

/// Build a reject-receive transaction to decline a pending TRST send.
///
/// The `send_block_hash` is the hash of the send block being rejected.
/// The TRST is logically returned to the sender.
pub fn build_reject_receive_tx(
    rejecter: &WalletAddress,
    send_block_hash: TxHash,
    now: Timestamp,
) -> Result<burst_transactions::reject_receive::RejectReceiveTx, WalletError> {
    let hash_data = format!("reject_receive:{}:{}:{}", rejecter, send_block_hash, now);
    let hash = burst_crypto::hash_transaction(hash_data.as_bytes());
    Ok(burst_transactions::reject_receive::RejectReceiveTx {
        hash,
        rejecter: rejecter.clone(),
        send_block_hash,
        timestamp: now,
        work: 0,
        signature: Signature([0u8; 64]),
    })
}

/// Account state snapshot needed to build a StateBlock.
pub struct AccountState {
    /// Hash of the head (most recent) block in this account's chain.
    /// `BlockHash::ZERO` for accounts with no blocks yet.
    pub head: BlockHash,
    /// Current block count in this account's chain (0 for new accounts).
    pub block_count: u64,
    /// Current consensus representative.
    pub representative: WalletAddress,
    /// BRN balance after the most recent block.
    pub brn_balance: u128,
    /// TRST balance after the most recent block.
    pub trst_balance: u128,
}

/// Convert a high-level `Transaction` into a `StateBlock` that can be submitted to the node.
///
/// Takes the account's current state (head hash, balances) and a transaction,
/// and produces a StateBlock with the computed block hash. The signature and work
/// fields are left zeroed — the caller signs and attaches PoW separately.
pub fn build_state_block(
    account_state: &AccountState,
    transaction: &burst_transactions::Transaction,
    previous_origin: TxHash,
) -> Result<StateBlock, WalletError> {
    let (block_type, link, brn_balance, trst_balance, representative) = match transaction {
        burst_transactions::Transaction::Burn(tx) => (
            BlockType::Burn,
            address_to_link(&tx.receiver)?,
            account_state.brn_balance.checked_sub(tx.amount).ok_or(
                WalletError::InsufficientBrn {
                    needed: tx.amount,
                    available: account_state.brn_balance,
                },
            )?,
            account_state.trst_balance,
            None,
        ),
        burst_transactions::Transaction::Send(tx) => (
            BlockType::Send,
            address_to_link(&tx.receiver)?,
            account_state.brn_balance,
            account_state.trst_balance.checked_sub(tx.amount).ok_or(
                WalletError::InsufficientTrst {
                    needed: tx.amount,
                    available: account_state.trst_balance,
                },
            )?,
            None,
        ),
        burst_transactions::Transaction::Split(tx) => {
            let total_output: u128 = tx.outputs.iter().map(|o| o.amount).sum();
            (
                BlockType::Split,
                BlockHash::new(*tx.parent_hash.as_bytes()),
                account_state.brn_balance,
                account_state.trst_balance.checked_sub(total_output).ok_or(
                    WalletError::InsufficientTrst {
                        needed: total_output,
                        available: account_state.trst_balance,
                    },
                )?,
                None,
            )
        }
        burst_transactions::Transaction::Merge(tx) => (
            BlockType::Merge,
            BlockHash::new(*tx.hash.as_bytes()),
            account_state.brn_balance,
            account_state.trst_balance,
            None,
        ),
        burst_transactions::Transaction::Endorse(tx) => (
            BlockType::Endorse,
            address_to_link(&tx.target)?,
            account_state
                .brn_balance
                .checked_sub(tx.burn_amount)
                .ok_or(WalletError::InsufficientBrn {
                    needed: tx.burn_amount,
                    available: account_state.brn_balance,
                })?,
            account_state.trst_balance,
            None,
        ),
        burst_transactions::Transaction::Challenge(tx) => (
            BlockType::Challenge,
            address_to_link(&tx.target)?,
            account_state
                .brn_balance
                .checked_sub(tx.stake_amount)
                .ok_or(WalletError::InsufficientBrn {
                    needed: tx.stake_amount,
                    available: account_state.brn_balance,
                })?,
            account_state.trst_balance,
            None,
        ),
        burst_transactions::Transaction::GovernanceProposal(tx) => (
            BlockType::GovernanceProposal,
            BlockHash::new(*tx.hash.as_bytes()),
            account_state.brn_balance,
            account_state.trst_balance,
            None,
        ),
        burst_transactions::Transaction::GovernanceVote(tx) => (
            BlockType::GovernanceVote,
            BlockHash::new(*tx.proposal_hash.as_bytes()),
            account_state.brn_balance,
            account_state.trst_balance,
            None,
        ),
        burst_transactions::Transaction::Delegate(tx) => (
            BlockType::Delegate,
            address_to_link(&tx.delegate)?,
            account_state.brn_balance,
            account_state.trst_balance,
            None,
        ),
        burst_transactions::Transaction::RevokeDelegation(tx) => (
            BlockType::RevokeDelegation,
            BlockHash::new(*tx.hash.as_bytes()),
            account_state.brn_balance,
            account_state.trst_balance,
            None,
        ),
        burst_transactions::Transaction::Receive(tx) => (
            BlockType::Receive,
            BlockHash::new(*tx.send_block_hash.as_bytes()),
            account_state.brn_balance,
            account_state.trst_balance.saturating_add(tx.amount),
            None,
        ),
        burst_transactions::Transaction::ChangeRepresentative(tx) => (
            BlockType::ChangeRepresentative,
            BlockHash::ZERO,
            account_state.brn_balance,
            account_state.trst_balance,
            Some(tx.new_representative.clone()),
        ),
        burst_transactions::Transaction::RejectReceive(tx) => (
            BlockType::RejectReceive,
            BlockHash::new(*tx.send_block_hash.as_bytes()),
            account_state.brn_balance,
            account_state.trst_balance,
            None,
        ),
        burst_transactions::Transaction::VerificationVote(tx) => (
            BlockType::VerificationVote,
            address_to_link(&tx.target_wallet)?,
            account_state
                .brn_balance
                .checked_sub(tx.stake_amount)
                .ok_or(WalletError::InsufficientBrn {
                    needed: tx.stake_amount,
                    available: account_state.brn_balance,
                })?,
            account_state.trst_balance,
            None,
        ),
    };

    let representative = representative.unwrap_or_else(|| account_state.representative.clone());

    let origin = match block_type {
        BlockType::Burn => *transaction.hash(),
        _ => previous_origin,
    };

    let mut block = StateBlock {
        version: CURRENT_BLOCK_VERSION,
        block_type,
        account: transaction.sender().clone(),
        previous: account_state.head,
        representative,
        brn_balance,
        trst_balance,
        link,
        origin,
        transaction: *transaction.hash(),
        timestamp: transaction.timestamp(),
        work: 0,
        signature: Signature([0u8; 64]),
        hash: BlockHash::ZERO,
    };

    block.hash = block.compute_hash();

    Ok(block)
}

/// Sign a `StateBlock` with the given private key.
///
/// Computes the Ed25519 signature over the block's hash and returns
/// a new block with the signature field populated.
pub fn sign_state_block(
    mut block: StateBlock,
    private_key: &burst_types::PrivateKey,
) -> StateBlock {
    block.signature = burst_crypto::sign_message(block.hash.as_bytes(), private_key);
    block
}

/// Build AND sign a state block in one step.
///
/// Convenience function that calls `build_state_block` followed by `sign_state_block`.
pub fn build_and_sign_state_block(
    account_state: &AccountState,
    transaction: &burst_transactions::Transaction,
    private_key: &burst_types::PrivateKey,
    previous_origin: TxHash,
) -> Result<StateBlock, WalletError> {
    let block = build_state_block(account_state, transaction, previous_origin)?;
    Ok(sign_state_block(block, private_key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_transactions::split::SplitOutput;

    fn test_address(suffix: &str) -> WalletAddress {
        let seed: Vec<u8> = suffix.as_bytes().iter().copied().cycle().take(32).collect();
        let mut seed_arr = [0u8; 32];
        seed_arr.copy_from_slice(&seed);
        let kp = burst_crypto::keypair_from_seed(&seed_arr);
        burst_crypto::derive_address(&kp.public)
    }

    fn test_account_state() -> AccountState {
        AccountState {
            head: BlockHash::ZERO,
            block_count: 0,
            representative: test_address("rep1"),
            brn_balance: 10_000,
            trst_balance: 5_000,
        }
    }

    #[test]
    fn build_burn_tx_creates_valid_tx() {
        let sender = test_address("sender1");
        let receiver = test_address("receiver1");
        let tx = build_burn_tx(&sender, &receiver, 100, Timestamp::new(1000)).unwrap();
        assert_eq!(tx.sender.as_str(), sender.as_str());
        assert_eq!(tx.receiver.as_str(), receiver.as_str());
        assert_eq!(tx.amount, 100);
        assert!(!tx.hash.is_zero());
    }

    #[test]
    fn build_receive_tx_creates_valid_tx() {
        let receiver = test_address("receiver1");
        let send_hash = TxHash::new([1u8; 32]);
        let tx = build_receive_tx(&receiver, send_hash, 500, Timestamp::new(2000)).unwrap();
        assert_eq!(tx.receiver.as_str(), receiver.as_str());
        assert_eq!(tx.amount, 500);
        assert!(!tx.hash.is_zero());
    }

    #[test]
    fn build_change_rep_tx_creates_valid_tx() {
        let account = test_address("account1");
        let new_rep = test_address("newrep1");
        let tx = build_change_rep_tx(&account, &new_rep, Timestamp::new(3000)).unwrap();
        assert_eq!(tx.account.as_str(), account.as_str());
        assert_eq!(tx.new_representative.as_str(), new_rep.as_str());
        assert!(!tx.hash.is_zero());
    }

    #[test]
    fn build_split_tx_creates_valid_tx() {
        let sender = test_address("sender1");
        let parent = TxHash::new([3u8; 32]);
        let origin = TxHash::new([4u8; 32]);
        let outputs = vec![
            SplitOutput {
                receiver: test_address("out1"),
                amount: 300,
            },
            SplitOutput {
                receiver: test_address("out2"),
                amount: 200,
            },
        ];
        let tx = build_split_tx(&sender, parent, origin, outputs, Timestamp::new(4000)).unwrap();
        assert_eq!(tx.outputs.len(), 2);
        assert!(!tx.hash.is_zero());
    }

    #[test]
    fn build_split_tx_rejects_empty_outputs() {
        let sender = test_address("sender1");
        let result = build_split_tx(
            &sender,
            TxHash::ZERO,
            TxHash::ZERO,
            vec![],
            Timestamp::new(0),
        );
        assert!(result.is_err());
    }

    #[test]
    fn build_merge_tx_creates_valid_tx() {
        let sender = test_address("sender1");
        let sources = vec![TxHash::new([5u8; 32]), TxHash::new([6u8; 32])];
        let tx = build_merge_tx(&sender, sources, Timestamp::new(5000)).unwrap();
        assert_eq!(tx.source_hashes.len(), 2);
        assert!(!tx.hash.is_zero());
    }

    #[test]
    fn build_merge_tx_rejects_single_source() {
        let sender = test_address("sender1");
        let result = build_merge_tx(&sender, vec![TxHash::ZERO], Timestamp::new(0));
        assert!(result.is_err());
    }

    #[test]
    fn build_challenge_tx_creates_valid_tx() {
        let challenger = test_address("challenger1");
        let target = test_address("target1");
        let tx = build_challenge_tx(&challenger, &target, 1000, Timestamp::new(6000)).unwrap();
        assert_eq!(tx.challenger.as_str(), challenger.as_str());
        assert_eq!(tx.target.as_str(), target.as_str());
        assert_eq!(tx.stake_amount, 1000);
        assert!(!tx.hash.is_zero());
    }

    #[test]
    fn build_challenge_tx_rejects_zero_stake() {
        let result = build_challenge_tx(
            &test_address("c1"),
            &test_address("t1"),
            0,
            Timestamp::new(0),
        );
        assert!(result.is_err());
    }

    #[test]
    fn build_state_block_for_burn() {
        let state = test_account_state();
        let burn = build_burn_tx(
            &test_address("sender1"),
            &test_address("receiver1"),
            100,
            Timestamp::new(1000),
        )
        .unwrap();
        let tx = burst_transactions::Transaction::Burn(burn);
        let block = build_state_block(&state, &tx, TxHash::ZERO).unwrap();

        assert_eq!(block.block_type, BlockType::Burn);
        assert_eq!(block.brn_balance, 9_900);
        assert_eq!(block.trst_balance, 5_000);
        assert_eq!(block.version, CURRENT_BLOCK_VERSION);
        assert!(!block.hash.is_zero());
    }

    #[test]
    fn build_state_block_for_send() {
        let state = test_account_state();
        let prev_origin = TxHash::new([9u8; 32]);
        let send = build_send_tx(
            &test_address("sender1"),
            &test_address("receiver1"),
            200,
            TxHash::new([1u8; 32]),
            TxHash::new([2u8; 32]),
            Timestamp::new(2000),
        )
        .unwrap();
        let tx = burst_transactions::Transaction::Send(send);
        let block = build_state_block(&state, &tx, prev_origin).unwrap();

        assert_eq!(block.block_type, BlockType::Send);
        assert_eq!(block.brn_balance, 10_000);
        assert_eq!(block.trst_balance, 4_800);
        assert_eq!(block.origin, prev_origin);
    }

    #[test]
    fn build_state_block_insufficient_brn() {
        let state = AccountState {
            brn_balance: 50,
            ..test_account_state()
        };
        let burn = build_burn_tx(
            &test_address("sender1"),
            &test_address("receiver1"),
            100,
            Timestamp::new(1000),
        )
        .unwrap();
        let tx = burst_transactions::Transaction::Burn(burn);
        let result = build_state_block(&state, &tx, TxHash::ZERO);
        assert!(result.is_err());
    }

    #[test]
    fn build_state_block_for_change_rep() {
        let state = test_account_state();
        let rep_change = build_change_rep_tx(
            &test_address("account1"),
            &test_address("newrep1"),
            Timestamp::new(3000),
        )
        .unwrap();
        let tx = burst_transactions::Transaction::ChangeRepresentative(rep_change);
        let block = build_state_block(&state, &tx, TxHash::ZERO).unwrap();

        assert_eq!(block.block_type, BlockType::ChangeRepresentative);
        assert_eq!(
            block.representative.as_str(),
            test_address("newrep1").as_str()
        );
    }

    #[test]
    fn build_and_sign_produces_valid_signature() {
        let kp = burst_crypto::generate_keypair();
        let address = burst_crypto::derive_address(&kp.public);
        let state = AccountState {
            head: BlockHash::ZERO,
            block_count: 0,
            representative: test_address("rep1"),
            brn_balance: 10_000,
            trst_balance: 5_000,
        };
        let burn = build_burn_tx(
            &address,
            &test_address("receiver1"),
            100,
            Timestamp::new(1000),
        )
        .unwrap();
        let tx = burst_transactions::Transaction::Burn(burn);
        let signed = build_and_sign_state_block(&state, &tx, &kp.private, TxHash::ZERO).unwrap();
        assert!(burst_crypto::verify_signature(
            signed.hash.as_bytes(),
            &signed.signature,
            &kp.public
        ));
    }
}
