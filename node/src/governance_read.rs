//! Capture display inputs from persisted state; never confer signing authority.
use burst_governance::DelegationEngine;
use burst_rpc::governance_snapshot::{Reader, Selection, Snapshot};
use burst_store::StoreError;
use burst_store_lmdb::LmdbStore;
use burst_types::{ProtocolParams, Timestamp};
use std::sync::Arc;

pub(crate) struct GovernanceReader {
    pub store: Arc<LmdbStore>,
    pub fallback_params: ProtocolParams,
}

impl Reader for GovernanceReader {
    fn capture(&self, selection: Selection, now: Timestamp) -> Result<Snapshot, StoreError> {
        let view = self.store.tx_begin_read()?;
        let governance = self
            .store
            .read_governance_snapshot(&view)?
            .ok_or_else(|| StoreError::Corruption("missing persisted governance state".into()))?;
        let params = self
            .store
            .brn_store()
            .get_meta_txn(&view, b"protocol_params")?
            .map(|bytes| {
                bincode::deserialize(&bytes)
                    .map_err(|error| StoreError::Corruption(error.to_string()))
            })
            .transpose()?
            .unwrap_or_else(|| self.fallback_params.clone());
        let (selected, next_hash) = match selection {
            Selection::Page {
                after,
                offset,
                count,
            } => {
                if count > 100 {
                    return Err(StoreError::Serialization(
                        "governance page exceeds limit".into(),
                    ));
                }
                let mut hashes: Vec<_> = governance
                    .proposals_after(after)
                    .skip(usize::try_from(offset).unwrap_or(usize::MAX))
                    .take(count as usize + 1)
                    .map(|proposal| proposal.hash)
                    .collect();
                let more = hashes.len() > count as usize;
                if more {
                    hashes.pop();
                }
                let next = more.then(|| hashes.last().copied()).flatten();
                (hashes, next)
            }
            Selection::Info(hash) => (
                governance
                    .get_proposal(&hash)
                    .map(|_| vec![hash])
                    .unwrap_or_default(),
                None,
            ),
            Selection::All => (governance.all_proposals().map(|p| p.hash).collect(), None),
        };
        let needs_electorate = selected.iter().any(|hash| {
            governance.projection_requires_electorate(
                governance.get_proposal(hash).unwrap(),
                now,
                &params,
            )
        });
        let (delegation, verified) = if needs_electorate {
            let delegation = self.store.read_delegation_snapshot(&view)?.ok_or_else(|| {
                StoreError::Corruption("missing persisted delegation state".into())
            })?;
            let verified = self.store.read_verified_snapshot(&view)?;
            if verified.len() > u32::MAX as usize {
                return Err(StoreError::Corruption(
                    "governance electorate exceeds supported size".into(),
                ));
            }
            (delegation, verified)
        } else {
            (DelegationEngine::default(), Arc::from([]))
        };
        let activation_conflict = if let Selection::Info(hash) = selection {
            if selected.is_empty() {
                None
            } else {
                let constitution =
                    self.store
                        .read_constitution_snapshot(&view)?
                        .ok_or_else(|| {
                            StoreError::Corruption("missing persisted constitution state".into())
                        })?;
                constitution.activation_conflict(&hash)
            }
        } else {
            None
        };
        Ok(Snapshot {
            projection_identity: self.store.read_governance_projection_identity(&view),
            generation: view.generation(),
            governance,
            selected,
            next_hash,
            delegation,
            verified,
            params,
            now,
            activation_conflict,
        })
    }
}
