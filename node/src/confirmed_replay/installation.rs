//! Atomic durable projection of one authenticated checkpoint successor.
//! Live workers must be quiesced and their caches replaced before resuming. This
//! primitive is gated until that coordination and agreed payment voting exist.

use super::*;
use crate::checkpoint_store::chain;
use bincode::Options;
use burst_consensus::checkpoint::{CheckpointAnchor, HumanRoster};
use burst_store::{meta::MetaStore, StoreError};
use burst_store_lmdb::{write_batch::checkpoint_projection::CheckpointBlock, LmdbStore};

pub const INSTALLED_KEY: &str = burst_store::meta::CERTIFIED_PAYMENT_AUTHORITY_KEY;
pub const CUTOFF_KEY: &str = "payment_epoch/v1/cutoff";

mod economics;
pub(crate) use economics::validate_retired;

pub struct PreparedInstallation {
    state: ConfirmedReplay,
    history: ConfirmedHistory,
    parent: CheckpointAnchor,
    anchor: CheckpointAnchor,
    record_key: String,
    record: Vec<u8>,
}

/// Returned only after the durable transaction and forced synchronization.
/// The caller must publish these engines and retire old elections as one live
/// transition before permitting admission or signing in the new epoch.
pub struct InstalledCheckpoint {
    pub brn: BrnEngine,
    pub trst: TrstEngine,
    pub verification: VerificationOrchestrator,
    pub(crate) governance: crate::governance_commit::CandidateGovernance,
    pub anchor: CheckpointAnchor,
    pub retired_blocks: Vec<BlockHash>,
    pub minority_promises: Vec<BlockHash>,
    pub retained_promises: Vec<BlockHash>,
    pub ledger_counts: (u64, u64, u64),
    pub frontiers: Vec<(WalletAddress, BlockHash)>,
}

fn error(message: impl ToString) -> StoreError {
    StoreError::Corruption(message.to_string())
}

pub(crate) fn authority_bytes(
    anchor: &CheckpointAnchor,
    cutoff: u64,
) -> Result<Vec<u8>, StoreError> {
    bincode::serialize(&(
        anchor.hash(),
        anchor.roster().humans(),
        anchor.quorum_bps(),
        cutoff,
    ))
    .map_err(error)
}

pub(crate) fn vote_limit_bytes(
    epoch: BlockHash,
    cutoff: u64,
    governance: Option<&crate::governance_commit::CandidateGovernance>,
) -> Result<Vec<u8>, StoreError> {
    let mut limit = None;
    if let Some(governance) = governance {
        for proposal in governance.governance.all_proposals().filter(|proposal| {
            matches!(
                proposal.phase,
                burst_governance::GovernancePhase::Cooldown
                    | burst_governance::GovernancePhase::Activation
            )
        }) {
            let at = proposal
                .activation_at
                .ok_or_else(|| error("certified activation has no timestamp"))?
                .as_secs();
            if at <= cutoff {
                return Err(error("certified activation was not settled at its cutoff"));
            }
            limit = Some(limit.map_or(at, |previous: u64| previous.min(at)));
        }
    }
    Ok(burst_store::meta::encode_payment_vote_limit(epoch, limit))
}

impl ConfirmedReplay {
    pub(crate) fn confirmed_governance_schedule(
        &self,
    ) -> Result<crate::confirmed_governance::ConfirmedGovernance, StoreError> {
        crate::confirmed_governance::ConfirmedGovernance::from_checkpoint(
            self.payment_epoch,
            self.params.clone(),
            self.governance.as_ref(),
        )
    }
    pub(crate) fn certified_vote_limit_bytes(&self) -> Result<Vec<u8>, StoreError> {
        vote_limit_bytes(
            self.payment_epoch,
            self.cutoff.as_secs(),
            self.governance.as_ref(),
        )
    }
}

impl PreparedInstallation {
    /// Read and authenticate the chain from configured genesis. Install only
    /// the immediate successor of the durable epoch, never jump to the tip.
    pub fn next(
        store: &LmdbStore,
        network: NetworkId,
        params: &ProtocolParams,
    ) -> Result<Option<Self>, StoreError> {
        if store
            .meta_store()
            .get_meta_txn(&store.tx_begin_read()?, snapshot::installed::BASE_KEY)?
            .is_some()
        {
            return Err(error(
                "installed compact state requires compact installation",
            ));
        }
        let chain = chain::load(store, network, params)?;
        let installed = store.meta_store().payment_epoch()?;
        let genesis =
            crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(network, params)?;
        let index = if installed == genesis.hash() {
            0
        } else {
            chain
                .iter()
                .position(|event| event.certificate().checkpoint.hash() == installed)
                .ok_or_else(|| error("installed payment epoch has no authenticated checkpoint"))?
                + 1
        };
        if index > 0 {
            let event = &chain[index - 1];
            let anchor = event
                .parent()
                .advance(event.certificate(), event.next_roster().clone())
                .map_err(error)?;
            let cutoff = event.certificate().checkpoint.cutoff;
            let txn = store.tx_begin_read()?;
            if store.meta_store().get_meta_txn(&txn, INSTALLED_KEY)?
                != Some(authority_bytes(&anchor, cutoff)?)
                || store
                    .meta_store()
                    .get_meta_txn(&txn, CUTOFF_KEY)?
                    .as_deref()
                    != Some(cutoff.to_be_bytes().as_slice())
            {
                return Err(error(
                    "installed epoch lacks its certified authority and cutoff",
                ));
            }
        }
        let Some(event) = chain.get(index) else {
            return Ok(None);
        };
        if event.parent().hash() != installed {
            return Err(error("checkpoint installer parent mismatch"));
        }
        let checkpoint = &event.certificate().checkpoint;
        let history = ConfirmedHistory::read_certified(
            store,
            network,
            genesis.hash(),
            checkpoint.cutoff,
            event.heads(),
            index.checked_sub(1).map_or(&[][..], |i| chain[i].heads()),
        )?;
        let state =
            ConfirmedReplay::replay_with_events(&history, network, params, &chain[..=index])
                .map_err(error)?;
        let roster = state.human_roster().map_err(error)?;
        let anchor = event
            .parent()
            .advance(event.certificate(), roster)
            .map_err(error)?;
        if state.payment_epoch != anchor.hash() || state.cutoff.as_secs() != checkpoint.cutoff {
            return Err(error(
                "replayed installation context differs from certificate",
            ));
        }
        let record_key = format!(
            "checkpoint_chain/v1/{}/{}/{}",
            network.as_str(),
            genesis.hash(),
            checkpoint.height
        );
        let record = store.meta_store().get_meta(&record_key)?;
        // A concurrent beacon attachment may change bytes, but never the manifest.
        if !chain::record_matches(&record, event)? {
            return Err(error("checkpoint changed during installation replay"));
        }
        Ok(Some(Self {
            state,
            history,
            parent: event.parent().clone(),
            anchor,
            record_key,
            record,
        }))
    }

    pub fn epoch(&self) -> BlockHash {
        self.anchor.hash()
    }
    pub fn roster(&self) -> &HumanRoster {
        self.anchor.roster()
    }
    pub(crate) fn anchor(&self) -> &CheckpointAnchor {
        &self.anchor
    }

    /// The source gate intentionally prevents use by ordinary running nodes.
    /// Own the writer so any error drops ALL projection and handoff writes.
    pub fn commit(self, store: &LmdbStore) -> Result<InstalledCheckpoint, StoreError> {
        if !cfg!(test) && !burst_verification::checkpoint_selection_enabled(self.anchor.network()) {
            return Err(error("certified live installation is not enabled"));
        }
        let mut batch = store.tx_begin_write()?;
        if batch.payment_epoch()? != self.parent.hash()
            || batch.get_meta(snapshot::installed::BASE_KEY)?.is_some()
            || batch.get_meta(&self.record_key)?.as_deref() != Some(self.record.as_slice())
        {
            return Err(error("checkpoint installation became stale; replay again"));
        }
        let mut heights = HashMap::new();
        let mut blocks = Vec::new();
        let mut included = HashSet::new();
        for block in self.history.replay_blocks() {
            let height = heights.entry(block.account.clone()).or_insert(0u64);
            *height += 1;
            included.insert(block.hash);
            blocks.push(CheckpointBlock {
                hash: block.hash,
                account: block.account.clone(),
                height: *height,
                bytes: bincode::serialize(block).map_err(error)?,
            });
        }
        let accounts: Vec<_> = self.state.accounts.values().cloned().collect();
        let retired_blocks =
            batch.replace_checkpoint_ledger(self.parent.hash(), &blocks, &accounts)?;
        validate_retired(&batch, self.parent.hash(), &retired_blocks)?;
        let governance = self.state.persist_projection(&mut batch, &self.anchor)?;
        let minority_promises = batch.advance_payment_epoch(
            &self.parent.hash(),
            &self.parent.hash(),
            &self.anchor.hash(),
            &included,
        )?;
        let ledger_counts = batch.ledger_counts()?;
        batch.commit()?;
        store.env().force_sync().map_err(error)?;
        Ok(InstalledCheckpoint {
            brn: self.state.brn,
            trst: self.state.trst,
            verification: self.state.verification,
            governance,
            anchor: self.anchor,
            retired_blocks,
            minority_promises,
            retained_promises: Vec::new(),
            ledger_counts,
            frontiers: accounts
                .into_iter()
                .filter(|account| account.block_count > 0)
                .map(|account| (account.address, account.head))
                .collect(),
        })
    }
}
