//! Consti engine — manages constitutional amendments.

use crate::amendment::Amendment;
use crate::document::ConstiDocument;
use crate::error::ConstiError;

pub struct ConstiEngine;

impl ConstiEngine {
    /// Submit a constitutional amendment.
    pub fn submit_amendment(&self, _amendment: Amendment) -> Result<(), ConstiError> {
        todo!("validate proposer, store amendment")
    }

    /// Vote on a constitutional amendment.
    pub fn vote_amendment(
        &self,
        _amendment_hash: &burst_types::TxHash,
        _voter: &burst_types::WalletAddress,
        _vote: burst_governance::proposal::ProposalContent,
    ) -> Result<(), ConstiError> {
        todo!("validate voter, check phase, record vote")
    }

    /// Activate an amendment — apply it to the constitution document.
    pub fn activate_amendment(
        &self,
        _amendment: &Amendment,
        _document: &mut ConstiDocument,
    ) -> Result<(), ConstiError> {
        todo!("add/modify articles in the constitution, increment version")
    }

    /// Get the current constitution.
    pub fn get_constitution(&self) -> ConstiDocument {
        todo!("load from storage")
    }
}
