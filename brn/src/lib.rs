//! BRN (Burn) — the birthright computation engine.
//!
//! BRN is a deterministic function of time, not a token on the ledger.
//! `BRN(w) = r × (t_now − t_verified(w)) − total_burned(w) − total_staked(w)`
//!
//! This crate handles:
//! - Balance computation from time and wallet state
//! - Recording burn operations (BRN → TRST minting)
//! - Staking/unstaking for verification and challenges
//! - Rate change splitting (preserving pre-change accrual)

pub mod engine;
pub mod error;
pub mod stake;
pub mod state;

pub use engine::BrnEngine;
pub use error::BrnError;
pub use stake::{Stake, StakeId, StakeKind};
pub use state::BrnWalletState;
