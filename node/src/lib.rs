//! BURST full node — orchestrates all protocol engines.
//!
//! The node is the central coordinator that:
//! - Processes incoming blocks/transactions
//! - Computes BRN balances
//! - Manages the TRST lifecycle and merger graph
//! - Coordinates humanity verification
//! - Handles governance proposals and voting
//! - Maintains clock synchronization
//! - Participates in consensus (representative voting for conflict resolution)

mod appeal_view;
mod backlog_scan;
pub mod block_processor;
mod block_validation;
mod block_retention;
pub mod bootstrap;
mod bootstrap_phase;
pub mod bounded_backlog;
mod causal_cementation;
#[cfg(test)]
mod certified_network_tests;
pub mod checkpoint_activation;
pub mod checkpoint_agreement;
mod checkpoint_beacon;
pub mod checkpoint_deadline;
pub mod checkpoint_driver;
pub mod checkpoint_governance;
mod checkpoint_installation;
pub mod checkpoint_outcome;
pub mod checkpoint_replay;
pub mod checkpoint_roster;
mod checkpoint_recovery;
mod checkpoint_selection;
pub mod checkpoint_service;
pub mod checkpoint_store;
pub mod checkpoint_sync;
pub mod checkpoint_transport;
pub mod config;
pub mod confirmation_processor;
mod confirmed_authority;
mod confirmed_governance;
pub mod confirmed_history;
pub mod confirmed_replay;
pub mod confirming_set;
pub mod connection_registry;
mod delegation_view;
mod durable_votes;
mod economic_candidate;
mod endorsement_commit;
pub mod error;
mod expiry_commit;
mod fork_resolution;
pub mod genesis_key;
mod genesis_state;
mod governance_commit;
pub mod governance_content;
mod graph_storage;
mod historical_dependencies;
mod historical_finality;
mod historical_lookup;
pub mod ledger_bridge;
pub mod ledger_cache;
pub mod ledger_event;
pub mod ledger_updater;
pub mod limits;
pub mod local_broadcaster;
pub mod logging;
pub mod metrics;
mod network_identity;
pub mod node;
pub mod online_weight;
pub mod parallel_processor;
mod payment_authority;
mod payment_epoch;
mod payment_exclusion;
mod payment_transition;
pub mod peer_connector;
mod peer_handshake;
pub mod priority_queue;
pub mod recently_confirmed;
mod release_policy;
pub mod rollback_plan;
pub mod shutdown;
pub mod snapshot_installation;
mod submission_results;
pub mod tracing_spans;
mod trst_storage;
pub mod unchecked;
mod verification_activation;
mod verification_commit;
mod verification_deadline;
pub mod verification_processor;
#[cfg(test)]
mod verification_test_support;
mod verification_transition;
mod verification_vote;
pub mod wire_message;

pub use block_processor::{
    BlockContext, BlockProcessor, BlockSource, ProcessResult, ProcessingQueue, RollbackResult,
};
pub use bootstrap::{BootstrapMessage, BootstrapServer, Bootstrapper};
pub use bounded_backlog::BoundedBacklog;
pub use config::NodeConfig;
pub use confirmation_processor::{
    CementResult, ChainWalker, ConfirmationProcessor, LmdbChainWalker,
};
pub use confirming_set::ConfirmingSet;
pub use connection_registry::ConnectionRegistry;
pub use error::NodeError;
pub use ledger_bridge::{process_block_economics, EconomicResult};
pub use ledger_event::{EventBus, LedgerEvent};
pub use ledger_updater::{
    create_pending_entry, delete_pending_entry, update_account_on_block, PendingInfo,
};
pub use limits::check_wallet_limits;
pub use local_broadcaster::LocalBroadcaster;
pub use logging::{init_logging, LogFormat};
pub use metrics::NodeMetrics;
pub use node::BurstNode;
pub use online_weight::OnlineWeightTracker;
pub use parallel_processor::ParallelBlockProcessor;
pub use peer_connector::{connect_to_peer, is_peer_connected, PeerConnectorContext};
pub use priority_queue::{work_difficulty, BlockPriorityQueue};
pub use recently_confirmed::RecentlyConfirmed;
pub use shutdown::ShutdownController;
pub use unchecked::{GapType, UncheckedMap};
pub use verification_processor::{VerificationOutcome, VerificationProcessor, VerifierPool};
pub use wire_message::{
    ConfirmAckMsg, ConfirmReqMsg, HandshakeMsg, KeepaliveMsg, WireMessage, WireVote,
};
