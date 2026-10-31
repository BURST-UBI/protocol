//! Application interface for an external humanity assessment.

use burst_types::WalletAddress;

/// Produces an assessment for an application to interpret.
/// An assessment alone does not authorize a protocol verification outcome.
pub trait VerificationMethod: Send + Sync {
    /// Human-readable name of this method.
    fn name(&self) -> &str;

    /// Assess whether a wallet holder appears to be a unique human.
    ///
    /// Implementations should return a confidence score in [0.0, 1.0].
    /// This interface does not enforce the range; evidence may be an empty string.
    fn assess(&self, wallet: &WalletAddress) -> VerificationAssessment;
}

/// The result of a verification method's assessment.
pub struct VerificationAssessment {
    /// Implementation-supplied confidence, expected in [0.0, 1.0].
    /// Applications must validate the value before using it.
    pub confidence: f64,
    /// Human-readable evidence summary.
    pub evidence: String,
    /// The verification method that produced this assessment.
    pub method: String,
}
