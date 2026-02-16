//! Group Trust Layer — off-chain social verification groups.
//!
//! Groups are self-organized social entities that vouch for their members.
//! They operate entirely off-chain and provide an additional trust signal
//! to receivers who want more than protocol-level verification.
//!
//! Design:
//! - Groups manage membership via their own chosen mechanism (centralized admin, voting, etc.)
//! - Each group exposes an HTTP endpoint: `GET /verify/{wallet_id}` → { valid: bool, score: f64 }
//! - Receivers can ping any group to check a sender's status before accepting TRST
//! - Nothing is on-chain — groups are a purely application-level trust overlay

pub mod client;
pub mod error;
pub mod registry;
pub mod types;

pub use client::GroupClient;
pub use error::GroupError;
pub use registry::GroupRegistry;
pub use types::{GroupInfo, MemberStatus};
