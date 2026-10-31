use super::{account_not_found, governance_projection, deserialize_proposal, require_faucet, parse_private_key,
    signing_predecessor, build_and_sign_block, submit_and_confirm};
use super::helpers::{validate_hash, parse_tx_hash, to_value};
use crate::{error::RpcError, pagination, state::RpcState};
use burst_governance::Proposal;
use burst_types::{BlockHash, Timestamp, TxHash};
use burst_store::StoreError;

/// Return hash-ordered proposals, projecting scheduled phases when a reader or
/// engine is available. The store-only fallback skips unreadable proposal rows;
/// its cursor advances over selected hashes, including skipped rows.
pub async fn handle_governance_proposals(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: GovernanceProposalsRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;

    let count = req.pagination.effective_count();
    let (after, offset) = pagination::decode_proposal_cursor(req.pagination.cursor.as_deref())
        .map_err(|error| RpcError::InvalidRequest(error.into()))?;

    if let Some(page) = governance_projection::page(state, after, offset, count).await? {
        let proposals = page
            .proposals
            .iter()
            .map(|projected| {
                let proposal = &projected.proposal;
                let (votes_yea, votes_nay) = current_vote_counts(proposal);
                ProposalSummary {
                    hash: proposal.hash.to_string(),
                    proposer: proposal.proposer.to_string(),
                    phase: format!("{:?}", proposal.phase),
                    phase_projected: projected.phase_projected,
                    description: proposal_description(proposal),
                    votes_yea,
                    votes_nay,
                }
            })
            .collect();
        let cursor = page
            .next_hash
            .as_ref()
            .map(pagination::encode_proposal_cursor);
        return Ok(to_value(&GovernanceProposalsResponse { proposals, cursor }));
    }

    let mut page_hashes =
        state
            .governance_store
            .proposal_hash_page(after.as_ref(), offset, count as usize + 1)?;
    let has_more = page_hashes.len() > count as usize;
    if has_more {
        page_hashes.pop();
    }
    let cursor = if has_more {
        page_hashes.last().map(pagination::encode_proposal_cursor)
    } else {
        None
    };

    let mut proposals = Vec::with_capacity(page_hashes.len());
    for proposal_hash in &page_hashes {
        if let Ok(data) = state.governance_store.get_proposal(proposal_hash) {
            if let Ok(proposal) = deserialize_proposal(&data) {
                let description = proposal_description(&proposal);
                let (votes_yea, votes_nay) = current_vote_counts(&proposal);

                proposals.push(ProposalSummary {
                    hash: format!("{}", proposal_hash),
                    proposer: proposal.proposer.to_string(),
                    phase: format!("{:?}", proposal.phase),
                    phase_projected: false,
                    description,
                    votes_yea,
                    votes_nay,
                });
            }
        }
    }

    Ok(to_value(&GovernanceProposalsResponse { proposals, cursor }))
}

/// Extract a human-readable description from proposal content.
fn proposal_description(proposal: &Proposal) -> String {
    match &proposal.content {
        burst_governance::ProposalContent::ParameterChange { param, new_value } => {
            format!("Change {:?} to {}", param, new_value)
        }
        burst_governance::ProposalContent::ConstitutionalAmendment { title, .. }
        | burst_governance::ProposalContent::ConstitutionalDiff { title, .. } => {
            format!("Constitutional amendment: {}", title)
        }
        burst_governance::ProposalContent::Emergency {
            description, param, ..
        } => {
            format!("EMERGENCY {:?}: {}", param, description)
        }
        burst_governance::ProposalContent::RepresentativeEviction {
            target,
            evict,
            reason,
        } => {
            let verb = if *evict { "Evict" } else { "Reinstate" };
            format!(
                "{} representative {} from ORV set: {}",
                verb, target, reason
            )
        }
    }
}

/// Return the current voting phase's counts, or both rounds' sums in other phases.
fn current_vote_counts(proposal: &Proposal) -> (u32, u32) {
    match proposal.phase {
        burst_governance::GovernancePhase::Exploration => (
            proposal.exploration_votes_yea,
            proposal.exploration_votes_nay,
        ),
        burst_governance::GovernancePhase::Promotion => {
            (proposal.promotion_votes_yea, proposal.promotion_votes_nay)
        }
        _ => (
            proposal.exploration_votes_yea + proposal.promotion_votes_yea,
            proposal.exploration_votes_nay + proposal.promotion_votes_nay,
        ),
    }
}

pub async fn handle_governance_vote(
    _params: serde_json::Value,
    _state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    // A public address is not authorization. Real governance stays available
    // through the signed DAG transaction path used by the desktop wallet.
    Err(RpcError::InvalidRequest(
        "governance votes require a signed GovernanceVote block submitted through process".into(),
    ))
}

#[cfg(test)]
mod governance_response_tests {
    use super::GovernanceProposalInfoResponse;

    #[test]
    fn amendment_text_is_complete_and_absent_for_other_proposals() {
        let text = "Article 1\nEvery verified person has an equal vote.";
        let mut response = GovernanceProposalInfoResponse {
            review: burst_governance::proposal::ProposalReview {
                hash: "a".repeat(64),
                phase: "Exploration".into(),
                phase_projected: true,
                phase_deadline: 2,
            },
            proposer: "brst_fixture".into(),
            description: "Amend article 1".into(),
            param: "constitution".into(),
            proposed_value: "Article 1".into(),
            votes_yea: 0,
            votes_nay: 0,
            votes_abstain: 0,
            created_at: 1,
            amendment_text: Some(text.into()),
            amendment_operations: None,
            activation_conflict: None,
        };
        assert_eq!(
            serde_json::to_value(&response).unwrap()["amendment_text"],
            text
        );
        response.amendment_text = None;
        assert!(serde_json::to_value(&response)
            .unwrap()
            .get("amendment_text")
            .is_none());
    }
}

pub async fn handle_governance_proposal_info(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: GovernanceProposalInfoRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_hash(&req.hash)?;

    let proposal_hash = parse_tx_hash(&req.hash)?;
    // Use the same read-only phase projection as governance_proposals. The legacy
    // proposal table is not the live node's governance state.
    let (proposal, phase_projected, timing, current_params, activation_conflict) =
        if let Some(projected) = governance_projection::info(state, proposal_hash).await? {
            projected
        } else {
            let data = state
                .governance_store
                .get_proposal(&proposal_hash)
                .map_err(|e| match e {
                    StoreError::NotFound(_) => RpcError::ProposalNotFound(req.hash.clone()),
                    other => RpcError::Store(other.to_string()),
                })?;
            let proposal = deserialize_proposal(&data)?;
            let current_params = state.current_params()?;
            let timing =
                burst_governance::GovernanceTiming::new(&current_params, &proposal.content);
            let conflict = state
                .consti_engine
                .lock()
                .await
                .activation_conflict(&proposal_hash);
            (proposal, false, timing, current_params, conflict)
        };

    let (param, proposed_value) = match &proposal.content {
        burst_governance::ProposalContent::ParameterChange { param, new_value } => {
            (format!("{:?}", param), new_value.to_string())
        }
        burst_governance::ProposalContent::ConstitutionalAmendment { title, .. }
        | burst_governance::ProposalContent::ConstitutionalDiff { title, .. } => {
            ("constitution".to_string(), title.clone())
        }
        burst_governance::ProposalContent::Emergency {
            param, new_value, ..
        } => (format!("{:?}", param), new_value.to_string()),
        burst_governance::ProposalContent::RepresentativeEviction { target, evict, .. } => (
            if *evict { "evict_rep" } else { "reinstate_rep" }.to_string(),
            target.to_string(),
        ),
    };

    let (votes_yea, votes_nay, votes_abstain) = match proposal.phase {
        burst_governance::GovernancePhase::Exploration => (
            proposal.exploration_votes_yea,
            proposal.exploration_votes_nay,
            proposal.exploration_votes_abstain,
        ),
        burst_governance::GovernancePhase::Promotion => (
            proposal.promotion_votes_yea,
            proposal.promotion_votes_nay,
            proposal.promotion_votes_abstain,
        ),
        _ => (
            proposal.exploration_votes_yea + proposal.promotion_votes_yea,
            proposal.exploration_votes_nay + proposal.promotion_votes_nay,
            proposal.exploration_votes_abstain + proposal.promotion_votes_abstain,
        ),
    };

    let phase_deadline = compute_phase_deadline(&proposal, &current_params, timing)?;
    let amendment_text = match &proposal.content {
        burst_governance::ProposalContent::ConstitutionalAmendment { text, .. } => {
            Some(text.clone())
        }
        _ => None,
    };
    let amendment_operations = match &proposal.content {
        burst_governance::ProposalContent::ConstitutionalDiff { operations, .. } => {
            Some(operations.clone())
        }
        _ => None,
    };

    Ok(to_value(&GovernanceProposalInfoResponse {
        review: burst_governance::proposal::ProposalReview {
            hash: req.hash,
            phase: format!("{:?}", proposal.phase),
            phase_projected,
            phase_deadline,
        },
        proposer: proposal.proposer.to_string(),
        description: proposal_description(&proposal),
        param,
        proposed_value,
        votes_yea,
        votes_nay,
        votes_abstain,
        created_at: proposal.created_at.as_secs(),
        amendment_text,
        amendment_operations,
        activation_conflict,
    }))
}

/// Return the displayed Unix-second deadline, or zero for an unhandled phase
/// or missing start/activation time. Cooldown prefers a scheduled activation time.
fn compute_phase_deadline(
    proposal: &Proposal,
    params: &burst_types::ProtocolParams,
    timing: burst_governance::GovernanceTiming,
) -> Result<u64, RpcError> {
    let deadline = match proposal.phase {
        burst_governance::GovernancePhase::Proposal => timing
            .proposal_end(proposal.created_at)
            .map_err(|error| RpcError::Store(error.to_string()))?
            .as_secs(),
        burst_governance::GovernancePhase::Exploration => proposal
            .exploration_started_at
            .map(|t| timing.voting_end(t).map(|at| at.as_secs()))
            .transpose()
            .map_err(|error| RpcError::Store(error.to_string()))?
            .unwrap_or(0),
        burst_governance::GovernancePhase::Cooldown => proposal
            .activation_at
            .map(|at| at.as_secs())
            .unwrap_or_else(|| {
                proposal
                    .cooldown_started_at
                    .map(|t| {
                        t.as_secs()
                            .saturating_add(params.governance_cooldown_duration_secs)
                    })
                    .unwrap_or(0)
            }),
        burst_governance::GovernancePhase::Activation => {
            proposal.activation_at.map(|t| t.as_secs()).unwrap_or(0)
        }
        _ => 0,
    };
    Ok(deadline)
}

/// Test-network helper that signs a compact parameter proposal with the supplied
/// key. Requires an enabled faucet and waits for confirmation before returning.
pub async fn handle_governance_propose_simple(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    require_faucet(state)?;

    let req: GovernanceProposeSimpleRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    let private_key = parse_private_key(&req.private_key)?;
    let public_key = burst_crypto::public_from_private(&private_key);
    let address = burst_crypto::derive_address(&public_key);

    let account = state
        .account_store
        .get_account(&address)
        .map_err(|e| account_not_found(e, address.as_str()))?;

    if account.head == BlockHash::ZERO {
        return Err(RpcError::InvalidRequest(
            "account has no blocks yet — burn BRN first".into(),
        ));
    }

    let param = parse_governable_param(&req.param)?;
    let new_value: u128 = req
        .new_value
        .parse()
        .map_err(|e| RpcError::InvalidRequest(format!("invalid new_value: {e}")))?;

    let content = burst_governance::ProposalContent::ParameterChange {
        param: param.clone(),
        new_value,
    };
    let link = BlockHash::new(
        content
            .encode_compact()
            .map_err(|e| RpcError::InvalidRequest(e.to_string()))?,
    );

    let now = Timestamp::now();

    let tx_hash = TxHash::new(burst_crypto::blake2b_256(
        &[
            address.as_str().as_bytes(),
            &now.as_secs().to_be_bytes(),
            b"governance_propose",
        ]
        .concat(),
    ));

    let (spent_brn, signing_grant) = signing_predecessor(state, &account.head)?;
    let private_key_bytes = private_key.0;
    let release_sources = state
        .brn_engine
        .lock()
        .await
        .get_wallet(&address)
        .map_or_else(Vec::new, |wallet| {
            wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES)
        });
    let block = tokio::task::spawn_blocking({
        let address = address.clone();
        let representative = account.representative.clone();
        let previous = account.head;
        let trst_balance = account.trst_balance;
        let work_generator = state.work_generator.clone();
        let min_work_difficulty = state.current_params()?.min_work_difficulty;
        let (payment_epoch, params_hash) = (
            state.meta_store.payment_epoch()?,
            state.current_params()?.params_hash(),
        );
        move || {
            let private_key = burst_types::PrivateKey(private_key_bytes);
            build_and_sign_block(
                signing_grant,
                payment_epoch,
                release_sources,
                burst_ledger::BlockType::GovernanceProposal,
                &address,
                previous,
                &representative,
                spent_brn,
                trst_balance,
                link,
                TxHash::ZERO,
                tx_hash,
                &private_key,
                &work_generator,
                min_work_difficulty,
                params_hash,
                Vec::new(),
            )
        }
    })
    .await
    .map_err(|e| RpcError::Server(format!("block build task failed: {e}")))??;

    submit_and_confirm(&block, state).await?;

    Ok(to_value(&GovernanceProposeSimpleResponse {
        block_hash: format!("{}", block.hash),
        proposal_hash: format!("{}", block.hash),
        account: address.to_string(),
        param: req.param,
        new_value: req.new_value,
    }))
}

// The block link carries the target public key; origin carries the eviction or
// reinstatement marker. This helper ignores `reason` and submits no proposal content.

pub async fn handle_governance_propose_eviction(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    require_faucet(state)?;

    let req: GovernanceProposeEvictionRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    let private_key = parse_private_key(&req.private_key)?;
    let public_key = burst_crypto::public_from_private(&private_key);
    let address = burst_crypto::derive_address(&public_key);

    let account = state
        .account_store
        .get_account(&address)
        .map_err(|e| account_not_found(e, address.as_str()))?;
    if account.head == BlockHash::ZERO {
        return Err(RpcError::InvalidRequest(
            "account has no blocks yet — burn BRN first".into(),
        ));
    }

    // Target address → 32-byte pubkey carried in the block `link`.
    let target_pubkey = burst_crypto::decode_address(&req.target).ok_or_else(|| {
        RpcError::InvalidRequest(format!("invalid target address: {}", req.target))
    })?;
    let link = BlockHash::new(target_pubkey);
    let origin = TxHash::new(if req.evict {
        burst_governance::ORV_EVICT_MARKER
    } else {
        burst_governance::ORV_REINSTATE_MARKER
    });

    let now = Timestamp::now();

    let tx_hash = TxHash::new(burst_crypto::blake2b_256(
        &[
            address.as_str().as_bytes(),
            &now.as_secs().to_be_bytes(),
            b"governance_propose_eviction",
            req.target.as_bytes(),
        ]
        .concat(),
    ));

    let (spent_brn, signing_grant) = signing_predecessor(state, &account.head)?;
    let private_key_bytes = private_key.0;
    let release_sources = state
        .brn_engine
        .lock()
        .await
        .get_wallet(&address)
        .map_or_else(Vec::new, |wallet| {
            wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES)
        });
    let block = tokio::task::spawn_blocking({
        let address = address.clone();
        let representative = account.representative.clone();
        let previous = account.head;
        let trst_balance = account.trst_balance;
        let work_generator = state.work_generator.clone();
        let min_work_difficulty = state.current_params()?.min_work_difficulty;
        let (payment_epoch, params_hash) = (
            state.meta_store.payment_epoch()?,
            state.current_params()?.params_hash(),
        );
        move || {
            let private_key = burst_types::PrivateKey(private_key_bytes);
            build_and_sign_block(
                signing_grant,
                payment_epoch,
                release_sources,
                burst_ledger::BlockType::GovernanceProposal,
                &address,
                previous,
                &representative,
                spent_brn,
                trst_balance,
                link,
                origin,
                tx_hash,
                &private_key,
                &work_generator,
                min_work_difficulty,
                params_hash,
                Vec::new(),
            )
        }
    })
    .await
    .map_err(|e| RpcError::Server(format!("block build task failed: {e}")))??;

    submit_and_confirm(&block, state).await?;

    Ok(to_value(&GovernanceProposeEvictionResponse {
        block_hash: format!("{}", block.hash),
        proposal_hash: format!("{}", block.hash),
        account: address.to_string(),
        target: req.target,
        evict: req.evict,
    }))
}

pub async fn handle_governance_vote_simple(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    require_faucet(state)?;

    let req: GovernanceVoteSimpleRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    let private_key = parse_private_key(&req.private_key)?;
    let public_key = burst_crypto::public_from_private(&private_key);
    let address = burst_crypto::derive_address(&public_key);

    let account = state
        .account_store
        .get_account(&address)
        .map_err(|e| account_not_found(e, address.as_str()))?;

    if account.head == BlockHash::ZERO {
        return Err(RpcError::InvalidRequest(
            "account has no blocks yet — burn BRN first".into(),
        ));
    }

    let vote_byte: u8 = match req.vote.to_lowercase().as_str() {
        "yea" | "yes" | "y" => 0,
        "nay" | "no" | "n" => 1,
        "abstain" | "a" => 2,
        _ => {
            return Err(RpcError::InvalidRequest(
                "vote must be 'yea', 'nay', or 'abstain'".into(),
            ))
        }
    };

    let proposal_hash_bytes = hex::decode(&req.proposal_hash)
        .map_err(|e| RpcError::InvalidRequest(format!("invalid proposal_hash hex: {e}")))?;
    if proposal_hash_bytes.len() != 32 {
        return Err(RpcError::InvalidRequest(
            "proposal_hash must be 32 bytes (64 hex chars)".into(),
        ));
    }
    let mut proposal_hash = [0u8; 32];
    proposal_hash.copy_from_slice(&proposal_hash_bytes);
    let link = BlockHash::new(proposal_hash);

    let mut tx_bytes = [0u8; 32];
    tx_bytes[0] = vote_byte;
    let transaction = TxHash::new(tx_bytes);

    let (spent_brn, signing_grant) = signing_predecessor(state, &account.head)?;
    let private_key_bytes = private_key.0;
    let release_sources = state
        .brn_engine
        .lock()
        .await
        .get_wallet(&address)
        .map_or_else(Vec::new, |wallet| {
            wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES)
        });
    let block = tokio::task::spawn_blocking({
        let address = address.clone();
        let representative = account.representative.clone();
        let previous = account.head;
        let trst_balance = account.trst_balance;
        let work_generator = state.work_generator.clone();
        let min_work_difficulty = state.current_params()?.min_work_difficulty;
        let (payment_epoch, params_hash) = (
            state.meta_store.payment_epoch()?,
            state.current_params()?.params_hash(),
        );
        move || {
            let private_key = burst_types::PrivateKey(private_key_bytes);
            build_and_sign_block(
                signing_grant,
                payment_epoch,
                release_sources,
                burst_ledger::BlockType::GovernanceVote,
                &address,
                previous,
                &representative,
                spent_brn,
                trst_balance,
                link,
                TxHash::ZERO,
                transaction,
                &private_key,
                &work_generator,
                min_work_difficulty,
                params_hash,
                Vec::new(),
            )
        }
    })
    .await
    .map_err(|e| RpcError::Server(format!("block build task failed: {e}")))??;

    submit_and_confirm(&block, state).await?;

    let vote_name = match vote_byte {
        0 => "yea",
        1 => "nay",
        _ => "abstain",
    };

    Ok(to_value(&GovernanceVoteSimpleResponse {
        block_hash: format!("{}", block.hash),
        account: address.to_string(),
        proposal_hash: req.proposal_hash,
        vote: vote_name.to_string(),
    }))
}

fn parse_governable_param(name: &str) -> Result<burst_governance::GovernableParam, RpcError> {
    burst_governance::GovernableParam::from_name(name)
        .map_err(|error| RpcError::InvalidRequest(error.to_string()))
}

pub use crate::contracts::governance::*;
