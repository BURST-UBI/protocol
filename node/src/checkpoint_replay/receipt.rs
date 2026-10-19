use super::*;
use burst_consensus::checkpoint::HumanDelegation;
use burst_types::BlockHash;
use std::sync::Arc;

/// Frozen execution inputs retained in a quorum-authenticated economic body.
/// Decoding alone grants no authority. Parent authority is supplied separately
/// by the authenticated archive path; full-history signing evidence is absent.
#[derive(Clone)]
pub(crate) struct SelectionReceipt {
    data: Arc<ReceiptData>,
}

#[derive(Serialize, Deserialize)]
struct ReceiptData {
    checkpoint: Checkpoint,
    roster: Vec<HumanDelegation>,
    request: SelectionRequest,
    heads: Vec<HistoryHead>,
    endorsements: BlockHash,
    parent_roster: BlockHash,
    parent_quorum_bps: u16,
    bootstrap_complete: bool,
    selection_history: BlockHash,
    lifecycle: SelectionLifecycle,
    payment_epoch: BlockHash,
}

impl ReplayedCheckpoint {
    pub(crate) fn receipt(&self) -> SelectionReceipt {
        SelectionReceipt {
            data: Arc::new(ReceiptData {
                checkpoint: self.checkpoint.clone(),
                roster: self.roster.humans().to_vec(),
                request: self.request.clone(),
                heads: self.heads.clone(),
                endorsements: self.endorsements,
                parent_roster: self.parent_roster,
                parent_quorum_bps: self.parent_quorum_bps,
                bootstrap_complete: self.bootstrap_complete,
                selection_history: self.selection_history,
                lifecycle: self.lifecycle,
                payment_epoch: self.payment_epoch,
            }),
        }
    }
}

impl SelectionReceipt {
    pub(crate) fn matches_replay(&self, replay: &ReplayedCheckpoint) -> bool {
        let ReceiptData {
            checkpoint,
            roster,
            request,
            heads,
            endorsements,
            parent_roster,
            parent_quorum_bps,
            bootstrap_complete,
            selection_history,
            lifecycle,
            payment_epoch,
        } = self.data.as_ref();
        checkpoint == &replay.checkpoint
            && roster == replay.roster.humans()
            && request == &replay.request
            && heads == &replay.heads
            && endorsements == &replay.endorsements
            && parent_roster == &replay.parent_roster
            && parent_quorum_bps == &replay.parent_quorum_bps
            && bootstrap_complete == &replay.bootstrap_complete
            && selection_history == &replay.selection_history
            && lifecycle == &replay.lifecycle
            && payment_epoch == &replay.payment_epoch
    }

    pub(crate) fn heads(&self) -> &[HistoryHead] {
        &self.data.heads
    }

    pub(crate) fn checkpoint(&self) -> &Checkpoint {
        &self.data.checkpoint
    }

    pub(crate) fn validate_at(&self, anchor: &CheckpointAnchor) -> Result<(), StoreError> {
        let checkpoint = &self.data.checkpoint;
        let lifecycle = match self.data.lifecycle {
            SelectionLifecycle::Verification(value) | SelectionLifecycle::Challenge(value) => value,
        };
        if checkpoint.network != anchor.network()
            || checkpoint.genesis != anchor.genesis_hash()
            || checkpoint.height == 0
            || checkpoint.height > anchor.height()
            || checkpoint.cutoff > anchor.cutoff()
            || self.data.payment_epoch != checkpoint.parent
            || checkpoint.parent.is_zero()
            || self.data.parent_roster.is_zero()
            || self.data.endorsements.is_zero()
            || self.data.selection_history.is_zero()
            || lifecycle.is_zero()
            || !(6667..=10_000).contains(&self.data.parent_quorum_bps)
            || checkpoint.next_quorum_bps != self.data.parent_quorum_bps
        {
            return Err(invalid("snapshot selection has inconsistent context"));
        }
        self.data
            .request
            .vote_context(checkpoint)
            .map_err(invalid)?;
        let roster = HumanRoster::new(self.data.roster.clone()).map_err(invalid)?;
        if roster.humans() != self.data.roster || roster.commitment() != checkpoint.next_roster {
            return Err(invalid("snapshot selection has inconsistent roster"));
        }
        if crate::confirmed_history::head_commitment(
            checkpoint.network,
            checkpoint.genesis,
            checkpoint.cutoff,
            &self.data.heads,
        )? != checkpoint.ledger_root
        {
            return Err(invalid("snapshot selection has inconsistent history heads"));
        }
        Ok(())
    }

    /// Only the authenticated snapshot reader calls this with its retained body.
    pub(crate) fn restore(
        &self,
        parent: &CheckpointAnchor,
    ) -> Result<ReplayedCheckpoint, StoreError> {
        if parent.roster().commitment() != self.data.parent_roster
            || parent.quorum_bps() != self.data.parent_quorum_bps
        {
            return Err(invalid("snapshot selection has different parent authority"));
        }
        parent
            .validate_successor(&self.data.checkpoint)
            .map_err(invalid)?;
        let roster = if self.data.roster == parent.roster().humans() {
            parent.roster().clone()
        } else {
            HumanRoster::new(self.data.roster.clone()).map_err(invalid)?
        };
        if roster.commitment() != self.data.checkpoint.next_roster {
            return Err(invalid("snapshot selection has inconsistent roster"));
        }
        Ok(ReplayedCheckpoint {
            checkpoint: self.data.checkpoint.clone(),
            roster,
            request: self.data.request.clone(),
            heads: self.data.heads.clone(),
            endorsements: self.data.endorsements,
            parent_roster: self.data.parent_roster,
            parent_quorum_bps: self.data.parent_quorum_bps,
            bootstrap_complete: self.data.bootstrap_complete,
            selection_history: self.data.selection_history,
            lifecycle: self.data.lifecycle,
            payment_epoch: self.data.payment_epoch,
            history_hashes: None,
        })
    }
}

impl Serialize for SelectionReceipt {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.data.as_ref().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SelectionReceipt {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self {
            data: Arc::new(ReceiptData::deserialize(deserializer)?),
        })
    }
}
