//! Read-only appeal review data reported by a node, not a certificate proof.

use crate::ChallengeReason;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppealReview {
    pub account: String,
    pub enabled: bool,
    pub challenge_active: bool,
    pub required_stake: String,
    pub outcome: Option<AppealOutcome>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppealOutcome {
    pub hash: String,
    pub checkpoint: String,
    pub reason: ChallengeReason,
    pub legitimate: bool,
    pub completed_at: u64,
}
