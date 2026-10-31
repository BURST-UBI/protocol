//! Payment weights are fixed by the installed checkpoint, not local cementation.

use crate::confirmed_replay::installation::{
    authority_bytes, vote_limit_bytes, CUTOFF_KEY, INSTALLED_KEY,
};
use bincode::Options;
use burst_consensus::{
    checkpoint::{HumanDelegation, HumanRoster},
    RepWeightCache,
};
use burst_store::{meta::MetaStore, StoreError};
use burst_store_lmdb::LmdbStore;
use burst_types::{BlockHash, NetworkId, ProtocolParams};

pub(crate) mod storage;

pub(crate) struct PaymentAuthority {
    pub epoch: BlockHash,
    pub roster: HumanRoster,
    pub quorum_bps: u16,
    pub cutoff: u64,
}

fn invalid(message: impl ToString) -> StoreError {
    StoreError::Corruption(message.to_string())
}

impl PaymentAuthority {
    pub fn weights(&self) -> RepWeightCache {
        let mut weights = RepWeightCache::new();
        weights.rebuild_from_accounts(
            self.roster
                .humans()
                .iter()
                .map(|human| (true, human.representative.clone())),
        );
        weights
    }

    /// Read the authority and related epoch/cutoff/limit records in one snapshot.
    /// Checks local consistency, not checkpoint signatures or replay validity;
    /// initialize performs startup authentication against certified history.
    pub fn read(store: &LmdbStore) -> Result<Option<Self>, StoreError> {
        let txn = store.tx_begin_read()?;
        let meta = store.meta_store();
        Self::read_at(|key| meta.get_meta_txn(&txn, key))
    }

    /// The caller must read every key from the same pinned transaction.
    /// Decodes the entire roster; the 64 MiB encoded limit is not a heap budget.
    pub(crate) fn read_at(
        mut get: impl FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>,
    ) -> Result<Option<Self>, StoreError> {
        let Some(bytes) = storage::read_at(&mut get)? else {
            if get(CUTOFF_KEY)?.is_some()
                || get(burst_store::meta::PAYMENT_VOTE_LIMIT_KEY)?.is_some()
                || get(burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY)?.is_some()
                || get(crate::confirmed_governance::KEY)?.is_some()
                || get(crate::confirmed_governance::HEADER)?.is_some()
            {
                return Err(invalid(
                    "payment cutoff or vote limit has no installed authority",
                ));
            }
            return Ok(None);
        };
        let (epoch, humans, quorum_bps, cutoff): (BlockHash, Vec<HumanDelegation>, u16, u64) =
            bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .with_limit(64 << 20)
                .reject_trailing_bytes()
                .deserialize(&bytes)
                .map_err(invalid)?;
        let roster = HumanRoster::new(humans).map_err(invalid)?;
        if epoch.is_zero()
            || !(6667..=10000).contains(&quorum_bps)
            || get(burst_store::meta::PAYMENT_EPOCH_KEY)?.as_deref()
                != Some(epoch.as_bytes().as_slice())
            || get(CUTOFF_KEY)?.as_deref() != Some(cutoff.to_be_bytes().as_slice())
            || get(&burst_store::meta::closed_payment_epoch_key(&epoch))?.is_some()
        {
            return Err(invalid("inconsistent installed payment authority"));
        }
        let limit = get(burst_store::meta::PAYMENT_VOTE_LIMIT_KEY)?
            .ok_or_else(|| invalid("certified payment vote limit is missing"))?;
        if burst_store::meta::decode_payment_vote_limit(&limit, epoch)?
            .is_some_and(|limit| limit <= cutoff)
        {
            return Err(invalid(
                "certified payment vote limit does not extend its cutoff",
            ));
        }
        let counting = get(burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY)?
            .ok_or_else(|| invalid("confirmed governance counting limit is missing"))?;
        if burst_store::meta::decode_payment_vote_limit(&counting, epoch)?
            .is_some_and(|limit| limit <= cutoff)
        {
            return Err(invalid(
                "confirmed governance counting limit does not extend its cutoff",
            ));
        }
        Ok(Some(Self {
            epoch,
            roster,
            quorum_bps,
            cutoff,
        }))
    }
}

/// Initialize the certified mode at genesis, or authenticate its installed
/// checkpoint on restart. Never derive authority from provisional accounts.
pub(crate) fn initialize(
    store: &LmdbStore,
    network: NetworkId,
    params: &ProtocolParams,
) -> Result<PaymentAuthority, StoreError> {
    let genesis = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(network, params)?;
    let epoch = store.meta_store().payment_epoch()?;
    if epoch == genesis.hash() {
        let cutoff = crate::genesis_state::block(network, params)
            .timestamp
            .as_secs();
        let expected = authority_bytes(&genesis, cutoff)?;
        let limit = vote_limit_bytes(epoch, cutoff, None)?;
        let has_authority = {
            let read = store.tx_begin_read()?;
            store
                .meta_store()
                .get_meta_txn(&read, INSTALLED_KEY)?
                .is_some()
        };
        if !has_authority {
            let heads = crate::confirmed_history::ConfirmedHistory::heads_at(store, u64::MAX)?;
            if !heads.is_empty() {
                let history = crate::confirmed_history::ConfirmedHistory::read(
                    store,
                    network,
                    genesis.genesis_hash(),
                    u64::MAX,
                    &heads,
                    &[],
                )?;
                if history.replay_blocks().iter().any(|block| {
                    crate::confirmed_governance::ConfirmedGovernance::applies_to(&block.block_type)
                }) {
                    return Err(invalid(
                        "uncertified governance history cannot initialize payment authority",
                    ));
                }
            }
        }
        {
            let mut batch = store.tx_begin_write()?;
            if batch.payment_epoch()? != epoch {
                return Err(invalid("payment authority changed during startup"));
            }
            match storage::read_at(|key| batch.get_meta(key))? {
                Some(bytes) if bytes != expected => {
                    return Err(invalid("genesis payment authority changed"))
                }
                Some(_) => {
                    if batch.get_meta(burst_store::meta::PAYMENT_VOTE_LIMIT_KEY)? != Some(limit) {
                        return Err(invalid("genesis payment vote limit changed"));
                    }
                }
                None => {
                    if batch.get_meta(CUTOFF_KEY)?.is_some()
                        || batch
                            .get_meta(burst_store::meta::PAYMENT_VOTE_LIMIT_KEY)?
                            .is_some()
                        || batch
                            .get_meta(burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY)?
                            .is_some()
                        || batch.has_confirmed_governance_storage()?
                    {
                        return Err(invalid(
                            "genesis cutoff or vote limit has no installed authority",
                        ));
                    }
                    storage::write(&mut batch, &expected)?;
                    batch.put_meta(CUTOFF_KEY, &cutoff.to_be_bytes())?;
                    batch.put_meta(burst_store::meta::PAYMENT_VOTE_LIMIT_KEY, &limit)?;
                    crate::confirmed_governance::ConfirmedGovernance::initial(
                        epoch,
                        params.clone(),
                    )
                    .persist(&mut batch)?;
                    batch.commit()?;
                    store.env().force_sync().map_err(invalid)?;
                }
            }
        }
        crate::confirmed_governance::ConfirmedGovernance::initial(epoch, params.clone())
            .verify_tail(store, network, genesis.genesis_hash())?;
    } else {
        let chain = crate::checkpoint_store::chain::load(store, network, params)?;
        let (index, event) = chain
            .iter()
            .enumerate()
            .find(|(_, event)| event.certificate().checkpoint.hash() == epoch)
            .ok_or_else(|| invalid("installed authority has no authenticated checkpoint"))?;
        let anchor = event
            .parent()
            .advance(event.certificate(), event.next_roster().clone())
            .map_err(invalid)?;
        if storage::read(store)?
            != Some(authority_bytes(
                &anchor,
                event.certificate().checkpoint.cutoff,
            )?)
        {
            return Err(invalid(
                "installed payment authority differs from certified history",
            ));
        }
        let cutoff = event.certificate().checkpoint.cutoff;
        let history = crate::confirmed_history::ConfirmedHistory::read_certified(
            store,
            network,
            genesis.genesis_hash(),
            cutoff,
            event.heads(),
            index
                .checked_sub(1)
                .map_or(&[][..], |index| chain[index].heads()),
        )?;
        let replay = crate::confirmed_replay::ConfirmedReplay::replay_with_events(
            &history,
            network,
            params,
            &chain[..=index],
        )
        .map_err(invalid)?;
        if store
            .meta_store()
            .get_meta(burst_store::meta::PAYMENT_VOTE_LIMIT_KEY)?
            != replay.certified_vote_limit_bytes()?
        {
            return Err(invalid("payment vote limit differs from certified history"));
        }
        replay.confirmed_governance_schedule()?.verify_tail(
            store,
            network,
            genesis.genesis_hash(),
        )?;
    }
    PaymentAuthority::read(store)?.ok_or_else(|| invalid("payment authority is missing"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orphaned_vote_limit_is_rejected_without_repairing_genesis_authority() {
        use burst_store::meta::{encode_payment_vote_limit, PAYMENT_VOTE_LIMIT_KEY};
        for key in [
            PAYMENT_VOTE_LIMIT_KEY,
            burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY,
            crate::confirmed_governance::KEY,
            crate::confirmed_governance::HEADER,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let params = ProtocolParams::burst_defaults();
            let genesis = crate::genesis_state::block(NetworkId::Dev, &params);
            let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
            let mut batch = store.tx_begin_write().unwrap();
            crate::payment_epoch::initialize(&mut batch, genesis.hash).unwrap();
            let limit = encode_payment_vote_limit(genesis.hash, None);
            batch.put_meta(key, &limit).unwrap();
            batch.commit().unwrap();
            assert!(PaymentAuthority::read(&store).is_err());
            assert!(initialize(&store, NetworkId::Dev, &params).is_err());
            assert_eq!(store.meta_store().get_meta(key).unwrap(), limit);
            assert!(matches!(
                store.meta_store().get_meta(INSTALLED_KEY),
                Err(StoreError::NotFound(_))
            ));
        }
    }

    #[test]
    fn genesis_authority_is_fixed_across_local_verification_and_restart() {
        let dir = tempfile::tempdir().unwrap();
        let params = ProtocolParams::burst_defaults();
        let genesis = crate::genesis_state::block(NetworkId::Dev, &params);
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        crate::payment_epoch::initialize(&mut batch, genesis.hash).unwrap();
        batch.commit().unwrap();
        assert!(PaymentAuthority::read(&store).unwrap().is_none());
        let authority = initialize(&store, NetworkId::Dev, &params).unwrap();
        assert_eq!(authority.epoch, genesis.hash);
        assert_eq!(authority.roster.total_humans(), 1);
        let mut batch = store.tx_begin_write().unwrap();
        let mut human = burst_store::account::AccountInfo::unopened(burst_crypto::derive_address(
            &burst_crypto::keypair_from_seed(&[17; 32]).public,
        ));
        human.state = burst_types::WalletState::Verified;
        batch.put_account_info(&human, false).unwrap();
        batch.commit().unwrap();
        assert_eq!(
            PaymentAuthority::read(&store)
                .unwrap()
                .unwrap()
                .roster
                .total_humans(),
            1
        );
        assert_eq!(
            PaymentAuthority::read(&store)
                .unwrap()
                .unwrap()
                .weights()
                .weight(&human.address),
            0
        );
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        assert_eq!(
            initialize(&store, NetworkId::Dev, &params)
                .unwrap()
                .weights()
                .weight(&genesis.account),
            10_000
        );
    }

    #[test]
    fn changed_authority_or_mismatched_epoch_fails_without_repairing() {
        let dir = tempfile::tempdir().unwrap();
        let params = ProtocolParams::burst_defaults();
        let genesis = crate::genesis_state::block(NetworkId::Dev, &params);
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        store
            .meta_store()
            .put_meta(
                burst_store::meta::PAYMENT_EPOCH_KEY,
                genesis.hash.as_bytes(),
            )
            .unwrap();
        initialize(&store, NetworkId::Dev, &params).unwrap();
        let correct = store.meta_store().get_meta(INSTALLED_KEY).unwrap();
        for bad in [
            vec![255],
            bincode::serialize(&(
                genesis.hash,
                vec![HumanDelegation {
                    human: genesis.account.clone(),
                    representative: genesis.account.clone(),
                }],
                8000u16,
                genesis.timestamp.as_secs(),
            ))
            .unwrap(),
        ] {
            store.meta_store().put_meta(INSTALLED_KEY, &bad).unwrap();
            assert!(initialize(&store, NetworkId::Dev, &params).is_err());
            assert_eq!(store.meta_store().get_meta(INSTALLED_KEY).unwrap(), bad);
        }
        store
            .meta_store()
            .put_meta(INSTALLED_KEY, &correct)
            .unwrap();
        store.meta_store().put_meta(CUTOFF_KEY, &[1]).unwrap();
        assert!(PaymentAuthority::read(&store).is_err());
        assert!(initialize(&store, NetworkId::Dev, &params).is_err());
        assert_eq!(store.meta_store().get_meta(CUTOFF_KEY).unwrap(), vec![1]);
        store.meta_store().delete_meta(INSTALLED_KEY).unwrap();
        assert!(PaymentAuthority::read(&store).is_err());
        assert!(initialize(&store, NetworkId::Dev, &params).is_err());
        assert_eq!(store.meta_store().get_meta(CUTOFF_KEY).unwrap(), vec![1]);
        assert!(store.meta_store().get_meta(INSTALLED_KEY).is_err());
        store
            .meta_store()
            .put_meta(INSTALLED_KEY, &correct)
            .unwrap();
        store
            .meta_store()
            .put_meta(CUTOFF_KEY, &genesis.timestamp.as_secs().to_be_bytes())
            .unwrap();
        let other_epoch = BlockHash::new([23; 32]);
        store
            .meta_store()
            .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, other_epoch.as_bytes())
            .unwrap();
        assert!(PaymentAuthority::read(&store).is_err());
        assert!(initialize(&store, NetworkId::Dev, &params).is_err());
        assert_eq!(store.meta_store().payment_epoch().unwrap(), other_epoch);
    }
}
