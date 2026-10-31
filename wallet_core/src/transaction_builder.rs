//! Unsigned transaction drafts and account-block construction.
//!
//! Builders do not query the ledger or submit operations. Transaction drafts have
//! zero work and signatures; their hashes are distinct from the resulting block hash.

use burst_ledger::state_block::{BlockType, StateBlock, CURRENT_BLOCK_VERSION};
use burst_types::{BlockHash, Signature, Timestamp, TxHash, WalletAddress};

use crate::error::WalletError;

/// Encode a wallet address as a 32-byte public key for the `link` field.

fn address_to_link(address: &WalletAddress) -> Result<BlockHash, WalletError> {
    burst_crypto::decode_address(address.as_str())
        .map(BlockHash::new)
        .ok_or_else(|| WalletError::InvalidAddress(address.as_str().to_string()))
}

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

/// Build a receive transaction to pocket a pending send.
///
/// `send_hash` becomes the receive block's source link. Supply the pending send's
/// block hash wrapped as `TxHash`, not an unrelated local draft identifier.
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

pub fn build_delegate_tx(
    delegator: &WalletAddress,
    delegate: &WalletAddress,
    now: Timestamp,
) -> Result<burst_transactions::delegate::DelegateTx, WalletError> {
    address_to_link(delegate)?;
    if delegator == delegate {
        return Err(WalletError::TransactionBuild(
            "cannot delegate to yourself".into(),
        ));
    }
    Ok(burst_transactions::delegate::DelegateTx {
        hash: burst_crypto::hash_transaction(
            format!("delegate:{delegator}:{delegate}:{now}").as_bytes(),
        ),
        delegator: delegator.clone(),
        delegate: delegate.clone(),
        timestamp: now,
        work: 0,
        signature: Signature([0; 64]),
    })
}

pub fn build_revoke_delegation_tx(
    delegator: &WalletAddress,
    now: Timestamp,
) -> Result<burst_transactions::delegate::RevokeDelegationTx, WalletError> {
    Ok(burst_transactions::delegate::RevokeDelegationTx {
        hash: burst_crypto::hash_transaction(
            format!("revoke_delegation:{delegator}:{now}").as_bytes(),
        ),
        delegator: delegator.clone(),
        timestamp: now,
        work: 0,
        signature: Signature([0; 64]),
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

/// Build a merge transaction to combine multiple TRST tokens into one.
///
/// Checks only that there are at least two inputs. Ownership, duplicate inputs
/// and expiry are checked when the node processes the block.
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
/// Rejects a zero stake. This draft does not reserve BRN or check eligibility.
pub fn build_challenge_tx(
    challenger: &WalletAddress,
    target: &WalletAddress,
    stake_amount: u128,
    reason: burst_types::ChallengeReason,
    now: Timestamp,
) -> Result<burst_transactions::challenge::ChallengeTx, WalletError> {
    if stake_amount == 0 {
        return Err(WalletError::TransactionBuild(
            "challenge stake amount must be greater than zero".to_string(),
        ));
    }

    let hash_data = format!(
        "challenge:{}:{}:{}:{:?}:{}",
        challenger, target, stake_amount, reason, now
    );
    let hash = burst_crypto::hash_transaction(hash_data.as_bytes());
    Ok(burst_transactions::challenge::ChallengeTx {
        hash,
        challenger: challenger.clone(),
        target: target.clone(),
        stake_amount,
        reason,
        appeal_of: None,
        timestamp: now,
        work: 0,
        signature: Signature([0u8; 64]),
    })
}

/// Build a meta-challenge bound to a specific outcome, not merely a wallet.
/// Admission remains subject to the node's release gate and outcome validation.
pub fn build_appeal_tx(
    challenger: &WalletAddress,
    target: &WalletAddress,
    stake_amount: u128,
    reason: burst_types::ChallengeReason,
    outcome: BlockHash,
    now: Timestamp,
) -> Result<burst_transactions::challenge::ChallengeTx, WalletError> {
    if outcome.is_zero() {
        return Err(WalletError::TransactionBuild(
            "appeal outcome must be nonzero".into(),
        ));
    }
    let mut tx = build_challenge_tx(challenger, target, stake_amount, reason, now)?;
    tx.hash = TxHash::new(burst_crypto::blake2b_256_multi(&[
        b"BURST/meta-challenge/v1\0",
        tx.hash.as_bytes(),
        outcome.as_bytes(),
    ]));
    tx.appeal_of = Some(outcome);
    Ok(tx)
}

fn compact_proposal_link(
    content: &burst_transactions::governance::ProposalContent,
) -> Result<BlockHash, WalletError> {
    let burst_transactions::governance::ProposalContent::ParameterChange {
        parameter,
        proposed_value,
        ..
    } = content
    else {
        return Err(WalletError::TransactionBuild(
            "constitutional proposals require the full-content proposal builder".into(),
        ));
    };
    let param = burst_governance::GovernableParam::from_name(parameter)
        .map_err(|error| WalletError::TransactionBuild(error.to_string()))?;
    let content = burst_governance::ProposalContent::ParameterChange {
        param,
        new_value: *proposed_value,
    };
    let bytes = content
        .encode_compact()
        .map_err(|error| WalletError::TransactionBuild(error.to_string()))?;
    Ok(BlockHash::new(bytes))
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
/// Requires a nonzero selection context and vote 1 (Legitimate), 2 (Illegitimate),
/// or 3 (Neither). It does not authenticate the context or check voter selection.
pub fn build_verification_vote_tx(
    voter: &WalletAddress,
    target_wallet: &WalletAddress,
    selection_context: BlockHash,
    vote: u8,
    stake_amount: u128,
    now: Timestamp,
) -> Result<burst_transactions::verification_vote::VerificationVoteTx, WalletError> {
    if selection_context.is_zero() {
        return Err(WalletError::TransactionBuild(
            "verification requires a certified selection context".into(),
        ));
    }
    if !(1..=3).contains(&vote) {
        return Err(WalletError::TransactionBuild(
            "vote must be 1 (Legitimate), 2 (Illegitimate), or 3 (Neither)".to_string(),
        ));
    }
    let hash_data = format!(
        "verification_vote:{}:{}:{}:{}:{}:{}",
        voter, target_wallet, selection_context, vote, stake_amount, now
    );
    let hash = burst_crypto::hash_transaction(hash_data.as_bytes());
    Ok(burst_transactions::verification_vote::VerificationVoteTx {
        hash,
        voter: voter.clone(),
        target_wallet: target_wallet.clone(),
        selection_context,
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
/// Building the draft does not change the pending send or either balance.
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

/// Caller-supplied account fields for block construction. This record has no
/// account address or proof binding it to the transaction's sender.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AccountState {
    pub signing_grant: BlockHash,
    pub payment_epoch: BlockHash,
    pub release_sources: Vec<BlockHash>,
    /// Hash of the head (most recent) block in this account's chain.
    /// `BlockHash::ZERO` for accounts with no blocks yet.
    pub head: BlockHash,
    /// Current block count in this account's chain (0 for new accounts).
    pub block_count: u64,
    /// Current consensus representative.
    pub representative: WalletAddress,
    /// BRN spending recorded on the account head, not the available balance.
    /// The serialized field is named `brn_balance`.
    #[serde(rename = "brn_balance")]
    pub brn_spent: u128,
    /// TRST balance after the most recent block.
    pub trst_balance: u128,
}

/// Build an unsigned block from the supplied account snapshot and transaction.
///
/// Computes the block hash; leaves signature and work zeroed. Local checks include
/// a nonzero payment epoch and balance arithmetic, but do not establish that the
/// snapshot is current, the account has enough BRN, or the operation is authorized.
/// The block uses `previous_origin` for sends; it does not resolve holding provenance
/// from the send draft. Higher-level callers must supply the correct origin.
pub fn build_state_block(
    account_state: &AccountState,
    transaction: &burst_transactions::Transaction,
    previous_origin: TxHash,
    params_hash: BlockHash,
) -> Result<StateBlock, WalletError> {
    if account_state.payment_epoch.is_zero() {
        return Err(WalletError::TransactionBuild(
            "missing payment epoch".into(),
        ));
    }
    let (block_type, link, brn_spent, trst_balance, representative) = match transaction {
        // BRN-spending blocks: the odometer increases by the spend amount.
        // Availability is validated by nodes against the computed BRN(w).
        burst_transactions::Transaction::Burn(tx) => (
            BlockType::Burn,
            address_to_link(&tx.receiver)?,
            checked_balance_add(account_state.brn_spent, tx.amount)?,
            account_state.trst_balance,
            None,
        ),
        burst_transactions::Transaction::Send(tx) => (
            BlockType::Send,
            address_to_link(&tx.receiver)?,
            account_state.brn_spent,
            account_state.trst_balance.checked_sub(tx.amount).ok_or(
                WalletError::InsufficientTrst {
                    needed: tx.amount,
                    available: account_state.trst_balance,
                },
            )?,
            None,
        ),
        burst_transactions::Transaction::Merge(tx) => (
            BlockType::Merge,
            BlockHash::new(*tx.hash.as_bytes()),
            account_state.brn_spent,
            account_state.trst_balance,
            None,
        ),
        burst_transactions::Transaction::Endorse(tx) => (
            BlockType::Endorse,
            address_to_link(&tx.target)?,
            checked_balance_add(account_state.brn_spent, tx.burn_amount)?,
            account_state.trst_balance,
            None,
        ),
        burst_transactions::Transaction::Challenge(tx) => (
            BlockType::Challenge,
            address_to_link(&tx.target)?,
            checked_balance_add(account_state.brn_spent, tx.stake_amount)?,
            account_state.trst_balance,
            None,
        ),
        burst_transactions::Transaction::GovernanceProposal(tx) => (
            BlockType::GovernanceProposal,
            compact_proposal_link(&tx.proposal)?,
            account_state.brn_spent,
            account_state.trst_balance,
            None,
        ),
        burst_transactions::Transaction::GovernanceVote(tx) => (
            BlockType::GovernanceVote,
            BlockHash::new(*tx.proposal_hash.as_bytes()),
            account_state.brn_spent,
            account_state.trst_balance,
            None,
        ),
        burst_transactions::Transaction::Delegate(tx) => (
            BlockType::Delegate,
            address_to_link(&tx.delegate)?,
            account_state.brn_spent,
            account_state.trst_balance,
            None,
        ),
        burst_transactions::Transaction::RevokeDelegation(tx) => (
            BlockType::RevokeDelegation,
            BlockHash::new(*tx.hash.as_bytes()),
            account_state.brn_spent,
            account_state.trst_balance,
            None,
        ),
        burst_transactions::Transaction::Receive(tx) => (
            BlockType::Receive,
            BlockHash::new(*tx.send_block_hash.as_bytes()),
            account_state.brn_spent,
            checked_balance_add(account_state.trst_balance, tx.amount)?,
            None,
        ),
        burst_transactions::Transaction::ChangeRepresentative(tx) => (
            BlockType::ChangeRepresentative,
            BlockHash::ZERO,
            account_state.brn_spent,
            account_state.trst_balance,
            Some(tx.new_representative.clone()),
        ),
        burst_transactions::Transaction::RejectReceive(tx) => (
            BlockType::RejectReceive,
            BlockHash::new(*tx.send_block_hash.as_bytes()),
            account_state.brn_spent,
            account_state.trst_balance,
            None,
        ),
        burst_transactions::Transaction::VerificationVote(tx) => (
            BlockType::VerificationVote,
            address_to_link(&tx.target_wallet)?,
            checked_balance_add(account_state.brn_spent, tx.stake_amount)?,
            account_state.trst_balance,
            None,
        ),
    };

    let representative = representative.unwrap_or_else(|| account_state.representative.clone());

    let origin = match transaction {
        burst_transactions::Transaction::GovernanceProposal(_) => TxHash::ZERO,
        // A burn starts a provenance chain at its transaction hash.
        _ if block_type == BlockType::Burn => *transaction.hash(),
        // A challenge encodes its reason (fraud vs benign inactivity) in origin.
        burst_transactions::Transaction::Challenge(tx) => TxHash::new(tx.reason.to_origin()),
        burst_transactions::Transaction::VerificationVote(tx) => {
            tx.selection_context.into_tx_hash()
        }
        _ => previous_origin,
    };

    // The block signature commits to the selected merge inputs.
    let merge_sources = match transaction {
        burst_transactions::Transaction::Merge(tx) => tx.source_hashes.clone(),
        _ => Vec::new(),
    };

    let mut block = StateBlock {
        signing_grant: account_state.signing_grant,
        delegation_payload: Vec::new(),
        version: CURRENT_BLOCK_VERSION,
        payment_epoch: account_state.payment_epoch,
        block_type,
        account: transaction.sender().clone(),
        previous: account_state.head,
        representative,
        brn_spent,
        trst_balance,
        link,
        origin,
        transaction: *transaction.hash(),
        timestamp: transaction.timestamp(),
        params_hash,
        target_head: match transaction {
            burst_transactions::Transaction::Challenge(tx) => match tx.appeal_of {
                Some(outcome) if outcome.is_zero() => {
                    return Err(WalletError::TransactionBuild(
                        "appeal outcome must be nonzero".into(),
                    ))
                }
                Some(outcome) => outcome,
                None => BlockHash::ZERO,
            },
            _ => BlockHash::ZERO,
        },
        merge_sources,
        release_sources: account_state.release_sources.clone(),
        work: 0,
        signature: Signature([0u8; 64]),
        hash: BlockHash::ZERO,
    };

    // Vote blocks encode the choice in `transaction` with zero padding.
    match transaction {
        burst_transactions::Transaction::GovernanceVote(tx) => {
            let mut encoded = [0u8; 32];
            encoded[0] = match tx.vote {
                burst_transactions::governance::GovernanceVote::Yea => 0,
                burst_transactions::governance::GovernanceVote::Nay => 1,
                burst_transactions::governance::GovernanceVote::Abstain => 2,
            };
            block.transaction = TxHash::new(encoded);
        }
        burst_transactions::Transaction::VerificationVote(tx) => {
            let mut encoded = [0u8; 32];
            encoded[0] = tx.vote;
            block.transaction = TxHash::new(encoded);
        }
        _ => {}
    }
    block.hash = block.compute_hash();

    Ok(block)
}

fn checked_balance_add(balance: u128, amount: u128) -> Result<u128, WalletError> {
    balance.checked_add(amount).ok_or_else(|| {
        WalletError::TransactionBuild("balance exceeds u128 protocol precision".into())
    })
}

/// Build an unsigned device handoff without changing identity or balances.
///
/// The caller must supply a fresh nonce; this helper checks only that it is nonzero.
/// Persist the signed draft before submission and adopt its hash as the local
/// grant only after confirmation.
pub fn build_signing_handoff(
    state: &AccountState,
    account: &WalletAddress,
    nonce: BlockHash,
    params_hash: BlockHash,
    now: Timestamp,
) -> Result<StateBlock, WalletError> {
    if state.head.is_zero() || state.block_count == 0 || nonce.is_zero() {
        return Err(WalletError::TransactionBuild(
            "handoff requires an open account and a fresh device nonce".into(),
        ));
    }
    let transaction = burst_transactions::Transaction::ChangeRepresentative(build_change_rep_tx(
        account,
        &state.representative,
        now,
    )?);
    let mut block = build_state_block(state, &transaction, TxHash::ZERO, params_hash)?;
    block.block_type = BlockType::ChangeSigner;
    block.link = nonce;
    block.transaction = TxHash::ZERO;
    // Stake release is a separate action; takeover must leave the odometer intact.
    block.release_sources.clear();
    block.brn_spent = state.brn_spent;
    block
        .validate_format()
        .map_err(|error| WalletError::TransactionBuild(error.into()))?;
    block.hash = block.compute_hash();
    Ok(block)
}

/// Sign the block's stored hash with the supplied private key.
///
/// Does not recompute the hash or check that the key controls the account. After
/// editing hashed fields, update `block.hash` with `compute_hash()` before signing.
pub fn sign_state_block(
    mut block: StateBlock,
    private_key: &burst_types::PrivateKey,
) -> StateBlock {
    block.signature = burst_crypto::sign_message(block.hash.as_bytes(), private_key);
    block
}

/// Build a block, compute its hash and sign it. Proof of work remains zero.
pub fn build_and_sign_state_block(
    account_state: &AccountState,
    transaction: &burst_transactions::Transaction,
    private_key: &burst_types::PrivateKey,
    previous_origin: TxHash,
    params_hash: BlockHash,
) -> Result<StateBlock, WalletError> {
    let block = build_state_block(account_state, transaction, previous_origin, params_hash)?;
    Ok(sign_state_block(block, private_key))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn test_address(suffix: &str) -> WalletAddress {
        let seed: Vec<u8> = suffix.as_bytes().iter().copied().cycle().take(32).collect();
        let mut seed_arr = [0u8; 32];
        seed_arr.copy_from_slice(&seed);
        let kp = burst_crypto::keypair_from_seed(&seed_arr);
        burst_crypto::derive_address(&kp.public)
    }

    fn test_account_state() -> AccountState {
        AccountState {
            signing_grant: burst_types::BlockHash::ZERO,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            release_sources: Vec::new(),
            head: BlockHash::ZERO,
            block_count: 0,
            representative: test_address("rep1"),
            brn_spent: 10_000,
            trst_balance: 5_000,
        }
    }

    #[test]
    fn signing_handoff_preserves_balances_and_successor_binding() {
        let mut state = test_account_state();
        state.head = BlockHash::new([2; 32]);
        state.block_count = 1;
        state.signing_grant = BlockHash::new([3; 32]);
        let account = test_address("handoff");
        let params = BlockHash::new([4; 32]);
        let handoff = build_signing_handoff(
            &state,
            &account,
            BlockHash::new([5; 32]),
            params,
            Timestamp::new(100),
        )
        .unwrap();
        assert_eq!(handoff.block_type, BlockType::ChangeSigner);
        assert_eq!(handoff.signing_grant, state.signing_grant);
        assert_eq!(handoff.account, account);
        assert_eq!(handoff.representative, state.representative);
        assert_eq!(
            (handoff.brn_spent, handoff.trst_balance),
            (state.brn_spent, state.trst_balance)
        );
        state.head = handoff.hash;
        state.signing_grant = handoff.active_signing_grant();
        let tx = burst_transactions::Transaction::Burn(
            build_burn_tx(&account, &account, 1, Timestamp::new(101)).unwrap(),
        );
        let next = build_state_block(&state, &tx, TxHash::ZERO, params).unwrap();
        next.validate_signing_grant(Some(&handoff)).unwrap();
        assert!(build_signing_handoff(
            &state,
            &account,
            BlockHash::ZERO,
            params,
            Timestamp::new(102)
        )
        .is_err());
    }

    #[test]
    fn block_builder_requires_and_preserves_payment_epoch() {
        let sender = test_address("sender1");
        let receiver = test_address("receiver1");
        let tx = burst_transactions::Transaction::Burn(
            build_burn_tx(&sender, &receiver, 100, Timestamp::new(1000)).unwrap(),
        );
        let mut state = test_account_state();
        let block = build_state_block(&state, &tx, TxHash::ZERO, BlockHash::ZERO).unwrap();
        assert_eq!(block.payment_epoch, state.payment_epoch);
        state.payment_epoch = BlockHash::ZERO;
        assert!(
            build_state_block(&state, &tx, TxHash::ZERO, BlockHash::ZERO)
                .unwrap_err()
                .to_string()
                .contains("missing payment epoch")
        );
    }

    #[test]
    fn compact_governance_proposal_encodes_content_and_clears_payment_origin() {
        use burst_transactions::governance::ProposalContent;
        let content = ProposalContent::ParameterChange {
            parameter: "brn_rate".into(),
            current_value: 123,
            proposed_value: 0,
        };
        let tx = burst_transactions::Transaction::GovernanceProposal(
            build_governance_proposal_tx(&test_address("owner"), content, Timestamp::new(1000))
                .unwrap(),
        );
        let block = build_state_block(
            &test_account_state(),
            &tx,
            TxHash::new([7; 32]),
            BlockHash::ZERO,
        )
        .unwrap();
        assert_eq!(block.block_type, BlockType::GovernanceProposal);
        assert_eq!(block.origin, TxHash::ZERO);
        assert_eq!(
            burst_governance::ProposalContent::decode_compact(block.link.as_bytes()).unwrap(),
            burst_governance::ProposalContent::ParameterChange {
                param: burst_governance::GovernableParam::BrnRate,
                new_value: 0
            }
        );
        for content in [
            ProposalContent::ParameterChange {
                parameter: "unknown".into(),
                current_value: 0,
                proposed_value: 1,
            },
            ProposalContent::ConstitutionalAmendment {
                title: "Rights".into(),
                text: "People vote".into(),
            },
        ] {
            let tx = burst_transactions::Transaction::GovernanceProposal(
                build_governance_proposal_tx(&test_address("owner"), content, Timestamp::new(1000))
                    .unwrap(),
            );
            assert!(
                build_state_block(&test_account_state(), &tx, TxHash::ZERO, BlockHash::ZERO)
                    .is_err()
            );
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
        let tx = build_challenge_tx(
            &challenger,
            &target,
            1000,
            burst_types::ChallengeReason::Fraud,
            Timestamp::new(6000),
        )
        .unwrap();
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
            burst_types::ChallengeReason::Fraud,
            Timestamp::new(0),
        );
        assert!(result.is_err());
    }

    #[test]
    fn appeal_builder_binds_outcome_and_never_downgrades_a_zero_reference() {
        let key = burst_crypto::keypair_from_seed(&[7; 32]);
        let challenger = burst_crypto::derive_address(&key.public);
        let target = test_address("target1");
        let now = Timestamp::new(6000);
        let reason = burst_types::ChallengeReason::Fraud;
        let ordinary = build_challenge_tx(&challenger, &target, 1000, reason, now).unwrap();
        let mut appeal = build_appeal_tx(
            &challenger,
            &target,
            1000,
            reason,
            BlockHash::new([3; 32]),
            now,
        )
        .unwrap();
        assert_ne!(appeal.hash, ordinary.hash);
        let other = build_appeal_tx(
            &challenger,
            &target,
            1000,
            reason,
            BlockHash::new([4; 32]),
            now,
        )
        .unwrap();
        assert_ne!(appeal.hash, other.hash);
        let state = AccountState {
            signing_grant: burst_types::BlockHash::ZERO,
            head: BlockHash::new([2; 32]),
            ..test_account_state()
        };
        let mut block = build_and_sign_state_block(
            &state,
            &burst_transactions::Transaction::Challenge(appeal.clone()),
            &key.private,
            TxHash::ZERO,
            BlockHash::ZERO,
        )
        .unwrap();
        assert_eq!(block.target_head, appeal.appeal_of.unwrap());
        assert_eq!(block.brn_spent, state.brn_spent + 1000);
        assert_eq!(block.trst_balance, state.trst_balance);
        assert!(burst_crypto::verify_signature(
            block.compute_hash().as_bytes(),
            &block.signature,
            &key.public
        ));
        block.target_head = BlockHash::new([4; 32]);
        assert!(!burst_crypto::verify_signature(
            block.compute_hash().as_bytes(),
            &block.signature,
            &key.public
        ));
        appeal.appeal_of = Some(BlockHash::ZERO);
        assert!(build_state_block(
            &state,
            &burst_transactions::Transaction::Challenge(appeal),
            TxHash::ZERO,
            BlockHash::ZERO
        )
        .is_err());
        assert!(build_appeal_tx(&challenger, &target, 1000, reason, BlockHash::ZERO, now).is_err());
        assert!(build_appeal_tx(
            &challenger,
            &target,
            0,
            reason,
            BlockHash::new([3; 32]),
            now
        )
        .is_err());
        let mut legacy = serde_json::to_value(&ordinary).unwrap();
        legacy.as_object_mut().unwrap().remove("appeal_of");
        let legacy: burst_transactions::challenge::ChallengeTx =
            serde_json::from_value(legacy).unwrap();
        assert_eq!(legacy.appeal_of, None);
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
        let block = build_state_block(&state, &tx, TxHash::ZERO, BlockHash::ZERO).unwrap();

        assert_eq!(block.block_type, BlockType::Burn);
        // Ascending odometer: 10_000 previously spent + 100 burned now.
        assert_eq!(block.brn_spent, 10_100);
        assert_eq!(block.trst_balance, 5_000);
        assert_eq!(block.version, CURRENT_BLOCK_VERSION);
        assert!(!block.hash.is_zero());
    }

    #[test]
    fn vote_blocks_encode_the_users_choice_not_a_digest() {
        use burst_transactions::{governance::GovernanceVote, Transaction};
        let state = test_account_state();
        for (vote, byte) in [
            (GovernanceVote::Yea, 0),
            (GovernanceVote::Nay, 1),
            (GovernanceVote::Abstain, 2),
        ] {
            let tx = build_governance_vote_tx(
                &test_address("voter"),
                TxHash::new([7; 32]),
                vote,
                Timestamp::new(1000),
            )
            .unwrap();
            let block = build_state_block(
                &state,
                &Transaction::GovernanceVote(tx),
                TxHash::ZERO,
                BlockHash::ZERO,
            )
            .unwrap();
            assert_eq!(block.transaction.as_bytes()[0], byte);
            assert!(block.transaction.as_bytes()[1..].iter().all(|b| *b == 0));
            assert_eq!(block.compute_hash(), block.hash);
        }
        for vote in 1..=3 {
            let tx = build_verification_vote_tx(
                &test_address("voter"),
                &test_address("target"),
                BlockHash::new([8; 32]),
                vote,
                10,
                Timestamp::new(1000),
            )
            .unwrap();
            let block = build_state_block(
                &state,
                &Transaction::VerificationVote(tx),
                TxHash::ZERO,
                BlockHash::ZERO,
            )
            .unwrap();
            assert_eq!(block.transaction.as_bytes()[0], vote);
            assert_eq!(block.origin, TxHash::new([8; 32]));
        }
    }

    #[test]
    fn verification_context_is_required_and_covered_by_the_signed_block() {
        let keys = burst_crypto::keypair_from_seed(&[83; 32]);
        let voter = burst_crypto::derive_address(&keys.public);
        let target = test_address("target");
        assert!(build_verification_vote_tx(
            &voter,
            &target,
            BlockHash::ZERO,
            1,
            10,
            Timestamp::new(1000)
        )
        .is_err());
        let transaction = build_verification_vote_tx(
            &voter,
            &target,
            BlockHash::new([8; 32]),
            1,
            10,
            Timestamp::new(1000),
        )
        .unwrap();
        let block = build_state_block(
            &test_account_state(),
            &burst_transactions::Transaction::VerificationVote(transaction),
            TxHash::new([99; 32]),
            BlockHash::ZERO,
        )
        .unwrap();
        let mut block = sign_state_block(block, &keys.private);
        assert_eq!(
            block.origin,
            TxHash::new([8; 32]),
            "the previous TRST origin is not a verification context"
        );
        assert!(burst_crypto::verify_signature(
            block.hash.as_bytes(),
            &block.signature,
            &keys.public
        ));
        block.origin = TxHash::new([9; 32]);
        assert!(!burst_crypto::verify_signature(
            block.compute_hash().as_bytes(),
            &block.signature,
            &keys.public
        ));
    }

    #[test]
    fn burn_cannot_silently_saturate_the_spent_odometer() {
        let mut state = test_account_state();
        state.brn_spent = u128::MAX;
        let tx = build_burn_tx(
            &test_address("sender"),
            &test_address("receiver"),
            1,
            Timestamp::new(1),
        )
        .unwrap();
        assert!(build_state_block(
            &state,
            &burst_transactions::Transaction::Burn(tx),
            TxHash::ZERO,
            BlockHash::ZERO
        )
        .is_err());
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
        let block = build_state_block(&state, &tx, prev_origin, BlockHash::ZERO).unwrap();

        assert_eq!(block.block_type, BlockType::Send);
        assert_eq!(block.brn_spent, 10_000);
        assert_eq!(block.trst_balance, 4_800);
        assert_eq!(block.origin, prev_origin);
    }

    #[test]
    fn build_state_block_burn_increases_spent_odometer() {
        // The odometer is cumulative spending, not an available balance —
        // the wallet cannot check BRN sufficiency from it (nodes validate
        // the spend against the computed BRN(w)). This records spending
        // beyond the previous cumulative amount without interpreting it as funds.
        let state = AccountState {
            signing_grant: burst_types::BlockHash::ZERO,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            brn_spent: 50,
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
        let block = build_state_block(&state, &tx, TxHash::ZERO, BlockHash::ZERO).unwrap();
        assert_eq!(block.brn_spent, 150);
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
        let block = build_state_block(&state, &tx, TxHash::ZERO, BlockHash::ZERO).unwrap();

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
            signing_grant: burst_types::BlockHash::ZERO,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            release_sources: Vec::new(),
            head: BlockHash::ZERO,
            block_count: 0,
            representative: test_address("rep1"),
            brn_spent: 10_000,
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
        let signed =
            build_and_sign_state_block(&state, &tx, &kp.private, TxHash::ZERO, BlockHash::ZERO)
                .unwrap();
        assert!(burst_crypto::verify_signature(
            signed.hash.as_bytes(),
            &signed.signature,
            &kp.public
        ));
    }
}
