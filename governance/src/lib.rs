//! Democratic governance for the BURST protocol.
//!
//! 4-phase process: Proposal → Voting → Cooldown → Activation
//!
//! Key principle: one wallet = one vote (not stake-weighted).
//! All protocol parameters are governable, including the governance parameters themselves.

pub mod delegation;
pub mod engine;
pub mod error;
pub mod params;
pub mod proposal;

pub use delegation::DelegationEngine;
pub use engine::GovernanceEngine;
pub use error::GovernanceError;
pub use params::GovernableParam;
pub use proposal::{GovernancePhase, Proposal};
