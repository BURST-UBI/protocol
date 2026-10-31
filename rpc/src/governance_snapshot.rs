//! Owned inputs for display projection after the storage view has closed.
use burst_governance::{DelegationEngine, GovernanceEngine};
use burst_store::StoreError;
use burst_types::{ProtocolParams, Timestamp, TxHash, WalletAddress};

#[derive(Clone, Copy)]
pub enum Selection {
    Page { after: Option<TxHash>, offset: u64, count: u32 },
    Info(TxHash),
    All,
}

pub struct Snapshot {
    /// Stable committed-view identity within this Reader; None disables reuse.
    pub generation: Option<usize>,
    /// Optional non-reused identity of all projection inputs. Providers must
    /// invalidate it on governance, delegation or electorate changes.
    pub projection_identity: Option<usize>,
    pub governance: GovernanceEngine,
    /// Every selected hash must exist in `governance`; projection assumes this.
    pub selected: Vec<TxHash>,
    pub next_hash: Option<TxHash>,
    /// Delegation state for tallying the selected proposals. May be empty when
    /// none requires an electorate at `now` under `params`.
    pub delegation: DelegationEngine,
    /// Verified electorate from the same view, or empty when no tally is needed.
    pub verified: std::sync::Arc<[WalletAddress]>,
    pub params: ProtocolParams,
    pub now: Timestamp,
    pub activation_conflict: Option<burst_consti::AmendmentConflict>,
}

/// Capture persisted inputs from one committed storage view and use the supplied
/// display time. Selected hashes must exist in the returned governance engine.
/// No transaction or live-engine lock may escape capture. Known generations must
/// identify all persisted inputs without reuse for different state during this
/// Reader's lifetime; unknown identity disables display-cache reuse.
pub trait Reader: Send + Sync {
    fn capture(&self, selection: Selection, now: Timestamp) -> Result<Snapshot, StoreError>;
}
