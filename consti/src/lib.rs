//! Constitutional articles, amendment patches and local amendment state.
//!
//! Governance and node replay authorize activation; this crate applies document
//! changes. Constitutional text guides participants but does not install
//! executable rules or independently authenticate a governance decision.

pub mod amendment;
pub mod document;
pub mod engine;
pub mod error;

pub use amendment::Amendment;
pub use document::ConstiDocument;
pub use engine::{AmendmentConflict, ConstiEngine};
pub use error::ConstiError;
