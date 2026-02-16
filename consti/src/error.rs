use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConstiError {
    #[error("amendment {0} not found")]
    AmendmentNotFound(String),

    #[error("amendment is not in the correct phase")]
    WrongPhase,

    #[error("constitutional supermajority not met")]
    SupermajorityNotMet,

    #[error("{0}")]
    Governance(#[from] burst_governance::GovernanceError),

    #[error("{0}")]
    Other(String),
}
