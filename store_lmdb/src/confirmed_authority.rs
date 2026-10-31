//! Stored confirmed-authority projections shared across managed commits.
use bincode::Options;
use burst_store::{account::AccountInfo, StoreError};
use burst_utils::SharedMap;
use burst_types::{BlockHash, Timestamp, WalletAddress, WalletState};
use serde::{Deserialize, Serialize};

pub const KEY: &str = "confirmed_authority/v1";
pub const HEADER: &str = "confirmed_authority/v2/header";
pub const ROW_PREFIX: &str = "confirmed_authority/v2/account/";

#[derive(Clone)]
pub(crate) struct Cached {
    pub snapshot: Snapshot,
    pub logical_bytes: u64,
    pub uses_row_storage: bool,
}
pub(crate) const MAX_BYTES: u64 = 64 << 20;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Authority {
    pub address: WalletAddress,
    pub state: WalletState,
    pub representative: WalletAddress,
    pub verified_at: Option<Timestamp>,
    pub opted_in_at: Option<Timestamp>,
    pub evicted: bool,
}
impl From<&AccountInfo> for Authority {
    fn from(account: &AccountInfo) -> Self {
        Self { address: account.address.clone(), state: account.state,
            representative: account.representative.clone(), verified_at: account.verified_at,
            opted_in_at: account.verifier_opted_in_at, evicted: account.orv_evicted }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub genesis: BlockHash,
    pub accounts: SharedMap<String, Authority>,
}
impl Snapshot {
    /// Decode bounded storage data and check genesis, account count and key agreement.
    /// The caller establishes that these records came from confirmed ledger state.
    pub fn decode(bytes: &[u8]) -> Result<Self, StoreError> {
        if bytes.len() as u64 > MAX_BYTES {
            return Err(StoreError::Corruption("confirmed authority exceeds byte limit".into()));
        }
        let state: Self = bincode::DefaultOptions::new().with_fixint_encoding()
            .with_limit(MAX_BYTES).reject_trailing_bytes().deserialize(bytes)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
        if state.genesis.is_zero() || state.accounts.is_empty() || state.accounts.len() > 1_000_000
            || state.accounts.iter().any(|(key, account)| key != account.address.as_str()) {
            return Err(StoreError::Corruption("invalid confirmed authority projection".into()));
        }
        Ok(state)
    }
}
