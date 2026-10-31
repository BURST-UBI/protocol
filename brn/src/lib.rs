//! BRN accrual, burns, stakes and release accounting.
//!
//! BRN is computed from eligible time and the shared rate history. Each interval
//! earns its historical rate; paused intervals earn nothing. Available balance
//! subtracts cumulative burns and active stakes from that accrual. Spendable
//! balance also subtracts returned stakes awaiting signed acknowledgment.
//!
//! This crate updates accounting state. Callers authenticate operations, decide
//! verification outcomes and apply any corresponding TRST payment.

pub mod engine;
pub mod pauses;
pub mod error;
pub mod releases;
pub mod rate_storage;
pub mod wallet_storage;
pub mod retention;
pub mod stake;
pub mod state;
pub mod wallet_states;

pub use engine::BrnEngine;
pub use error::BrnError;
pub use stake::{Stake, StakeId, StakeKind};
pub use state::{BrnWalletState, RateHistory, RateSegment};
