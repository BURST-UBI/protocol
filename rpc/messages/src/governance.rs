use crate::pagination::PaginationParams;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct GovernanceProposalsRequest {
    #[serde(flatten)]
    pub pagination: PaginationParams,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProposalSummary {
    pub hash: String,
    pub proposer: String,
    pub phase: String,
    pub phase_projected: bool,
    pub description: String,
    pub votes_yea: u32,
    pub votes_nay: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GovernanceProposalsResponse {
    pub proposals: Vec<ProposalSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Shape of the unsupported unsigned vote action. Submit a signed block via `process`.
#[derive(Debug, Serialize, Deserialize)]
pub struct GovernanceVoteRequest {
    pub proposal_hash: String,
    pub voter: String,
    pub vote: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GovernanceVoteResponse {
    pub proposal_hash: String,
    pub vote: String,
    pub accepted: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GovernanceProposalInfoRequest {
    pub hash: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GovernanceProposalInfoResponse {
    #[serde(flatten)]
    pub review: burst_governance::proposal::ProposalReview,
    pub proposer: String,
    pub description: String,
    pub param: String,
    pub proposed_value: String,
    pub votes_yea: u32,
    pub votes_nay: u32,
    pub votes_abstain: u32,
    pub created_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amendment_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amendment_operations: Option<Vec<burst_governance::proposal::AmendmentOp>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activation_conflict: Option<burst_consti::AmendmentConflict>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GovernanceProposeSimpleRequest {
    pub private_key: String,
    pub param: String,
    pub new_value: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GovernanceProposeSimpleResponse {
    pub block_hash: String,
    pub proposal_hash: String,
    pub account: String,
    pub param: String,
    pub new_value: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GovernanceProposeEvictionRequest {
    pub private_key: String,
    /// The representative address to evict or reinstate.
    pub target: String,
    /// `true` to evict, `false` to reinstate.
    pub evict: bool,
    /// Accepted but ignored by `governance_propose_eviction`; not stored on-chain.
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GovernanceProposeEvictionResponse {
    pub block_hash: String,
    pub proposal_hash: String,
    pub account: String,
    pub target: String,
    pub evict: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GovernanceVoteSimpleRequest {
    pub private_key: String,
    pub proposal_hash: String,
    pub vote: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GovernanceVoteSimpleResponse {
    pub block_hash: String,
    pub account: String,
    pub proposal_hash: String,
    pub vote: String,
}
