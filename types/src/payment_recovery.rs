//! Read-only recovery of a signed account slot through a trusted node.
//! This is not a transferable finality proof or permission to re-sign a payment.

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SlotQuery {
    pub account: String,
    pub previous: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SlotReview {
    pub account: String,
    pub previous: String,
    pub successor: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ExclusionQuery {
    pub account: String,
    pub previous: String,
    pub payment_epoch: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ExclusionReview {
    pub account: String,
    pub previous: String,
    pub payment_epoch: String,
    pub installed_epoch: String,
    pub excluded: bool,
}
