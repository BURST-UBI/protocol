//! The current state of the on-chain constitution.

use serde::{Deserialize, Serialize};

/// The Consti document — the current constitution state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConstiDocument {
    /// Ordered list of adopted articles.
    pub articles: Vec<Article>,
    /// Version number (incremented with each amendment).
    pub version: u64,
    /// History of version changes: (version, description).
    pub version_history: Vec<VersionEntry>,
}

/// A record of a version change to the constitution.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VersionEntry {
    /// The version number after this change.
    pub version: u64,
    /// Description of what changed (typically the amendment title).
    pub description: String,
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
    /// The amendment version that introduced or last modified this article.
    pub introduced_by_amendment: u64,
    /// Whether this article has been repealed.
    pub repealed: bool,
}

impl ConstiDocument {
    /// Create the genesis constitution (empty, version 0).
    pub fn genesis() -> Self {
        Self {
            articles: Vec::new(),
            version: 0,
            version_history: Vec::new(),
        }
    }

    /// Get an article by its number (returns None for repealed articles).
    pub fn get_article(&self, number: u64) -> Option<&Article> {
        self.articles
            .iter()
            .find(|article| article.number == number && !article.repealed)
    }

    /// Get an article by its number, including repealed ones.
    pub fn get_article_including_repealed(&self, number: u64) -> Option<&Article> {
        self.articles.iter().find(|article| article.number == number)
    }

    /// Get the total number of active (non-repealed) articles.
    pub fn article_count(&self) -> usize {
        self.articles.iter().filter(|a| !a.repealed).count()
    }

    /// Get the total number of articles including repealed ones.
    pub fn total_article_count(&self) -> usize {
        self.articles.len()
    }

    /// Get the next available article number.
    pub fn next_article_number(&self) -> u64 {
        self.articles
            .iter()
            .map(|a| a.number)
            .max()
            .unwrap_or(0)
            + 1
    }

    /// Check if an article number exists and is not repealed.
    pub fn has_active_article(&self, number: u64) -> bool {
        self.articles
            .iter()
            .any(|a| a.number == number && !a.repealed)
    }
}
