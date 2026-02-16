//! TRST portfolio management — tracking transferable, expired, and revoked TRST.

use burst_types::{Timestamp, TrstState};
use serde::{Deserialize, Serialize};

/// A TRST holding in the wallet's portfolio.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrstHolding {
    pub token_id: String,
    pub amount: u128,
    pub origin_wallet: String,
    pub origin_timestamp: Timestamp,
    pub state: TrstState,
    pub time_to_expiry_secs: Option<u64>,
}

/// Summary of a wallet's TRST portfolio.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortfolioSummary {
    pub transferable: u128,
    pub expired: u128,
    pub revoked: u128,
    pub total: u128,
    pub num_tokens: usize,
}
