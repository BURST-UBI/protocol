//! Constitutional articles, version history and derived lookup caches.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Constitutional document with cached indexes and counts. Public article
/// mutation and deserialization do not automatically rebuild those caches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConstiDocument {
    /// Ordered list of adopted articles.
    pub articles: Vec<Article>,
    /// Version number (incremented with each amendment).
    pub version: u64,
    /// History of version changes: (version, description).
    pub version_history: Vec<VersionEntry>,
    /// Article number to vector position for expected constant-time lookup.
    #[serde(default)]
    pub(crate) article_index: HashMap<u64, usize>,
    /// Cached count of active (non-repealed) articles.
    #[serde(default)]
    active_count: usize,
    /// Next available article number (tracked incrementally).
    #[serde(default = "default_next_number")]
    next_number: u64,
}

/// Read-only stored representation; cached indexes and counters are not rebuilt.
#[derive(Serialize)]
pub struct DocumentView<'a> {
    pub articles: &'a [Article],
    pub version: u64,
    pub version_history: &'a [VersionEntry],
    pub article_index: &'a HashMap<u64, usize>,
    pub active_count: usize,
    pub next_number: u64,
}

fn default_next_number() -> u64 {
    1
}

/// A record of a version change to the constitution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionEntry {
    /// The version number after this change.
    pub version: u64,
    /// Description of what changed (typically the amendment title).
    pub description: String,
}

/// A single article in the constitution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Article {
    /// Article number.
    pub number: u64,
    /// Title of the article.
    pub title: String,
    /// Full text.
    pub text: String,
    /// Document version that introduced, last modified or repealed this article.
    pub introduced_by_amendment: u64,
    /// Whether this article has been repealed.
    pub repealed: bool,
}

impl ConstiDocument {
    pub fn snapshot_view(&self) -> DocumentView<'_> {
        DocumentView {
            articles: &self.articles,
            version: self.version,
            version_history: &self.version_history,
            article_index: &self.article_index,
            active_count: self.active_count,
            next_number: self.next_number,
        }
    }

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

    /// Seed the three launch articles on fraud, evidence and participant rights
    /// at version zero. Their text is stored content, not executable validation.
    pub fn bootstrap() -> Self {
        let seed = [
            (
                "Legitimacy and Fraud",
                "A legitimate participant is one unique living human holding exactly \
                 one wallet. Fraud is the creation or operation of a wallet that does \
                 not correspond to a unique living human — duplicate wallets, wallets \
                 for non-existent people, or wallets operated on behalf of a person \
                 without their consent. Fraud is grounds for a challenge that, if \
                 upheld, unverifies the wallet and revokes all TRST it originated.",
            ),
            (
                "Standards of Evidence for Verification",
                "Verifiers must independently assess whether a wallet holder is a \
                 unique living human. The protocol fixes no single method; acceptable \
                 evidence and its interpretation are decided by the community and may \
                 evolve. A verifier who cannot form a judgment should abstain rather \
                 than approve.",
            ),
            (
                "Rights and Responsibilities of Participants",
                "Every verified human has one vote and accrues BRN at the same rate. \
                 Any verified participant may challenge another's legitimacy by \
                 staking BRN, and may be challenged in turn. A holder deactivated for \
                 inactivity (not fraud) keeps the TRST they legitimately earned and \
                 may re-verify. Delegation of a vote is always revocable by its owner.",
            ),
        ];
        let mut doc = Self::genesis();
        for (title, text) in seed {
            let number = doc.next_number;
            doc.articles.push(Article {
                number,
                title: title.to_string(),
                text: text.to_string(),
                introduced_by_amendment: 0,
                repealed: false,
            });
            doc.next_number += 1;
        }
        doc.rebuild_index();
        doc
    }

    /// Recompute indexes and counters from articles. Does not validate duplicate
    /// numbers or amendment history; callers must validate restored content.
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

    /// Append an active article, replacing its supplied number with the next one.
    /// Callers must provide a non-repealed article and valid document counters.
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
