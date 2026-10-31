//! Receipt roots only. Authentication, transitive resolution and retirement are
//! separate operations; a settled receipt still carries anti-replay obligations.
use super::*;

mod deadlines;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Reference<'a> {
    Account(&'a WalletAddress),
    Block(BlockHash),
    Checkpoint(BlockHash),
    CommitteeContext(BlockHash),
    Parameters(BlockHash),
    Outcome(BlockHash),
    RewardOrigin(TxHash),
    Boundary(Timestamp),
}

fn invalid(reason: impl ToString) -> StoreError {
    StoreError::Corruption(reason.to_string())
}

pub(crate) const PREFIXES: &[&str] = &[
    "verification_stake/",
    "verification_receipt/",
    "verification_outcome_sources/",
    "verification_transition_sources/",
    "verification_identity_source/",
    "verification_identity_binding/",
    "verification_identity_bound/",
    "verification_latest_outcome/",
    "verification_certified_outcome/",
    "verification_activation/",
    "verification_deadline/",
    "verification_default/",
];

fn key_hash(value: &str) -> Result<BlockHash, StoreError> {
    let mut bytes = [0; 32];
    hex::decode_to_slice(value, &mut bytes).map_err(invalid)?;
    let hash = BlockHash::new(bytes);
    if hash.is_zero() || value != hash.to_string() {
        return Err(invalid("noncanonical lifecycle hash"));
    }
    Ok(hash)
}

pub(crate) fn visit_row(
    key: &str,
    bytes: &[u8],
    visit: impl FnMut(Reference<'_>) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    if key.starts_with("verification_stake/") {
        visit_stake(key, bytes, visit)
    } else if key.starts_with("verification_receipt/") {
        visit_outcome(key, bytes, visit)
    } else if [
        "verification_activation/",
        "verification_deadline/",
        "verification_default/",
    ]
    .iter()
    .any(|prefix| key.starts_with(prefix))
    {
        deadlines::visit(key, bytes, visit)
    } else {
        visit_link(key, bytes, visit)
    }
}

/// These local links name dependencies; they do not themselves certify them.
/// Keep the certification value in the checkpoint namespace rather than treating
/// it as a signed ledger block. Dependency-set decoding retains existing bounds.
pub(crate) fn visit_link(
    key: &str,
    bytes: &[u8],
    mut visit: impl FnMut(Reference<'_>) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    for prefix in [
        "verification_outcome_sources/v1/",
        "verification_transition_sources/v1/",
    ] {
        if let Some(suffix) = key.strip_prefix(prefix) {
            let owner = key_hash(suffix)?;
            let sources = dependencies::decode(bytes)?;
            if !sources.contains(&owner) {
                return Err(invalid("lifecycle sources omit their owner"));
            }
            visit(if prefix.starts_with("verification_outcome_") {
                Reference::Outcome(owner)
            } else {
                Reference::Block(owner)
            })?;
            for source in sources {
                visit(Reference::Block(source))?;
            }
            return Ok(());
        }
    }
    for prefix in [
        "verification_identity_source/v1/",
        "verification_identity_bound/v1/",
        "verification_latest_outcome/v1/",
    ] {
        if let Some(suffix) = key.strip_prefix(prefix) {
            if !suffix.starts_with("brst_") || suffix.len() <= 5 || suffix.contains('/') {
                return Err(invalid("invalid lifecycle account key"));
            }
            let account = WalletAddress::new(suffix);
            let source = dependencies::decode_source(bytes)?;
            visit(Reference::Account(&account))?;
            visit(if prefix.starts_with("verification_latest_") {
                Reference::Outcome(source)
            } else {
                Reference::Block(source)
            })?;
            return Ok(());
        }
    }
    for (prefix, checkpoint) in [
        ("verification_identity_binding/v1/", false),
        ("verification_certified_outcome/v1/", true),
    ] {
        if let Some(suffix) = key.strip_prefix(prefix) {
            let owner = key_hash(suffix)?;
            let source = dependencies::decode_source(bytes)?;
            visit(if checkpoint {
                Reference::Outcome(owner)
            } else {
                Reference::Block(owner)
            })?;
            visit(if checkpoint {
                Reference::Checkpoint(source)
            } else {
                Reference::Block(source)
            })?;
            return Ok(());
        }
    }
    Err(invalid("unsupported lifecycle link schema"))
}

pub(crate) fn visit_stake(
    key: &str,
    bytes: &[u8],
    mut visit: impl FnMut(Reference<'_>) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let suffix = key
        .strip_prefix("verification_stake/v2/")
        .ok_or_else(|| invalid("unsupported stake receipt key"))?;
    let mut parts = suffix.split('/');
    let challenge = match parts.next() {
        Some("vote") => false,
        Some("challenge") => true,
        _ => return Err(invalid("unknown stake receipt kind")),
    };
    let account = |value: Option<&str>| -> Result<WalletAddress, StoreError> {
        let value = value
            .filter(|s| s.starts_with("brst_") && s.len() > 5)
            .ok_or_else(|| invalid("invalid stake receipt account"))?;
        Ok(WalletAddress::new(value))
    };
    let target = account(parts.next())?;
    let staker = account(parts.next())?;
    let encoded = parts
        .next()
        .ok_or_else(|| invalid("missing stake attempt"))?;
    let mut hash = [0; 32];
    hex::decode_to_slice(encoded, &mut hash).map_err(invalid)?;
    let attempt = BlockHash::new(hash);
    if parts.next().is_some()
        || attempt.is_zero()
        || key != stake_key(&target, &staker, challenge, attempt)
    {
        return Err(invalid("noncanonical stake receipt key"));
    }
    let receipt: StakeReceipt = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(128)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(invalid)?;
    if receipt.block.is_zero() || receipt.settled_by == Some(BlockHash::ZERO) {
        return Err(invalid("invalid stake receipt dependency"));
    }
    visit(Reference::Account(&target))?;
    visit(Reference::Account(&staker))?;
    visit(if challenge {
        Reference::Block(attempt)
    } else {
        Reference::CommitteeContext(attempt)
    })?;
    visit(Reference::Block(receipt.block))?;
    if let Some(settlement) = receipt.settled_by {
        visit(Reference::Outcome(settlement))?;
    }
    Ok(())
}

pub(crate) fn visit_outcome(
    key: &str,
    bytes: &[u8],
    mut visit: impl FnMut(Reference<'_>) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let receipt = receipts::decode(bytes).map_err(invalid)?;
    if key != receipts::key(receipt.source) {
        return Err(invalid("outcome receipt key mismatch"));
    }
    visit(Reference::Outcome(receipt.source))?;
    visit(Reference::Account(&receipt.target))?;
    visit(Reference::Boundary(receipt.completed_at))?;
    if let Some(previous) = receipt.previous {
        visit(Reference::Outcome(previous))?;
    }
    match &receipt.verdict {
        receipts::Verdict::Verified | receipts::Verdict::Failed => {}
        receipts::Verdict::Challenge { admission, .. } => visit(Reference::Block(*admission))?,
        receipts::Verdict::Appeal {
            admission,
            reviewed,
            ..
        } => {
            visit(Reference::Block(*admission))?;
            visit(Reference::Outcome(*reviewed))?;
        }
    }
    for reward in &receipt.rewards {
        visit(Reference::Account(&reward.recipient))?;
        visit(Reference::RewardOrigin(reward.origin))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
