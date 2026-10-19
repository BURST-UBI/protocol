//! Group Trust Layer — off-chain social verification groups.
//!
//! Groups are self-organized social entities that vouch for their members.
//! They operate entirely off-chain and provide an additional trust signal
//! to receivers who want more than protocol-level verification.
//!
//! Design:
//! - Groups manage membership via their own chosen mechanism (centralized admin, voting, etc.)
//! - Each group exposes an HTTP endpoint: `GET /verify/{wallet_id}` → { valid: bool, score: f64 }
//! - Receivers query trusted groups about TRST originators, not just the immediate sender
//! - Nothing is on-chain — groups are a purely application-level trust overlay

pub mod client;
pub mod error;
pub mod registry;
pub mod trust_graph;
pub mod types;

pub use client::GroupClient;
pub use error::GroupError;
pub use registry::GroupRegistry;
pub use trust_graph::MembershipPath;
pub use types::{GroupInfo, MemberStatus};
