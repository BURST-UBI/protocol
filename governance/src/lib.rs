//! Democratic governance for the BURST protocol.
//!
//! Four phases: Proposal → Voting → Cooldown → Activation.
//! Voting is stored as Exploration for snapshot compatibility.
//!
//! Key principle: one wallet = one vote (not stake-weighted).
//! All protocol parameters are governable, including the governance parameters themselves.

pub mod ballot;
pub mod content;
pub mod delegation;
pub mod engine;
pub mod error;
pub mod key_delegation;
pub mod params;
mod projection;
pub mod proposal;
mod timing;

pub use delegation::{DelegationEngine, DelegationScope, DelegationSnapshot, ScopedDelegation};
pub use engine::GovernanceEngine;
pub use error::GovernanceError;
pub use params::GovernableParam;
pub use proposal::{
    GovernancePhase, Proposal, ProposalContent, ORV_EVICT_MARKER, ORV_REINSTATE_MARKER,
};
pub use timing::GovernanceTiming;
