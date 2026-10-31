//! Off-chain group membership queries, registry and bounded trust paths.
//!
//! Wallets can use these results as an additional payment-acceptance policy.
//! Group membership does not replace protocol verification or grant ledger rights.

pub mod client;
pub mod error;
pub mod evaluation;
pub mod registry;
pub mod trust_graph;
pub mod types;

pub use client::GroupClient;
pub use error::GroupError;
pub use evaluation::GroupEvaluation;
pub use registry::GroupRegistry;
pub use trust_graph::MembershipPath;
pub use types::{GroupInfo, MemberStatus};
