//! Group trust layer types.

use serde::{Deserialize, Serialize};

/// Information about a registered group.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupInfo {
    /// Unique group identifier (human-readable).
    pub id: String,
    /// Display name of the group.
    pub name: String,
    /// Description of the group and its verification standards.
    pub description: String,
    /// Base URL of the group's verification API.
    pub endpoint_url: String,
    /// Number of members (self-reported).
    pub member_count: u64,
}

/// Response from a group's member verification endpoint.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemberStatus {
    /// Whether the wallet is currently a valid group member.
    pub valid: bool,
    /// A trust score [0.0, 1.0] assigned by the group.
    pub score: f64,
    /// Optional metadata (e.g., membership level, join date).
    pub metadata: Option<serde_json::Value>,
}
