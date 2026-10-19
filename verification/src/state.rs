//! Verification process state tracking.

use burst_types::{Timestamp, WalletAddress};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Preserve the existing map wire shape while making embedded snapshot bytes
/// independent of HashMap insertion order and randomized hashing.
pub(crate) fn ordered_wallet_map<T: Serialize, S: serde::Serializer>(
    values: &std::collections::HashMap<WalletAddress, T>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    values
        .iter()
        .map(|(wallet, value)| (wallet.as_str(), value))
        .collect::<std::collections::BTreeMap<_, _>>()
        .serialize(serializer)
}

/// The current state of a verification process for a wallet.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerificationState {
    /// The wallet being verified.
    pub target: WalletAddress,
    /// Current phase of verification.
    pub phase: VerificationPhase,
    /// Endorsements received so far.
    #[serde(serialize_with = "ordered_endorsements")]
    pub endorsements: Vec<Endorsement>,
    /// O(1) lookup set for endorser duplicate detection.
    #[serde(default, serialize_with = "ordered_wallets")]
    pub endorser_set: HashSet<WalletAddress>,
    /// Verifiers selected for this round.
    pub selected_verifiers: Vec<WalletAddress>,
    /// Votes cast so far.
    #[serde(serialize_with = "ordered_votes")]
    pub votes: Vec<VerifierVote>,
    /// Number of re-votes that have occurred.
    pub revote_count: u32,
    /// Verifiers excluded from future revote rounds (previous round participants).
    #[serde(serialize_with = "ordered_wallets")]
    pub excluded_verifiers: HashSet<WalletAddress>,
    /// When this verification process started.
    pub started_at: Timestamp,
}

fn ordered_wallets<S: serde::Serializer>(
    wallets: &HashSet<WalletAddress>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut wallets: Vec<_> = wallets.iter().collect();
    wallets.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    wallets.serialize(serializer)
}

fn sorted_votes(votes: &[VerifierVote]) -> Vec<&VerifierVote> {
    let mut sorted: Vec<_> = votes.iter().collect();
    sorted.sort_by_key(|vote| {
        (
            vote.verifier.as_str(),
            vote.timestamp.as_secs(),
            vote.vote as u8,
            vote.stake_amount,
        )
    });
    sorted
}

pub(crate) fn ordered_votes<S: serde::Serializer>(
    votes: &[VerifierVote],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    sorted_votes(votes).serialize(serializer)
}

pub(crate) fn ordered_vote_map<S: serde::Serializer>(
    votes: &std::collections::HashMap<WalletAddress, Vec<VerifierVote>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    votes
        .iter()
        .map(|(wallet, votes)| (wallet.as_str(), sorted_votes(votes)))
        .collect::<std::collections::BTreeMap<_, _>>()
        .serialize(serializer)
}

fn ordered_endorsements<S: serde::Serializer>(
    endorsements: &[Endorsement],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut sorted: Vec<_> = endorsements.iter().collect();
    sorted.sort_by_key(|endorsement| {
        (
            endorsement.endorser.as_str(),
            endorsement.timestamp.as_secs(),
            endorsement.burn_amount,
        )
    });
    sorted.serialize(serializer)
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
    /// Previously verified but deactivated via a benign (inactivity) challenge —
    /// distinct from `Unverified`: the holder was real and their TRST is NOT
    /// revoked (whitepaper §Unverification Without Revocation).
    Deactivated,
}

impl VerificationPhase {
    /// A previously verified wallet must collect a new set of endorsements.
    pub fn can_reverify(&self) -> bool {
        matches!(self, Self::Unverified | Self::Deactivated)
    }
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
