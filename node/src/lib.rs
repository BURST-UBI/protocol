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

pub mod block_processor;
pub mod bootstrap;
pub mod bounded_backlog;
pub mod config;
pub mod confirmation_processor;
pub mod confirming_set;
pub mod connection_registry;
pub mod error;
pub mod ledger_bridge;
pub mod ledger_cache;
pub mod ledger_event;
pub mod ledger_updater;
pub mod limits;
pub mod local_broadcaster;
pub mod logging;
pub mod metrics;
pub mod node;
pub mod online_weight;
pub mod parallel_processor;
pub mod peer_connector;
pub mod priority_queue;
pub mod recently_confirmed;
pub mod shutdown;
pub mod tracing_spans;
pub mod unchecked;
pub mod verification_processor;
pub mod wire_message;

pub use block_processor::{
    BlockContext, BlockProcessor, BlockSource, ProcessResult, ProcessingQueue, RollbackResult,
};
pub use bootstrap::{BootstrapClient, BootstrapMessage, BootstrapServer};
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
