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
/// This is the critical integration point where the block-lattice
/// meets the token lifecycle engines. Returns an `EconomicResult`
/// describing what economic effects the block had.
///
/// `prev_brn_balance` is the cumulative BRN spent recorded on the account's
/// previous block (or 0 for the first block). The `brn_balance` field is an
/// ascending odometer of committed BRN, so `block.brn_balance - prev` is the
/// burn/stake amount of this block.
pub fn process_block_economics(
    block: &StateBlock,
    brn_engine: &mut BrnEngine,
    trst_engine: &mut TrstEngine,
    now: Timestamp,
    trst_expiry_secs: u64,
    prev_brn_balance: u128,
) -> EconomicResult {
    process_with_engines(
        block,
        &mut (brn_engine, trst_engine),
        now,
        trst_expiry_secs,
        prev_brn_balance,
    )
}

pub(crate) fn process_with_engines(
    block: &StateBlock,
    engines: &mut impl crate::economic_candidate::Engines,
    now: Timestamp,
    trst_expiry_secs: u64,
    prev_brn_balance: u128,
) -> EconomicResult {
    match block.block_type {
        BlockType::ChangeRepresentative => EconomicResult::RepChange {
            account: block.account.clone(),
            old_rep: None,
            new_rep: block.representative.clone(),
            balance: block.trst_balance,
        },
        BlockType::Burn => {
            let burn_amount = block.brn_balance.saturating_sub(prev_brn_balance);
            let receiver = extract_receiver_from_link(&block.link);
            let burn_tx_hash = block.hash.into_tx_hash();

            if burn_amount == 0 {
                return EconomicResult::Rejected {
                    reason: "burn block with zero burn amount".into(),
                };
            }

            // Record the BRN burn FIRST — TRST may only ever be minted from
            // successfully burned BRN (1:1). A failed burn rejects the block
            // before anything is minted or persisted.
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
        // An Open block with a non-zero link is a receive-open (Nano-style):
        // the account's first block pockets a pending send.
        BlockType::Open if !block.link.is_zero() && block.trst_balance > 0 => {
            EconomicResult::Receive {
                receiver: block.account.clone(),
                send_block_hash: block.link,
                trst_balance_after: block.trst_balance,
            }
        }
        BlockType::Merge => {
            // TRST merge — multiple tokens combined into one.
            // Expiry: the merged token's effective expiry is the *earliest*
            // origin_timestamp among all merged parents + trst_expiry_secs.
            // Expired tokens can be consolidated together, never mixed with live tokens.
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
            // another wallet's humanity. The burn is the odometer delta.
            let burn_amount = block.brn_balance.saturating_sub(prev_brn_balance);
            let target = extract_receiver_from_link(&block.link);

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
            let stake_amount = block.brn_balance.saturating_sub(prev_brn_balance);
            let Some(target) = extract_receiver_from_link(&block.link) else {
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
            let target = extract_receiver_from_link(&block.link);
            let stake_amount = block.brn_balance.saturating_sub(prev_brn_balance);
            let vote_value = block.transaction.as_bytes()[0];

            // Legitimate/Illegitimate votes stake BRN; "Neither" votes carry
            // no stake (whitepaper §Verifiers). Record the lock so the
            // computed available balance reflects it.
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

            EconomicResult::VerificationVoteResult {
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
/// content transfer is not yet connected to signed block admission.
fn decode_proposal_content(link: &BlockHash, origin: &TxHash) -> Option<ProposalContent> {
    let obytes = *origin.as_bytes();
    if obytes == burst_governance::ORV_EVICT_MARKER {
        return extract_receiver_from_link(link).map(|target| {
            ProposalContent::RepresentativeEviction {
                target,
                evict: true,
                reason: String::new(),
            }
        });
    }
    if obytes == burst_governance::ORV_REINSTATE_MARKER {
        return extract_receiver_from_link(link).map(|target| {
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

/// Validate the portions of a single-origin pending payment without flattening
/// their distinct composition histories.
pub(crate) fn validate_pending(pending: &burst_store::pending::PendingInfo) -> Result<(), String> {
    let first = pending
        .provenance
        .first()
        .ok_or("missing pending provenance")?;
    if pending.amount == 0
        || pending.provenance.iter().any(|p| {
            p.amount == 0
                || p.origin != first.origin
                || p.origin_wallet != first.origin_wallet
                || p.origin_timestamp != first.origin_timestamp
                || p.effective_origin_timestamp != first.effective_origin_timestamp
                || p.revoked_amounts
                    .iter()
                    .any(|(origin, chunk)| *origin == [0; 32] || chunk.amount == 0)
                || p.revoked_amounts
                    .values()
                    .try_fold(0u128, |sum, chunk| sum.checked_add(chunk.amount))
                    .is_none_or(|sum| sum > p.amount)
        })
        || pending
            .provenance
            .iter()
            .try_fold(0u128, |sum, p| sum.checked_add(p.amount))
            != Some(pending.amount)
    {
        return Err("inconsistent pending provenance".into());
    }
    Ok(())
}

/// Deliver each composition separately. The caller owns a disposable candidate
/// engine, so a later invalid portion cannot leave a partially applied payment.
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
    for (index, portion) in pending.provenance.iter().enumerate() {
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
        trst.receive_settled_token(token, &portion.revoked_amounts, now)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Create a `TrstToken` for a receiver based on the pending entry provenance.
///
/// This constructs the unsigned delivery shape for one origin. The caller
/// must apply the pending entry's settled revoked chunks before tracking it.
/// The token's `link` points at the SEND transaction it derives from
/// (whitepaper: "hash of the immediately preceding transaction").
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
        // No provenance — the sender wasn't tracked in the TRST engine when
        // the pending entry was created. The amount is still fully backed by
        // the on-chain send (validated against a real pending entry), but
        // deep provenance is unavailable; anchor the token at the send tx.
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

/// Create the token returned to the sender when a pending send's TRST expires
/// before the receiver claims it (IMPLEMENTATION_DECISIONS 6.16a).
///
/// The token keeps its original provenance and expiry timeline — it comes
/// back expired (that's why it's being returned), counting toward the
/// sender's expired/reputation balance rather than the receiver's.
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

/// Result of processing a block through the economic engines.
#[derive(Clone, Debug)]
pub enum EconomicResult {
    /// BRN was burned and TRST was minted for a receiver.
    /// The burn is recorded before the mint; both succeeded (atomicity is
    /// enforced by `process_block_economics` returning `Rejected` otherwise).
    BurnAndMint {
        burn_amount: u128,
        mint_token: Option<TrstToken>,
    },
    /// BRN was burned but no valid receiver was found.
    BurnOnly { burn_amount: u128 },
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
    /// TRST merge from multiple tokens.
    Merge { account: WalletAddress },
    /// Endorsement — BRN burned to vouch for another wallet's humanity.
    /// The burn was recorded successfully (failures reject the block).
    Endorse {
        burn_amount: u128,
        target: Option<WalletAddress>,
    },
    /// Challenge — BRN staked to contest a wallet's verification.
    /// The stake was recorded successfully (failures reject the block).
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
    /// Governance activation — a parameter change is being applied on-chain.
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
            brn_balance: 1500,
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
            brn_balance: 1000,
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
            brn_balance: 1000,
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
            brn_balance: 1336,
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
            brn_balance: 2000,
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
            brn_balance: 1500,
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
            brn_balance: 1000,
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
