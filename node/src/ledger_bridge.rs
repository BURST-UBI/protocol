//! Bridges the block-lattice to the economic engines (TRST, BRN, verification).
//! Called after a block is accepted and persisted.

use burst_brn::{BrnEngine, Stake, StakeKind};
use burst_governance::ProposalContent;
use burst_ledger::{BlockType, StateBlock};
use burst_transactions::governance::GovernanceVote;
use burst_trst::{TrstEngine, TrstToken};
use burst_types::{BlockHash, Timestamp, WalletAddress};

/// Process a confirmed block through the economic engines.
///
/// This is the critical integration point where the block-lattice
/// meets the token lifecycle engines. Returns an `EconomicResult`
/// describing what economic effects the block had.
///
/// `prev_brn_balance` is the BRN balance from the account's previous block
/// (or 0 for the first block). Required to compute burn/stake deltas since
/// the block only stores the post-operation balance.
pub fn process_block_economics(
    block: &StateBlock,
    brn_engine: &mut BrnEngine,
    trst_engine: &mut TrstEngine,
    now: Timestamp,
    trst_expiry_secs: u64,
    prev_brn_balance: u128,
) -> EconomicResult {
    match block.block_type {
        BlockType::Burn => {
            let burn_amount = prev_brn_balance.saturating_sub(block.brn_balance);
            let receiver = extract_receiver_from_link(&block.link);
            let burn_tx_hash = block.hash.into_tx_hash();

            if let Some(receiver_addr) = receiver {
                // Attempt TRST mint BEFORE recording the BRN burn so that
                // a mint failure doesn't leave the BRN engine in a dirty state.
                let mint_token = match trst_engine.mint(
                    burn_tx_hash,
                    receiver_addr,
                    burn_amount,
                    block.account.clone(),
                    now,
                ) {
                    Ok(token) => Some(token),
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            burn_amount,
                            account = %block.account,
                            "TRST mint failed — rejecting burn to preserve BRN/TRST invariant"
                        );
                        return EconomicResult::Rejected {
                            reason: format!("TRST mint failed: {e}"),
                        };
                    }
                };
                let burn_result = record_brn_burn(brn_engine, &block.account, burn_amount, now);
                EconomicResult::BurnAndMint {
                    burn_amount,
                    burn_result,
                    mint_token,
                }
            } else {
                let burn_result = record_brn_burn(brn_engine, &block.account, burn_amount, now);
                EconomicResult::BurnOnly {
                    burn_amount,
                    burn_result,
                }
            }
        }
        BlockType::Send => {
            // TRST transfer — sender's balance decreases.
            //
            // Expiry correctness: The block processor validates that the send
            // amount does not exceed the sender's transferable balance (via
            // TrstEngine::transferable_balance). This filters out expired and
            // revoked tokens before the block is accepted. For wallets whose
            // token portfolio is tracked in the TrstEngine, this is an exact
            // check; for untracked wallets the check is skipped (the engine
            // coverage grows as new mints occur).
            //
            // The actual pending entry is created by the block processor task.
            // The TRST engine transfer is invoked when the receiver publishes
            // the corresponding Receive block.
            let receiver = extract_receiver_from_link(&block.link);
            EconomicResult::Send {
                sender: block.account.clone(),
                receiver,
                trst_balance_after: block.trst_balance,
            }
        }
        BlockType::Receive => EconomicResult::Receive {
            receiver: block.account.clone(),
            send_block_hash: block.link,
            trst_balance_after: block.trst_balance,
        },
        BlockType::Split => {
            // TRST split — one token becomes multiple tokens.
            // Expiry: the child tokens inherit the parent's origin_timestamp,
            // so each child expires at `origin_timestamp + trst_expiry_secs`.
            // The block processor validates that the split amount does not
            // exceed the sender's transferable balance (same check as Send).
            // The TrstEngine enforces that the parent token is not expired
            // before allowing a split. Once the TRST index is populated,
            // the expiry index entries should be updated for the new children.
            if trst_expiry_secs > 0 {
                tracing::trace!(
                    account = %block.account,
                    trst_expiry_secs,
                    "split block — child tokens inherit parent expiry"
                );
            }
            EconomicResult::Split {
                account: block.account.clone(),
            }
        }
        BlockType::Merge => {
            // TRST merge — multiple tokens combined into one.
            // Expiry: the merged token's effective expiry is the *earliest*
            // origin_timestamp among all merged parents + trst_expiry_secs.
            // The TrstEngine rejects merges that include expired tokens.
            // Once the TRST index is populated, old expiry entries should be
            // removed and a new entry created for the merged token.
            if trst_expiry_secs > 0 {
                tracing::trace!(
                    account = %block.account,
                    trst_expiry_secs,
                    "merge block — merged token uses earliest parent expiry"
                );
            }
            EconomicResult::Merge {
                account: block.account.clone(),
            }
        }
        BlockType::Endorse => {
            // Endorsement — the endorser permanently burns BRN to vouch for
            // another wallet's humanity. The burn amount is the delta between
            // the previous BRN balance and the post-endorsement balance.
            let burn_amount = prev_brn_balance.saturating_sub(block.brn_balance);
            let target = extract_receiver_from_link(&block.link);
            let burn_result = record_brn_burn(brn_engine, &block.account, burn_amount, now);

            EconomicResult::Endorse {
                burn_amount,
                burn_result,
                target,
            }
        }
        BlockType::Challenge => {
            // Challenge — the challenger temporarily stakes BRN to contest
            // another wallet's verification. The stake is returned if the
            // challenge succeeds, forfeited otherwise.
            let stake_amount = prev_brn_balance.saturating_sub(block.brn_balance);
            let target = extract_receiver_from_link(&block.link);
            let target_str = target
                .as_ref()
                .map(|w| w.as_str().to_string())
                .unwrap_or_default();

            let stake_result = record_brn_stake(
                brn_engine,
                &block.account,
                stake_amount,
                StakeKind::Challenge {
                    target_wallet: target_str.into(),
                },
                now,
            );

            EconomicResult::Challenge {
                stake_amount,
                stake_result,
                target,
            }
        }
        BlockType::RejectReceive => EconomicResult::RejectReceive {
            rejecter: block.account.clone(),
            send_block_hash: block.link,
        },
        BlockType::ChangeRepresentative => EconomicResult::RepChange {
            account: block.account.clone(),
            old_rep: None,
            new_rep: block.representative.clone(),
            balance: block.trst_balance,
        },
        BlockType::GovernanceProposal => {
            let proposal_hash = block.transaction;
            let content = decode_proposal_content_from_link(&block.link);
            EconomicResult::GovernanceProposal {
                proposer: block.account.clone(),
                proposal_hash,
                content,
            }
        }
        BlockType::GovernanceVote => {
            let proposal_hash = block.link.into_tx_hash();
            match decode_governance_vote(block.transaction.as_bytes()[0]) {
                Some(vote) => EconomicResult::GovernanceVote {
                    voter: block.account.clone(),
                    proposal_hash,
                    vote,
                },
                None => {
                    tracing::warn!(
                        voter = %block.account,
                        byte = block.transaction.as_bytes()[0],
                        "unknown governance vote byte, ignoring block"
                    );
                    EconomicResult::NoEconomicEffect
                }
            }
        }
        BlockType::VerificationVote => {
            let voter = block.account.clone();
            let target = extract_receiver_from_link(&block.link);
            let stake_amount = prev_brn_balance.saturating_sub(block.brn_balance);
            let vote_value = block.transaction.as_bytes()[0];
            EconomicResult::VerificationVoteResult {
                voter,
                target,
                vote: vote_value,
                stake: stake_amount,
            }
        }
        _ => EconomicResult::NoEconomicEffect,
    }
}

/// Record a BRN burn in the engine.
///
/// Temporarily removes the wallet state from the engine's map to split the
/// mutable borrow (engine vs. wallet state), then reinserts after the call.
fn record_brn_burn(
    brn_engine: &mut BrnEngine,
    account: &WalletAddress,
    amount: u128,
    now: Timestamp,
) -> Result<(), String> {
    if let Some(mut state) = brn_engine.wallets.remove(account) {
        let result = brn_engine.record_burn(&mut state, amount, now);
        brn_engine.wallets.insert(account.clone(), state);
        result.map_err(|e| e.to_string())
    } else {
        Err("wallet not tracked in BRN engine".to_string())
    }
}

/// Record a BRN stake in the engine.
///
/// Uses the same remove-reinsert pattern as [`record_brn_burn`] to satisfy
/// the borrow checker when `stake(&mut self, &mut BrnWalletState, ...)`.
fn record_brn_stake(
    brn_engine: &mut BrnEngine,
    account: &WalletAddress,
    amount: u128,
    kind: StakeKind,
    now: Timestamp,
) -> Result<Stake, String> {
    if let Some(mut state) = brn_engine.wallets.remove(account) {
        let result = brn_engine.stake(account, &mut state, amount, kind, now);
        brn_engine.wallets.insert(account.clone(), state);
        result.map_err(|e| e.to_string())
    } else {
        Err("wallet not tracked in BRN engine".to_string())
    }
}

/// Decode a `GovernanceVote` from the first byte of the transaction field.
///
/// Encoding: 0 = Yea, 1 = Nay, 2 = Abstain. Returns `None` for unknown values.
fn decode_governance_vote(byte: u8) -> Option<GovernanceVote> {
    match byte {
        0 => Some(GovernanceVote::Yea),
        1 => Some(GovernanceVote::Nay),
        2 => Some(GovernanceVote::Abstain),
        _ => None,
    }
}

/// Try to decode a `ProposalContent` from a GovernanceProposal block's link field.
///
/// The link field is expected to contain a bincode-serialized `ProposalContent`.
/// Returns `None` if the link is all zeros or deserialization fails (e.g., the
/// block was created before content encoding was implemented, or the content
/// is too large to fit in 32 bytes).
fn decode_proposal_content_from_link(link: &BlockHash) -> Option<ProposalContent> {
    let bytes = link.as_bytes();
    if bytes.iter().all(|&b| b == 0) {
        return None;
    }
    bincode::deserialize::<ProposalContent>(bytes).ok()
}

/// Extract a receiver `WalletAddress` from a block's link field.
///
/// The link field stores the receiver's 32-byte public key (encoded via
/// `burst_crypto::decode_address` on the sender side). This function
/// reconstructs the full `brst_`-prefixed address using `derive_address`.
/// Returns `None` if the link is all zeros.
pub(crate) fn extract_receiver_from_link(link: &burst_types::BlockHash) -> Option<WalletAddress> {
    let bytes = link.as_bytes();
    if bytes.iter().all(|&b| b == 0) {
        return None;
    }
    let pubkey = burst_types::PublicKey(*bytes);
    Some(burst_crypto::derive_address(&pubkey))
}

/// Create a `TrstToken` for a receiver based on the pending entry provenance.
///
/// If the pending entry has provenance from one origin, the token carries
/// that origin directly. If multiple origins were consumed (spanning a
/// send across multiple TRST tokens), the receiver gets a token with
/// `origin_proportions` — effectively a pre-merged token.
pub fn create_received_token(
    receive_block: &StateBlock,
    pending: &burst_store::pending::PendingInfo,
    _expiry_secs: u64,
) -> TrstToken {
    use burst_trst::token::OriginProportion;

    let token_id = burst_types::TxHash::new(*receive_block.hash.as_bytes());

    if pending.provenance.len() == 1 {
        let p = &pending.provenance[0];
        TrstToken {
            id: token_id,
            amount: pending.amount,
            origin: p.origin,
            link: burst_types::TxHash::new(*receive_block.hash.as_bytes()),
            holder: receive_block.account.clone(),
            origin_timestamp: p.origin_timestamp,
            effective_origin_timestamp: p.effective_origin_timestamp,
            state: burst_types::TrstState::Active,
            origin_wallet: p.origin_wallet.clone(),
            origin_proportions: p.origin_proportions.clone(),
        }
    } else if pending.provenance.len() > 1 {
        let effective_ts = pending
            .provenance
            .iter()
            .map(|p| p.effective_origin_timestamp)
            .min_by_key(|ts| ts.as_secs())
            .unwrap_or(pending.timestamp);
        let origin_ts = pending
            .provenance
            .iter()
            .map(|p| p.origin_timestamp)
            .min_by_key(|ts| ts.as_secs())
            .unwrap_or(pending.timestamp);
        let proportions: Vec<OriginProportion> = pending
            .provenance
            .iter()
            .flat_map(|p| {
                if p.origin_proportions.is_empty() {
                    vec![OriginProportion {
                        origin: p.origin,
                        origin_wallet: p.origin_wallet.clone(),
                        amount: p.amount,
                    }]
                } else {
                    p.origin_proportions.clone()
                }
            })
            .collect();
        TrstToken {
            id: token_id,
            amount: pending.amount,
            origin: token_id,
            link: burst_types::TxHash::new(*receive_block.hash.as_bytes()),
            holder: receive_block.account.clone(),
            origin_timestamp: origin_ts,
            effective_origin_timestamp: effective_ts,
            state: burst_types::TrstState::Active,
            origin_wallet: pending.source.clone(),
            origin_proportions: proportions,
        }
    } else {
        // No provenance — sender wasn't tracked. Create a basic token
        // with the sender as origin_wallet and current timestamp.
        TrstToken {
            id: token_id,
            amount: pending.amount,
            origin: token_id,
            link: burst_types::TxHash::new(*receive_block.hash.as_bytes()),
            holder: receive_block.account.clone(),
            origin_timestamp: pending.timestamp,
            effective_origin_timestamp: pending.timestamp,
            state: burst_types::TrstState::Active,
            origin_wallet: pending.source.clone(),
            origin_proportions: Vec::new(),
        }
    }
}

/// Result of processing a block through the economic engines.
#[derive(Clone, Debug)]
pub enum EconomicResult {
    /// BRN was burned and TRST was minted for a receiver.
    BurnAndMint {
        burn_amount: u128,
        burn_result: Result<(), String>,
        mint_token: Option<TrstToken>,
    },
    /// BRN was burned but no valid receiver was found.
    BurnOnly {
        burn_amount: u128,
        burn_result: Result<(), String>,
    },
    /// TRST send (pending entry created by block processor).
    Send {
        sender: WalletAddress,
        receiver: Option<WalletAddress>,
        trst_balance_after: u128,
    },
    /// TRST receive from pending.
    Receive {
        receiver: WalletAddress,
        send_block_hash: BlockHash,
        trst_balance_after: u128,
    },
    /// TRST split into multiple tokens.
    Split { account: WalletAddress },
    /// TRST merge from multiple tokens.
    Merge { account: WalletAddress },
    /// Endorsement — BRN burned to vouch for another wallet's humanity.
    Endorse {
        burn_amount: u128,
        burn_result: Result<(), String>,
        target: Option<WalletAddress>,
    },
    /// Challenge — BRN staked to contest a wallet's verification.
    Challenge {
        stake_amount: u128,
        stake_result: Result<Stake, String>,
        target: Option<WalletAddress>,
    },
    /// Representative change.
    RepChange {
        account: WalletAddress,
        old_rep: Option<WalletAddress>,
        new_rep: WalletAddress,
        balance: u128,
    },
    /// Governance proposal submitted.
    GovernanceProposal {
        proposer: WalletAddress,
        proposal_hash: burst_types::TxHash,
        content: Option<burst_governance::ProposalContent>,
    },
    /// Governance vote cast.
    GovernanceVote {
        voter: WalletAddress,
        proposal_hash: burst_types::TxHash,
        vote: burst_transactions::governance::GovernanceVote,
    },
    /// TRST receive rejected — pending entry returned to sender.
    RejectReceive {
        rejecter: WalletAddress,
        send_block_hash: BlockHash,
    },
    /// Verification vote — verifier cast a vote on a wallet's humanity.
    VerificationVoteResult {
        voter: WalletAddress,
        target: Option<WalletAddress>,
        vote: u8,
        stake: u128,
    },
    /// Block rejected due to economic invariant violation.
    Rejected { reason: String },
    /// No economic effect (e.g. epoch, delegation).
    NoEconomicEffect,
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_ledger::{BlockType, StateBlock, CURRENT_BLOCK_VERSION};
    use burst_types::{BlockHash, Signature, Timestamp, TxHash, WalletAddress};

    fn real_address_from_seed(seed: &[u8; 32]) -> WalletAddress {
        let kp = burst_crypto::keypair_from_seed(seed);
        burst_crypto::derive_address(&kp.public)
    }

    fn test_account() -> WalletAddress {
        real_address_from_seed(&[0x11; 32])
    }

    fn test_representative() -> WalletAddress {
        real_address_from_seed(&[0x22; 32])
    }

    fn test_receiver() -> WalletAddress {
        real_address_from_seed(&[0x33; 32])
    }

    fn test_target() -> WalletAddress {
        real_address_from_seed(&[0x44; 32])
    }

    fn make_burn_block_for_receiver(receiver: &WalletAddress) -> StateBlock {
        let link = match burst_crypto::decode_address(receiver.as_str()) {
            Some(pubkey) => BlockHash::new(pubkey),
            None => BlockHash::ZERO,
        };

        let mut block = StateBlock {
            version: CURRENT_BLOCK_VERSION,
            block_type: BlockType::Burn,
            account: test_account(),
            previous: BlockHash::new([0x11; 32]),
            representative: test_representative(),
            brn_balance: 500,
            trst_balance: 0,
            link,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_000),
            work: 0,
            signature: Signature([1u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block
    }

    fn make_send_block() -> StateBlock {
        let mut block = StateBlock {
            version: CURRENT_BLOCK_VERSION,
            block_type: BlockType::Send,
            account: test_account(),
            previous: BlockHash::new([0x11; 32]),
            representative: test_representative(),
            brn_balance: 1000,
            trst_balance: 50,
            link: BlockHash::new([0xAA; 32]),
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_001),
            work: 0,
            signature: Signature([2u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block
    }

    fn make_rep_change_block() -> StateBlock {
        let new_rep = real_address_from_seed(&[0x55; 32]);
        let mut block = StateBlock {
            version: CURRENT_BLOCK_VERSION,
            block_type: BlockType::ChangeRepresentative,
            account: test_account(),
            previous: BlockHash::new([0x11; 32]),
            representative: new_rep,
            brn_balance: 1000,
            trst_balance: 100,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_002),
            work: 0,
            signature: Signature([3u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block
    }

    fn make_endorse_block_for_target(target: &WalletAddress) -> StateBlock {
        let link = match burst_crypto::decode_address(target.as_str()) {
            Some(pubkey) => BlockHash::new(pubkey),
            None => BlockHash::ZERO,
        };

        let mut block = StateBlock {
            version: CURRENT_BLOCK_VERSION,
            block_type: BlockType::Endorse,
            account: test_account(),
            previous: BlockHash::new([0x11; 32]),
            representative: test_representative(),
            brn_balance: 664,
            trst_balance: 0,
            link,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_003),
            work: 0,
            signature: Signature([4u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block
    }

    fn make_challenge_block_for_target(target: &WalletAddress) -> StateBlock {
        let link = match burst_crypto::decode_address(target.as_str()) {
            Some(pubkey) => BlockHash::new(pubkey),
            None => BlockHash::ZERO,
        };

        let mut block = StateBlock {
            version: CURRENT_BLOCK_VERSION,
            block_type: BlockType::Challenge,
            account: test_account(),
            previous: BlockHash::new([0x11; 32]),
            representative: test_representative(),
            brn_balance: 0,
            trst_balance: 0,
            link,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_004),
            work: 0,
            signature: Signature([5u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block
    }

    #[test]
    fn burn_block_produces_burn_and_mint_result() {
        let mut brn_engine = BrnEngine::with_rate(10, Timestamp::new(0));
        let mut trst_engine = TrstEngine::new();
        let now = Timestamp::new(1_000_000);

        // Track the sender wallet in BRN engine
        let wallet_state = burst_brn::BrnWalletState::new(Timestamp::new(0));
        brn_engine.track_wallet(test_account(), wallet_state);

        let block = make_burn_block_for_receiver(&test_receiver());
        let prev_brn_balance: u128 = 1000;

        let result = process_block_economics(
            &block,
            &mut brn_engine,
            &mut trst_engine,
            now,
            3600,
            prev_brn_balance,
        );
        match result {
            EconomicResult::BurnAndMint {
                burn_amount,
                burn_result,
                mint_token,
            } => {
                assert_eq!(burn_amount, 500); // 1000 - 500
                assert!(burn_result.is_ok());
                assert!(mint_token.is_some());
                let token = mint_token.unwrap();
                assert_eq!(token.amount, 500);
                assert_eq!(token.origin_wallet, test_account());
            }
            _ => panic!("Expected BurnAndMint, got {:?}", result),
        }
    }

    #[test]
    fn burn_block_with_zero_link_produces_burn_only() {
        let mut brn_engine = BrnEngine::with_rate(10, Timestamp::new(0));
        let mut trst_engine = TrstEngine::new();
        let now = Timestamp::new(1_000_000);

        let wallet_state = burst_brn::BrnWalletState::new(Timestamp::new(0));
        brn_engine.track_wallet(test_account(), wallet_state);

        let mut block = StateBlock {
            version: CURRENT_BLOCK_VERSION,
            block_type: BlockType::Burn,
            account: test_account(),
            previous: BlockHash::new([0x11; 32]),
            representative: test_representative(),
            brn_balance: 500,
            trst_balance: 0,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_000),
            work: 0,
            signature: Signature([1u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();

        let prev_brn_balance: u128 = 1000;
        let result = process_block_economics(
            &block,
            &mut brn_engine,
            &mut trst_engine,
            now,
            3600,
            prev_brn_balance,
        );
        match result {
            EconomicResult::BurnOnly {
                burn_amount,
                burn_result,
            } => {
                assert_eq!(burn_amount, 500);
                assert!(burn_result.is_ok());
            }
            _ => panic!("Expected BurnOnly, got {:?}", result),
        }
    }

    #[test]
    fn send_block_produces_send_result() {
        let mut brn_engine = BrnEngine::with_rate(10, Timestamp::new(0));
        let mut trst_engine = TrstEngine::new();
        let now = Timestamp::new(1_000_000);
        let block = make_send_block();

        let result =
            process_block_economics(&block, &mut brn_engine, &mut trst_engine, now, 3600, 1000);
        match result {
            EconomicResult::Send {
                sender,
                trst_balance_after,
                ..
            } => {
                assert_eq!(sender, test_account());
                assert_eq!(trst_balance_after, 50);
            }
            _ => panic!("Expected Send, got {:?}", result),
        }
    }

    #[test]
    fn rep_change_block_captures_new_representative() {
        let mut brn_engine = BrnEngine::with_rate(10, Timestamp::new(0));
        let mut trst_engine = TrstEngine::new();
        let now = Timestamp::new(1_000_000);
        let block = make_rep_change_block();

        let result =
            process_block_economics(&block, &mut brn_engine, &mut trst_engine, now, 3600, 1000);
        match result {
            EconomicResult::RepChange {
                account,
                old_rep,
                new_rep,
                balance,
            } => {
                assert_eq!(account, test_account());
                assert!(old_rep.is_none());
                assert_eq!(
                    new_rep.as_str(),
                    real_address_from_seed(&[0x55; 32]).as_str()
                );
                assert_eq!(balance, 100);
            }
            _ => panic!("Expected RepChange, got {:?}", result),
        }
    }

    #[test]
    fn epoch_block_has_no_economic_effect() {
        let mut brn_engine = BrnEngine::with_rate(10, Timestamp::new(0));
        let mut trst_engine = TrstEngine::new();
        let now = Timestamp::new(1_000_000);

        let mut block = StateBlock {
            version: CURRENT_BLOCK_VERSION,
            block_type: BlockType::Epoch,
            account: test_account(),
            previous: BlockHash::new([0x11; 32]),
            representative: test_representative(),
            brn_balance: 1000,
            trst_balance: 100,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_000),
            work: 0,
            signature: Signature([1u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();

        let result =
            process_block_economics(&block, &mut brn_engine, &mut trst_engine, now, 3600, 1000);
        assert!(matches!(result, EconomicResult::NoEconomicEffect));
    }

    #[test]
    fn endorse_block_records_brn_burn() {
        let mut brn_engine = BrnEngine::with_rate(10, Timestamp::new(0));
        let mut trst_engine = TrstEngine::new();
        let now = Timestamp::new(1_000_000);

        let wallet_state = burst_brn::BrnWalletState::new(Timestamp::new(0));
        brn_engine.track_wallet(test_account(), wallet_state);

        let target_addr = test_target();
        let block = make_endorse_block_for_target(&target_addr);
        let prev_brn_balance: u128 = 1000;

        let result = process_block_economics(
            &block,
            &mut brn_engine,
            &mut trst_engine,
            now,
            3600,
            prev_brn_balance,
        );
        match result {
            EconomicResult::Endorse {
                burn_amount,
                burn_result,
                target,
            } => {
                assert_eq!(burn_amount, 336); // 1000 - 664
                assert!(burn_result.is_ok());
                assert!(target.is_some());
                assert_eq!(target.unwrap().as_str(), target_addr.as_str());
            }
            _ => panic!("Expected Endorse, got {:?}", result),
        }
    }

    #[test]
    fn challenge_block_records_brn_stake() {
        let mut brn_engine = BrnEngine::with_rate(10, Timestamp::new(0));
        let mut trst_engine = TrstEngine::new();
        let now = Timestamp::new(1_000_000);

        let wallet_state = burst_brn::BrnWalletState::new(Timestamp::new(0));
        brn_engine.track_wallet(test_account(), wallet_state);

        let target_addr = test_target();
        let block = make_challenge_block_for_target(&target_addr);
        let prev_brn_balance: u128 = 1000;

        let result = process_block_economics(
            &block,
            &mut brn_engine,
            &mut trst_engine,
            now,
            3600,
            prev_brn_balance,
        );
        match result {
            EconomicResult::Challenge {
                stake_amount,
                stake_result,
                target,
            } => {
                assert_eq!(stake_amount, 1000); // 1000 - 0
                assert!(stake_result.is_ok());
                let stake = stake_result.unwrap();
                assert_eq!(stake.amount, 1000);
                assert!(!stake.resolved);
                assert!(target.is_some());
                assert_eq!(target.unwrap().as_str(), target_addr.as_str());
            }
            _ => panic!("Expected Challenge, got {:?}", result),
        }
    }

    #[test]
    fn extract_receiver_from_zero_link_returns_none() {
        let link = BlockHash::ZERO;
        assert!(extract_receiver_from_link(&link).is_none());
    }

    #[test]
    fn extract_receiver_from_valid_link() {
        let expected_addr = test_receiver();
        let pubkey_bytes = burst_crypto::decode_address(expected_addr.as_str()).unwrap();
        let link = BlockHash::new(pubkey_bytes);
        let receiver = extract_receiver_from_link(&link);
        assert!(receiver.is_some());
        assert_eq!(receiver.unwrap().as_str(), expected_addr.as_str());
    }

    #[test]
    fn block_hash_into_tx_hash_preserves_bytes() {
        let bytes = [0xAB; 32];
        let block_hash = BlockHash::new(bytes);
        let tx_hash = block_hash.into_tx_hash();
        assert_eq!(*tx_hash.as_bytes(), bytes);
    }
}
