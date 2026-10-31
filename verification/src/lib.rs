//! Endorsement, verifier selection, voting and challenge state.
//!
//! Nodes authenticate operations and apply these transitions to agreed ledger
//! state. Evidence assessment is separate from vote counting and settlement.

mod attempts;
pub mod bootstrap;
pub mod challenge;
pub mod committee;
pub mod endorsement;
pub mod error;
pub mod method;
pub mod orchestrator;
pub mod outcomes;
pub mod selection_request;
pub mod shared;
pub mod state;
mod tracked_map;
pub mod votes;
pub mod voting;

/// Global switch for certified selection. Experimental builds can enable it
/// on Dev and Test through `checkpoint_selection_enabled`.
pub const CHECKPOINT_SELECTION_ENABLED: bool = false;

/// Experimental artifacts can exercise the normal protocol on test networks.
/// This does not authorize Live or replace certificate, clock or beacon checks.
pub const fn checkpoint_selection_enabled(network: burst_types::NetworkId) -> bool {
    CHECKPOINT_SELECTION_ENABLED
        || (cfg!(feature = "experimental-testnet")
            && matches!(
                network,
                burst_types::NetworkId::Test | burst_types::NetworkId::Dev
            ))
}

/// Global appeal gate; node policy may enable appeals on experimental networks.
pub const META_CHALLENGES_ENABLED: bool = false;

pub use challenge::{AppealContext, ChallengeEngine, ChallengeReason};
pub use endorsement::EndorsementEngine;
pub use error::VerificationError;
pub use method::VerificationMethod;
pub use orchestrator::{
    CommitteeActivation, OrchestratorSnapshot, VerificationEvent, VerificationOrchestrator,
};
pub use outcomes::{
    compute_challenge_outcome, compute_verification_outcomes, ChallengeOutcomeEvent,
    ChallengeResult, EndorserOutcome, VerificationOutcomeEvent, VerificationResult,
    VerifierOutcome,
};
pub use state::VerificationState;
pub use voting::{NeitherPenaltyAction, NeitherVoteTracker, VerificationVoting, Vote};
