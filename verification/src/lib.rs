//! Unique Humanity Verification (UHV) system.
//!
//! Two-phase process:
//! 1. **Endorsement**: People in the wallet holder's social circle permanently burn BRN to vouch.
//! 2. **Verification**: Randomly selected verifiers vote (Legitimate / Illegitimate / Neither).
//!
//! Plus the post-verification **challenge** mechanism: any verified wallet can
//! challenge another at any time by staking BRN.
//!
//! The verification *method* is modular — the protocol specifies *that* verification
//! must happen, not *how*. Different methods can be plugged in.

mod attempts;
pub mod bootstrap;
pub mod challenge;
pub mod endorsement;
pub mod error;
pub mod method;
pub mod orchestrator;
pub mod outcomes;
pub mod selection_request;
pub mod state;
pub mod verifier_selection;
pub mod voting;

/// Default-build release gate. Full lifecycle and missed-deadline recovery
/// qualification remain incomplete. The explicit testnet policy below is not
/// public-release approval.
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

/// Appeal admission and reversal require their own live/recovery evidence.
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
pub use verifier_selection::VerifierSelector;
pub use voting::{NeitherPenaltyAction, NeitherVoteTracker, VerificationVoting, Vote};
