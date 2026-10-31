//! Account-chain blocks, frontier helpers, snapshots and genesis construction.
//!
//! Accounts have separate signed chains. The node coordinates conflicts and
//! shared protocol transitions, including certified checkpoints. These ledger
//! types do not replace node admission, consensus or authenticated recovery.

pub mod account_chain;
pub mod error;
pub mod frontier;
pub mod genesis;
pub mod legacy;
#[doc(hidden)]
#[deprecated(note = "Compatibility interface; not the live ledger application boundary")]
pub use legacy::ledger;
#[doc(hidden)]
#[deprecated(note = "Candidate selection only; not production retirement authority")]
pub use legacy::pruning;
pub mod snapshot;
pub mod state_block;

pub use account_chain::AccountChain;
pub use error::LedgerError;
pub use frontier::DagFrontier;
pub use genesis::{legacy_genesis_fixture, legacy_genesis_fixture_hash, GenesisConfig};
#[doc(hidden)]
#[deprecated(note = "Compatibility interface; not implemented by the live node")]
pub use legacy::ledger::{Ledger, LedgerSummary};
#[doc(hidden)]
#[deprecated(note = "Candidate selection only; use authenticated retirement in the node")]
pub use legacy::pruning::{PruningCandidates, CandidateCounts, PruningConfig};
pub use snapshot::{AccountSnapshot, LedgerSnapshot};
pub use state_block::{BlockType, StateBlock, CURRENT_BLOCK_VERSION};
