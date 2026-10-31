//! Typed transaction payloads and structural checks used by wallet and node code.
//!
//! These payloads cover the variants in `Transaction`, not every ledger block
//! operation. Block authoring maps them into the ledger's state-block format.
//! Structural validation does not authenticate signatures or establish balances,
//! eligibility or permission to apply an operation; admission checks those rules.

pub mod burn;
pub mod challenge;
pub mod delegate;
pub mod endorse;
pub mod error;
pub mod governance;
pub mod merge;
pub mod receive;
pub mod reject_receive;
pub mod representative;
pub mod send;
pub mod validation;
pub mod verification_vote;

use burst_types::{Signature, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// Transaction payloads supported by this crate. Additional operations use
/// ledger blocks directly.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Transaction {
    Burn(burn::BurnTx),
    Send(send::SendTx),
    Merge(merge::MergeTx),
    Endorse(endorse::EndorseTx),
    Challenge(challenge::ChallengeTx),
    GovernanceProposal(governance::GovernanceProposalTx),
    GovernanceVote(governance::GovernanceVoteTx),
    Delegate(delegate::DelegateTx),
    RevokeDelegation(delegate::RevokeDelegationTx),
    Receive(receive::ReceiveTx),
    ChangeRepresentative(representative::ChangeRepresentativeTx),
    RejectReceive(reject_receive::RejectReceiveTx),
    VerificationVote(verification_vote::VerificationVoteTx),
}

impl Transaction {
    /// Stored transaction identifier; this accessor does not recompute it.
    pub fn hash(&self) -> &TxHash {
        match self {
            Self::Burn(tx) => &tx.hash,
            Self::Send(tx) => &tx.hash,
            Self::Merge(tx) => &tx.hash,
            Self::Endorse(tx) => &tx.hash,
            Self::Challenge(tx) => &tx.hash,
            Self::GovernanceProposal(tx) => &tx.hash,
            Self::GovernanceVote(tx) => &tx.hash,
            Self::Delegate(tx) => &tx.hash,
            Self::RevokeDelegation(tx) => &tx.hash,
            Self::Receive(tx) => &tx.hash,
            Self::ChangeRepresentative(tx) => &tx.hash,
            Self::RejectReceive(tx) => &tx.hash,
            Self::VerificationVote(tx) => &tx.hash,
        }
    }

    /// Account named as the actor, including the recipient for a receive.
    /// Reading this field does not authenticate that account.
    pub fn sender(&self) -> &WalletAddress {
        match self {
            Self::Burn(tx) => &tx.sender,
            Self::Send(tx) => &tx.sender,
            Self::Merge(tx) => &tx.sender,
            Self::Endorse(tx) => &tx.endorser,
            Self::Challenge(tx) => &tx.challenger,
            Self::GovernanceProposal(tx) => &tx.proposer,
            Self::GovernanceVote(tx) => &tx.voter,
            Self::Delegate(tx) => &tx.delegator,
            Self::RevokeDelegation(tx) => &tx.delegator,
            Self::Receive(tx) => &tx.receiver,
            Self::ChangeRepresentative(tx) => &tx.account,
            Self::RejectReceive(tx) => &tx.rejecter,
            Self::VerificationVote(tx) => &tx.voter,
        }
    }

    pub fn timestamp(&self) -> Timestamp {
        match self {
            Self::Burn(tx) => tx.timestamp,
            Self::Send(tx) => tx.timestamp,
            Self::Merge(tx) => tx.timestamp,
            Self::Endorse(tx) => tx.timestamp,
            Self::Challenge(tx) => tx.timestamp,
            Self::GovernanceProposal(tx) => tx.timestamp,
            Self::GovernanceVote(tx) => tx.timestamp,
            Self::Delegate(tx) => tx.timestamp,
            Self::RevokeDelegation(tx) => tx.timestamp,
            Self::Receive(tx) => tx.timestamp,
            Self::ChangeRepresentative(tx) => tx.timestamp,
            Self::RejectReceive(tx) => tx.timestamp,
            Self::VerificationVote(tx) => tx.timestamp,
        }
    }

    /// Stored proof-of-work nonce, without work validation.
    pub fn work(&self) -> u64 {
        match self {
            Self::Burn(tx) => tx.work,
            Self::Send(tx) => tx.work,
            Self::Merge(tx) => tx.work,
            Self::Endorse(tx) => tx.work,
            Self::Challenge(tx) => tx.work,
            Self::GovernanceProposal(tx) => tx.work,
            Self::GovernanceVote(tx) => tx.work,
            Self::Delegate(tx) => tx.work,
            Self::RevokeDelegation(tx) => tx.work,
            Self::Receive(tx) => tx.work,
            Self::ChangeRepresentative(tx) => tx.work,
            Self::RejectReceive(tx) => tx.work,
            Self::VerificationVote(tx) => tx.work,
        }
    }

    /// Stored signature, without authentication.
    pub fn signature(&self) -> &Signature {
        match self {
            Self::Burn(tx) => &tx.signature,
            Self::Send(tx) => &tx.signature,
            Self::Merge(tx) => &tx.signature,
            Self::Endorse(tx) => &tx.signature,
            Self::Challenge(tx) => &tx.signature,
            Self::GovernanceProposal(tx) => &tx.signature,
            Self::GovernanceVote(tx) => &tx.signature,
            Self::Delegate(tx) => &tx.signature,
            Self::RevokeDelegation(tx) => &tx.signature,
            Self::Receive(tx) => &tx.signature,
            Self::ChangeRepresentative(tx) => &tx.signature,
            Self::RejectReceive(tx) => &tx.signature,
            Self::VerificationVote(tx) => &tx.signature,
        }
    }
}
