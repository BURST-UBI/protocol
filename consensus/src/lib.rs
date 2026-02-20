//! Consensus — double-spend resolution via representative voting.
//!
//! Inspired by Nano's Open Representative Voting (ORV):
//! - Each account delegates its weight to a representative.
//! - Representatives vote on conflicting blocks.
//! - A block is confirmed when it receives ≥ 67% of online voting weight.
//! - Finality is typically sub-second.
//!
//! ## Module overview
//!
//! - [`election`] — Election state machine (Passive → Active → Confirmed/Expired).
//! - [`active_elections`] — Container managing all ongoing elections.
//! - [`vote_info`] — Per-voter vote data with final/non-final distinction.
//! - [`vote_cache`] — Pre-election vote storage for out-of-order vote arrival.
//! - [`voting`] — Representative voting with per-voter tracking and tallying.
//! - [`conflict`] — Fork detection in account chains.
//! - [`representative`] — Representative identity and weight.
//! - [`error`] — Consensus error types.

pub mod active_elections;
pub mod backlog_scanner;
pub mod conflict;
pub mod election;
pub mod equivocation;
pub mod error;
pub mod fork_cache;
pub mod online_weight;
pub mod rep_crawler;
pub mod rep_weights;
pub mod representative;
pub mod request_aggregator;
pub mod scheduler;
pub mod vote_by_hash;
pub mod vote_cache;
pub mod vote_generator;
pub mod vote_info;
pub mod vote_rebroadcast;
pub mod vote_solicitor;
pub mod vote_spacing;
pub mod voting;

pub use active_elections::ActiveElections;
pub use backlog_scanner::BacklogScanner;
pub use conflict::ConflictDetector;
pub use election::{Election, ElectionState, ElectionStatus};
pub use equivocation::{EquivocationDetector, EquivocationProof};
pub use error::ConsensusError;
pub use fork_cache::ForkCache;
pub use online_weight::OnlineWeightSampler;
pub use rep_crawler::{DiscoveredRep, RepCrawler};
pub use rep_weights::RepWeightCache;
pub use representative::Representative;
pub use request_aggregator::RequestAggregator;
pub use scheduler::{ElectionBehavior, HintedScheduler, PriorityScheduler};
pub use vote_by_hash::VoteByHash;
pub use vote_cache::VoteCache;
pub use vote_generator::{GeneratedVote, VoteGenerator};
pub use vote_info::{VoteInfo, VoteResult};
pub use vote_rebroadcast::VoteRebroadcaster;
pub use vote_solicitor::VoteSolicitor;
pub use vote_spacing::VoteSpacing;
pub use voting::RepresentativeVoting;
