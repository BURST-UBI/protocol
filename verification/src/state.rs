//! Verification process state tracking.

use burst_types::{Timestamp, WalletAddress};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// The current state of a verification process for a wallet.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerificationState {
    /// The wallet being verified.
    pub target: WalletAddress,
    /// Current phase of verification.
    pub phase: VerificationPhase,
    /// Endorsements received so far.
    pub endorsements: Vec<Endorsement>,
    /// Verifiers selected for this round.
    pub selected_verifiers: Vec<WalletAddress>,
    /// Votes cast so far.
    pub votes: Vec<VerifierVote>,
    /// Number of re-votes that have occurred.
    pub revote_count: u32,
    /// Verifiers excluded from future revote rounds (previous round participants).
    pub excluded_verifiers: HashSet<WalletAddress>,
    /// When this verification process started.
    pub started_at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationPhase {
    /// Collecting endorsements.
    Endorsing,
    /// Verifiers are voting.
    Voting,
    /// Verification complete — passed.
    Verified,
    /// Verification complete — failed.
    Failed,
    /// Under challenge (re-verification).
    Challenged,
    /// Previously verified but fraud confirmed via challenge.
    Unverified,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Endorsement {
    pub endorser: WalletAddress,
    pub burn_amount: u128,
    pub timestamp: Timestamp,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifierVote {
    pub verifier: WalletAddress,
    pub vote: super::voting::Vote,
    pub stake_amount: u128,
    pub timestamp: Timestamp,
}
