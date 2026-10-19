use std::sync::Arc;

use crate::verification_commit::receipts;
use burst_types::{
    appeal::{AppealOutcome, AppealReview},
    BlockHash, ProtocolParams, WalletAddress,
};

pub(crate) struct Reader {
    pub store: Arc<burst_store_lmdb::LmdbStore>,
    pub initial_params: ProtocolParams,
}

#[cfg(test)]
mod tests;

impl Reader {
    pub fn read(&self, account: &WalletAddress, enabled: bool) -> Result<AppealReview, String> {
        burst_crypto::decode_address(account.as_str()).ok_or("invalid wallet address")?;
        let txn = self.store.tx_begin_read().map_err(|e| e.to_string())?;
        let meta = self.store.meta_store();
        let get = |key: &str| meta.get_meta_txn(&txn, key).map_err(|e| e.to_string());
        let params: ProtocolParams = match get("protocol_params")? {
            Some(bytes) => bincode::deserialize(&bytes).map_err(|e| e.to_string())?,
            None => self.initial_params.clone(),
        };
        let snapshot = get(crate::endorsement_commit::SNAPSHOT_KEY)?
            .map(|bytes| burst_verification::OrchestratorSnapshot::decode(&bytes))
            .transpose()
            .map_err(|e| e.to_string())?;
        let mut review = AppealReview {
            account: account.to_string(),
            enabled,
            challenge_active: snapshot
                .as_ref()
                .is_some_and(|s| s.active_challenges.contains_key(account)),
            required_stake: params.challenge_stake_amount.to_string(),
            outcome: None,
        };
        let Some(latest) = get(&receipts::latest_key(account))? else {
            return Ok(review);
        };
        let hash = BlockHash::new(
            latest
                .try_into()
                .map_err(|_| "invalid latest outcome reference")?,
        );
        let receipt =
            receipts::decode(&get(&receipts::key(hash))?.ok_or("outcome receipt missing")?)?;
        if receipt.source != hash || &receipt.target != account || snapshot.is_none() {
            return Err("outcome history differs from account state".into());
        }
        let Some((_, reason, result)) = receipt.challenge_judgment() else {
            return Ok(review);
        };
        let legitimate = match result {
            receipts::ChallengeVerdict::Rejected => true,
            receipts::ChallengeVerdict::Upheld => false,
            receipts::ChallengeVerdict::LegacyExpired => return Ok(review),
        };
        let Some(marker) = get(&receipts::certified_key(hash))? else {
            return Ok(review);
        };
        let checkpoint = BlockHash::new(
            marker
                .try_into()
                .map_err(|_| "invalid outcome certification")?,
        );
        if checkpoint.is_zero() {
            return Err("invalid outcome certification".into());
        }
        review.outcome = Some(AppealOutcome {
            hash: hash.to_string(),
            checkpoint: checkpoint.to_string(),
            reason,
            legitimate,
            completed_at: receipt.completed_at.as_secs(),
        });
        Ok(review)
    }
}
