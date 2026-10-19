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
    ) -> Result<(Self, RecoveryBase), StoreError> {
        match read_base(get, network, params, directory)? {
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
        // Choose before constructing the future, so polling does not move a
        // second recovery state machine onto the caller's stack.
        match self {
            Self::Archive(snapshot) =>
                Box::pin(snapshot.recover_stored_epoch_from_peers(store, registry, target)),
            Self::Genesis => Box::pin(RecoveredSnapshot::recover_genesis_from_peers(
                store, network, params, registry, Some(target),
            )),
        }
    }
}
