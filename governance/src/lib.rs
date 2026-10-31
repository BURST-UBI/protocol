//! Governance proposals, ballots and parameter changes.
//!
//! Ordinary proposals collect endorsements, hold one vote, then enter cooldown
//! before scheduled activation. Emergency proposals skip endorsements and cooldown.
//! `Exploration` is the stored name of the voting phase; `Promotion` is retained
//! for old records and is not a second ballot in the current lifecycle.
//!
//! Voting weight comes from eligible verified accounts. The node supplies the
//! electorate, authenticates operations and applies certified outcomes. This crate
//! manages proposal state and the supported parameter registry.

pub mod ballot;
pub mod content;
pub mod engine;
pub mod error;
pub mod params;
pub mod proposal;
mod proposal_preview;
pub mod record_references;
pub mod signing_delegation;
mod timing;
pub mod vote_delegation;

pub use engine::GovernanceEngine;
pub use error::GovernanceError;
pub use params::GovernableParam;
pub use proposal::{
    GovernancePhase, Proposal, ProposalContent, ORV_EVICT_MARKER, ORV_REINSTATE_MARKER,
};
pub use timing::GovernanceTiming;
pub use vote_delegation::{
    DelegationEngine, DelegationScope, DelegationSnapshot, ScopedDelegation,
};
