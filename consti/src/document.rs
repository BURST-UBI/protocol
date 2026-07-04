//! The current state of the on-chain constitution.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The Consti document — the current constitution state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConstiDocument {
    /// Ordered list of adopted articles.
    pub articles: Vec<Article>,
    /// Version number (incremented with each amendment).
    pub version: u64,
    /// History of version changes: (version, description).
    pub version_history: Vec<VersionEntry>,
    /// Index: article number → position in `articles` Vec for O(1) lookup.
    #[serde(default)]
    pub(crate) article_index: HashMap<u64, usize>,
    /// Cached count of active (non-repealed) articles.
    #[serde(default)]
    active_count: usize,
    /// Next available article number (tracked incrementally).
    #[serde(default = "default_next_number")]
    next_number: u64,
}

fn default_next_number() -> u64 {
    1
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
            article_index: HashMap::new(),
            active_count: 0,
            next_number: 1,
        }
    }

    /// Rebuild internal indexes from the articles Vec.
    /// Call this after deserialization if indexes are empty.
    pub fn rebuild_index(&mut self) {
        self.article_index.clear();
        self.active_count = 0;
        self.next_number = 1;
        for (pos, article) in self.articles.iter().enumerate() {
            self.article_index.insert(article.number, pos);
            if !article.repealed {
                self.active_count += 1;
            }
            if article.number >= self.next_number {
                self.next_number = article.number + 1;
            }
        }
    }

    /// Get an article by its number (returns None for repealed articles).
    pub fn get_article(&self, number: u64) -> Option<&Article> {
        self.article_index
            .get(&number)
            .and_then(|&pos| self.articles.get(pos))
            .filter(|a| !a.repealed)
    }

    /// Get a mutable reference to an article by its number (returns None for repealed articles).
    pub fn get_article_mut(&mut self, number: u64) -> Option<&mut Article> {
        self.article_index
            .get(&number)
            .copied()
            .and_then(|pos| self.articles.get_mut(pos))
            .filter(|a| !a.repealed)
    }

    /// Get an article by its number, including repealed ones.
    pub fn get_article_including_repealed(&self, number: u64) -> Option<&Article> {
        self.article_index
            .get(&number)
            .and_then(|&pos| self.articles.get(pos))
    }

    /// Get the total number of active (non-repealed) articles.
    pub fn article_count(&self) -> usize {
        self.active_count
    }

    /// Get the total number of articles including repealed ones.
    pub fn total_article_count(&self) -> usize {
        self.articles.len()
    }

    /// Get the next available article number.
    pub fn next_article_number(&self) -> u64 {
        self.next_number
    }

    /// Check if an article number exists and is not repealed.
    pub fn has_active_article(&self, number: u64) -> bool {
        self.get_article(number).is_some()
    }

    /// Add a new article and update indexes. Returns the assigned article number.
    pub fn push_article(&mut self, mut article: Article) -> u64 {
        let number = self.next_number;
        article.number = number;
        let pos = self.articles.len();
        self.articles.push(article);
        self.article_index.insert(number, pos);
        self.active_count += 1;
        self.next_number = number + 1;
        number
    }

    /// Mark an article as repealed by number. Returns true if found and repealed.
    pub fn repeal_article(&mut self, number: u64) -> bool {
        if let Some(&pos) = self.article_index.get(&number) {
            if let Some(article) = self.articles.get_mut(pos) {
                if !article.repealed {
                    article.repealed = true;
                    self.active_count = self.active_count.saturating_sub(1);
                    return true;
                }
            }
        }
        false
    }
}
