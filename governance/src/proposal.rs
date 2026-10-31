//! Governance proposal records, including retained legacy phase fields.

use burst_types::{Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// Operation marker in a representative-eviction proposal's signed `origin`
/// field. The signed `link` field carries the target representative's public key.
/// Builders and decoders share this marker to distinguish the proposal kind.
pub const ORV_EVICT_MARKER: [u8; 32] = *b"BURST_ORV_EVICT_MARKER__________";
/// Signed operation marker for representative reinstatement.
pub const ORV_REINSTATE_MARKER: [u8; 32] = *b"BURST_ORV_REINSTATE_MARKER______";

/// The phases of a governance proposal.
///
/// Ordinary flow: endorsements, one voting window, cooldown, scheduled activation.
/// Emergency flow: one voting window followed by scheduled activation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GovernancePhase {
    /// Collect endorsements backed by BRN burns.
    Proposal,
    /// The single voting window, stored under its original name.
    Exploration,
    /// Waiting period after a successful ordinary vote. No second vote occurs.
    Cooldown,
    /// Legacy snapshot value. New proposals cannot enter or vote in this phase.
    Promotion,
    /// Approved and scheduled, awaiting application through the node's agreed transition.
    Activation,
    /// Endorsements or the voting thresholds were insufficient.
    Rejected,
    /// Activation was applied. Later proposals may change the same setting again.
    Activated,
    /// The proposal was withdrawn by the proposer before leaving the Proposal phase.
    Withdrawn,
    /// Approved, but an earlier activation removed an article this amendment needs.
    Conflicted,
    /// Explicit bootstrap veto, finalized before the scheduled activation.
    Vetoed,
}

/// A governance proposal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Eligibility denominator: a provisional snapshot before counting, replaced
    /// by the supplied electorate's size when the effective tally is computed.
    pub total_eligible_voters: u32,

    // Raw direct-vote counts while voting; effective counts after tallying.
    /// When the voting window started.
    pub exploration_started_at: Option<Timestamp>,
    /// Votes: yea.
    pub exploration_votes_yea: u32,
    /// Votes: nay.
    pub exploration_votes_nay: u32,
    /// Votes: abstain.
    pub exploration_votes_abstain: u32,

    /// When cooldown started.
    pub cooldown_started_at: Option<Timestamp>,

    // Legacy second-ballot fields; current proposals do not use them.
    /// Legacy promotion start time.
    pub promotion_started_at: Option<Timestamp>,
    /// Legacy promotion votes: yea.
    pub promotion_votes_yea: u32,
    /// Legacy promotion votes: nay.
    pub promotion_votes_nay: u32,
    /// Legacy promotion votes: abstain.
    pub promotion_votes_abstain: u32,

    /// Retained round field. Current vote failure rejects the proposal;
    /// it does not increment this field or restart the proposal.
    pub round: u32,

    /// When the proposal was created.
    pub created_at: Timestamp,
    /// Deterministic activation timestamp derived from phase deadlines.
    pub activation_at: Option<Timestamp>,
}

/// What a governance proposal changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalContent {
    /// Change a numeric protocol parameter.
    ParameterChange {
        param: super::params::GovernableParam,
        new_value: u128,
    },
    /// Legacy single-article addition. Diff proposals use `ConstitutionalDiff`.
    ConstitutionalAmendment { title: String, text: String },
    /// Emergency parameter change using the governed emergency voting rules.
    /// Skips endorsements and cooldown; counting and activation delays still apply.
    Emergency {
        description: String,
        param: super::params::GovernableParam,
        new_value: u128,
    },
    /// Evict a representative from transaction consensus, or reinstate one.
    ///
    /// Governance ballots decide eviction independently of the target's
    /// representative weight. Activation updates the account's `orv_evicted`
    /// flag; an evicted representative contributes no consensus weight.
    /// Its delegators can choose another representative.
    RepresentativeEviction {
        /// The representative to evict or reinstate.
        target: WalletAddress,
        /// `true` to evict, `false` to reinstate.
        evict: bool,
        /// Human-readable justification (audit trail).
        reason: String,
    },
    /// Ordered edits to the current constitution. Existing amendment history
    /// is immutable; these operations change the materialized document.
    ConstitutionalDiff {
        title: String,
        operations: Vec<AmendmentOp>,
    },
}

/// Signed constitutional edits, shared with the constitution executor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AmendmentOp {
    AddArticle {
        title: String,
        text: String,
    },
    ModifyArticle {
        article_number: u64,
        new_text: String,
    },
    RepealArticle {
        article_number: u64,
    },
}

/// Required proposal fields shared by RPC responses and signing review.
/// Missing fields must fail deserialization, never acquire permissive defaults.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ProposalReview {
    pub hash: String,
    pub phase: String,
    pub phase_projected: bool,
    pub phase_deadline: u64,
}
