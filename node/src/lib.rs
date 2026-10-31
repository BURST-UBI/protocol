//! Node runtime: block admission, economic transitions, consensus and recovery.
//!
//! [`BurstNode`] connects the domain engines to persistent storage and peer workers.
//! Applications supply public RPC and WebSocket listeners through
//! [`BurstNode::start_with_services`].

mod appeal_view;
mod backlog_scan;
pub mod block_processor;
mod block_retention;
mod block_validation;
pub mod bootstrap;
mod bootstrap_phase;
pub mod bounded_backlog;
mod causal_cementation;
#[cfg(test)]
mod certified_network_tests;
mod governance_read;
mod paged_metadata;

pub mod config;
mod confirmed_authority;
mod confirmed_governance;
pub mod confirmed_history;
pub mod confirmed_replay;
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
mod governance_storage;
mod graph_storage;
mod historical_dependencies;
mod verification_storage;
pub(crate) use confirmed_replay::retirement::worker as body_retirement;
pub mod account_work_pool;
pub mod block_economics;
mod brn_storage;
mod historical_finality;
mod historical_lookup;
pub mod ledger_cache;
pub mod ledger_updater;
pub mod limits;
pub mod local_broadcaster;
pub mod metrics;
mod network_identity;
pub mod node;
pub mod online_weight;
mod ordered_radix;
mod payment_authority;
mod payment_epoch;
mod payment_exclusion;
mod payment_transition;
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
#[cfg(test)]
mod verification_test_support;
mod verification_transition;
mod verification_vote;
pub mod wire_message;

pub use account_work_pool::AccountWorkPool;
pub use block_economics::{process_block_economics, EconomicResult};
pub use block_processor::{
    BlockContext, BlockProcessor, BlockSource, ProcessResult, ProcessingQueue, RollbackResult,
};
pub use bootstrap::{BootstrapMessage, BootstrapServer, Bootstrapper};
pub use bounded_backlog::BoundedBacklog;
pub use config::NodeConfig;
pub use connection_registry::ConnectionRegistry;
pub use error::NodeError;
pub use ledger_updater::{
    create_pending_entry, delete_pending_entry, update_account_on_block, PendingInfo,
};
pub use limits::check_wallet_spending_limit;
pub use local_broadcaster::LocalBroadcaster;
pub use metrics::NodeMetrics;
pub use node::BurstNode;
pub use online_weight::OnlineWeightTracker;
pub use peer_connector::{connect_to_peer, is_peer_connected, PeerConnectorContext};
pub use priority_queue::{work_difficulty, BlockPriorityQueue};
pub use recently_confirmed::RecentlyConfirmed;
pub use shutdown::ShutdownController;
pub use unchecked::{GapType, UncheckedMap};
pub use wire_message::{
    ConfirmAckMsg, ConfirmReqMsg, HandshakeMsg, KeepaliveMsg, WireMessage, WireVote,
};

#[cfg(test)]
pub(crate) mod test_rpc;

pub mod transport;
pub(crate) use transport::peer_handshake;
pub use transport::{connection_registry, peer_connector};

pub mod checkpoints;
pub use checkpoints::activation as checkpoint_activation;
pub use checkpoints::agreement as checkpoint_agreement;
pub(crate) use checkpoints::beacon as checkpoint_beacon;
pub use checkpoints::deadline as checkpoint_deadline;
pub use checkpoints::driver as checkpoint_driver;
pub use checkpoints::governance as checkpoint_governance;
pub(crate) use checkpoints::installation as checkpoint_installation;
pub use checkpoints::outcome as checkpoint_outcome;
pub(crate) use checkpoints::recovery as checkpoint_recovery;
pub use checkpoints::replay as checkpoint_replay;
pub use checkpoints::roster as checkpoint_roster;
pub(crate) use checkpoints::selection as checkpoint_selection;
pub use checkpoints::service as checkpoint_service;
pub use checkpoints::store as checkpoint_store;
pub use checkpoints::sync as checkpoint_sync;
pub use checkpoints::transport as checkpoint_transport;

pub mod services;
