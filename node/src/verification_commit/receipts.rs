use super::*;
use burst_types::ChallengeReason;

const MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Resolve only against the candidate's replay-derived history. A live-store
/// receipt by itself is not evidence that its outcome has been certified.
pub(super) fn appeal_context(
    batch: &impl Ledger,
    block: &StateBlock,
) -> Result<burst_verification::AppealContext, String> {
    if block.block_type != BlockType::Challenge
        || block.target_head.is_zero()
        || block.hash == block.target_head
    {
        return Err("appeal reference is invalid".into());
    }
    let target = crate::ledger_bridge::extract_receiver_from_link(&block.link)
        .ok_or("appeal target is invalid")?;
    let latest = batch
        .get_meta(&latest_key(&target))
        .map_err(|error| error.to_string())?
        .ok_or("appeal target has no outcome")?;
    if latest.as_slice() != block.target_head.as_bytes() {
        return Err("appeal does not reference the latest target outcome".into());
    }
    let bytes = batch
        .get_meta(&key(block.target_head))
        .map_err(|error| error.to_string())?
        .ok_or("appeal outcome receipt is missing")?;
    let receipt = decode(&bytes)?;
    if receipt.source != block.target_head
        || receipt.target != target
        || receipt.completed_at > block.timestamp
    {
        return Err("appeal outcome history mismatch".into());
    }
    let (admission, reason, result) = receipt
        .challenge_judgment()
        .ok_or("appeal requires a completed challenge")?;
    let previously_legitimate = match result {
        ChallengeVerdict::Upheld => false,
        ChallengeVerdict::Rejected => true,
        ChallengeVerdict::LegacyExpired => {
            return Err("legacy expiry has no certified appeal judgment".into())
        }
    };
    if ChallengeReason::from_origin(block.origin.as_bytes()) != reason {
        return Err("appeal cannot change the challenged reason".into());
    }
    let certified = batch
        .get_meta(&certified_key(receipt.source))
        .map_err(|error| error.to_string())?
        .ok_or("appeal outcome is not certified")?;
    if certified.len() != 32 || certified.iter().all(|byte| *byte == 0) {
        return Err("appeal outcome certification is invalid".into());
    }
    for (hash, is_admission) in [(admission, true), (receipt.source, false)] {
        let bytes = batch.get_block(&hash).map_err(|error| error.to_string())?;
        let source: StateBlock = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(burst_types::MAX_P2P_MESSAGE_BYTES as u64)
            .reject_trailing_bytes()
            .deserialize(&bytes)
            .map_err(|error| error.to_string())?;
        if source.hash != hash
            || source.compute_hash() != hash
            || source.timestamp > receipt.completed_at
            || crate::ledger_bridge::extract_receiver_from_link(&source.link).as_ref()
                != Some(&target)
            || (is_admission
                && (source.block_type != BlockType::Challenge
                    || ChallengeReason::from_origin(source.origin.as_bytes()) != reason))
            || (!is_admission
                && !matches!(
                    source.block_type,
                    BlockType::Challenge | BlockType::VerificationVote
                ))
        {
            return Err("appeal receipt has an unrelated signed source".into());
        }
    }
    Ok(burst_verification::AppealContext {
        reviewed_outcome: receipt.source,
        previously_legitimate,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Verdict {
    Verified,
    Failed,
    Challenge {
        admission: BlockHash,
        reason: ChallengeReason,
        result: ChallengeVerdict,
    },
    Appeal {
        admission: BlockHash,
        reason: ChallengeReason,
        result: ChallengeVerdict,
        reviewed: BlockHash,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ChallengeVerdict {
    Upheld,
    Rejected,
    LegacyExpired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Reward {
    pub recipient: WalletAddress,
    pub origin: TxHash,
    pub amount: u128,
}

/// Replay-derived settlement history, not independent authority to undo it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Receipt {
    pub source: BlockHash,
    pub target: WalletAddress,
    pub completed_at: Timestamp,
    pub previous: Option<BlockHash>,
    pub verdict: Verdict,
    pub rewards: Vec<Reward>,
}

impl Receipt {
    pub(crate) fn challenge_judgment(
        &self,
    ) -> Option<(BlockHash, ChallengeReason, &ChallengeVerdict)> {
        match &self.verdict {
            Verdict::Challenge {
                admission,
                reason,
                result,
            }
            | Verdict::Appeal {
                admission,
                reason,
                result,
                ..
            } => Some((*admission, *reason, result)),
            _ => None,
        }
    }
}

pub(crate) fn certified_key(source: BlockHash) -> String {
    format!("verification_certified_outcome/v1/{source}")
}

/// Called only after the replayed checkpoint has passed certificate and state
/// checks. These local markers are reconstructed, never imported as authority.
pub(crate) fn certify_metadata(
    metadata: &mut std::collections::HashMap<String, Vec<u8>>,
    checkpoint: BlockHash,
) -> Result<(), String> {
    if checkpoint.is_zero() {
        return Err("outcome certification has no checkpoint".into());
    }
    let mut sources = Vec::new();
    for (name, bytes) in metadata
        .iter()
        .filter(|(name, _)| name.starts_with("verification_receipt/v1/"))
    {
        let receipt = decode(bytes)?;
        if *name != key(receipt.source) {
            return Err("outcome receipt key mismatch".into());
        }
        sources.push(receipt.source);
    }
    for source in sources {
        metadata
            .entry(certified_key(source))
            .or_insert_with(|| checkpoint.as_bytes().to_vec());
    }
    Ok(())
}

pub(crate) fn key(source: BlockHash) -> String {
    format!("verification_receipt/v1/{source}")
}

pub(crate) fn latest_key(target: &WalletAddress) -> String {
    format!("verification_latest_outcome/v1/{target}")
}

pub(super) fn reward_origin(source: BlockHash, recipient: &WalletAddress) -> TxHash {
    TxHash::new(burst_crypto::blake2b_256_multi(&[
        b"BURST/verification-reward/v1\0",
        source.as_bytes(),
        recipient.as_str().as_bytes(),
    ]))
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Receipt, String> {
    let receipt: Receipt = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_BYTES)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(|error| error.to_string())?;
    if receipt.source.is_zero()
        || receipt.previous == Some(receipt.source)
        || receipt.previous == Some(BlockHash::ZERO)
        || receipt
            .rewards
            .windows(2)
            .any(|pair| pair[0].recipient.as_str() >= pair[1].recipient.as_str())
        || receipt.rewards.iter().any(|reward| {
            reward.amount == 0 || reward.origin != reward_origin(receipt.source, &reward.recipient)
        })
        || matches!(&receipt.verdict, Verdict::Challenge { admission, .. } if admission.is_zero())
        || matches!(&receipt.verdict, Verdict::Appeal { admission, reviewed, result, .. }
            if admission.is_zero() || reviewed.is_zero() || *reviewed == receipt.source
                || receipt.previous != Some(*reviewed) || *result == ChallengeVerdict::LegacyExpired)
    {
        return Err("invalid verification outcome receipt".into());
    }
    Ok(receipt)
}

pub(super) fn prepare(
    batch: &impl Ledger,
    context: &Settlement,
    events: &[VerificationEvent],
) -> Result<Receipt, String> {
    if batch
        .get_meta(&key(context.hash))
        .map_err(|error| error.to_string())?
        .is_some()
    {
        return Err("verification outcome already recorded".into());
    }
    let mut verdict = None;
    for event in events {
        let next = match event {
            VerificationEvent::VerificationComplete { wallet, result, .. }
                if wallet == &context.target =>
            {
                match result {
                    VerificationResult::Verified => Verdict::Verified,
                    VerificationResult::Failed => Verdict::Failed,
                }
            }
            VerificationEvent::ChallengeResolved { wallet, outcome }
                if wallet == &context.target =>
            {
                let admission = outcome
                    .challenge_context
                    .ok_or("challenge outcome has no admission")?;
                let bytes = batch
                    .get_block(&admission)
                    .map_err(|error| error.to_string())?;
                let block: StateBlock = bincode::DefaultOptions::new()
                    .with_fixint_encoding()
                    .with_limit(burst_types::MAX_P2P_MESSAGE_BYTES as u64)
                    .reject_trailing_bytes()
                    .deserialize(&bytes)
                    .map_err(|error| error.to_string())?;
                if block.hash != admission
                    || block.compute_hash() != admission
                    || block.block_type != BlockType::Challenge
                    || block.account != outcome.challenger
                    || crate::ledger_bridge::extract_receiver_from_link(&block.link).as_ref()
                        != Some(&context.target)
                {
                    return Err("challenge receipt has an unrelated admission".into());
                }
                let reason = ChallengeReason::from_origin(block.origin.as_bytes());
                let result = match outcome.outcome {
                    ChallengeResult::FraudConfirmed => ChallengeVerdict::Upheld,
                    ChallengeResult::ChallengeRejected => ChallengeVerdict::Rejected,
                    ChallengeResult::Expired => ChallengeVerdict::LegacyExpired,
                };
                if let Some(appeal) = &outcome.appeal {
                    if &appeal_context(batch, &block)? != appeal {
                        return Err("appeal settlement differs from reviewed receipt".into());
                    }
                    Verdict::Appeal {
                        admission,
                        reason,
                        result,
                        reviewed: appeal.reviewed_outcome,
                    }
                } else {
                    if !block.target_head.is_zero() {
                        return Err("appeal cannot settle as an ordinary challenge".into());
                    }
                    Verdict::Challenge {
                        admission,
                        reason,
                        result,
                    }
                }
            }
            _ => continue,
        };
        if verdict.replace(next).is_some() {
            return Err("multiple verdicts for one verification settlement".into());
        }
    }
    let previous = batch
        .get_meta(&latest_key(&context.target))
        .map_err(|error| error.to_string())?
        .map(|bytes| -> Result<BlockHash, String> {
            let hash = BlockHash::new(
                bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| "invalid latest outcome reference")?,
            );
            let bytes = batch
                .get_meta(&key(hash))
                .map_err(|error| error.to_string())?
                .ok_or("latest verification outcome is missing")?;
            let receipt = decode(&bytes)?;
            if receipt.source != hash
                || receipt.target != context.target
                || receipt.completed_at > context.timestamp
            {
                return Err("verification outcome history mismatch".into());
            }
            Ok(hash)
        })
        .transpose()?;
    Ok(Receipt {
        source: context.hash,
        target: context.target.clone(),
        completed_at: context.timestamp,
        previous,
        verdict: verdict.ok_or("verification settlement has no verdict")?,
        rewards: Vec::new(),
    })
}

pub(super) fn persist(batch: &mut impl Ledger, receipt: &Receipt) -> Result<(), String> {
    let bytes = bincode::serialize(receipt).map_err(|error| error.to_string())?;
    decode(&bytes)?;
    batch
        .put_meta(&key(receipt.source), &bytes)
        .map_err(|error| error.to_string())?;
    batch
        .put_meta(&latest_key(&receipt.target), receipt.source.as_bytes())
        .map_err(|error| error.to_string())
}
