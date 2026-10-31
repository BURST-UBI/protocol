use burst_consensus::GeneratedVote;
use burst_types::BlockHash;
use crate::durable_votes::VoteGenerationError;

type Wait = (BlockHash, BlockHash, Vec<BlockHash>);
type ResultItem = ((BlockHash, BlockHash), Result<GeneratedVote, VoteGenerationError>);

/// Only consecutive deferrals share a publication batch. Leave a ready vote or
/// an error in place so successful vote counting and broadcasts retain order.
pub(super) fn take_run<I: Iterator<Item = ResultItem>>(
    first: Wait, remaining: &mut std::iter::Peekable<I>,
) -> Vec<Wait> {
    let mut waits = vec![first];
    while waits.len() < 32 {
        if !matches!(remaining.peek(), Some((_, Err(VoteGenerationError::DependenciesPending(_))))) {
            break;
        }
        let ((root, candidate), result) = remaining.next().expect("peeked pending result");
        let Err(VoteGenerationError::DependenciesPending(dependencies)) = result else { unreachable!() };
        waits.push((root, candidate, dependencies));
    }
    waits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deferred_runs_are_bounded_and_leave_ready_votes_and_errors_in_order() {
        let hash = |n| BlockHash::new([n; 32]);
        let pending = |n| ((hash(n), hash(n)), Err(VoteGenerationError::DependenciesPending(vec![hash(200)])));
        let key = burst_crypto::keypair_from_seed(&[7; 32]);
        let mut signer = burst_consensus::VoteGenerator::new(burst_crypto::derive_address(&key.public), key.private.0);
        let ready = signer.generate_final_vote(hash(90));
        let mut results = (2..=70).map(pending)
            .chain([((hash(90), hash(90)), Ok(ready.clone())), pending(91),
                ((hash(92), hash(92)), Err(burst_store::StoreError::Backend("test failure".into()).into())), pending(93)])
            .peekable();
        let mut observed = Vec::new();
        let mut first = (hash(1), hash(1), vec![hash(200)]);
        for expected in [32, 32, 6] {
            let run = take_run(first, &mut results);
            assert_eq!(run.len(), expected);
            observed.extend(run.into_iter().map(|wait| wait.0));
            if expected == 6 { break; }
            let ((root, candidate), Err(VoteGenerationError::DependenciesPending(deps))) = results.next().unwrap() else { panic!() };
            first = (root, candidate, deps);
        }
        assert_eq!(observed, (1..=70).map(hash).collect::<Vec<_>>());
        let ((root, _), Ok(vote)) = results.next().unwrap() else { panic!() };
        assert_eq!(root, hash(90));
        assert_eq!(vote.signature, ready.signature);
        let ((root, candidate), Err(VoteGenerationError::DependenciesPending(deps))) = results.next().unwrap() else { panic!() };
        assert_eq!(take_run((root, candidate, deps), &mut results).len(), 1);
        assert!(matches!(results.next(), Some(((root, _), Err(VoteGenerationError::Store(_)))) if root == hash(92)));
        assert!(matches!(results.next(), Some(((root, _), Err(VoteGenerationError::DependenciesPending(_)))) if root == hash(93)));
        assert!(results.next().is_none());
    }
}
