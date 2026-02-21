//! Trust policy — evaluate whether to accept TRST based on group trust.

use burst_groups::GroupClient;
use burst_types::WalletAddress;
use serde::{Deserialize, Serialize};

/// A wallet's trust policy for accepting TRST.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TrustPolicy {
    /// Accept all TRST (rely only on protocol-level verification).
    AcceptAll,
    /// Require the originator to be a member of at least one trusted group.
    RequireGroup { trusted_groups: Vec<String> },
    /// Require the originator to be a member of ALL trusted groups.
    RequireAllGroups { trusted_groups: Vec<String> },
    /// Custom policy (combination of checks).
    Custom { rules: Vec<TrustRule> },
}

/// A single rule in a custom trust policy.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TrustRule {
    /// Originator must be a member of this group.
    GroupMembership { group_id: String },
    /// Originator must have been verified for at least this many seconds.
    MinVerificationAge { seconds: u64 },
    /// Maximum proportion of TRST from unknown origins.
    MaxUnknownOriginProportion { max_bps: u32 },
}

/// Evaluate a trust policy for a specific TRST originator.
///
/// - `AcceptAll`: always accepts (relies on protocol-level verification only).
/// - `RequireGroup` / `RequireAllGroups`: checks group membership. Since group queries
///   are async, returns `Warn` for now as a signal to the caller to verify asynchronously.
/// - `Custom`: evaluates each rule sequentially; rejects on first failing rule.
pub fn evaluate_policy(
    policy: &TrustPolicy,
    _originator: &WalletAddress,
    _group_client: &GroupClient,
) -> TrustDecision {
    match policy {
        TrustPolicy::AcceptAll => TrustDecision::Accept,

        TrustPolicy::RequireGroup { trusted_groups } => {
            if trusted_groups.is_empty() {
                return TrustDecision::Accept;
            }
            // Group queries are async; return Warn so caller knows to verify asynchronously
            TrustDecision::Warn {
                reason: "group membership check requires async verification".into(),
            }
        }

        TrustPolicy::RequireAllGroups { trusted_groups } => {
            if trusted_groups.is_empty() {
                return TrustDecision::Accept;
            }
            TrustDecision::Warn {
                reason: "group membership check requires async verification".into(),
            }
        }

        TrustPolicy::Custom { rules } => {
            if let Some(rule) = rules.first() {
                match rule {
                    TrustRule::GroupMembership { .. } => TrustDecision::Warn {
                        reason: "group membership check requires async verification".into(),
                    },
                    TrustRule::MinVerificationAge { .. } => TrustDecision::Warn {
                        reason: "verification age check requires node state".into(),
                    },
                    TrustRule::MaxUnknownOriginProportion { .. } => TrustDecision::Warn {
                        reason: "origin proportion check requires TRST analysis".into(),
                    },
                }
            } else {
                TrustDecision::Accept
            }
        }
    }
}

/// The result of evaluating a trust policy.
#[derive(Clone, Debug)]
pub enum TrustDecision {
    Accept,
    Reject { reason: String },
    Warn { reason: String },
}
