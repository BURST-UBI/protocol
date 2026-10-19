use super::super::{canonical, tail::cemented::AppliedTail};
use super::*;
use crate::confirmed_replay::ConfirmedReplay;
use bincode::Options;
use burst_types::{TxHash, WalletAddress};
use std::collections::HashMap;

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

    pub(super) fn replay(
        batch: &LmdbWriteTransaction<'_>,
        recovered: &RecoveredSnapshot,
        applied: &AppliedTail,
        ordering: Vec<(burst_types::BlockHash, burst_types::BlockHash)>,
    ) -> Result<Self, StoreError> {
        let mut state = if ordering.is_empty() {
            recovered
                .continuation
                .replay_applied(applied, &recovered.selections)
        } else {
            recovered.continuation.replay_applied_ordered(
                applied,
                &recovered.selections,
                &ordering,
                &mut |_, _| Ok(()),
            )
        }
        .map_err(invalid)?;
        let bytes = batch
            .get_meta(crate::endorsement_commit::SNAPSHOT_KEY)?
            .ok_or_else(|| invalid("missing applied verification snapshot"))?;
        if bytes.len() > 64 << 20 {
            return Err(invalid("applied verification snapshot exceeds limit"));
        }
        let actual = burst_verification::OrchestratorSnapshot::decode(&bytes).map_err(invalid)?;
        super::publication::materialize(actual, &mut state, &recovered.selections)?;
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
        batch.verify_checkpoint_lookup_indexes()?;
        let mut account_count = 0usize;
        for actual in batch.account_entries()? {
            let actual = actual?;
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
            if canonical::encode(&expected).map_err(invalid)?
                != canonical::encode(&actual).map_err(invalid)?
            {
                return Err(invalid(
                    "applied account afterimage differs from economic replay",
                ));
            }
        }
        if account_count != self.state.accounts.len() {
            return Err(invalid("applied account inventory differs from replay"));
        }
        let restored = crate::payment_transition::restore(batch, &self.state.params)?;
        let actual = restored.brn;
        let expected = &self.state.brn;
        if actual.wallets != expected.wallets
            || actual.rate_history != expected.rate_history
            || actual.next_stake_id() != expected.next_stake_id()
        {
            return Err(invalid("persisted BRN state differs from applied replay"));
        }
        let mut actual_trst = restored.trst;
        let mut expected_trst = self.state.trst.clone();
        // Compare private copies at the same clock. This changes only derived
        // expiry state, never revocation, ownership, or signed balances.
        actual_trst.set_expiry_period(self.state.params.trst_expiry_secs, self.state.cutoff);
        expected_trst.set_expiry_period(self.state.params.trst_expiry_secs, self.state.cutoff);
        if !actual_trst.wallet_snapshot_eq(&expected_trst)
            || actual_trst.merger_graph != expected_trst.merger_graph
        {
            return Err(invalid(
                "persisted TRST provenance differs from applied replay",
            ));
        }
        verify_pending(batch, &self.state.pending)?;
        super::caches::verify(batch, &self.state)
    }
}

fn verify_pending(
    batch: &LmdbWriteTransaction<'_>,
    expected_pending: &HashMap<(WalletAddress, TxHash), burst_store::pending::PendingInfo>,
) -> Result<(), StoreError> {
    let mut pending_count = 0usize;
    for row in batch.pending_entries()? {
        let (key, bytes) = row?;
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
        if canonical::encode(&info).map_err(invalid)?
            != canonical::encode(expected).map_err(invalid)?
        {
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
            provenance: Vec::new(),
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
        for case in 0..6 {
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
                _ => {
                    batch
                        .put_pending(
                            &destination,
                            &[9; 32],
                            &bincode::serialize(&pending).unwrap(),
                        )
                        .unwrap();
                }
            }
            if case < 4 {
                batch
                    .put_pending(
                        &destination,
                        hash.as_bytes(),
                        &bincode::serialize(&changed).unwrap(),
                    )
                    .unwrap();
            }
            assert!(verify_pending(&batch, &expected).is_err(), "case {case}");
            drop(batch);
            verify_pending(&store.tx_begin_write().unwrap(), &expected).unwrap();
        }
    }
}
