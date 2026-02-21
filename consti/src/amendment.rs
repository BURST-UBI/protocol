//! Constitutional amendments with diff-based operations.

use burst_governance::proposal::GovernancePhase;
use burst_types::{Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// A diff operation on the constitution.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AmendmentOp {
    /// Add a new article with the given title and text.
    AddArticle { title: String, text: String },
    /// Modify an existing article's text (by number).
    ModifyArticle {
        article_number: u64,
        new_text: String,
    },
    /// Repeal an existing article (by number).
    RepealArticle { article_number: u64 },
}

/// A proposed constitutional amendment.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Amendment {
    pub hash: TxHash,
    pub proposer: WalletAddress,
    pub title: String,
    pub text: String,
    pub phase: GovernancePhase,
    pub votes_yea: u32,
    pub votes_nay: u32,
    pub votes_abstain: u32,
    pub created_at: Timestamp,
    /// Diff-based operations to apply to the constitution.
    /// If empty, falls back to legacy behavior (add article from title/text).
    pub operations: Vec<AmendmentOp>,
}
