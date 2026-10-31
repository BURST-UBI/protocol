use super::*;
use crate::confirmed_replay::snapshot::installed::RecoveredSnapshot;

/// Per-transfer-service replay progress for exports without a snapshot base.
/// Stored observations are rechecked by the same advance path as peer recovery.
#[derive(Default)]
pub(crate) struct ExportRecovery {
    cached: Option<RecoveredSnapshot>,
    context: Option<(NetworkId, burst_types::BlockHash)>,
}

impl ExportRecovery {
    pub(crate) fn from_recovery(
        recovered: Option<&crate::confirmed_replay::snapshot::installed::ReplayCache>,
        store: &LmdbStore,
        network: NetworkId,
        params: &ProtocolParams,
    ) -> Result<Self, StoreError> {
        let Some(recovered) = recovered.filter(|cache| cache.base.is_none()) else {
            return Ok(Self::default());
        };
        let expected = ReplayedCheckpoint::genesis_anchor(network, params)?;
        let anchor = recovered.recovered.continuation.anchor();
        if recovered.recovered.uses_snapshot_archive()
            || anchor.network() != network
            || anchor.genesis_hash() != expected.genesis_hash()
        {
            return Err(invalid("export recovery belongs to a different genesis"));
        }
        Ok(Self {
            cached: Some(recovered.recovered.fork(store.env().path())?),
            context: Some((network, params.params_hash())),
        })
    }

    #[cfg(test)]
    pub(crate) fn height(&self) -> Option<u64> {
        self.cached
            .as_ref()
            .map(|cached| cached.continuation.anchor().height())
    }

    pub(super) fn clear(&mut self) {
        self.cached = None;
        self.context = None;
    }

    pub(super) fn genesis_selection(
        &mut self,
        store: &LmdbStore,
        network: NetworkId,
        params: &ProtocolParams,
        certificate: &CheckpointCertificate,
    ) -> Result<&RecoveredSnapshot, StoreError> {
        let context = (network, params.params_hash());
        if self.context.is_some_and(|previous| previous != context) {
            return Err(invalid("export replay has different network parameters"));
        }
        let epoch = certificate.checkpoint.hash();
        let recovered = match self.cached.take() {
            Some(cached) => {
                let anchor = cached.continuation.anchor();
                let target = if certificate.checkpoint.height > anchor.height() {
                    epoch
                } else {
                    anchor.hash()
                };
                cached.advance_locally(store, target)?
            }
            None => RecoveredSnapshot::recover_genesis_locally(store, network, params, epoch)?,
        };
        self.cached = Some(recovered);
        self.context = Some(context);
        Ok(self.cached.as_ref().expect("assigned export replay"))
    }
}
