//! Bridges the block-lattice to the economic engines (TRST, BRN, verification).
//! Computes candidate effects before the caller commits and publishes them.

use burst_brn::{BrnEngine, Stake, StakeKind};
use burst_governance::ProposalContent;
use burst_ledger::{BlockType, StateBlock};
use burst_transactions::governance::GovernanceVote;
use burst_trst::{TrstEngine, TrstToken};
use burst_types::{BlockHash, Timestamp, TxHash, WalletAddress};

/// Process a candidate block against isolated economic engines.
///
/// Returns candidate effects. The caller validates remaining dependencies and
/// commits them atomically before publishing live state.
///
/// `previous_brn_spent` is the cumulative BRN spent recorded on the account's
/// previous block (or 0 for the first block). The `brn_spent` field is an
/// ascending odometer of committed BRN, so `block.brn_spent - previous_brn_spent` is the
/// burn/stake amount of this block.
pub fn process_block_economics(
    block: &StateBlock,
    brn_engine: &mut BrnEngine,
    trst_engine: &mut TrstEngine,
    now: Timestamp,
    trst_expiry_secs: u64,
    previous_brn_spent: u128,
) -> EconomicResult {
    process_with_engines(
        block,
        &mut (brn_engine, trst_engine),
        now,
        trst_expiry_secs,
        previous_brn_spent,
    )
}

pub(crate) fn process_with_engines(
    block: &StateBlock,
    engines: &mut impl crate::economic_candidate::Engines,
    now: Timestamp,
    trst_expiry_secs: u64,
    previous_brn_spent: u128,
) -> EconomicResult {
    match block.block_type {
        BlockType::ChangeRepresentative => EconomicResult::RepChange {
            account: block.account.clone(),
            old_rep: None,
            new_rep: block.representative.clone(),
            balance: block.trst_balance,
        },
        BlockType::Burn => {
            let burn_amount = block.brn_spent.saturating_sub(previous_brn_spent);
            let receiver = wallet_from_link(&block.link);
            let burn_tx_hash = block.hash.into_tx_hash();

            if burn_amount == 0 {
                return EconomicResult::Rejected {
                    reason: "burn block with zero burn amount".into(),
                };
            }

            // Record the candidate burn before constructing its matching TRST.
            if let Err(e) = record_brn_burn(engines.brn_mut(), &block.account, burn_amount, now) {
                tracing::error!(
                    error = %e,
                    burn_amount,
                    account = %block.account,
                    "BRN burn failed — rejecting block"
                );
                return EconomicResult::Rejected {
                    reason: format!("BRN burn failed: {e}"),
                };
            }

            if let Some(receiver_addr) = receiver {
                match engines.trst_mut().mint(
                    burn_tx_hash,
                    receiver_addr,
                    burn_amount,
                    block.account.clone(),
                    now,
                ) {
                    Ok(token) => EconomicResult::BurnAndMint {
                        burn_amount,
                        mint_token: Some(token),
                    },
                    Err(e) => {
                        // Undo the burn — burn and mint must be atomic.
                        undo_brn_burn(engines.brn_mut(), &block.account, burn_amount);
                        tracing::error!(
                            error = %e,
                            burn_amount,
                            account = %block.account,
                            "TRST mint failed — burn undone, block rejected"
                        );
                        EconomicResult::Rejected {
                            reason: format!("TRST mint failed: {e}"),
                        }
                    }
                }
            } else {
                EconomicResult::BurnOnly { burn_amount }
            }
        }
        BlockType::Send => {
            // Candidate processing checks transferable value for the chosen origin,
            // rejects missing sender provenance, and stages the debit plus pending
            // receipt. This result only describes the requested balance change.
            let receiver = wallet_from_link(&block.link);
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
        // An opening block can claim a positive pending payment.
        BlockType::Open if !block.link.is_zero() && block.trst_balance > 0 => {
            EconomicResult::Receive {
                receiver: block.account.clone(),
                send_block_hash: block.link,
                trst_balance_after: block.trst_balance,
            }
        }
        BlockType::Merge => {
            // Input selection, expiry checks and index changes belong to candidate
            // processing. This branch reports the requested merge only.
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
            // another wallet's humanity. The burn is the odometer delta.
            let burn_amount = block.brn_spent.saturating_sub(previous_brn_spent);
            let target = wallet_from_link(&block.link);

            if burn_amount == 0 {
                return EconomicResult::Rejected {
                    reason: "endorse block requires a BRN burn".into(),
                };
            }
            if let Err(e) = record_brn_burn(engines.brn_mut(), &block.account, burn_amount, now) {
                return EconomicResult::Rejected {
                    reason: format!("endorsement BRN burn failed: {e}"),
                };
            }

            EconomicResult::Endorse {
                burn_amount,
                target,
            }
        }
        BlockType::Challenge => {
            // Challenge — the challenger temporarily stakes BRN to contest
            // another wallet's verification. The stake is returned if the
            // challenge succeeds, forfeited otherwise.
            let stake_amount = block.brn_spent.saturating_sub(previous_brn_spent);
            let Some(target) = wallet_from_link(&block.link) else {
                return EconomicResult::Rejected {
                    reason: "challenge block requires a target wallet".into(),
                };
            };

            if stake_amount == 0 {
                return EconomicResult::Rejected {
                    reason: "challenge block requires a BRN stake".into(),
                };
            }
            let stake = match record_brn_stake(
                engines.brn_mut(),
                &block.account,
                stake_amount,
                StakeKind::Challenge {
                    target_wallet: target.clone(),
                },
                now,
            ) {
                Ok(s) => s,
                Err(e) => {
                    return EconomicResult::Rejected {
                        reason: format!("challenge BRN stake failed: {e}"),
                    }
                }
            };

            EconomicResult::Challenge {
                stake_amount,
                stake,
                target: Some(target),
            }
        }
        BlockType::RejectReceive => EconomicResult::RejectReceive {
            rejecter: block.account.clone(),
            send_block_hash: block.link,
        },
        BlockType::GovernanceEndorse => {
            let Some(burn_amount) = block.brn_spent.checked_sub(previous_brn_spent)
                .filter(|amount| *amount > 0) else {
                return EconomicResult::Rejected {
                    reason: "governance endorsement requires a positive BRN burn".into(),
                };
            };
            if let Err(reason) = record_brn_burn(engines.brn_mut(), &block.account, burn_amount, now) {
                return EconomicResult::Rejected {
                    reason: format!("governance endorsement BRN burn failed: {reason}"),
                };
            }
            EconomicResult::GovernanceEndorse { burn_amount }
        }
        BlockType::GovernanceProposal => {
            // Identity binds content, signer and account position. A caller-
            // chosen transaction field must not squat on another proposal id.
            let proposal_hash = block.hash.into_tx_hash();
            let content = decode_proposal_content(&block.link, &block.origin);
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
            let target = wallet_from_link(&block.link);
            let stake_amount = block.brn_spent.saturating_sub(previous_brn_spent);
            let vote_value = block.transaction.as_bytes()[0];

            // Record the submitted stake delta in the candidate. Vote admission
            // separately checks that Legitimate/Illegitimate use the required
            // stake and Neither uses zero.
            if stake_amount > 0 {
                let target_addr = target.clone().unwrap_or_else(|| voter.clone());
                if let Err(e) = record_brn_stake(
                    engines.brn_mut(),
                    &block.account,
                    stake_amount,
                    StakeKind::Verification {
                        target_wallet: target_addr,
                    },
                    now,
                ) {
                    return EconomicResult::Rejected {
                        reason: format!("verification vote BRN stake failed: {e}"),
                    };
                }
            }

            EconomicResult::VerificationVote {
                voter,
                target,
                vote: vote_value,
                stake: stake_amount,
            }
        }
        BlockType::GovernanceActivation => {
            let proposal_hash = burst_types::TxHash::new(*block.link.as_bytes());
            let new_params_hash = BlockHash::new(*block.transaction.as_bytes());
            EconomicResult::GovernanceActivation {
                proposal_hash,
                new_params_hash,
            }
        }
        _ => EconomicResult::NoEconomicEffect,
    }
}

/// Record a BRN burn in the engine.
///
/// Temporarily removes the wallet state from the engine's map to split the
/// mutable borrow (engine vs. wallet state), then reinserts after the call.
pub(crate) fn record_brn_burn(
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

/// Undo a just-recorded BRN burn (compensating action when the paired TRST
/// mint fails — burn and mint must be atomic).
fn undo_brn_burn(brn_engine: &mut BrnEngine, account: &WalletAddress, amount: u128) {
    if let Some(state) = brn_engine.wallets.get_mut(account) {
        state.total_burned = state.total_burned.saturating_sub(amount);
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

/// Decode a `ProposalContent` from a GovernanceProposal block.
///
/// Eviction/reinstatement proposals are recognised by the `origin` sentinel;
/// their `link` holds the target representative's 32-byte pubkey (the on-chain
/// justification `reason` is not carried — it is a UX/audit field only). All
/// other proposals decode canonical zero-padded content from `link`.
/// A zero link encodes a zero BRN rate; it is not an absent proposal. Full
/// content is resolved separately by the governance admission path.
fn decode_proposal_content(link: &BlockHash, origin: &TxHash) -> Option<ProposalContent> {
    let origin_bytes = *origin.as_bytes();
    if origin_bytes == burst_governance::ORV_EVICT_MARKER {
        return wallet_from_link(link).map(|target| {
            ProposalContent::RepresentativeEviction {
                target,
                evict: true,
                reason: String::new(),
            }
        });
    }
    if origin_bytes == burst_governance::ORV_REINSTATE_MARKER {
        return wallet_from_link(link).map(|target| {
            ProposalContent::RepresentativeEviction {
                target,
                evict: false,
                reason: String::new(),
            }
        });
    }
    if !origin.is_zero() {
        return None;
    }
    ProposalContent::decode_compact(link.as_bytes()).ok()
}

/// Decode an account-valued link as a wallet address.
///
/// The link field stores the receiver's 32-byte public key (encoded via
/// `burst_crypto::decode_address` on the sender side). This function
/// reconstructs the full `brst_`-prefixed address using `derive_address`.
/// Returns `None` if the link is all zeros.
pub(crate) fn wallet_from_link(link: &burst_types::BlockHash) -> Option<WalletAddress> {
    let bytes = link.as_bytes();
    if bytes.iter().all(|&b| b == 0) {
        return None;
    }
    let pubkey = burst_types::PublicKey(*bytes);
    Some(burst_crypto::derive_address(&pubkey))
}

/// Validate the portions of a single-origin pending payment without flattening
/// their distinct composition histories.
pub(crate) fn validate_pending(pending: &burst_store::pending::PendingInfo) -> Result<(), String> {
    burst_store::pending::validate_pending_provenance(pending)
}

/// Deliver all compositions as one ordered run after validating the payment.
pub(crate) fn deliver_pending(
    trst: &mut TrstEngine,
    pending: &burst_store::pending::PendingInfo,
    template: TrstToken,
    now: Timestamp,
) -> Result<(), String> {
    if pending.return_credit {
        return Err("expiry return credit cannot deliver tokens twice".into());
    }
    validate_pending(pending)?;
    let portions = pending.provenance.iter().enumerate().map(|(index, portion)| {
        let mut token = template.clone();
        if pending.provenance.len() > 1 {
            token.id = burst_types::TxHash::new(burst_crypto::blake2b_256_multi(&[
                b"BURST/pending-composition/v1\0",
                template.id.as_bytes(),
                &(index as u64).to_be_bytes(),
            ]));
        }
        token.basis = portion.basis;
        token.amount = portion.amount;
        (token, &portion.revoked_amounts)
    });
    trst.receive_settled_batch(portions, now).map_err(|e| e.to_string())
}

/// Create a `TrstToken` for a receiver based on the pending entry provenance.
///
/// This constructs the unsigned delivery shape for one origin. The caller
/// must apply the pending entry's settled revoked chunks before tracking it.
/// The token's `link` identifies the source send or burn block.
pub fn create_received_token(
    receive_block: &StateBlock,
    pending: &burst_store::pending::PendingInfo,
    _expiry_secs: u64,
) -> TrstToken {
    let token_id = burst_types::TxHash::new(*receive_block.hash.as_bytes());
    let send_tx = burst_types::TxHash::new(*receive_block.link.as_bytes());

    // The delivery writer expands this template into distinct composition portions.
    if let Some(p) = pending.provenance.first() {
        TrstToken {
            basis: p.basis,
            id: token_id,
            amount: pending.amount,
            origin: p.origin,
            link: send_tx,
            holder: receive_block.account.clone(),
            origin_timestamp: p.origin_timestamp,
            effective_origin_timestamp: p.effective_origin_timestamp,
            state: burst_types::TrstState::Active,
            origin_wallet: p.origin_wallet.clone(),
            revoked_origin: None,
        }
    } else {
        // Fallback shape for callers supplying no provenance. Construction alone
        // does not establish backing; pending validation and delivery must
        // still accept the entry before this token can be tracked.
        tracing::warn!(
            receiver = %receive_block.account,
            send_tx = %send_tx,
            amount = pending.amount,
            "pending entry has no provenance — anchoring received token at the send tx"
        );
        TrstToken {
            basis: Default::default(),
            id: token_id,
            amount: pending.amount,
            origin: send_tx,
            link: send_tx,
            holder: receive_block.account.clone(),
            origin_timestamp: pending.timestamp,
            effective_origin_timestamp: pending.timestamp,
            state: burst_types::TrstState::Active,
            origin_wallet: pending.source.clone(),
            revoked_origin: None,
        }
    }
}

/// Construct the holding returned to the payment sender after pending expiry.
/// Preserves provenance and age; delivery and return-credit creation are separate
/// caller responsibilities. Expired value is not evidence of reputation.
pub fn create_returned_token(
    destination: &WalletAddress,
    send_block_hash: &burst_types::TxHash,
    pending: &burst_store::pending::PendingInfo,
) -> TrstToken {
    // Deterministic id: every node derives the same token without a
    // corresponding on-chain transaction.
    let token_id = burst_types::TxHash::new(burst_crypto::blake2b_256_multi(&[
        send_block_hash.as_bytes(),
        destination.as_str().as_bytes(),
        b"trst-pending-return",
    ]));

    if let Some(p) = pending.provenance.first() {
        TrstToken {
            basis: p.basis,
            id: token_id,
            amount: pending.amount,
            origin: p.origin,
            link: *send_block_hash,
            holder: pending.source.clone(),
            origin_timestamp: p.origin_timestamp,
            effective_origin_timestamp: p.effective_origin_timestamp,
            state: burst_types::TrstState::Active, // receive_token normalizes to Expired
            origin_wallet: p.origin_wallet.clone(),
            revoked_origin: None,
        }
    } else {
        TrstToken {
            basis: Default::default(),
            id: token_id,
            amount: pending.amount,
            origin: token_id,
            link: *send_block_hash,
            holder: pending.source.clone(),
            origin_timestamp: pending.timestamp,
            effective_origin_timestamp: pending.timestamp,
            state: burst_types::TrstState::Active,
            origin_wallet: pending.source.clone(),
            revoked_origin: None,
        }
    }
}

/// Candidate effects, before durable commit and publication.
/// Some variants describe work the caller must still validate and apply.
#[derive(Clone, Debug)]
pub enum EconomicResult {
    /// The candidate burn succeeded and its matching TRST token was constructed.
    /// Persistence and pending-receipt creation remain the caller's responsibility.
    BurnAndMint {
        burn_amount: u128,
        mint_token: Option<TrstToken>,
    },
    /// BRN was burned but no valid receiver was found.
    BurnOnly { burn_amount: u128 },
    /// Send intent; the caller stages the pending entry.
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
    /// TRST merge from multiple tokens.
    Merge { account: WalletAddress },
    /// Endorsement — BRN burned to vouch for another wallet's humanity.
    /// The burn was recorded in the isolated candidate.
    Endorse {
        burn_amount: u128,
        target: Option<WalletAddress>,
    },
    /// Challenge — BRN staked to contest a wallet's verification.
    /// The stake was recorded in the isolated candidate.
    Challenge {
        stake_amount: u128,
        stake: Stake,
        target: Option<WalletAddress>,
    },
    /// Representative change.
    RepChange {
        account: WalletAddress,
        old_rep: Option<WalletAddress>,
        new_rep: WalletAddress,
        balance: u128,
    },
    /// Proposal support burn recorded in the isolated economic candidate.
    GovernanceEndorse { burn_amount: u128 },
    /// Proposal contents for the governance transition.
    GovernanceProposal {
        proposer: WalletAddress,
        proposal_hash: burst_types::TxHash,
        content: Option<burst_governance::ProposalContent>,
    },
    /// Vote intent for the governance transition.
    GovernanceVote {
        voter: WalletAddress,
        proposal_hash: burst_types::TxHash,
        vote: burst_transactions::governance::GovernanceVote,
    },
    /// Request to reject a receipt; the caller must stage its return to the sender.
    RejectReceive {
        rejecter: WalletAddress,
        send_block_hash: BlockHash,
    },
    /// Humanity vote and stake for the verification transition.
    VerificationVote {
        voter: WalletAddress,
        target: Option<WalletAddress>,
        vote: u8,
        stake: u128,
    },
    /// Activation intent; this result does not authorize a parameter change.
    GovernanceActivation {
        proposal_hash: burst_types::TxHash,
        new_params_hash: BlockHash,
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
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Burn,
            account: test_account(),
            previous: BlockHash::new([0x11; 32]),
            representative: test_representative(),
            // Ascending odometer: prev 1000 spent + 500 burned now.
            brn_spent: 1500,
            trst_balance: 0,
            link,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_000),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([1u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        block
    }

    fn make_send_block() -> StateBlock {
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Send,
            account: test_account(),
            previous: BlockHash::new([0x11; 32]),
            representative: test_representative(),
            brn_spent: 1000,
            trst_balance: 50,
            link: BlockHash::new([0xAA; 32]),
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_001),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
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
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::ChangeRepresentative,
            account: test_account(),
            previous: BlockHash::new([0x11; 32]),
            representative: new_rep,
            brn_spent: 1000,
            trst_balance: 100,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_002),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
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
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Endorse,
            account: test_account(),
            previous: BlockHash::new([0x11; 32]),
            representative: test_representative(),
            // Ascending odometer: prev 1000 spent + 336 burned now.
            brn_spent: 1336,
            trst_balance: 0,
            link,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_003),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
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
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Challenge,
            account: test_account(),
            previous: BlockHash::new([0x11; 32]),
            representative: test_representative(),
            // Ascending odometer: prev 1000 spent + 1000 staked now.
            brn_spent: 2000,
            trst_balance: 0,
            link,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_004),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
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
        let previous_brn_spent: u128 = 1000;

        let result = process_block_economics(
            &block,
            &mut brn_engine,
            &mut trst_engine,
            now,
            3600,
            previous_brn_spent,
        );
        match result {
            EconomicResult::BurnAndMint {
                burn_amount,
                mint_token,
            } => {
                assert_eq!(burn_amount, 500); // 1500 - 1000
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
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Burn,
            account: test_account(),
            previous: BlockHash::new([0x11; 32]),
            representative: test_representative(),
            // Ascending odometer: prev 1000 spent + 500 burned now.
            brn_spent: 1500,
            trst_balance: 0,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_000),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([1u8; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();

        let previous_brn_spent: u128 = 1000;
        let result = process_block_economics(
            &block,
            &mut brn_engine,
            &mut trst_engine,
            now,
            3600,
            previous_brn_spent,
        );
        match result {
            EconomicResult::BurnOnly { burn_amount } => {
                assert_eq!(burn_amount, 500);
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
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Epoch,
            account: test_account(),
            previous: BlockHash::new([0x11; 32]),
            representative: test_representative(),
            brn_spent: 1000,
            trst_balance: 100,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1_000_000),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
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
        let previous_brn_spent: u128 = 1000;

        let result = process_block_economics(
            &block,
            &mut brn_engine,
            &mut trst_engine,
            now,
            3600,
            previous_brn_spent,
        );
        match result {
            EconomicResult::Endorse {
                burn_amount,
                target,
            } => {
                assert_eq!(burn_amount, 336); // 1336 - 1000
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
        let previous_brn_spent: u128 = 1000;

        let result = process_block_economics(
            &block,
            &mut brn_engine,
            &mut trst_engine,
            now,
            3600,
            previous_brn_spent,
        );
        match result {
            EconomicResult::Challenge {
                stake_amount,
                stake,
                target,
            } => {
                assert_eq!(stake_amount, 1000); // 2000 - 1000
                assert_eq!(stake.amount, 1000);
                assert!(!stake.resolved);
                assert!(target.is_some());
                assert_eq!(target.unwrap().as_str(), target_addr.as_str());
            }
            _ => panic!("Expected Challenge, got {:?}", result),
        }
    }

    #[test]
    fn wallet_from_zero_link_returns_none() {
        let link = BlockHash::ZERO;
        assert!(wallet_from_link(&link).is_none());
    }

    #[test]
    fn wallet_from_valid_link() {
        let expected_addr = test_receiver();
        let pubkey_bytes = burst_crypto::decode_address(expected_addr.as_str()).unwrap();
        let link = BlockHash::new(pubkey_bytes);
        let receiver = wallet_from_link(&link);
        assert!(receiver.is_some());
        assert_eq!(receiver.unwrap().as_str(), expected_addr.as_str());
    }

    #[test]
    fn governance_proposal_decoder_preserves_zero_rate_and_rejects_extra_payload() {
        let content = ProposalContent::ParameterChange {
            param: burst_governance::GovernableParam::BrnRate,
            new_value: 0,
        };
        let mut link = content.encode_compact().unwrap();
        assert_eq!(link, [0; 32]);
        assert_eq!(
            decode_proposal_content(&BlockHash::new(link), &TxHash::ZERO),
            Some(content)
        );
        assert!(decode_proposal_content(
            &BlockHash::new(link),
            &TxHash::new(burst_governance::content::FULL_CONTENT_MARKER)
        )
        .is_none());
        assert!(decode_proposal_content(&BlockHash::new(link), &TxHash::new([7; 32])).is_none());
        link[31] = 1;
        assert!(decode_proposal_content(&BlockHash::new(link), &TxHash::ZERO).is_none());
    }

    #[test]
    fn block_hash_into_tx_hash_preserves_bytes() {
        let bytes = [0xAB; 32];
        let block_hash = BlockHash::new(bytes);
        let tx_hash = block_hash.into_tx_hash();
        assert_eq!(*tx_hash.as_bytes(), bytes);
    }
}
