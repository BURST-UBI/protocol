//! Consensus — double-spend resolution via representative voting.
//!
//! Inspired by Nano's Open Representative Voting (ORV):
//! - Each account delegates its weight to a representative.
//! - Representatives vote on conflicting blocks.
//! - A block is confirmed when it receives ≥ 67% of online voting weight.
//! - Finality is typically sub-second.

pub mod conflict;
pub mod error;
pub mod representative;
pub mod voting;

pub use conflict::ConflictDetector;
pub use error::ConsensusError;
pub use representative::Representative;
pub use voting::RepresentativeVoting;
