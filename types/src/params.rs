//! Protocol parameters — the two core parameters plus all governance-tunable values.
//!
//! Every field is democratically governable via the 5-phase governance process.

use crate::amount::BRN_UNIT;
use serde::{Deserialize, Serialize};

/// All protocol parameters stored by every node.
///
/// The general equation: BURST is defined by `brn_rate` and `trst_expiry_secs`.
/// Normal money is the special case where `brn_rate = 0` and `trst_expiry_secs = u64::MAX`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProtocolParams {
    // ── The General Equation ─────────────────────────────────────────────
    /// BRN accrual rate: raw units per second per verified wallet.
    /// Default: ~1 BRN/hour = 277_777_777_777_778 raw/second.
    pub brn_rate: u128,

    /// TRST expiry period in seconds from the origin burn timestamp.
    /// Set to `u64::MAX` for "never expires" (normal money mode).
    pub trst_expiry_secs: u64,

    // ── Verification ─────────────────────────────────────────────────────
    /// Number of endorsers required before verification begins.
    pub endorsement_threshold: u32,

    /// BRN amount (raw) each endorser must permanently burn.
    /// Default: 336 BRN (~2 weeks' accrual at 1 BRN/hour).
    pub endorsement_burn_amount: u128,

    /// Number of verifiers randomly selected per verification.
    pub num_verifiers: u32,

    /// Fraction (as basis points, e.g., 9000 = 90%) of legitimate votes needed to pass.
    pub verification_threshold_bps: u32,

    /// BRN amount (raw) each verifier must temporarily stake.
    /// Default: 500 BRN (~3 weeks' accrual).
    pub verifier_stake_amount: u128,

    /// Maximum number of re-votes before verification fails.
    pub max_revotes: u32,

    // ── Challenges ───────────────────────────────────────────────────────
    /// BRN amount (raw) a challenger must stake to initiate a challenge.
    /// Default: 1000 BRN (~6 weeks' accrual).
    pub challenge_stake_amount: u128,

    /// Cooldown duration (seconds) for verifiers penalized for excessive Neither voting.
    /// Penalized verifiers are excluded from verifier selection for this duration.
    /// Default: 7 days = 604800 seconds.
    pub neither_penalty_cooldown_secs: u64,

    // ── Governance (5-phase: Proposal → Exploration → Cooldown → Promotion → Activation) ──
    /// Duration of the Proposal phase in seconds.
    pub governance_proposal_duration_secs: u64,

    /// Duration of the Exploration vote phase in seconds.
    pub governance_exploration_duration_secs: u64,

    /// Duration of the Cooldown phase in seconds.
    pub governance_cooldown_duration_secs: u64,

    /// Duration of the Promotion vote phase in seconds.
    pub governance_promotion_duration_secs: u64,

    /// Supermajority threshold for parameter governance (basis points).
    pub governance_supermajority_bps: u32,

    /// Quorum threshold: minimum fraction of verified wallets that must vote (basis points).
    pub governance_quorum_bps: u32,

    /// Number of endorsements (BRN burns) needed to advance a proposal past spam filter.
    pub governance_proposal_endorsements: u32,

    /// Exponential moving average of past participation (basis points).
    /// Used for adaptive quorum biasing. Updated after each vote.
    pub governance_ema_participation_bps: u32,

    /// BRN cost (raw) to submit a governance proposal. Default: 336 BRN.
    pub governance_proposal_cost: u128,

    /// Maximum rounds a proposal can be reset to Proposal phase after failure
    /// before being terminally rejected. Default: 3.
    pub governance_max_rounds: u32,

    /// Duration (seconds) of the proposal competition window. Multiple proposals
    /// collect endorsements during this window; only the winner advances. Default: 7 days.
    pub governance_proposal_window_secs: u64,

    /// Propagation buffer (seconds) between voting end and vote counting.
    /// Allows late-arriving votes to propagate through the DAG before tallying.
    /// Default: 3600 (1 hour).
    pub governance_propagation_buffer_secs: u64,

    // ── Consti ────────────────────────────────────────────────────────────
    /// Supermajority threshold for constitutional amendments (basis points).
    /// Separate from parameter governance — can be higher or lower.
    /// Self-referential: changing this value requires hitting this same threshold.
    pub consti_supermajority_bps: u32,

    /// Quorum threshold for constitutional amendments (basis points).
    /// Separate from governance quorum — independently governable.
    pub consti_quorum_bps: u32,

    /// Duration (seconds) of a verification session before it expires.
    pub verification_timeout_secs: u64,

    /// Duration (seconds) of a challenge review period.
    pub challenge_duration_secs: u64,

    /// Endorser reward ratio on successful verification (basis points, 1000 = 10%).
    pub endorser_reward_bps: u32,

    // ── Economic ─────────────────────────────────────────────────────────
    /// Spending limit for newly verified wallets (TRST raw units; 0 = no limit).
    pub new_wallet_spending_limit: u128,

    /// Duration (seconds) that new-wallet spending limit applies.
    pub new_wallet_limit_duration_secs: u64,

    /// Number of verified wallets required to exit bootstrap phase.
    pub bootstrap_exit_threshold: u32,

    // ── Anti-Spam ────────────────────────────────────────────────────────
    /// Minimum proof-of-work difficulty for transaction submission.
    pub min_work_difficulty: u64,

    /// Maximum transactions per day for new wallets (rate limiting).
    pub new_wallet_tx_limit_per_day: u32,

    /// Duration (seconds) that new-wallet rate limit applies.
    pub new_wallet_rate_limit_duration_secs: u64,
}

impl ProtocolParams {
    /// 1 BRN per hour expressed as raw units per second (rounded up).
    pub const BRN_RATE_1_PER_HOUR: u128 = BRN_UNIT / 3600 + 1; // 277_777_777_777_778

    /// BURST UBI defaults — the intended configuration for the live network.
    pub fn burst_defaults() -> Self {
        Self {
            brn_rate: Self::BRN_RATE_1_PER_HOUR,
            trst_expiry_secs: 365 * 24 * 3600, // 1 year

            endorsement_threshold: 3,
            endorsement_burn_amount: 336 * BRN_UNIT,
            num_verifiers: 7,
            verification_threshold_bps: 9000, // 90%
            verifier_stake_amount: 500 * BRN_UNIT,
            max_revotes: 3,

            challenge_stake_amount: 1000 * BRN_UNIT,
            neither_penalty_cooldown_secs: 7 * 24 * 3600, // 7 days

            governance_proposal_duration_secs: 7 * 24 * 3600, // 1 week
            governance_exploration_duration_secs: 14 * 24 * 3600, // 2 weeks
            governance_cooldown_duration_secs: 7 * 24 * 3600, // 1 week
            governance_promotion_duration_secs: 14 * 24 * 3600, // 2 weeks
            governance_supermajority_bps: 8000,               // 80%
            governance_quorum_bps: 3000,                      // 30%
            governance_proposal_endorsements: 10,
            governance_ema_participation_bps: 5000, // 50% initial assumption
            governance_proposal_cost: 336 * BRN_UNIT,
            governance_max_rounds: 3,
            governance_proposal_window_secs: 7 * 24 * 3600, // 7 days
            governance_propagation_buffer_secs: 3600,       // 1 hour

            consti_supermajority_bps: 9000,           // 90%
            consti_quorum_bps: 3000,                  // 30%
            verification_timeout_secs: 7 * 24 * 3600, // 1 week
            challenge_duration_secs: 7 * 24 * 3600,   // 1 week
            endorser_reward_bps: 1000,                // 10%

            new_wallet_spending_limit: 0,
            new_wallet_limit_duration_secs: 0,
            bootstrap_exit_threshold: 50,

            min_work_difficulty: 0xffff_f000_0000_0000,
            new_wallet_tx_limit_per_day: 10,
            new_wallet_rate_limit_duration_secs: 30 * 24 * 3600, // 30 days
        }
    }
}

/// Default is the BURST UBI configuration.
impl Default for ProtocolParams {
    fn default() -> Self {
        Self::burst_defaults()
    }
}
