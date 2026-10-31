use super::*;
use crate::checkpoint_replay::SelectionReceipt;

/// One authenticated continuation, retaining a decoded receipt between pages.
/// Each step charges a genesis head, receipt load (including empty receipts), or
/// receipt head. Loading or validating a receipt is not split across steps.
/// Only receipts containing requested non-genesis heads are validated here;
/// this walk is not a substitute for validating the full selection history.
pub(in crate::confirmed_replay) struct CommittedHistoryWalk<'a> {
    base: &'a SnapshotContinuation,
    genesis_pending: bool,
    next_receipt: usize,
    receipt: Option<SelectionReceipt>,
    head_index: usize,
    receipt_validated: bool,
    complete: bool,
    failed: bool,
}

impl<'a> CommittedHistoryWalk<'a> {
    pub(super) fn new(base: &'a SnapshotContinuation) -> Self {
        Self {
            base,
            genesis_pending: true,
            next_receipt: 0,
            receipt: None,
            head_index: 0,
            receipt_validated: false,
            complete: false,
            failed: false,
        }
    }

    pub(in crate::confirmed_replay) fn advance(
        &mut self,
        budget: usize,
        mut wanted: impl FnMut(BlockHash) -> Result<bool, ReplayError>,
        mut visit: impl FnMut(HistoryCursor) -> Result<(), ReplayError>,
    ) -> Result<bool, ReplayError> {
        if self.failed {
            return Err(super::super::invalid(
                "committed history traversal already failed",
            ));
        }
        self.failed = true;
        if budget == 0 {
            return Err(super::super::invalid("zero committed history page budget"));
        }
        if !self.complete {
            let genesis = self.base.anchor.genesis_hash();
            for _ in 0..budget {
                if self.genesis_pending {
                    self.genesis_pending = false;
                    if wanted(genesis)? {
                        let head = HistoryHead {
                            account: crate::genesis_key::genesis_address(
                                self.base.anchor.network(),
                            ),
                            height: 1,
                            hash: genesis,
                        };
                        visit(SnapshotContinuation::cursor_for_committed_head(
                            &self.base.anchor,
                            self.base.context,
                            &self.base.state,
                            &head,
                        )?)?;
                    }
                    continue;
                }
                if let Some(receipt) = &self.receipt {
                    if let Some(head) = receipt.heads().get(self.head_index) {
                        self.head_index += 1;
                        if head.hash != genesis && wanted(head.hash)? {
                            if !self.receipt_validated {
                                receipt
                                    .validate_at(&self.base.anchor)
                                    .map_err(|error| super::super::invalid(error.to_string()))?;
                                self.receipt_validated = true;
                            }
                            visit(SnapshotContinuation::cursor_for_committed_head(
                                &self.base.anchor,
                                self.base.context,
                                &self.base.state,
                                head,
                            )?)?;
                        }
                        continue;
                    }
                }
                self.receipt = self
                    .base
                    .state
                    .selection_receipts
                    .get(self.next_receipt)?
                    .map(|row| row.into_owned());
                if self.receipt.is_none() {
                    self.complete = true;
                    break;
                }
                self.next_receipt += 1;
                self.head_index = 0;
                self.receipt_validated = false;
            }
        }
        self.failed = false;
        Ok(self.complete)
    }
}
