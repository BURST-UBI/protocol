use super::*;
use burst_store_lmdb::{block_undo::UndoTable, read_view::LedgerReadView};

/// Recovery chooses a real writer (with incremental caches) or a captured reader.
/// Both paths retain the same economic comparisons; this type grants no publication.
#[derive(Clone, Copy)]
pub(in crate::confirmed_replay::snapshot::installed) enum EconomicRead<'a, 'db> {
    Writer(&'a LmdbWriteTransaction<'db>),
    Captured(&'a LedgerReadView<'db>),
}

impl EconomicRead<'_, '_> {
    pub(in crate::confirmed_replay::snapshot::installed) fn view(&self) -> LedgerReadView<'_> {
        match self {
            Self::Writer(writer) => writer.read_view(),
            Self::Captured(view) => (*view).clone(),
        }
    }

    pub(super) fn verify_brn_and_restore_trst(
        &self,
        params: &ProtocolParams,
        expected: &burst_brn::BrnEngine,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<burst_trst::TrstEngine, StoreError> {
        match self {
            Self::Writer(writer) => {
                let restored = crate::payment_transition::restore(writer, params)?;
                if restored.brn.wallets != expected.wallets
                    || restored.brn.rate_history != expected.rate_history
                    || restored.brn.next_stake_id() != expected.next_stake_id()
                {
                    return Err(invalid("persisted BRN state differs from applied replay"));
                }
                Ok(restored.trst)
            }
            Self::Captured(view) => {
                view.verify_brn_engine_with_poll(expected, 131_072, 64 << 20, poll)?;
                view.read_trst_engine_for_comparison_with_poll(params.trst_expiry_secs, poll)
            }
        }
    }

    pub(in crate::confirmed_replay::snapshot::installed) fn verification(
        &self,
    ) -> Result<burst_verification::VerificationOrchestrator, StoreError> {
        match self {
            Self::Writer(writer) => crate::verification_storage::load(writer),
            Self::Captured(view) => crate::verification_storage::load_view(view),
        }
    }

    pub(in crate::confirmed_replay::snapshot::installed) fn verification_snapshot(
        &self,
    ) -> Result<burst_verification::OrchestratorSnapshot, StoreError> {
        if let Self::Captured(view) = self {
            if view.uses_verification_records()? {
                // Record-level checks do not bound the combined snapshot size.
                let snapshot = view.read_verification_records()?.snapshot();
                if snapshot.encoded_size().map_err(invalid)? > 64 << 20 {
                    return Err(invalid("applied engine snapshot exceeds limit"));
                }
                return Ok(snapshot);
            }
        }
        let bytes = self.read(
            UndoTable::Meta,
            crate::endorsement_commit::SNAPSHOT_KEY.as_bytes(),
        )?;
        burst_verification::OrchestratorSnapshot::decode(&bytes).map_err(invalid)
    }

    pub(in crate::confirmed_replay::snapshot::installed) fn governance(
        &self,
    ) -> Result<Option<burst_governance::engine::GovernanceEngine>, StoreError> {
        match self {
            Self::Writer(writer) => crate::governance_storage::load(writer),
            Self::Captured(view) => crate::governance_storage::load_view(view),
        }
    }

    pub(in crate::confirmed_replay::snapshot::installed) fn read(
        &self,
        table: UndoTable,
        key: &[u8],
    ) -> Result<Vec<u8>, StoreError> {
        let bytes = match self {
            Self::Writer(writer) => writer.undo_value(table, key)?,
            Self::Captured(view)
                if table == UndoTable::Meta
                    && key == crate::endorsement_commit::SNAPSHOT_KEY.as_bytes()
                    && view.uses_verification_records()? =>
            {
                let snapshot = self.verification()?.snapshot();
                if snapshot.encoded_size().map_err(invalid)? > 64 << 20 {
                    return Err(invalid("applied engine snapshot exceeds limit"));
                }
                Some(snapshot.encode().map_err(invalid)?)
            }
            Self::Captured(view) => view.undo_value_ref(table, key)?.map(<[u8]>::to_vec),
        }
        .ok_or_else(|| invalid("missing applied engine snapshot"))?;
        if bytes.len() > 64 << 20 {
            return Err(invalid("applied engine snapshot exceeds limit"));
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_verification_matches_virtual_writer_images_in_all_formats() {
        for format in 0..3 {
            let directory = tempfile::tempdir().unwrap();
            let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
            let mut engine = burst_verification::VerificationOrchestrator::new();
            let owner =
                burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[31; 32]).public);
            engine.initialize_genesis(&owner);
            let mut snapshot = engine.snapshot();
            let votes: Vec<_> = (32..35)
                .map(|seed| burst_verification::state::VerifierVote {
                    verifier: burst_crypto::derive_address(
                        &burst_crypto::keypair_from_seed(&[seed; 32]).public,
                    ),
                    vote: burst_verification::Vote::Legitimate,
                    stake_amount: 10,
                    timestamp: burst_types::Timestamp::new(100),
                })
                .collect();
            let current = snapshot.states.get_mut(&owner).unwrap();
            current.votes = votes.clone().into();
            current.selected_verifiers = votes.iter().map(|vote| vote.verifier.clone()).collect();
            snapshot.prior_votes.insert(owner.clone(), votes);
            engine = burst_verification::VerificationOrchestrator::restore(snapshot);
            let key = crate::endorsement_commit::SNAPSHOT_KEY;
            let mut writer = store.tx_begin_write().unwrap();
            writer
                .put_meta(key, &engine.snapshot().encode().unwrap())
                .unwrap();
            if format > 0 {
                writer.migrate_verification_records().unwrap();
            }
            if format == 2 {
                writer.normalize_verification_records().unwrap();
            }
            let expected = EconomicRead::Writer(&writer)
                .read(UndoTable::Meta, key.as_bytes())
                .unwrap();
            assert_eq!(
                EconomicRead::Writer(&writer)
                    .verification_snapshot()
                    .unwrap()
                    .encode()
                    .unwrap(),
                expected
            );
            writer.commit().unwrap();
            let reader = store.tx_begin_read().unwrap();
            let view = store.read_view(&reader).unwrap();
            let captured = EconomicRead::Captured(&view);
            assert_eq!(
                captured.read(UndoTable::Meta, key.as_bytes()).unwrap(),
                expected
            );
            assert_eq!(
                captured.verification_snapshot().unwrap().encode().unwrap(),
                expected
            );
            assert_eq!(
                captured
                    .verification()
                    .unwrap()
                    .snapshot()
                    .encode()
                    .unwrap(),
                expected
            );
            if format > 0 {
                use super::super::super::caches::verify_verification_records_view as verify;
                assert!(view
                    .undo_value_ref(UndoTable::Meta, key.as_bytes())
                    .unwrap()
                    .is_none());
                verify(&view, &engine, &|_| Ok(())).unwrap();
                let canonical = crate::confirmed_replay::snapshot::canonical::encode;
                let original = canonical(&engine.snapshot()).unwrap();
                assert_eq!(
                    crate::confirmed_replay::snapshot::canonical::encode(&engine.snapshot_view())
                        .unwrap(),
                    original
                );
                for change in 0..4 {
                    let mut snapshot = engine.snapshot();
                    match change {
                        0 => {
                            snapshot.states.get_mut(&owner).unwrap().votes.reverse();
                            snapshot.prior_votes.get_mut(&owner).unwrap().reverse();
                        }
                        1 => snapshot
                            .states
                            .get_mut(&owner)
                            .unwrap()
                            .selected_verifiers
                            .reverse(),
                        2 => snapshot.states.get_mut(&owner).unwrap().votes[0].stake_amount += 1,
                        _ => snapshot.neither_tracker.configure(1234, 42),
                    }
                    let same = canonical(&snapshot).unwrap() == original;
                    assert_eq!(same, change == 0);
                    let candidate = burst_verification::VerificationOrchestrator::restore(snapshot);
                    assert_eq!(
                        crate::confirmed_replay::snapshot::canonical::encode(
                            &candidate.snapshot_view()
                        )
                        .unwrap(),
                        canonical(&candidate.snapshot()).unwrap()
                    );
                    let mut streamed = Vec::new();
                    crate::confirmed_replay::snapshot::canonical::encode_verification_into(
                        &candidate,
                        &mut streamed,
                        Some(directory.path()),
                    )
                    .unwrap();
                    assert_eq!(streamed, canonical(&candidate.snapshot()).unwrap());
                    assert_eq!(verify(&view, &candidate, &|_| Ok(())).is_ok(), same);
                }
                let empty = burst_verification::VerificationOrchestrator::new();
                assert!(verify(&view, &empty, &|_| Ok(())).is_err());
                let mut extra = engine.clone();
                extra.initialize_genesis(&burst_crypto::derive_address(
                    &burst_crypto::keypair_from_seed(&[32; 32]).public,
                ));
                assert!(verify(&view, &extra, &|_| Ok(())).is_err());
                for stage in [
                    "metadata:verification-canonical",
                    "metadata:verification-expected",
                    "metadata:verification-record",
                    "storage:verification-row",
                    "storage:verification-finish",
                ] {
                    let error = verify(&view, &engine, &|at| {
                        if at == stage {
                            Err(StoreError::StaleRead(stage.into()))
                        } else {
                            Ok(())
                        }
                    })
                    .unwrap_err();
                    assert!(
                        matches!(error, StoreError::StaleRead(ref message) if message == stage)
                    );
                }
            }
            let mut writer = store.tx_begin_write().unwrap();
            if format == 0 {
                writer.put_meta(key, b"corrupt").unwrap();
            } else {
                writer
                    .put_meta("verification_records_format", &[99])
                    .unwrap();
            }
            writer.commit().unwrap();
            assert_eq!(
                captured.read(UndoTable::Meta, key.as_bytes()).unwrap(),
                expected
            );
            assert_eq!(
                captured.verification_snapshot().unwrap().encode().unwrap(),
                expected
            );
            let writer = store.tx_begin_write().unwrap();
            assert!(EconomicRead::Writer(&writer)
                .verification_snapshot()
                .is_err());
            assert!(EconomicRead::Captured(&writer.read_view())
                .verification_snapshot()
                .is_err());
            assert!(EconomicRead::Writer(&writer).verification().is_err());
            assert!(EconomicRead::Captured(&writer.read_view())
                .verification()
                .is_err());
        }
    }
}
