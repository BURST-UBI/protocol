//! Protocol parameters — the two core parameters plus all governance-tunable values.
//!
//! Every field is democratically governable via the 4-phase governance process.

use serde::{Deserialize, Serialize};

/// All protocol parameters stored by every node.
///
/// The general equation: BURST is defined by `brn_rate` and `trst_expiry_secs`.
/// Normal money is the special case where `brn_rate = 0` and `trst_expiry_secs = u64::MAX`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProtocolParams {
    // ── The General Equation ─────────────────────────────────────────────
    /// BRN accrual rate: raw units per second per verified wallet.
    pub brn_rate: u128,

    /// TRST expiry period in seconds from the origin burn timestamp.
    /// Set to `u64::MAX` for "never expires" (normal money mode).
    pub trst_expiry_secs: u64,

    // ── Verification ─────────────────────────────────────────────────────
    /// Number of endorsers required before verification begins.
    pub endorsement_threshold: u32,

    /// BRN amount each endorser must permanently burn.
    pub endorsement_burn_amount: u128,

    /// Number of verifiers randomly selected per verification.
    pub num_verifiers: u32,

    /// Fraction (as basis points, e.g., 9000 = 90%) of legitimate votes needed to pass.
    pub verification_threshold_bps: u32,

    /// BRN amount each verifier must temporarily stake.
    pub verifier_stake_amount: u128,

    /// Maximum number of re-votes before verification fails.
    pub max_revotes: u32,

    // ── Challenges ───────────────────────────────────────────────────────
    /// BRN amount a challenger must stake to initiate a challenge.
    pub challenge_stake_amount: u128,

    // ── Governance ───────────────────────────────────────────────────────
    /// Duration of the Proposal phase in seconds.
    pub governance_proposal_duration_secs: u64,

    /// Duration of the Voting phase in seconds.
    pub governance_voting_duration_secs: u64,

    /// Duration of the Cooldown phase in seconds.
    pub governance_cooldown_duration_secs: u64,

    /// Supermajority threshold for parameter governance (basis points).
    pub governance_supermajority_bps: u32,

    /// Quorum threshold: minimum fraction of verified wallets that must vote (basis points).
    pub governance_quorum_bps: u32,

    /// Number of endorsements (BRN burns) needed to advance a proposal past spam filter.
    pub governance_proposal_endorsements: u32,

    // ── Consti ────────────────────────────────────────────────────────────
    /// Supermajority threshold for constitutional amendments (basis points).
    /// Separate from parameter governance — can be higher or lower.
    pub consti_supermajority_bps: u32,

    // ── Economic ─────────────────────────────────────────────────────────
    /// Spending limit for newly verified wallets (TRST raw units; 0 = no limit).
    pub new_wallet_spending_limit: u128,

    /// Duration (seconds) that new-wallet spending limit applies.
    pub new_wallet_limit_duration_secs: u64,

    // ── Anti-Spam ────────────────────────────────────────────────────────
    /// Minimum proof-of-work difficulty for transaction submission.
    pub min_work_difficulty: u64,
}

impl Default for ProtocolParams {
    fn default() -> Self {
        Self {
            // Normal money defaults: no UBI, no expiry
            brn_rate: 0,
            trst_expiry_secs: u64::MAX,

            // Verification
            endorsement_threshold: 3,
            endorsement_burn_amount: 100,
            num_verifiers: 7,
            verification_threshold_bps: 9000,
            verifier_stake_amount: 50,
            max_revotes: 3,

            // Challenges
            challenge_stake_amount: 100,

            // Governance
            governance_proposal_duration_secs: 7 * 24 * 3600,   // 1 week
            governance_voting_duration_secs: 14 * 24 * 3600,     // 2 weeks
            governance_cooldown_duration_secs: 7 * 24 * 3600,    // 1 week
            governance_supermajority_bps: 6600,                   // 66%
            governance_quorum_bps: 3000,                          // 30%
            governance_proposal_endorsements: 10,

            // Consti
            consti_supermajority_bps: 8000, // 80% for constitutional amendments

            // Economic
            new_wallet_spending_limit: 0,
            new_wallet_limit_duration_secs: 0,

            // Anti-Spam
            min_work_difficulty: 0xffff_f000_0000_0000,
        }
    }
}
