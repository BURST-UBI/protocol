//! The current state of the on-chain constitution.

use serde::{Deserialize, Serialize};

/// The Consti document — the current constitution state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConstiDocument {
    /// Ordered list of adopted articles.
    pub articles: Vec<Article>,
    /// Version number (incremented with each amendment).
    pub version: u64,
}

/// A single article in the constitution.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Article {
    /// Article number.
    pub number: u64,
    /// Title of the article.
    pub title: String,
    /// Full text.
    pub text: String,
    /// The amendment that introduced or last modified this article.
    pub introduced_by_amendment: u64,
}

impl ConstiDocument {
    /// Create the genesis constitution (empty, version 0).
    pub fn genesis() -> Self {
        Self {
            articles: Vec::new(),
            version: 0,
        }
    }
}
