//! Governance proposals and their lifecycle.

use burst_types::{Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// The 4 phases of a governance proposal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GovernancePhase {
    /// Phase 1: Collecting endorsements (BRN burns) to advance.
    Proposal,
    /// Phase 2: Verified wallets vote (one wallet = one vote).
    Voting,
    /// Phase 3: No voting. Community discusses and prepares.
    Cooldown,
    /// Phase 4: At a deterministic timestamp, all nodes apply the change.
    Activation,
    /// The proposal was rejected (did not meet supermajority/quorum).
    Rejected,
    /// The proposal was activated and is now in effect.
    Activated,
}

/// A governance proposal.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Proposal {
    /// Hash of the proposal transaction.
    pub hash: TxHash,
    /// Who proposed it.
    pub proposer: WalletAddress,
    /// Current phase.
    pub phase: GovernancePhase,
    /// What is being proposed.
    pub content: ProposalContent,
    /// Endorsements received (BRN burns to advance past spam filter).
    pub endorsement_count: u32,
    /// Votes: (voter, vote).
    pub votes_yea: u32,
    pub votes_nay: u32,
    pub votes_abstain: u32,
    /// Total verified wallets at the time of voting (for quorum calculation).
    pub total_eligible_voters: u32,
    /// Timestamps for phase transitions.
    pub created_at: Timestamp,
    pub voting_started_at: Option<Timestamp>,
    pub cooldown_started_at: Option<Timestamp>,
    pub activation_at: Option<Timestamp>,
}

/// What a governance proposal changes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProposalContent {
    /// Change a numeric protocol parameter.
    ParameterChange {
        param: super::params::GovernableParam,
        new_value: u128,
    },
    /// Constitutional amendment (handled by the consti crate).
    ConstitutionalAmendment {
        title: String,
        text: String,
    },
}
