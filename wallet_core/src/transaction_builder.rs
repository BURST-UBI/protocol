//! Transaction building helpers.

use burst_types::{Timestamp, TxHash, WalletAddress};
use crate::error::WalletError;

/// Build a burn transaction (BRN → TRST).
pub fn build_burn_tx(
    _sender: &WalletAddress,
    _receiver: &WalletAddress,
    _amount: u128,
    _now: Timestamp,
) -> Result<burst_transactions::burn::BurnTx, WalletError> {
    todo!("create BurnTx, compute hash, sign")
}

/// Build a send transaction (transfer TRST).
pub fn build_send_tx(
    _sender: &WalletAddress,
    _receiver: &WalletAddress,
    _amount: u128,
    _link: TxHash,
    _origin: TxHash,
    _now: Timestamp,
) -> Result<burst_transactions::send::SendTx, WalletError> {
    todo!("create SendTx, compute hash, sign")
}

/// Build an endorsement transaction.
pub fn build_endorse_tx(
    _endorser: &WalletAddress,
    _target: &WalletAddress,
    _burn_amount: u128,
    _now: Timestamp,
) -> Result<burst_transactions::endorse::EndorseTx, WalletError> {
    todo!("create EndorseTx, compute hash, sign")
}

/// Build a governance vote transaction.
pub fn build_governance_vote_tx(
    _voter: &WalletAddress,
    _proposal_hash: TxHash,
    _vote: burst_transactions::governance::GovernanceVote,
    _now: Timestamp,
) -> Result<burst_transactions::governance::GovernanceVoteTx, WalletError> {
    todo!("create GovernanceVoteTx, compute hash, sign")
}
