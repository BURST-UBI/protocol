//! Modular verification method trait.
//!
//! The protocol does not specify HOW humanity is proven — only THAT verifiers voted.
//! Different communities can use different methods within the same protocol.

use burst_types::WalletAddress;

/// A pluggable verification method.
///
/// Implementations might include:
/// - Native endorser/verifier model
/// - Trust graphs (Circles-style)
/// - Biometric verification (Worldcoin-style)
/// - Government ID verification
/// - Composable identity (Gitcoin Passport-style)
pub trait VerificationMethod: Send + Sync {
    /// Human-readable name of this method.
    fn name(&self) -> &str;

    /// Assess whether a wallet holder appears to be a unique human.
    ///
    /// Returns a confidence score [0.0, 1.0] and optional evidence.
    fn assess(&self, wallet: &WalletAddress) -> VerificationAssessment;
}

/// The result of a verification method's assessment.
pub struct VerificationAssessment {
    /// Confidence that this is a unique human [0.0, 1.0].
    pub confidence: f64,
    /// Human-readable evidence summary.
    pub evidence: String,
    /// The verification method that produced this assessment.
    pub method: String,
}
