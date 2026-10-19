#[cfg(test)]
use super::{CandidateEconomics, EconomicContext};
use burst_brn::BrnEngine;
#[cfg(test)]
use burst_ledger::StateBlock;
use burst_store_lmdb::LmdbWriteTransaction;
use burst_trst::TrstEngine;

/// Candidate state stays borrowed until an operation can change economics.
/// Owned copies are published only by the caller after its durable commit.
pub(crate) struct StagedEngines<'a> {
    brn: &'a BrnEngine,
    trst: &'a TrstEngine,
    staged_brn: Option<BrnEngine>,
    staged_trst: Option<TrstEngine>,
}

pub(crate) trait Engines {
    fn brn_mut(&mut self) -> &mut BrnEngine;
    fn trst(&self) -> &TrstEngine;
    fn trst_mut(&mut self) -> &mut TrstEngine;
}

impl Engines for (&mut BrnEngine, &mut TrstEngine) {
    fn brn_mut(&mut self) -> &mut BrnEngine {
        self.0
    }

    fn trst(&self) -> &TrstEngine {
        self.1
    }

    fn trst_mut(&mut self) -> &mut TrstEngine {
        self.1
    }
}

impl Engines for StagedEngines<'_> {
    fn brn_mut(&mut self) -> &mut BrnEngine {
        self.staged_brn.get_or_insert_with(|| self.brn.clone())
    }

    fn trst(&self) -> &TrstEngine {
        self.staged_trst.as_ref().unwrap_or(self.trst)
    }

    fn trst_mut(&mut self) -> &mut TrstEngine {
        self.staged_trst.get_or_insert_with(|| self.trst.clone())
    }
}

impl<'a> StagedEngines<'a> {
    pub(crate) fn new(brn: &'a BrnEngine, trst: &'a TrstEngine) -> Self {
        Self {
            brn,
            trst,
            staged_brn: None,
            staged_trst: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn apply(
        &mut self,
        block: &StateBlock,
        context: EconomicContext<'_>,
    ) -> Result<CandidateEconomics, String> {
        super::apply_with_engines(block, context, self)
    }

    pub(crate) fn mutable(&mut self) -> (&mut BrnEngine, &mut TrstEngine) {
        let brn = self.staged_brn.get_or_insert_with(|| self.brn.clone());
        let trst = self.staged_trst.get_or_insert_with(|| self.trst.clone());
        (brn, trst)
    }

    pub(crate) fn persist(&self, batch: &mut LmdbWriteTransaction<'_>) -> Result<(), String> {
        if let Some(brn) = &self.staged_brn {
            crate::ledger_updater::persist_brn_delta(batch, self.brn, brn)?;
        }
        if let Some(trst) = &self.staged_trst {
            if !trst
                .merger_graph
                .shares_storage_with(&self.trst.merger_graph)
            {
                crate::graph_storage::persist_delta(
                    batch,
                    &self.trst.merger_graph,
                    &trst.merger_graph,
                )
                .map_err(|error| error.to_string())?;
            }
            if !trst.shares_wallet_snapshot_with(self.trst) {
                crate::trst_storage::persist_delta(batch, self.trst, trst)
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    pub(crate) fn into_staged(self) -> (Option<BrnEngine>, Option<TrstEngine>) {
        (self.staged_brn, self.staged_trst)
    }
}

#[cfg(test)]
mod tests;
