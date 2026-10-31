use crate::{
    signed_client::{parse_address, parse_hash},
    transaction_builder::build_appeal_tx,
    wallet::ProcessResponse,
    NodeClient, WalletError,
};
use burst_transactions::Transaction;
use burst_types::{appeal::AppealReview, KeyPair, Timestamp, TxHash, WalletAddress};

// Check response fields, not the certificate behind the reported checkpoint.
fn validate(review: &AppealReview, target: &WalletAddress) -> Result<u128, WalletError> {
    if &parse_address(&review.account)? != target {
        return Err(WalletError::Other(
            "Appeal response names another wallet".into(),
        ));
    }
    let stake = review
        .required_stake
        .parse::<u128>()
        .map_err(|_| WalletError::Other("Invalid appeal stake from node".into()))?;
    if stake == 0 || stake.to_string() != review.required_stake {
        return Err(WalletError::Other("Invalid appeal stake from node".into()));
    }
    if let Some(outcome) = &review.outcome {
        if parse_hash(&outcome.hash)?.is_zero() || parse_hash(&outcome.checkpoint)?.is_zero() {
            return Err(WalletError::Other("Appeal outcome is not certified".into()));
        }
    }
    Ok(stake)
}

pub(crate) fn prepare(
    current: &AppealReview,
    reviewed: &AppealReview,
    sponsor: &WalletAddress,
    now: Timestamp,
) -> Result<Transaction, WalletError> {
    let target = parse_address(&reviewed.account)?;
    validate(reviewed, &target)?;
    let stake = validate(current, &target)?;
    if !current.enabled || !reviewed.enabled {
        return Err(WalletError::Other(
            "Appeals are not enabled on this node".into(),
        ));
    }
    if current.challenge_active {
        return Err(WalletError::Other(
            "A review is already active for this wallet".into(),
        ));
    }
    if current.outcome != reviewed.outcome || current.required_stake != reviewed.required_stake {
        return Err(WalletError::Other(
            "The outcome or stake changed. Load the decision and review it again".into(),
        ));
    }
    let outcome = current
        .outcome
        .as_ref()
        .ok_or_else(|| WalletError::Other("No certified challenge outcome to appeal".into()))?;
    build_appeal_tx(
        sponsor,
        &target,
        stake,
        outcome.reason,
        parse_hash(&outcome.hash)?,
        now,
    )
    .map(Transaction::Challenge)
}

impl NodeClient {
    /// Fetch the node's appeal status and check target, stake and outcome references.
    /// Does not verify a checkpoint certificate or submit an appeal.
    pub async fn appeal_review(&self, target: &WalletAddress) -> Result<AppealReview, WalletError> {
        parse_address(target.as_str())?;
        let review: AppealReview = serde_json::from_value(
            self.rpc_call(
                "appeal_review",
                serde_json::json!({"account":target.as_str()}),
            )
            .await?,
        )
        .map_err(|e| WalletError::Node(format!("Invalid appeal review: {e}")))?;
        validate(&review, target)?;
        Ok(review)
    }

    /// Refetch status and require the reviewed outcome and stake to remain unchanged
    /// before using the normal signed-submission path. Node admission still applies.
    pub async fn appeal_signed(
        &self,
        keys: &KeyPair,
        reviewed: &AppealReview,
        now: Timestamp,
    ) -> Result<ProcessResponse, WalletError> {
        let transaction = async {
            let target = parse_address(&reviewed.account)?;
            let current = self.appeal_review(&target).await?;
            prepare(
                &current,
                reviewed,
                &burst_crypto::derive_address(&keys.public),
                now,
            )
        }
        .await
        .map_err(|e| WalletError::Other(format!("appeal not submitted: {e}")))?;
        self.submit_signed(keys, &transaction, TxHash::ZERO).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::{appeal::AppealOutcome, ChallengeReason};

    #[test]
    fn review_binds_the_outcome_and_stake_and_fails_closed_on_changed_inputs() {
        let account =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[1; 32]).public);
        let review = AppealReview {
            account: account.to_string(),
            enabled: true,
            challenge_active: false,
            required_stake: "50".into(),
            outcome: Some(AppealOutcome {
                hash: "a".repeat(64),
                checkpoint: "b".repeat(64),
                reason: ChallengeReason::Fraud,
                legitimate: false,
                completed_at: 1,
            }),
        };
        let Transaction::Challenge(tx) =
            prepare(&review, &review, &account, Timestamp::new(2)).unwrap()
        else {
            panic!()
        };
        assert_eq!(tx.appeal_of, Some(parse_hash(&"a".repeat(64)).unwrap()));
        for change in 0..8 {
            let mut current = review.clone();
            match change {
                0 => current.enabled = false,
                1 => current.challenge_active = true,
                2 => current.required_stake = "51".into(),
                3 => current.outcome.as_mut().unwrap().hash = "c".repeat(64),
                4 => current.outcome.as_mut().unwrap().reason = ChallengeReason::Inactivity,
                5 => current.outcome.as_mut().unwrap().legitimate = true,
                6 => current.outcome = None,
                _ => current.outcome.as_mut().unwrap().checkpoint = "0".repeat(64),
            }
            assert!(prepare(&current, &review, &account, Timestamp::new(2)).is_err());
        }
    }
}
