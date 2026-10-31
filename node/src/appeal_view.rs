//! Appeal display data from one local read transaction.
//! Certification is represented by a stored marker; this reader does not verify
//! checkpoint signatures or decide whether a new appeal would pass admission.

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
    /// Report current challenge state and, when present, the latest certified
    /// challenge judgment. Other latest receipts do not trigger a search for an
    /// older judgment. enabled is supplied by the caller and does not gate reads.
    pub fn read(&self, account: &WalletAddress, enabled: bool) -> Result<AppealReview, String> {
        burst_crypto::decode_address(account.as_str()).ok_or("invalid wallet address")?;
        let txn = self.store.tx_begin_read().map_err(|e| e.to_string())?;
        let meta = self.store.meta_store();
        let get = |key: &str| meta.get_meta_txn(&txn, key).map_err(|e| e.to_string());
        let params: ProtocolParams = match get("protocol_params")? {
            Some(bytes) => bincode::deserialize(&bytes).map_err(|e| e.to_string())?,
            None => self.initial_params.clone(),
        };
        let (state_present, challenge_active) = match get("verification_records_format")?.as_deref()
        {
            Some([format @ (1 | 2)]) => {
                use burst_verification::orchestrator::records::VerificationRecord;
                if get(crate::endorsement_commit::SNAPSHOT_KEY)?.is_some() {
                    return Err("mixed aggregate and keyed verification state".into());
                }
                let rules_key = format!("{}0", VerificationRecord::STORAGE_PREFIX);
                let rules = get(&rules_key)?.ok_or("missing verification rules")?;
                VerificationRecord::from_storage_entry(&rules_key, &rules)?;
                let legacy_key = VerificationRecord::target_storage_key(account)?;
                let active = if *format == 2 {
                    use burst_verification::orchestrator::records::normalized;
                    if get(&legacy_key)?.is_some() {
                        return Err("aggregate target in normalized state".into());
                    }
                    let key = normalized::target_header_key(account)?;
                    get(&key)?
                        .map(|bytes| normalized::header_has_active_challenge(account, &bytes))
                        .transpose()?
                        .unwrap_or(false)
                } else {
                    get(&legacy_key)?
                        .map(|bytes| {
                            VerificationRecord::from_storage_entry(&legacy_key, &bytes)?
                                .has_active_challenge()
                        })
                        .transpose()?
                        .unwrap_or(false)
                };
                (true, active)
            }
            Some(_) => return Err("unsupported verification record format".into()),
            None => {
                let snapshot = get(crate::endorsement_commit::SNAPSHOT_KEY)?
                    .map(|bytes| burst_verification::OrchestratorSnapshot::decode(&bytes))
                    .transpose()
                    .map_err(|e| e.to_string())?;
                (
                    snapshot.is_some(),
                    snapshot
                        .as_ref()
                        .is_some_and(|state| state.active_challenges.contains_key(account)),
                )
            }
        };
        let mut review = AppealReview {
            account: account.to_string(),
            enabled,
            challenge_active,
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
        if receipt.source != hash || &receipt.target != account || !state_present {
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
