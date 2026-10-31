use super::super::tail::cemented::AppliedTail;
use super::*;
use crate::confirmed_replay::ConfirmedReplay;
use bincode::Options;
use burst_types::{TxHash, WalletAddress};
use std::collections::HashMap;
mod read;
pub(super) use read::EconomicRead;

#[derive(Clone)]
pub(super) struct AppliedEconomics {
    pub(super) state: ConfirmedReplay,
    pub(super) ordering: Vec<(burst_types::BlockHash, burst_types::BlockHash)>,
}

impl AppliedEconomics {
    pub(super) fn mint_indexes(&self) -> Vec<(TxHash, burst_types::Timestamp)> {
        let mut mints: Vec<_> = self
            .state
            .minted_burns
            .iter()
            .map(|(token, at)| {
                (
                    *token,
                    burst_types::Timestamp::new(
                        at.as_secs()
                            .saturating_add(self.state.params.trst_expiry_secs),
                    ),
                )
            })
            .collect();
        mints.sort_by_key(|(token, _)| *token.as_bytes());
        mints
    }

    pub(super) fn replay_tail(
        recovered: &RecoveredSnapshot,
        applied: &AppliedTail,
        ordering: &[(burst_types::BlockHash, burst_types::BlockHash)],
    ) -> Result<ConfirmedReplay, StoreError> {
        Self::replay_tail_until(recovered, applied, ordering, || false)?
            .ok_or_else(|| invalid("uncancelled economic replay stopped"))
    }

    pub(super) fn replay_tail_until(
        recovered: &RecoveredSnapshot,
        applied: &AppliedTail,
        ordering: &[(burst_types::BlockHash, burst_types::BlockHash)],
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<ConfirmedReplay>, StoreError> {
        if cancelled() {
            return Ok(None);
        }
        let stage = std::time::Instant::now();
        let mut interrupted = false;
        let state = recovered.continuation.replay_applied_ordered(
            applied,
            &recovered.selections,
            ordering,
            &mut |_, _| {
                if cancelled() {
                    interrupted = true;
                    return Err(crate::confirmed_replay::ReplayError::Invalid(
                        "economic replay cancelled".into(),
                    ));
                }
                Ok(())
            },
        );
        if interrupted {
            return Ok(None);
        }
        let state = state.map_err(invalid)?;
        tracing::debug!(
            elapsed_ms = stage.elapsed().as_millis(),
            "applied economics replayed tail"
        );
        Ok(Some(state))
    }

    pub(super) fn materialize_view(
        view: &burst_store_lmdb::read_view::LedgerReadView<'_>,
        recovered: &RecoveredSnapshot,
        state: ConfirmedReplay,
        ordering: Vec<(burst_types::BlockHash, burst_types::BlockHash)>,
    ) -> Result<Self, StoreError> {
        Self::materialize_read(EconomicRead::Captured(view), recovered, state, ordering)
    }

    fn materialize_read(
        input: EconomicRead<'_, '_>,
        recovered: &RecoveredSnapshot,
        mut state: ConfirmedReplay,
        ordering: Vec<(burst_types::BlockHash, burst_types::BlockHash)>,
    ) -> Result<Self, StoreError> {
        let stage = std::time::Instant::now();
        let actual = input.verification()?.snapshot();
        if actual.encoded_size().map_err(invalid)? > 64 << 20 {
            return Err(invalid("applied verification snapshot exceeds limit"));
        }
        tracing::debug!(
            elapsed_ms = stage.elapsed().as_millis(),
            "applied economics loaded verification state"
        );
        let stage = std::time::Instant::now();
        super::publication::materialize(
            actual,
            &mut state,
            &recovered.selections,
            input.view().storage_directory(),
        )?;
        tracing::debug!(
            elapsed_ms = stage.elapsed().as_millis(),
            "applied economics checked verification publication"
        );
        Ok(Self { state, ordering })
    }

    pub(super) fn account(
        &self,
        address: &WalletAddress,
    ) -> Option<&burst_store::account::AccountInfo> {
        self.state.account(address)
    }

    pub(super) fn verify_unchanged(
        &self,
        batch: &LmdbWriteTransaction<'_>,
    ) -> Result<(), StoreError> {
        self.verify_runtime_state(batch).map(|_| ())
    }

    /// Return the same staged TRST state whose provenance was just compared.
    /// The caller must keep this writer unchanged before comparing expiry caches.
    pub(super) fn verify_runtime_state(
        &self,
        batch: &LmdbWriteTransaction<'_>,
    ) -> Result<burst_trst::TrstEngine, StoreError> {
        self.verify_read(EconomicRead::Writer(batch), &|_| Ok(()))
    }

    pub(super) fn verify_view(
        &self,
        view: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    ) -> Result<(), StoreError> {
        self.verify_view_with_poll(view, &|_| Ok(()))
    }

    pub(super) fn verify_view_with_poll(
        &self,
        view: &burst_store_lmdb::read_view::LedgerReadView<'_>,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.verify_read(EconomicRead::Captured(view), poll)
            .map(|_| ())
    }

    fn verify_read(
        &self,
        input: EconomicRead<'_, '_>,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<burst_trst::TrstEngine, StoreError> {
        poll("metadata:economics-start")?;
        let batch = input.view();
        batch.verify_checkpoint_lookup_indexes_with_poll(poll)?;
        let mut account_count = 0usize;
        for actual in batch.account_entries()? {
            let actual = actual?;
            poll("metadata:economic-account")?;
            account_count += 1;
            let mut expected = self
                .state
                .account(&actual.address)
                .ok_or_else(|| invalid("applied account has no replayed origin"))?
                .clone();
            // Finality is checked by the separate cemented cut. Expiry counters
            // may reflect a later local clock; revocation must match provenance.
            if self.state.trst.get_portfolio(&actual.address).is_some() {
                crate::ledger_updater::update_token_counters(
                    &mut expected,
                    &self.state.trst,
                    self.state.cutoff,
                )
                .map_err(invalid)?;
            }
            expected.confirmation_height = actual.confirmation_height;
            expected.expired_trst = actual.expired_trst;
            if expected != actual {
                return Err(invalid(
                    "applied account afterimage differs from economic replay",
                ));
            }
        }
        if account_count != self.state.accounts.len() {
            return Err(invalid("applied account inventory differs from replay"));
        }
        poll("metadata:economic-restore")?;
        let actual_trst =
            input.verify_brn_and_restore_trst(&self.state.params, &self.state.brn, poll)?;
        poll("metadata:economic-restored")?;
        let failure = std::cell::RefCell::new(None);
        let engine_poll = |stage| {
            poll(stage).map_err(|error| {
                let message = error.to_string();
                *failure.borrow_mut() = Some(error);
                message
            })
        };
        let matches = actual_trst
            .wallet_snapshot_eq_at_with_poll(
                &self.state.trst,
                self.state.params.trst_expiry_secs,
                self.state.cutoff,
                &engine_poll,
            )
            .and_then(|wallets_match| {
                if !wallets_match {
                    return Ok(false);
                }
                actual_trst
                    .merger_graph
                    .snapshot_eq_with_poll(&self.state.trst.merger_graph, &engine_poll)
            });
        if let Some(error) = failure.into_inner() {
            return Err(error);
        }
        if !matches.map_err(invalid)? {
            return Err(invalid(
                "persisted TRST provenance differs from applied replay",
            ));
        }
        verify_pending_view_with_poll(&batch, &self.state.pending, poll)?;
        poll("metadata:economic-caches")?;
        super::caches::verify_read_with_poll(input, &self.state, poll)?;
        Ok(actual_trst)
    }
}

#[cfg(test)]
fn verify_pending(
    batch: &LmdbWriteTransaction<'_>,
    expected_pending: &HashMap<(WalletAddress, TxHash), burst_store::pending::PendingInfo>,
) -> Result<(), StoreError> {
    verify_pending_view(&batch.read_view(), expected_pending)
}

#[cfg(test)]
fn verify_pending_view(
    batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    expected_pending: &HashMap<(WalletAddress, TxHash), burst_store::pending::PendingInfo>,
) -> Result<(), StoreError> {
    verify_pending_view_with_poll(batch, expected_pending, &|_| Ok(()))
}

fn verify_pending_view_with_poll(
    batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    expected_pending: &HashMap<(WalletAddress, TxHash), burst_store::pending::PendingInfo>,
    poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let mut pending_count = 0usize;
    for row in batch.pending_entries()? {
        let (key, bytes) = row?;
        poll("metadata:economic-pending")?;
        pending_count += 1;
        if key.len() <= 32 {
            return Err(invalid("invalid applied pending key"));
        }
        let (address, source) = key.split_at(key.len() - 32);
        let address = std::str::from_utf8(address).map_err(invalid)?;
        if burst_crypto::decode_address(address).is_none() {
            return Err(invalid("invalid applied pending destination"));
        }
        let source = TxHash::new(source.try_into().map_err(invalid)?);
        let info: burst_store::pending::PendingInfo = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(64 << 20)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map_err(invalid)?;
        let expected = expected_pending
            .get(&(WalletAddress::new(address), source))
            .ok_or_else(|| invalid("pending transfer has no replayed origin"))?;
        if &info != expected {
            return Err(invalid("pending transfer differs from applied replay"));
        }
    }
    if pending_count != expected_pending.len() {
        return Err(invalid("pending transfers differ from applied replay"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::pending::PendingInfo;
    use burst_types::{PublicKey, Timestamp};

    #[test]
    fn pending_recovery_compares_each_claim_and_the_complete_inventory() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let destination = burst_crypto::derive_address(&PublicKey([1; 32]));
        let hash = TxHash::new([2; 32]);
        let pending = PendingInfo {
            source: burst_crypto::derive_address(&PublicKey([3; 32])),
            amount: 7,
            timestamp: Timestamp::new(4),
            provenance: vec![burst_store::pending::PendingProvenance {
                basis: burst_types::TrstBasis::Entire,
                amount: 7,
                origin: TxHash::new([4; 32]),
                origin_wallet: destination.clone(),
                origin_timestamp: Timestamp::new(1),
                effective_origin_timestamp: Timestamp::new(2),
                revoked_amounts: std::collections::BTreeMap::from([(
                    [5; 32],
                    burst_types::TrstRevocationChunk {
                        amount: 1,
                        restored: false,
                    },
                )]),
            }],
            return_credit: true,
        };
        let expected = HashMap::from([((destination.clone(), hash), pending.clone())]);
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_pending(
                &destination,
                hash.as_bytes(),
                &bincode::serialize(&pending).unwrap(),
            )
            .unwrap();
        batch.commit().unwrap();
        let reader = store.tx_begin_read().unwrap();
        let view = store.read_view(&reader).unwrap();
        verify_pending_view(&view, &expected).unwrap();
        let reached = std::cell::Cell::new(false);
        assert!(verify_pending_view_with_poll(&view, &expected, &|stage| {
            assert_eq!(stage, "metadata:economic-pending");
            reached.set(true);
            Err(StoreError::Backend("pending checkpoint sentinel".into()))
        })
        .unwrap_err()
        .to_string()
        .contains("pending checkpoint sentinel"));
        assert!(reached.get());
        verify_pending_view(&view, &expected).unwrap();
        for case in 0..16 {
            let mut batch = store.tx_begin_write().unwrap();
            verify_pending(&batch, &expected).unwrap();
            let mut changed = pending.clone();
            match case {
                0 => {
                    changed.amount += 1;
                }
                1 => {
                    changed.return_credit = false;
                }
                2 => {
                    changed.source = destination.clone();
                }
                3 => {
                    changed.timestamp = Timestamp::new(5);
                }
                4 => {
                    batch.delete_pending(&destination, hash.as_bytes()).unwrap();
                }
                5 => {
                    batch
                        .put_pending(
                            &destination,
                            &[9; 32],
                            &bincode::serialize(&pending).unwrap(),
                        )
                        .unwrap();
                }
                6 => changed.provenance[0].basis = burst_types::TrstBasis::Single(hash),
                7 => changed.provenance[0].amount += 1,
                8 => changed.provenance[0].origin = hash,
                9 => changed.provenance[0].origin_wallet = changed.source.clone(),
                10 => changed.provenance[0].origin_timestamp = Timestamp::new(9),
                11 => changed.provenance[0].effective_origin_timestamp = Timestamp::new(9),
                12 => {
                    changed.provenance[0]
                        .revoked_amounts
                        .get_mut(&[5; 32])
                        .unwrap()
                        .amount += 1
                }
                13 => {
                    changed.provenance[0]
                        .revoked_amounts
                        .get_mut(&[5; 32])
                        .unwrap()
                        .restored = true
                }
                14 => {
                    changed.provenance[0].revoked_amounts.insert(
                        [6; 32],
                        burst_types::TrstRevocationChunk {
                            amount: 1,
                            restored: false,
                        },
                    );
                }
                _ => changed.provenance.clear(),
            }
            if case < 4 || case >= 6 {
                batch
                    .put_pending(
                        &destination,
                        hash.as_bytes(),
                        &bincode::serialize(&changed).unwrap(),
                    )
                    .unwrap();
            }
            assert!(verify_pending(&batch, &expected).is_err(), "case {case}");
            assert!(
                verify_pending_view(&batch.read_view(), &expected).is_err(),
                "view case {case}"
            );
            verify_pending_view(&view, &expected).unwrap();
            drop(batch);
            verify_pending(&store.tx_begin_write().unwrap(), &expected).unwrap();
        }
    }
}
