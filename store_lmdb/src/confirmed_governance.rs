//! Confirmed schedule data, independent of provisional governance state.
use bincode::Options;
use burst_store::StoreError;
use burst_types::{BlockHash, ProtocolParams, Timestamp, WalletAddress};
use burst_utils::{SharedMap, SharedSet};
use serde::{Deserialize, Serialize};

pub const KEY: &str = "confirmed_governance/v2";
pub const HEADER: &str = "confirmed_governance/v3/header";
pub const ROW_PREFIX: &str = "confirmed_governance/v3/proposal/";
pub const MAX_PROPOSALS: usize = 65_536;
pub(crate) const MAX_BYTES: u64 = 16 << 20;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub proposer: WalletAddress,
    pub created_at: u64,
    pub timing: burst_governance::GovernanceTiming,
    pub endorsements: u32,
    pub last_endorsement: Option<u64>,
    pub withdrawn_at: Option<u64>,
    pub exploration_started_at: Option<u64>,
}

impl Schedule {
    pub fn counting_limit(&self, params: &ProtocolParams) -> Result<Option<u64>, StoreError> {
        if self.withdrawn_at.is_some()
            || (self.exploration_started_at.is_none()
                && self.endorsements == 0
                && params.governance_proposal_endorsements != 0)
        {
            return Ok(None);
        }
        let start = match self.exploration_started_at {
            Some(start) => Timestamp::new(start),
            None => self
                .timing
                .proposal_end(Timestamp::new(self.created_at))
                .map_err(|e| StoreError::Corruption(e.to_string()))?,
        };
        self.timing
            .counting_at(start)
            .map(|at| Some(at.as_secs()))
            .map_err(|e| StoreError::Corruption(e.to_string()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub epoch: BlockHash,
    pub params: ProtocolParams,
    pub proposals: SharedMap<[u8; 32], Schedule>,
}

impl Snapshot {
    /// Decode stored schedules with byte and proposal-count bounds. This does not
    /// authenticate confirmation or validate each schedule against ledger history.
    pub fn decode(bytes: &[u8]) -> Result<Self, StoreError> {
        if bytes.len() as u64 > MAX_BYTES {
            return Err(StoreError::Corruption(
                "confirmed governance exceeds byte limit".into(),
            ));
        }
        let snapshot: Self = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(MAX_BYTES)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
        if snapshot.proposals.len() > MAX_PROPOSALS {
            return Err(StoreError::Corruption(
                "confirmed governance exceeds proposal limit".into(),
            ));
        }
        Ok(snapshot)
    }

    pub fn limit_bytes(&self) -> Result<Vec<u8>, StoreError> {
        let mut limit: Option<u64> = None;
        for proposal in self.proposals.values() {
            if let Some(at) = proposal.counting_limit(&self.params)? {
                limit = Some(limit.map_or(at, |previous| previous.min(at)));
            }
        }
        Ok(burst_store::meta::encode_payment_vote_limit(
            self.epoch, limit,
        ))
    }
}

#[derive(Clone)]
pub(crate) struct Cached {
    pub snapshot: Snapshot,
    pub logical_bytes: u64,
    pub deadlines: SharedSet<(u64, [u8; 32])>,
    pub uses_row_storage: bool,
}
impl Cached {
    pub fn limit_bytes(&self) -> Vec<u8> {
        burst_store::meta::encode_payment_vote_limit(
            self.snapshot.epoch,
            self.deadlines.iter().next().map(|(at, _)| *at),
        )
    }
}
