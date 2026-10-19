//! Completion metadata is rebuilt from certified boundaries, never peer snapshots.

use bincode::Options;
use burst_types::{BlockHash, WalletAddress};
use burst_verification::CommitteeActivation;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Completion {
    pub activation: CommitteeActivation,
    pub lifecycle: BlockHash,
    pub completed_at: u64,
    pub absent: Vec<WalletAddress>,
}

pub(crate) fn key(context: BlockHash) -> String {
    format!("verification_deadline/v1/{context}")
}

pub(crate) fn voter_key(target: &WalletAddress, voter: &WalletAddress) -> String {
    format!("verification_default/v1/{target}/{voter}")
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Completion, String> {
    let value: Completion = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(crate::checkpoint_store::MAX_RECORD_BYTES)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(|error| error.to_string())?;
    if value.activation.context.is_zero()
        || value.activation.selection.is_zero()
        || value.activation.params_hash.is_zero()
        || value.activation.params_hash != value.activation.rules_at_activation.params_hash()
        || value.lifecycle.is_zero()
        || value.activation.started_at >= value.activation.deadline
        || value.completed_at < value.activation.deadline
        || value.absent.is_empty()
        || value
            .absent
            .windows(2)
            .any(|pair| pair[0].as_str() >= pair[1].as_str())
    {
        return Err("invalid certified deadline completion".into());
    }
    Ok(value)
}

pub(crate) fn commitment(values: &[Completion]) -> Result<BlockHash, String> {
    if values.is_empty() {
        return Err("deadline checkpoint has no pending completions".into());
    }
    if bincode::serialized_size(values).map_err(|error| error.to_string())?
        > crate::checkpoint_store::MAX_RECORD_BYTES
    {
        return Err("deadline completion batch exceeds replay bounds".into());
    }
    let bytes = bincode::serialize(values).map_err(|error| error.to_string())?;
    Ok(BlockHash::new(burst_crypto::blake2b_256_multi(&[
        b"BURST/normal-deadline-checkpoint/v2\0",
        &bytes,
    ])))
}

pub(crate) fn is_recorded_default(
    ledger: &impl crate::verification_commit::ledger::Ledger,
    target: &WalletAddress,
    voter: &WalletAddress,
    challenge: Option<BlockHash>,
) -> Result<bool, String> {
    Ok(recorded_default(ledger, target, voter)?
        .is_some_and(|completion| challenge.is_none_or(|origin| completion.lifecycle == origin)))
}

fn recorded_default(
    ledger: &impl crate::verification_commit::ledger::Ledger,
    target: &WalletAddress,
    voter: &WalletAddress,
) -> Result<Option<Completion>, String> {
    let Some(context) = ledger
        .get_meta(&voter_key(target, voter))
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let context = BlockHash::new(
        context
            .as_slice()
            .try_into()
            .map_err(|_| "invalid deadline voter binding")?,
    );
    let bytes = ledger
        .get_meta(&key(context))
        .map_err(|error| error.to_string())?
        .ok_or("deadline voter has no completion")?;
    let completion = decode(&bytes)?;
    if completion.activation.target != *target
        || completion.activation.context != context
        || !completion.absent.contains(voter)
    {
        return Err("deadline voter differs from certified completion".into());
    }
    Ok(Some(completion))
}

pub(crate) fn history_defaults(
    orchestrator: &burst_verification::VerificationOrchestrator,
    target: &WalletAddress,
    lifecycle: crate::checkpoint_replay::SelectionLifecycle,
    mut read: impl FnMut(&WalletAddress) -> Result<Option<Completion>, String>,
) -> Result<std::collections::HashMap<WalletAddress, (BlockHash, u64)>, String> {
    let mut defaults = std::collections::HashMap::new();
    let origin = match lifecycle {
        crate::checkpoint_replay::SelectionLifecycle::Verification(origin)
        | crate::checkpoint_replay::SelectionLifecycle::Challenge(origin) => origin,
    };
    for vote in orchestrator.prior_panel_votes(target) {
        if let Some(value) = read(&vote.verifier)? {
            // A verifier may serve again in a later, independent lifecycle.
            if value.lifecycle != origin {
                continue;
            }
            if value.activation.target != *target || !value.absent.contains(&vote.verifier) {
                return Err("prior default belongs to a different lifecycle".into());
            }
            let deadline = value.activation.deadline;
            defaults.insert(vote.verifier.clone(), (commitment(&[value])?, deadline));
        }
    }
    Ok(defaults)
}

pub(crate) fn live_history_commitment(
    ledger: &impl crate::verification_commit::ledger::Ledger,
    orchestrator: &burst_verification::VerificationOrchestrator,
    target: &WalletAddress,
    lifecycle: crate::checkpoint_replay::SelectionLifecycle,
) -> Result<BlockHash, String> {
    let defaults = history_defaults(orchestrator, target, lifecycle, |voter| {
        recorded_default(ledger, target, voter)
    })?;
    orchestrator
        .selection_history_with_defaults(target, &defaults)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint_replay::SelectionLifecycle;
    use burst_types::Timestamp;
    use burst_verification::{state::VerifierVote, VerificationOrchestrator, Vote};

    #[test]
    fn prior_defaults_are_scoped_to_the_normal_or_challenge_lifecycle() {
        let target = WalletAddress::new("brst_target");
        let voter = WalletAddress::new("brst_voter");
        let origin = BlockHash::new([1; 32]);
        let mut snapshot = VerificationOrchestrator::new().snapshot();
        snapshot.prior_votes.insert(
            target.clone(),
            vec![VerifierVote {
                verifier: voter.clone(),
                vote: Vote::Neither,
                stake_amount: 0,
                timestamp: Timestamp::new(20),
            }],
        );
        let orchestrator = VerificationOrchestrator::restore(snapshot);
        let value = Completion {
            activation: CommitteeActivation {
                target: target.clone(),
                selection: BlockHash::new([2; 32]),
                context: BlockHash::new([3; 32]),
                params_hash: burst_types::ProtocolParams::default().params_hash(),
                rules_at_activation: burst_types::ProtocolParams::default(),
                started_at: 10,
                deadline: 20,
            },
            lifecycle: origin,
            completed_at: 21,
            absent: vec![voter.clone()],
        };
        for lifecycle in [
            SelectionLifecycle::Verification(origin),
            SelectionLifecycle::Challenge(origin),
        ] {
            let defaults = history_defaults(&orchestrator, &target, lifecycle, |_| {
                Ok(Some(value.clone()))
            })
            .unwrap();
            assert_eq!(
                defaults[&voter],
                (commitment(std::slice::from_ref(&value)).unwrap(), 20)
            );
            let mut previous = value.clone();
            previous.lifecycle = BlockHash::new([5; 32]);
            assert!(
                history_defaults(&orchestrator, &target, lifecycle, |_| Ok(Some(
                    previous.clone()
                )))
                .unwrap()
                .is_empty()
            );
            previous.lifecycle = origin;
            previous.activation.target = voter.clone();
            assert!(
                history_defaults(&orchestrator, &target, lifecycle, |_| Ok(Some(
                    previous.clone()
                )))
                .is_err()
            );
            previous.activation.target = target.clone();
            previous.absent.clear();
            assert!(
                history_defaults(&orchestrator, &target, lifecycle, |_| Ok(Some(
                    previous.clone()
                )))
                .is_err()
            );
        }
    }
}
