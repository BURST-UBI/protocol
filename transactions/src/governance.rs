//! Governance transactions: proposals and votes.

use burst_types::{Signature, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// A governance proposal transaction.
///
/// Proposes a change to a protocol parameter or a constitutional amendment.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GovernanceProposalTx {
    pub hash: TxHash,
    pub proposer: WalletAddress,
    pub timestamp: Timestamp,
    /// What is being proposed.
    pub proposal: ProposalContent,
    pub work: u64,
    pub signature: Signature,
}

/// The content of a governance proposal.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProposalContent {
    /// Change a numeric protocol parameter.
    ParameterChange {
        parameter: String,
        current_value: u128,
        proposed_value: u128,
    },
    /// Amend the on-chain constitution (Consti).
    ConstitutionalAmendment { title: String, text: String },
}

/// A governance vote transaction.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GovernanceVoteTx {
    pub hash: TxHash,
    /// The wallet casting the vote (may be a delegate).
    pub voter: WalletAddress,
    /// Hash of the proposal being voted on.
    pub proposal_hash: TxHash,
    /// The vote.
    pub vote: GovernanceVote,
    pub timestamp: Timestamp,
    pub work: u64,
    pub signature: Signature,
}

/// A vote on a governance proposal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GovernanceVote {
    /// In favor of the proposal.
    Yea,
    /// Against the proposal.
    Nay,
    /// Abstain (counted for quorum but not for supermajority).
    Abstain,
}
