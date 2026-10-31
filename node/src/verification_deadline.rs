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

/// Check completion encoding and internal consistency, not checkpoint signatures
/// or committee membership. The caller supplies authenticated metadata.
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

#[cfg(test)]
thread_local! {
    static COMMITMENT_CALLS: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

pub(crate) fn commitment(values: &[Completion]) -> Result<BlockHash, String> {
    #[cfg(test)]
    COMMITMENT_CALLS.with(|count| {
        if let Some(value) = count.get() { count.set(Some(value + 1)); }
    });
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

#[cfg(test)]
fn history_defaults(
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

/// Match prior voters to caller-validated completions for this target and lifecycle.
/// The first match wins; each contributing completion is hashed once.
pub(crate) fn history_defaults_from_completions(
    orchestrator: &burst_verification::VerificationOrchestrator,
    target: &WalletAddress,
    origin: BlockHash,
    completions: &[Completion],
) -> Result<std::collections::HashMap<WalletAddress, (BlockHash, u64)>, String> {
    let mut needed: std::collections::HashSet<_> = orchestrator.prior_panel_votes(target)
        .iter().map(|vote| &vote.verifier).collect();
    let mut defaults = std::collections::HashMap::new();
    for value in completions {
        if needed.is_empty() { break; }
        if value.activation.target != *target || value.lifecycle != origin { continue; }
        let mut hash = None;
        for voter in &value.absent {
            if needed.remove(voter) {
                let hash = match hash {
                    Some(hash) => hash,
                    None => {
                        let computed = commitment(std::slice::from_ref(value))?;
                        hash = Some(computed);
                        computed
                    }
                };
                defaults.insert(voter.clone(), (hash, value.activation.deadline));
            }
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
    let defaults = history_defaults_from_metadata(orchestrator, target, lifecycle, |key| {
        ledger.get_meta(key).map_err(|error| error.to_string())
    })?;
    orchestrator
        .selection_history_with_defaults(target, &defaults)
        .map_err(|error| error.to_string())
}

/// Read bindings first, then decode each referenced completion once. Only voter
/// bindings are grouped in memory; completion bodies are released between groups.
pub(crate) fn history_defaults_from_metadata(
    orchestrator: &burst_verification::VerificationOrchestrator,
    target: &WalletAddress,
    lifecycle: crate::checkpoint_replay::SelectionLifecycle,
    mut read: impl FnMut(&str) -> Result<Option<Vec<u8>>, String>,
) -> Result<std::collections::HashMap<WalletAddress, (BlockHash, u64)>, String> {
    let origin = match lifecycle {
        crate::checkpoint_replay::SelectionLifecycle::Verification(origin)
        | crate::checkpoint_replay::SelectionLifecycle::Challenge(origin) => origin,
    };
    let mut bindings = std::collections::BTreeMap::<BlockHash, Vec<&WalletAddress>>::new();
    for vote in orchestrator.prior_panel_votes(target) {
        if let Some(bytes) = read(&voter_key(target, &vote.verifier))? {
            let context = BlockHash::new(bytes.as_slice().try_into()
                .map_err(|_| "invalid deadline voter binding")?);
            bindings.entry(context).or_default().push(&vote.verifier);
        }
    }
    let mut defaults = std::collections::HashMap::new();
    for (context, voters) in bindings {
        let bytes = read(&key(context))?.ok_or("deadline voter has no completion")?;
        let value = decode(&bytes)?;
        if value.activation.target != *target || value.activation.context != context
            || voters.iter().any(|voter| value.absent
                .binary_search_by(|absent| absent.as_str().cmp(voter.as_str())).is_err())
        {
            return Err("deadline voter differs from certified completion".into());
        }
        if value.lifecycle != origin { continue; }
        let hash = commitment(std::slice::from_ref(&value))?;
        for voter in voters {
            defaults.insert(voter.clone(), (hash, value.activation.deadline));
        }
    }
    Ok(defaults)
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
        // Overlapping completions retain the first match and share one hash per
        // contributing completion; unrelated targets and lifecycles are ignored.
        let mut snapshot = orchestrator.snapshot();
        let voters: Vec<_> = (0..128).map(|i| WalletAddress::new(format!("brst_voter_{i:03}"))).collect();
        snapshot.prior_votes.insert(target.clone(), voters.iter().map(|verifier| VerifierVote {
            verifier: verifier.clone(), vote: Vote::Neither, stake_amount: 0,
            timestamp: Timestamp::new(20),
        }).collect());
        let orchestrator = VerificationOrchestrator::restore(snapshot);
        let mut first = value.clone();
        first.absent = voters[..96].to_vec();
        let mut second = value.clone();
        second.activation.context = BlockHash::new([9; 32]);
        second.activation.deadline = 22;
        second.completed_at = 23;
        second.absent = voters[64..].to_vec();
        let mut other_lifecycle = first.clone();
        other_lifecycle.lifecycle = BlockHash::new([8; 32]);
        let mut other_target = first.clone();
        other_target.activation.target = WalletAddress::new("brst_other");
        let completions = vec![other_lifecycle, other_target, first, second];
        let expected = history_defaults(&orchestrator, &target, SelectionLifecycle::Verification(origin), |voter| {
            Ok(completions.iter().find(|c| c.lifecycle == origin && c.activation.target == target
                && c.absent.contains(voter)).cloned())
        }).unwrap();
        COMMITMENT_CALLS.with(|count| count.set(Some(0)));
        let actual = history_defaults_from_completions(&orchestrator, &target, origin, &completions).unwrap();
        let calls = COMMITMENT_CALLS.with(|count| count.replace(None)).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 128);
        assert_eq!(calls, 2, "one commitment per contributing completion");
        assert_eq!(actual[&voters[64]].1, 20, "overlap retains the first match");
        assert!(history_defaults_from_completions(&orchestrator, &target, BlockHash::ZERO, &completions).unwrap().is_empty());

        let mut metadata = std::collections::HashMap::new();
        for completion in &completions[2..] {
            metadata.insert(key(completion.activation.context), bincode::serialize(completion).unwrap());
        }
        for (i, voter) in voters.iter().enumerate() {
            let context = completions[if i < 96 { 2 } else { 3 }].activation.context;
            metadata.insert(voter_key(&target, voter), context.as_bytes().to_vec());
        }
        for lifecycle in [SelectionLifecycle::Verification(origin), SelectionLifecycle::Challenge(origin)] {
            let mut completion_reads = 0;
            COMMITMENT_CALLS.with(|count| count.set(Some(0)));
            let actual = history_defaults_from_metadata(&orchestrator, &target, lifecycle, |key| {
                completion_reads += usize::from(key.starts_with("verification_deadline/v1/"));
                Ok(metadata.get(key).cloned())
            }).unwrap();
            let calls = COMMITMENT_CALLS.with(|count| count.replace(None)).unwrap();
            assert_eq!(actual, expected);
            assert_eq!(completion_reads, 2);
            assert_eq!(calls, 2);
        }
        let lifecycle = SelectionLifecycle::Verification(origin);
        let read = |rows: &std::collections::HashMap<String, Vec<u8>>| {
            history_defaults_from_metadata(&orchestrator, &target, lifecycle, |key| Ok(rows.get(key).cloned()))
        };
        let mut invalid = metadata.clone();
        invalid.insert(voter_key(&target, &voters[0]), vec![1]);
        assert!(read(&invalid).unwrap_err().contains("invalid deadline voter binding"));
        let context = completions[2].activation.context;
        let mut invalid = metadata.clone();
        invalid.remove(&key(context));
        assert!(read(&invalid).unwrap_err().contains("no completion"));
        let mut changed = completions[2].clone();
        changed.activation.target = WalletAddress::new("brst_wrong");
        let mut invalid = metadata.clone();
        invalid.insert(key(context), bincode::serialize(&changed).unwrap());
        assert!(read(&invalid).is_err());
        for kind in 0..3 {
            let mut changed = completions[2].clone();
            match kind {
                0 => changed.activation.context = BlockHash::new([44; 32]),
                1 => { changed.absent.remove(0); },
                _ => changed.absent.swap(0, 1),
            }
            let mut rows = metadata.clone();
            rows.insert(key(context), bincode::serialize(&changed).unwrap());
            assert!(read(&rows).is_err());
        }
        assert!(history_defaults_from_metadata(&orchestrator, &target,
            SelectionLifecycle::Verification(BlockHash::ZERO), |key| Ok(metadata.get(key).cloned())).unwrap().is_empty());
        assert!(history_defaults_from_metadata(&orchestrator, &target, lifecycle,
            |_| Err("read fault".into())).unwrap_err().contains("read fault"));

    }
}
