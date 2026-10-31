//! The deadline is replay-derived checkpoint metadata, never receipt time.

use bincode::Options;
use burst_ledger::StateBlock;
use burst_types::BlockHash;

use crate::verification_commit::ledger::Ledger;

pub(crate) use burst_verification::CommitteeActivation as Activation;

pub(crate) fn decode(bytes: &[u8]) -> Result<Activation, String> {
    let activation: Activation = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(4096)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(|error| error.to_string())?;
    if activation.context.is_zero()
        || activation.selection.is_zero()
        || activation.params_hash != activation.rules_at_activation.params_hash()
        || activation.deadline <= activation.started_at
    {
        return Err("invalid certified activation metadata".into());
    }
    Ok(activation)
}

pub(crate) fn key(context: BlockHash) -> String {
    format!("verification_activation/v1/{context}")
}

/// Enforce an existing activation's target, context and exclusive deadline.
/// Missing activation imposes no deadline here; committee authorization and
/// the lower timestamp bound are checked separately.
pub(crate) fn validate_vote(ledger: &impl Ledger, block: &StateBlock) -> Result<(), String> {
    let context = BlockHash::new(*block.origin.as_bytes());
    let Some(bytes) = ledger
        .get_meta(&key(context))
        .map_err(|error| error.to_string())?
    else {
        // A certified committee may vote before activation. It cannot time out
        // until a later agreed event has established its window.
        return Ok(());
    };
    let activation = decode(&bytes)?;
    if activation.context != context
        || crate::block_economics::wallet_from_link(&block.link).as_ref()
            != Some(&activation.target)
    {
        return Err("invalid certified activation metadata".into());
    }
    if block.timestamp.as_secs() >= activation.deadline {
        return Err("verification deadline has passed; agreed completion is required".into());
    }
    Ok(())
}
