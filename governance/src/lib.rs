//! Democratic governance for the BURST protocol.
//!
//! 5-phase process (Tezos-inspired): Proposal → Exploration → Cooldown → Promotion → Activation
//! Emergency fast-track: Exploration → Promotion → Activation (24h periods, 95% supermajority)
//! With adaptive quorum biasing (EMA-based).
//!
//! Key principle: one wallet = one vote (not stake-weighted).
//! All protocol parameters are governable, including the governance parameters themselves.

pub mod delegation;
pub mod engine;
pub mod error;
pub mod params;
pub mod proposal;

pub use delegation::{DelegationEngine, DelegationScope, DelegationSnapshot, ScopedDelegation};
pub use engine::GovernanceEngine;
pub use error::GovernanceError;
pub use params::GovernableParam;
pub use proposal::{GovernancePhase, Proposal, ProposalContent};
