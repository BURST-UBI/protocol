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
pub fn evaluate_policy(
    _policy: &TrustPolicy,
    _originator: &WalletAddress,
    _group_client: &GroupClient,
) -> TrustDecision {
    todo!("check originator against all trust rules")
}

/// The result of evaluating a trust policy.
#[derive(Clone, Debug)]
pub enum TrustDecision {
    Accept,
    Reject { reason: String },
    Warn { reason: String },
}
