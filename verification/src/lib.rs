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

pub mod challenge;
pub mod endorsement;
pub mod error;
pub mod method;
pub mod orchestrator;
pub mod outcomes;
pub mod state;
pub mod verifier_selection;
pub mod voting;

pub use challenge::ChallengeEngine;
pub use endorsement::EndorsementEngine;
pub use error::VerificationError;
pub use method::VerificationMethod;
pub use orchestrator::{OrchestratorSnapshot, VerificationEvent, VerificationOrchestrator};
pub use outcomes::{
    ChallengeOutcomeEvent, ChallengeResult, EndorserOutcome, VerificationOutcomeEvent,
    VerificationResult, VerifierOutcome, compute_challenge_outcome, compute_verification_outcomes,
};
pub use state::VerificationState;
pub use verifier_selection::VerifierSelector;
pub use voting::{NeitherPenaltyAction, NeitherVoteTracker, VerificationVoting, Vote};
