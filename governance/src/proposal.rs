//! Governance proposals and their lifecycle (Tezos-inspired).

use burst_types::{Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// Origin-field sentinel marking a `GovernanceProposal` block as an ORV
/// *eviction* proposal. The 32-byte block `link` only fits one pubkey, so it
/// carries the TARGET representative's pubkey while the (also hashed/signed)
/// `origin` field carries this discriminant. A fixed 32-byte sentinel — no real
/// `TxHash` collides in practice. Shared by the proposal builder (RPC) and the
/// block decoder (node) so both agree.
pub const ORV_EVICT_MARKER: [u8; 32] = *b"BURST_ORV_EVICT_MARKER__________";
/// As [`ORV_EVICT_MARKER`], but for a *reinstatement* proposal.
pub const ORV_REINSTATE_MARKER: [u8; 32] = *b"BURST_ORV_REINSTATE_MARKER______";

/// The phases of a governance proposal (Tezos-inspired).
///
/// Normal flow: Proposal → Exploration (1st vote) → Cooldown → Promotion (2nd vote) → Activation
/// Emergency flow: Exploration → Promotion → Activation (skip Proposal and Cooldown)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GovernancePhase {
    /// Phase 1: Collecting endorsements (BRN burns) to advance past spam filter.
    Proposal,
    /// Phase 2: First vote — "Should we explore this change?"
    Exploration,
    /// Phase 3: No voting. Community discusses and prepares.
    Cooldown,
    /// Phase 4: Second vote — "Should we actually activate this?"
    Promotion,
    /// Phase 5: At a deterministic timestamp, all nodes apply the change.
    Activation,
    /// The proposal was rejected (did not meet supermajority/quorum at any voting stage).
    Rejected,
    /// The proposal was activated and is now in effect.
    Activated,
    /// The proposal was withdrawn by the proposer before leaving the Proposal phase.
    Withdrawn,
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
    /// Total verified wallets at the time of voting (for quorum calculation).
    pub total_eligible_voters: u32,

    // ── Exploration vote (Phase 2) ──────────────────────────────────────
    /// When exploration voting started.
    pub exploration_started_at: Option<Timestamp>,
    /// Exploration votes: yea.
    pub exploration_votes_yea: u32,
    /// Exploration votes: nay.
    pub exploration_votes_nay: u32,
    /// Exploration votes: abstain.
    pub exploration_votes_abstain: u32,

    // ── Cooldown (Phase 3) ──────────────────────────────────────────────
    /// When cooldown started.
    pub cooldown_started_at: Option<Timestamp>,

    // ── Promotion vote (Phase 4) ────────────────────────────────────────
    /// When promotion voting started.
    pub promotion_started_at: Option<Timestamp>,
    /// Promotion votes: yea.
    pub promotion_votes_yea: u32,
    /// Promotion votes: nay.
    pub promotion_votes_nay: u32,
    /// Promotion votes: abstain.
    pub promotion_votes_abstain: u32,

    // ── Retry tracking ──────────────────────────────────────────────────
    /// Current round (0-indexed). Incremented each time a proposal fails
    /// and is reset to the Proposal phase for another attempt.
    pub round: u32,

    // ── Timestamps ──────────────────────────────────────────────────────
    /// When the proposal was created.
    pub created_at: Timestamp,
    /// When activation was scheduled (set after Promotion passes).
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
    ConstitutionalAmendment { title: String, text: String },
    /// Emergency parameter change — fast-tracked lifecycle with higher thresholds.
    /// Skips Proposal and Cooldown phases, 24-hour voting periods, 95% supermajority.
    Emergency {
        description: String,
        param: super::params::GovernableParam,
        new_value: u128,
    },
    /// Evict a representative from the ORV consensus set — or reinstate one.
    ///
    /// This is the bad-actor backstop for consensus. It is decided by the same
    /// one-verified-human-one-vote governance process as everything else, and
    /// deliberately NOT by ORV (expired-TRST / delegation) weight — so a rep that
    /// has amassed consensus weight cannot vote down its own eviction. On
    /// activation the target account's `orv_evicted` flag is set (`evict = true`)
    /// or cleared (`evict = false`); an evicted rep then contributes zero weight,
    /// and its delegators regain their voice by re-delegating to an honest rep.
    RepresentativeEviction {
        /// The representative to evict or reinstate.
        target: WalletAddress,
        /// `true` to evict, `false` to reinstate.
        evict: bool,
        /// Human-readable justification (audit trail).
        reason: String,
    },
}
