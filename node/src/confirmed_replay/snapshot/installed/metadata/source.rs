use super::*;

pub(super) enum RecoverySource {
    Archive(Box<AuthenticatedSnapshot>),
    Genesis,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RecoveryBase {
    Archive(BaseCommitment),
    Genesis,
}

impl RecoveryBase {
    pub(super) fn commitment(self) -> Option<BaseCommitment> {
        match self {
            Self::Archive(commitment) => Some(commitment),
            Self::Genesis => None,
        }
    }

    pub(super) fn verify(&self, batch: &LmdbWriteTransaction<'_>) -> Result<(), StoreError> {
        self.verify_view(&batch.read_view())
    }

    pub(super) fn verify_view(
        &self,
        batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    ) -> Result<(), StoreError> {
        match self {
            Self::Archive(commitment) => commitment.verify(|key| batch.get_meta(key)),
            Self::Genesis if batch.get_meta(BASE_KEY)?.is_none() => Ok(()),
            Self::Genesis => Err(invalid("snapshot base appeared during genesis recovery")),
        }
    }
}

impl RecoverySource {
    pub(super) fn read(
        get: impl FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>,
        network: NetworkId,
        params: &ProtocolParams,
        directory: &std::path::Path,
        check: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<(Self, RecoveryBase), StoreError> {
        let genesis =
            crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(network, params)?;
        match super::super::read_base_at_until(get, &genesis, directory, check)? {
            Some((snapshot, commitment)) => Ok((
                Self::Archive(Box::new(snapshot)),
                RecoveryBase::Archive(commitment),
            )),
            None => Ok((Self::Genesis, RecoveryBase::Genesis)),
        }
    }

    pub(super) fn recover(
        self,
        store: Arc<LmdbStore>,
        network: NetworkId,
        params: ProtocolParams,
        registry: Arc<RwLock<ConnectionRegistry>>,
        target: BlockHash,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<RecoveredSnapshot, StoreError>> + Send>,
    > {
        // Box only the selected recovery future, keeping its state off the caller's stack.
        match self {
            Self::Archive(snapshot) => {
                Box::pin(snapshot.recover_stored_epoch_from_peers(store, registry, target))
            }
            Self::Genesis => Box::pin(RecoveredSnapshot::recover_genesis_from_peers(
                store,
                network,
                params,
                registry,
                Some(target),
            )),
        }
    }
}
