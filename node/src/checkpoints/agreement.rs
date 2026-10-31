//! Durable checkpoint round transitions authorized by confirmed-history replay.

use bincode::Options;
use burst_consensus::checkpoint::{
    Checkpoint, CheckpointAnchor, CheckpointCertificate, CheckpointSignature,
};
use burst_consensus::checkpoint_round::{Ballot, Proposal, Quorum, RoundState, SignedBallot, Step};
use burst_crypto::{blake2b_256, derive_address, public_from_private, sign_message};
use burst_store::{meta::MetaStore, StoreError};
use burst_store_lmdb::LmdbStore;
use burst_types::{PrivateKey, ProtocolParams, WalletAddress};
#[cfg(test)]
pub(crate) use reconciliation::reconcile_recovery_journals;
use serde::{Deserialize, Serialize};

use crate::checkpoint_replay::{
    CheckpointManifest, ObservedSigningClock, ReplayedCheckpoint, SigningClock,
};

const MAX_STATE_BYTES: u64 = 16 * 1024 * 1024;

mod compact;
pub(crate) use compact::recover_compact_candidates_shared;
mod compact_input;
pub use compact_input::{CompactReplayCandidate, CompactReplayInput};
mod reconciliation;
pub(crate) mod retention;
pub(crate) use compact::apply_compact_candidates_observed;
pub use compact::{
    apply_compact_candidates, apply_compact_selections, recover_compact_candidates_from_peers,
    recover_compact_selections_from_peers,
};
mod replay;
pub(crate) use reconciliation::{
    reconcile_recovery_journals_view_with_poll, reconcile_snapshot_journals,
};
pub use replay::{ReplayInput, ReplayManifest, ReplayedCandidate};

pub enum Command<'a> {
    Propose(Checkpoint),
    Proposal(&'a Proposal),
    Prepared(&'a Proposal, &'a Quorum),
    Decide(CheckpointCertificate),
    Timeout {
        round: u64,
        step: Step,
        evidence: &'a [SignedBallot],
    },
    CatchUp {
        round: u64,
        evidence: &'a [SignedBallot],
    },
    Resume,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outgoing {
    Proposal(Box<Proposal>),
    Vote(SignedBallot),
    Decision(CheckpointCertificate),
}

/// `proof` authorizes re-proposing exactly these inputs, not replacing a pool
/// after the beacon is known. The validator must replay the checkpoint in both
/// cases. Fresh selection signatures require the pre-beacon deadline check;
/// fresh outcome signatures require a cutoff known to be in the trusted past.
pub struct Validation<'a> {
    pub checkpoint: &'a Checkpoint,
    pub proof: Option<&'a Quorum>,
    pub decided: bool,
}

#[derive(Serialize, Deserialize)]
struct Snapshot {
    version: u8,
    state: RoundState,
    manifests: Option<Vec<ReplayManifest>>,
    checksum: [u8; 32],
}

impl Snapshot {
    fn checksum(&self) -> Result<[u8; 32], StoreError> {
        let bytes = bincode::serialize(&(&self.state, &self.manifests)).map_err(invalid)?;
        Ok(blake2b_256(&bytes))
    }

    fn validate_manifest_coverage(&self) -> Result<(), StoreError> {
        let Some(manifests) = &self.manifests else {
            return Ok(());
        };
        let mut remaining = self.state.retained_values();
        if manifests.len() != remaining.len() {
            return Err(invalid("checkpoint state has incomplete replay manifests"));
        }
        for manifest in manifests {
            let hash = manifest.checkpoint().hash();
            let position = remaining
                .iter()
                .position(|value| *value == hash)
                .ok_or_else(|| invalid("duplicate or unrelated checkpoint manifest"))?;
            remaining.swap_remove(position);
        }
        Ok(())
    }

    fn validate_manifests(&self, anchor: &CheckpointAnchor) -> Result<(), StoreError> {
        self.validate_manifest_coverage()?;
        for manifest in self.manifests.iter().flatten() {
            anchor
                .validate_successor(manifest.checkpoint())
                .map_err(invalid)?;
        }
        Ok(())
    }
}

fn state_key(
    anchor: &CheckpointAnchor,
    representative: &WalletAddress,
) -> Result<String, StoreError> {
    let height = anchor
        .height()
        .checked_add(1)
        .ok_or_else(|| invalid("checkpoint height exhausted"))?;
    // Parent is deliberately absent: a second branch must hit the same record.
    Ok(format!(
        "checkpoint_round/v1/{}/{}/{representative}/{height}",
        anchor.network().as_str(),
        anchor.genesis_hash()
    ))
}

fn invalid(message: impl ToString) -> StoreError {
    StoreError::Corruption(message.to_string())
}

fn decode_snapshot(bytes: &[u8]) -> Result<Snapshot, StoreError> {
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(invalid("checkpoint state exceeds limit"));
    }
    let options = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .reject_trailing_bytes()
        .with_limit(MAX_STATE_BYTES);
    let snapshot: Snapshot = if bytes.first() == Some(&2) {
        #[derive(Serialize, Deserialize)]
        struct Legacy {
            version: u8,
            state: RoundState,
            manifests: Option<Vec<CheckpointManifest>>,
            checksum: [u8; 32],
        }
        let old: Legacy = options.deserialize(bytes).map_err(invalid)?;
        let checksum =
            blake2b_256(&bincode::serialize(&(&old.state, &old.manifests)).map_err(invalid)?);
        if checksum != old.checksum {
            return Err(invalid("invalid legacy checkpoint checksum"));
        }
        let mut converted = Snapshot {
            version: 3,
            state: old.state,
            manifests: old
                .manifests
                .map(|values| values.into_iter().map(ReplayManifest::Selection).collect()),
            checksum: [0; 32],
        };
        converted.checksum = converted.checksum()?;
        converted
    } else {
        options.deserialize(bytes).map_err(invalid)?
    };
    if snapshot.version != 3 || snapshot.checksum != snapshot.checksum()? {
        return Err(invalid("invalid checkpoint state checksum or version"));
    }
    Ok(snapshot)
}

fn decode(
    bytes: &[u8],
    anchor: &CheckpointAnchor,
    representative: &WalletAddress,
) -> Result<Snapshot, StoreError> {
    let snapshot = decode_snapshot(bytes)?;
    snapshot
        .state
        .validate(anchor, representative)
        .map_err(invalid)?;
    snapshot.validate_manifests(anchor)?;
    Ok(snapshot)
}

pub fn read_state(
    store: &LmdbStore,
    anchor: &CheckpointAnchor,
    representative: &WalletAddress,
) -> Result<Option<RoundState>, StoreError> {
    match store
        .meta_store()
        .get_meta(&state_key(anchor, representative)?)
    {
        Ok(bytes) => decode(&bytes, anchor, representative).map(|snapshot| Some(snapshot.state)),
        Err(StoreError::NotFound(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Recover selection choices. Reject journals containing other checkpoint kinds;
/// use `read_candidates` for those.
pub fn read_replays(
    store: &LmdbStore,
    anchor: &CheckpointAnchor,
    representative: &WalletAddress,
    initial_params: &ProtocolParams,
) -> Result<Vec<ReplayedCheckpoint>, StoreError> {
    read_candidates(store, anchor, representative, initial_params)?
        .into_iter()
        .map(|value| match value {
            ReplayedCandidate::Selection(value) => Ok(*value),
            ReplayedCandidate::ChallengeOutcome(_) => Err(invalid(
                "outcome recovery requires the mixed-candidate signer",
            )),
            ReplayedCandidate::Roster(_) => Err(invalid(
                "roster recovery requires the mixed-candidate signer",
            )),
            ReplayedCandidate::Activation(_) => Err(invalid(
                "activation recovery requires the mixed-candidate signer",
            )),
            ReplayedCandidate::Deadline(_) => Err(invalid(
                "deadline recovery requires the mixed-candidate signer",
            )),
            ReplayedCandidate::Governance(_) => Err(invalid(
                "governance recovery requires the mixed-candidate signer",
            )),
        })
        .collect()
}

/// Replay every manifest retained in the signer journal and validate its parent.
/// Recovery must include these choices before the signer resumes.
pub fn read_candidates(
    store: &LmdbStore,
    anchor: &CheckpointAnchor,
    representative: &WalletAddress,
    initial_params: &ProtocolParams,
) -> Result<Vec<ReplayedCandidate>, StoreError> {
    read_retained_manifests(store, anchor, representative)?
        .iter()
        .map(|manifest| {
            let replay = manifest.replay(store, initial_params)?;
            replay.input().validate_parent(anchor)?;
            Ok(replay)
        })
        .collect()
}

/// Read validated journal inputs without requiring a full local block prefix.
/// These manifests are recovery requests, not replay or signing authority.
/// Compact recovery must authenticate their witnesses and replay every retained
/// value. The signing transaction rechecks the journal after that work.
pub fn read_retained_manifests(
    store: &LmdbStore,
    anchor: &CheckpointAnchor,
    representative: &WalletAddress,
) -> Result<Vec<ReplayManifest>, StoreError> {
    let bytes = match store
        .meta_store()
        .get_meta(&state_key(anchor, representative)?)
    {
        Ok(bytes) => bytes,
        Err(StoreError::NotFound(_)) => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let snapshot = decode(&bytes, anchor, representative)?;
    snapshot
        .manifests
        .ok_or_else(|| invalid("checkpoint signer has no replay manifests"))
}

/// Apply and fsync a transition before producing any signature. LMDB's writer
/// serializes competing tasks using the same signer and prevents stale-state
/// replacement. Separate machines must never run copies of one signer key.
/// Include the saved choices returned by `read_replays` along with any new
/// candidate. A transition may retain a lock and a different prepared value.
pub fn apply(
    store: &LmdbStore,
    anchor: &CheckpointAnchor,
    private_key: &PrivateKey,
    command: Command<'_>,
    replays: &[&ReplayedCheckpoint],
    clock: Option<SigningClock>,
) -> Result<Vec<Outgoing>, StoreError> {
    apply_observed(
        store,
        anchor,
        private_key,
        command,
        replays,
        clock.map(ObservedSigningClock::new),
    )
}

pub(crate) fn apply_observed(
    store: &LmdbStore,
    anchor: &CheckpointAnchor,
    private_key: &PrivateKey,
    command: Command<'_>,
    replays: &[&ReplayedCheckpoint],
    clock: Option<ObservedSigningClock>,
) -> Result<Vec<Outgoing>, StoreError> {
    let inputs: Vec<_> = replays
        .iter()
        .map(|value| ReplayInput::Selection(value))
        .collect();
    apply_candidates_observed(store, anchor, private_key, command, &inputs, clock)
}

/// Every event kind uses the same durable signer record and payment vote fence.
/// Production signing of non-selection events requires the checkpoint release gate.
pub fn apply_candidates(
    store: &LmdbStore,
    anchor: &CheckpointAnchor,
    private_key: &PrivateKey,
    command: Command<'_>,
    replays: &[ReplayInput<'_>],
    clock: Option<SigningClock>,
) -> Result<Vec<Outgoing>, StoreError> {
    apply_candidates_observed(
        store,
        anchor,
        private_key,
        command,
        replays,
        clock.map(ObservedSigningClock::new),
    )
}

pub(crate) fn apply_candidates_observed(
    store: &LmdbStore,
    anchor: &CheckpointAnchor,
    private_key: &PrivateKey,
    command: Command<'_>,
    replays: &[ReplayInput<'_>],
    clock: Option<ObservedSigningClock>,
) -> Result<Vec<Outgoing>, StoreError> {
    if !cfg!(test)
        && !burst_verification::checkpoint_selection_enabled(anchor.network())
        && replays
            .iter()
            .any(|value| !matches!(value, ReplayInput::Selection(_)))
    {
        return Err(invalid(
            "mixed checkpoint signing is behind the release gate",
        ));
    }
    // Six retained state references plus one new candidate. Prune after the
    // transition so repeated rounds cannot accumulate unbounded manifests.
    if replays.is_empty() || replays.len() > 7 {
        return Err(invalid(
            "checkpoint transition needs one to seven replay candidates",
        ));
    }
    let mut manifests = Vec::new();
    for replay in replays {
        replay.validate_parent(anchor)?;
        let manifest = replay.manifest();
        if manifests.contains(&manifest) {
            return Err(invalid("duplicate checkpoint replay candidate"));
        }
        manifests.push(manifest);
    }
    transition(
        store,
        anchor,
        private_key,
        command,
        Some(manifests),
        |input| {
            let replay = replays
                .iter()
                .find(|replay| replay.checkpoint() == input.checkpoint)
                .ok_or_else(|| invalid("checkpoint commitments differ from confirmed replay"))?;
            replay.validate(anchor, input, clock.and_then(ObservedSigningClock::current))
        },
        |batch, state| {
            let representative = derive_address(&public_from_private(private_key));
            // Learning or retransmitting an authenticated certificate creates
            // no local signature. A minority payment promise must not prevent
            // an observer from accepting the old roster's decision.
            if state.decision().is_some() {
                if batch.payment_epoch()? == anchor.hash() {
                    batch.seal_consensus_after_certificate(&representative, &anchor.hash())?;
                }
                return Ok(());
            }
            let retained = state.retained_values();
            for replay in replays {
                if !retained.contains(&replay.checkpoint().hash()) {
                    continue;
                }
                let (epoch, hashes) = match replay {
                    ReplayInput::Selection(selection) => {
                        (selection.payment_epoch(), selection.history_hashes()?)
                    }
                    ReplayInput::ChallengeOutcome(outcome) => {
                        (outcome.state().payment_epoch(), outcome.history_hashes())
                    }
                    ReplayInput::Roster(roster) => {
                        (roster.parent().hash(), roster.history_hashes())
                    }
                    ReplayInput::Activation(activation) => {
                        (activation.parent().hash(), activation.history_hashes())
                    }
                    ReplayInput::Deadline(deadline) => {
                        (deadline.parent().hash(), deadline.history_hashes())
                    }
                    ReplayInput::Governance(governance) => {
                        (governance.parent().hash(), governance.history_hashes())
                    }
                };
                batch.seal_consensus_votes(&representative, &epoch, &anchor.hash(), hashes)?;
            }
            Ok(())
        },
    )
}

#[cfg(test)]
fn apply_with_validator(
    store: &LmdbStore,
    anchor: &CheckpointAnchor,
    private_key: &PrivateKey,
    command: Command<'_>,
    validate: impl FnMut(Validation<'_>) -> Result<(), StoreError>,
) -> Result<Vec<Outgoing>, StoreError> {
    transition(
        store,
        anchor,
        private_key,
        command,
        None,
        validate,
        |_, _| Ok(()),
    )
}

fn transition(
    store: &LmdbStore,
    anchor: &CheckpointAnchor,
    private_key: &PrivateKey,
    command: Command<'_>,
    mut manifests: Option<Vec<ReplayManifest>>,
    mut validate: impl FnMut(Validation<'_>) -> Result<(), StoreError>,
    mut seal: impl FnMut(
        &mut burst_store_lmdb::LmdbWriteTransaction<'_>,
        &RoundState,
    ) -> Result<(), StoreError>,
) -> Result<Vec<Outgoing>, StoreError> {
    let representative = derive_address(&public_from_private(private_key));
    let key = state_key(anchor, &representative)?;
    let mut batch = store.tx_begin_write()?;
    let old = batch.get_meta(&key)?;
    let mut state = match &old {
        Some(bytes) => {
            let snapshot = decode(bytes, anchor, &representative)?;
            match (&snapshot.manifests, &manifests) {
                (Some(saved), Some(replayed)) if saved.iter().all(|m| replayed.contains(m)) => (),
                (None, None) => (),
                _ => {
                    return Err(invalid(
                        "recovery requires every retained checkpoint manifest",
                    ))
                }
            }
            snapshot.state
        }
        None => {
            let legacy = format!(
                "checkpoint_vote/v1/{}/{}/{representative}/{}",
                anchor.network().as_str(),
                anchor.genesis_hash(),
                anchor.height() + 1
            );
            if batch.get_meta(&legacy)?.is_some() {
                return Err(invalid(
                    "legacy checkpoint reservation requires explicit recovery",
                ));
            }
            RoundState::new(anchor, representative.clone()).map_err(invalid)?
        }
    };
    let mut ballot: Option<Ballot> = None;
    let mut emit_proposal = false;
    let mut emit_decision = false;
    let resume = matches!(command, Command::Resume);
    match command {
        Command::Propose(fresh) => {
            let body = state.propose(anchor, fresh).map_err(invalid)?;
            validate(Validation {
                checkpoint: &body.checkpoint,
                proof: state.proposal_proof(),
                decided: false,
            })?;
            emit_proposal = true;
        }
        Command::Proposal(proposal) => {
            proposal.verify(anchor).map_err(invalid)?;
            validate(Validation {
                checkpoint: &proposal.body.checkpoint,
                proof: proposal.justification.as_ref(),
                decided: false,
            })?;
            ballot = Some(state.accept_proposal(anchor, proposal).map_err(invalid)?);
        }
        Command::Prepared(proposal, proof) => {
            proposal.verify(anchor).map_err(invalid)?;
            proof.verify(anchor).map_err(invalid)?;
            validate(Validation {
                checkpoint: &proposal.body.checkpoint,
                proof: Some(proof),
                decided: false,
            })?;
            ballot = state
                .observe_prepared(anchor, proposal, proof)
                .map_err(invalid)?;
        }
        Command::Decide(certificate) => {
            anchor.verify_certificate(&certificate).map_err(invalid)?;
            validate(Validation {
                checkpoint: &certificate.checkpoint,
                proof: None,
                decided: true,
            })?;
            state.decide(anchor, certificate).map_err(invalid)?;
            emit_decision = true;
        }
        Command::Timeout {
            round,
            step,
            evidence,
        } => {
            ballot = state
                .timeout(anchor, round, step, evidence)
                .map_err(invalid)?
        }
        Command::CatchUp { round, evidence } => state
            .catch_up_round(anchor, round, evidence)
            .map_err(invalid)?,
        Command::Resume => (),
    }
    state.validate(anchor, &representative).map_err(invalid)?;
    if let Some(manifests) = &mut manifests {
        let retained = state.retained_values();
        manifests.retain(|manifest| retained.contains(&manifest.checkpoint().hash()));
    }
    let mut snapshot = Snapshot {
        version: 3,
        checksum: [0; 32],
        state,
        manifests,
    };
    snapshot.validate_manifests(anchor)?;
    snapshot.checksum = snapshot.checksum()?;
    let bytes = bincode::serialize(&snapshot).map_err(invalid)?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(invalid("checkpoint state exceeds limit"));
    }
    seal(&mut batch, &snapshot.state)?;
    batch.put_meta(&key, &bytes)?;
    batch.commit()?;
    store.force_sync().map_err(invalid)?;

    let state = &snapshot.state;
    let sign = |payload: &[u8]| CheckpointSignature {
        representative: representative.clone(),
        signature: sign_message(payload, private_key),
    };
    let mut outgoing = Vec::new();
    if emit_decision || (resume && state.decision().is_some()) {
        if let Some(certificate) = state.decision() {
            outgoing.push(Outgoing::Decision(certificate.clone()));
        }
        return Ok(outgoing);
    }
    if emit_proposal || resume {
        if let Some(body) = state.proposal() {
            outgoing.push(Outgoing::Proposal(Box::new(Proposal {
                body: body.clone(),
                signer: sign(&body.signing_payload()),
                justification: state.proposal_proof().cloned(),
            })));
        }
    }
    if resume {
        for ballot in [state.prevote(), state.precommit()].into_iter().flatten() {
            outgoing.push(Outgoing::Vote(SignedBallot {
                ballot: ballot.clone(),
                signer: sign(&ballot.signing_payload()),
            }));
        }
    } else if let Some(ballot) = ballot {
        let signer = sign(&ballot.signing_payload());
        outgoing.push(Outgoing::Vote(SignedBallot { ballot, signer }));
    }
    Ok(outgoing)
}

#[cfg(test)]
mod tests {
    use super::apply_with_validator as apply;
    use super::*;
    use burst_consensus::checkpoint::{HumanDelegation, HumanRoster};
    use burst_consensus::checkpoint_round::{Phase, ProposalBody};
    use burst_crypto::keypair_from_seed;
    use burst_types::{BlockHash, KeyPair, NetworkId};

    #[cfg(unix)]
    mod process_crash;

    fn transition(
        store: &LmdbStore,
        anchor: &CheckpointAnchor,
        private_key: &PrivateKey,
        command: Command<'_>,
        manifests: Option<Vec<ReplayManifest>>,
        validate: impl FnMut(Validation<'_>) -> Result<(), StoreError>,
    ) -> Result<Vec<Outgoing>, StoreError> {
        super::transition(
            store,
            anchor,
            private_key,
            command,
            manifests,
            validate,
            |_, _| Ok(()),
        )
    }

    fn keys() -> Vec<KeyPair> {
        (1..=4).map(|n| keypair_from_seed(&[n; 32])).collect()
    }
    fn anchor() -> CheckpointAnchor {
        let roster = HumanRoster::new(
            keys()
                .iter()
                .map(|key| {
                    let address = derive_address(&key.public);
                    HumanDelegation {
                        human: address.clone(),
                        representative: address,
                    }
                })
                .collect(),
        )
        .unwrap();
        CheckpointAnchor::genesis(NetworkId::Dev, BlockHash::new([9; 32]), 100, roster, 6700)
            .unwrap()
    }
    fn checkpoint(anchor: &CheckpointAnchor, value: u8) -> Checkpoint {
        Checkpoint {
            network: NetworkId::Dev,
            genesis: anchor.genesis_hash(),
            parent: anchor.hash(),
            height: 1,
            cutoff: 101,
            ledger_root: BlockHash::new([value; 32]),
            params_hash: BlockHash::new([2; 32]),
            next_roster: anchor.roster().commitment(),
            selection_root: BlockHash::new([3; 32]),
            next_quorum_bps: 6700,
        }
    }
    fn sign(key: &KeyPair, payload: &[u8]) -> CheckpointSignature {
        CheckpointSignature {
            representative: derive_address(&key.public),
            signature: sign_message(payload, &key.private),
        }
    }
    fn proposal(anchor: &CheckpointAnchor, value: u8, round: u64) -> Proposal {
        let body = ProposalBody {
            checkpoint: checkpoint(anchor, value),
            round,
            valid_round: None,
        };
        let key = keys()
            .into_iter()
            .find(|key| derive_address(&key.public) == *anchor.proposer(round))
            .unwrap();
        Proposal {
            signer: sign(&key, &body.signing_payload()),
            body,
            justification: None,
        }
    }
    fn vote(output: Vec<Outgoing>) -> SignedBallot {
        assert_eq!(output.len(), 1);
        match output.into_iter().next().unwrap() {
            Outgoing::Vote(vote) => vote,
            _ => panic!("expected ballot"),
        }
    }
    fn open(path: &std::path::Path) -> LmdbStore {
        LmdbStore::open(path, 32, 32 * 1024 * 1024).unwrap()
    }
    fn fixture_valid(input: Validation<'_>) -> Result<(), StoreError> {
        if input.checkpoint.ledger_root != BlockHash::new([10; 32]) {
            return Err(invalid("fixture replay disagrees with proposed ledger"));
        }
        Ok(())
    }

    #[test]
    fn streamed_authority_preserves_decisions_and_unfinished_rounds() {
        let anchor = anchor();
        let keys = keys();
        let checkpoint = checkpoint(&anchor, 10);
        let ballot = Ballot::precommit(&checkpoint, 0);
        let certificate = CheckpointCertificate {
            checkpoint,
            round: 0,
            signatures: keys[..3]
                .iter()
                .map(|key| sign(key, &ballot.signing_payload()))
                .collect(),
        };
        let next = anchor
            .advance(&certificate, anchor.roster().clone())
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let store = open(directory.path());
        apply(
            &store,
            &anchor,
            &keys[0].private,
            Command::Decide(certificate),
            fixture_valid,
        )
        .unwrap();
        {
            let batch = store.tx_begin_write().unwrap();
            let before = batch.checkpoint_metadata(b"").unwrap();
            assert!(reconcile_snapshot_journals(&batch, [Ok(anchor.clone())]).is_err());
            reconcile_recovery_journals(&batch, [Ok(anchor.clone())]).unwrap();
            assert_eq!(batch.checkpoint_metadata(b"").unwrap(), before);
        }
        apply(
            &store,
            &next,
            &keys[0].private,
            Command::Resume,
            fixture_valid,
        )
        .unwrap();
        let batch = store.tx_begin_write().unwrap();
        let before = batch.checkpoint_metadata(b"").unwrap();
        assert!(reconcile_snapshot_journals(&batch, std::iter::once(Ok(anchor.clone()))).is_err());
        assert!(reconcile_recovery_journals(&batch, [Ok(anchor.clone())]).is_err());
        reconcile_snapshot_journals(&batch, [anchor.clone(), next.clone()].into_iter().map(Ok))
            .unwrap();
        for at in [
            "journal:round-index",
            "journal:round-authority",
            "journal:round-signer",
        ] {
            let reached = std::cell::Cell::new(false);
            assert!(reconcile_recovery_journals_view_with_poll(
                &batch.read_view(),
                [Ok(anchor.clone()), Ok(next.clone())],
                &|stage| {
                    if stage == at {
                        reached.set(true);
                        return Err(StoreError::Backend("round checkpoint sentinel".into()));
                    }
                    Ok(())
                }
            )
            .unwrap_err()
            .to_string()
            .contains("round checkpoint sentinel"));
            assert!(reached.get(), "checkpoint not reached: {at}");
        }
        assert!(reconcile_snapshot_journals(
            &batch,
            [anchor.clone(), anchor.clone(), next.clone()]
                .into_iter()
                .map(Ok)
        )
        .is_err());
        let stream = [
            Ok(anchor),
            Ok(next),
            Err(invalid("authority page unavailable")),
        ];
        assert!(reconcile_snapshot_journals(&batch, stream)
            .unwrap_err()
            .to_string()
            .contains("authority page unavailable"));
        assert_eq!(batch.checkpoint_metadata(b"").unwrap(), before);
    }

    #[test]
    fn streamed_authority_counts_delegated_representative_once() {
        let keys = keys();
        let representative = derive_address(&keys[0].public);
        let roster = HumanRoster::new(
            keys.iter()
                .map(|key| HumanDelegation {
                    human: derive_address(&key.public),
                    representative: representative.clone(),
                })
                .collect(),
        )
        .unwrap();
        let anchor =
            CheckpointAnchor::genesis(NetworkId::Dev, BlockHash::new([9; 32]), 100, roster, 6700)
                .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let store = open(directory.path());
        apply(
            &store,
            &anchor,
            &keys[0].private,
            Command::Resume,
            fixture_valid,
        )
        .unwrap();
        let batch = store.tx_begin_write().unwrap();
        let before = batch.checkpoint_metadata(b"").unwrap();
        assert!(!before.is_empty());
        reconcile_snapshot_journals(&batch, [Ok(anchor)]).unwrap();
        assert_eq!(batch.checkpoint_metadata(b"").unwrap(), before);
    }

    #[test]
    fn streamed_authority_rejects_uncovered_round_keys() {
        let anchor = anchor();
        let outsider = derive_address(&keypair_from_seed(&[99; 32]).public);
        let member = derive_address(&keys()[0].public);
        for (representative, height) in [(&outsider, "1"), (&member, "2"), (&member, "01")] {
            let directory = tempfile::tempdir().unwrap();
            let store = open(directory.path());
            let key = format!(
                "checkpoint_round/v1/{}/{}/{representative}/{height}",
                anchor.network().as_str(),
                anchor.hash()
            );
            let mut batch = store.tx_begin_write().unwrap();
            batch.put_meta(&key, &[1, 2, 3]).unwrap();
            let before = batch.checkpoint_metadata(b"").unwrap();
            assert!(reconcile_snapshot_journals(&batch, [Ok(anchor.clone())]).is_err());
            assert_eq!(batch.checkpoint_metadata(b"").unwrap(), before);
        }
    }

    #[test]
    fn streamed_authority_rejects_broken_input_without_local_journals() {
        let directory = tempfile::tempdir().unwrap();
        let store = open(directory.path());
        let batch = store.tx_begin_write().unwrap();
        let before = batch.checkpoint_metadata(b"").unwrap();
        assert!(reconcile_snapshot_journals(&batch, std::iter::empty()).is_err());
        reconcile_snapshot_journals(&batch, std::iter::once(Ok(anchor()))).unwrap();
        assert!(reconcile_snapshot_journals(
            &batch,
            [Ok(anchor()), Err(invalid("missing authority page"))]
        )
        .is_err());
        assert!(reconcile_snapshot_journals(&batch, [Ok(anchor()), Ok(anchor())]).is_err());
        assert_eq!(batch.checkpoint_metadata(b"").unwrap(), before);
    }

    #[test]
    fn manifests_follow_locks_and_prepared_values_and_prune_only_unused_choices() {
        let anchor = anchor();
        let keys = keys();
        let key = keys
            .iter()
            .find(|key| derive_address(&key.public) == *anchor.proposer(2))
            .unwrap();
        let representative = derive_address(&key.public);
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let a = proposal(&anchor, 10, 0);
        let b = proposal(&anchor, 11, 1);
        let manifest = |p: &Proposal| {
            ReplayManifest::Selection(CheckpointManifest {
                checkpoint: p.body.checkpoint.clone(),
                heads: Vec::new(),
                target: representative.clone(),
            })
        };
        let ma = manifest(&a);
        let mb = manifest(&b);
        let quorum = |p: &Proposal| {
            let ballot = Ballot {
                network: anchor.network(),
                genesis: anchor.genesis_hash(),
                parent: anchor.hash(),
                height: 1,
                round: p.body.round,
                phase: Phase::Prevote,
                value: Some(p.body.checkpoint.hash()),
            };
            let signatures = keys[..3]
                .iter()
                .map(|key| sign(key, &ballot.signing_payload()))
                .collect();
            Quorum { ballot, signatures }
        };
        transition(
            &store,
            &anchor,
            &key.private,
            Command::Proposal(&a),
            Some(vec![ma.clone()]),
            |_| Ok(()),
        )
        .unwrap();
        transition(
            &store,
            &anchor,
            &key.private,
            Command::Prepared(&a, &quorum(&a)),
            Some(vec![ma.clone()]),
            |_| Ok(()),
        )
        .unwrap();
        let mut ballot = quorum(&a).ballot;
        ballot.round = 2;
        ballot.value = None;
        let evidence: Vec<_> = keys[..2]
            .iter()
            .map(|key| SignedBallot {
                ballot: ballot.clone(),
                signer: sign(key, &ballot.signing_payload()),
            })
            .collect();
        transition(
            &store,
            &anchor,
            &key.private,
            Command::CatchUp {
                round: 2,
                evidence: &evidence,
            },
            Some(vec![ma.clone()]),
            |_| Ok(()),
        )
        .unwrap();
        transition(
            &store,
            &anchor,
            &key.private,
            Command::Prepared(&b, &quorum(&b)),
            Some(vec![ma.clone(), mb.clone()]),
            |_| Ok(()),
        )
        .unwrap();
        drop(store);
        let store = open(dir.path());
        let name = state_key(&anchor, &representative).unwrap();
        let saved = store.meta_store().get_meta(&name).unwrap();
        let snapshot = decode(&saved, &anchor, &representative).unwrap();
        assert_eq!(snapshot.manifests.as_ref().unwrap().len(), 2);
        retention::visit_round(
            &name,
            &saved,
            anchor.network(),
            anchor.genesis_hash(),
            |row| {
                assert_eq!(row.key, name);
                assert_eq!(row.height, 1);
                assert_eq!(row.representative, representative.as_str());
                assert_eq!(row.state, &snapshot.state);
                assert_eq!(row.manifests, snapshot.manifests.as_deref());
                Ok(())
            },
        )
        .unwrap();
        let mut missing = Snapshot {
            version: 3,
            state: snapshot.state.clone(),
            manifests: None,
            checksum: [0; 32],
        };
        missing.checksum = missing.checksum().unwrap();
        let bytes = bincode::serialize(&missing).unwrap();
        assert!(
            decode(&bytes, &anchor, &representative).is_ok(),
            "legacy recovery remains supported"
        );
        assert!(retention::visit_round(
            &name,
            &bytes,
            anchor.network(),
            anchor.genesis_hash(),
            |_| { panic!("missing manifests cannot complete retention") }
        )
        .is_err());

        for incomplete in [vec![ma.clone()], vec![mb.clone()]] {
            assert!(transition(
                &store,
                &anchor,
                &key.private,
                Command::Resume,
                Some(incomplete),
                |_| Ok(())
            )
            .is_err());
            assert_eq!(store.meta_store().get_meta(&name).unwrap(), saved);
        }
        let out = transition(
            &store,
            &anchor,
            &key.private,
            Command::Propose(a.body.checkpoint.clone()),
            Some(vec![ma.clone(), mb.clone()]),
            |_| Ok(()),
        )
        .unwrap();
        let Outgoing::Proposal(reproposed) = &out[0] else {
            panic!("expected prepared proposal")
        };
        assert_eq!(reproposed.body.checkpoint, b.body.checkpoint);
        let both = Some(vec![ma, mb.clone()]);
        transition(
            &store,
            &anchor,
            &key.private,
            Command::Proposal(reproposed),
            both.clone(),
            |_| Ok(()),
        )
        .unwrap();
        transition(
            &store,
            &anchor,
            &key.private,
            Command::Prepared(reproposed, &quorum(reproposed)),
            both,
            |_| Ok(()),
        )
        .unwrap();
        let snapshot = decode(
            &store.meta_store().get_meta(&name).unwrap(),
            &anchor,
            &representative,
        )
        .unwrap();
        assert_eq!(snapshot.manifests.unwrap(), vec![mb]);
    }

    #[test]
    fn manifest_checksum_and_completeness_are_checked_before_recovery() {
        let anchor = anchor();
        let keys = keys();
        let key = &keys[0];
        let representative = derive_address(&key.public);
        let p = proposal(&anchor, 10, 0);
        let mut state = RoundState::new(&anchor, representative.clone()).unwrap();
        state.accept_proposal(&anchor, &p).unwrap();
        let manifest = CheckpointManifest {
            checkpoint: p.body.checkpoint,
            heads: Vec::new(),
            target: representative.clone(),
        };
        let mut snapshot = Snapshot {
            version: 3,
            state,
            manifests: Some(vec![ReplayManifest::Selection(manifest)]),
            checksum: [0; 32],
        };
        snapshot.checksum = snapshot.checksum().unwrap();
        let encode = |snapshot: &Snapshot| bincode::serialize(snapshot).unwrap();
        assert!(decode(&encode(&snapshot), &anchor, &representative).is_ok());
        let ReplayManifest::Selection(manifest) = &mut snapshot.manifests.as_mut().unwrap()[0]
        else {
            panic!("selection fixture")
        };
        manifest.target = derive_address(&keys[1].public);
        assert!(decode(&encode(&snapshot), &anchor, &representative).is_err());
        snapshot.manifests = Some(Vec::new());
        snapshot.checksum = snapshot.checksum().unwrap();
        assert!(decode(&encode(&snapshot), &anchor, &representative).is_err());
        snapshot.version = 1;
        assert!(decode(&encode(&snapshot), &anchor, &representative).is_err());
    }

    #[test]
    fn retained_rounds_preserve_all_manifest_kinds_and_reject_uncovered_choices() {
        let anchor = anchor();
        let representative = derive_address(&keys()[0].public);
        let p = proposal(&anchor, 10, 0);
        let mut state = RoundState::new(&anchor, representative.clone()).unwrap();
        state.accept_proposal(&anchor, &p).unwrap();
        let checkpoint = p.body.checkpoint;
        let heads = vec![crate::confirmed_history::HistoryHead {
            account: representative.clone(),
            height: 7,
            hash: BlockHash::new([42; 32]),
        }];
        let variants = [
            ReplayManifest::Selection(CheckpointManifest {
                checkpoint: checkpoint.clone(),
                heads: heads.clone(),
                target: representative.clone(),
            }),
            ReplayManifest::ChallengeOutcome(crate::checkpoint_outcome::ChallengeOutcomeManifest {
                checkpoint: checkpoint.clone(),
                heads: heads.clone(),
                target: representative.clone(),
            }),
            ReplayManifest::Roster(crate::checkpoint_roster::RosterManifest {
                checkpoint: checkpoint.clone(),
                heads: heads.clone(),
            }),
            ReplayManifest::Activation(crate::checkpoint_activation::ActivationManifest {
                checkpoint: checkpoint.clone(),
                heads: heads.clone(),
                activation: crate::checkpoint_activation::ActivationProof {
                    selection: BlockHash::new([43; 32]),
                    value: [44; 32],
                    proof: vec![45, 46],
                    round: 47,
                },
            }),
            ReplayManifest::Deadline(crate::checkpoint_deadline::DeadlineManifest {
                checkpoint: checkpoint.clone(),
                heads: heads.clone(),
            }),
            ReplayManifest::Governance(crate::checkpoint_governance::GovernanceManifest {
                checkpoint: checkpoint.clone(),
                heads,
                proposals: vec![burst_types::TxHash::new([48; 32])],
            }),
        ];
        let name = state_key(&anchor, &representative).unwrap();
        for manifest in variants {
            let mut snapshot = Snapshot {
                version: 3,
                state: state.clone(),
                manifests: Some(vec![manifest.clone()]),
                checksum: [0; 32],
            };
            snapshot.checksum = snapshot.checksum().unwrap();
            let bytes = bincode::serialize(&snapshot).unwrap();
            retention::visit_round(
                &name,
                &bytes,
                anchor.network(),
                anchor.genesis_hash(),
                |row| {
                    assert_eq!(row.state, &state);
                    assert_eq!(row.manifests, Some(&[manifest.clone()][..]));
                    Ok(())
                },
            )
            .unwrap();
            for key in [
                name.replace("/v1/", "/v2/"),
                format!("{}/01", name.rsplit_once('/').unwrap().0),
                name.replace("/dev/", "/live/"),
            ] {
                assert!(retention::visit_round(
                    &key,
                    &bytes,
                    anchor.network(),
                    anchor.genesis_hash(),
                    |_| Ok(())
                )
                .is_err());
            }
            let mut corrupted = bytes.clone();
            *corrupted.last_mut().unwrap() ^= 1;
            assert!(retention::visit_round(
                &name,
                &corrupted,
                anchor.network(),
                anchor.genesis_hash(),
                |_| Ok(())
            )
            .is_err());
            snapshot.manifests = Some(Vec::new());
            snapshot.checksum = snapshot.checksum().unwrap();
            assert!(retention::visit_round(
                &name,
                &bincode::serialize(&snapshot).unwrap(),
                anchor.network(),
                anchor.genesis_hash(),
                |_| Ok(())
            )
            .is_err());
        }
    }

    #[test]
    fn version_two_journal_keeps_its_precommit_lock_when_upgraded() {
        #[derive(Serialize)]
        struct Legacy {
            version: u8,
            state: RoundState,
            manifests: Option<Vec<CheckpointManifest>>,
            checksum: [u8; 32],
        }
        let anchor = anchor();
        let keys = keys();
        let representative = derive_address(&keys[0].public);
        let proposal = proposal(&anchor, 10, 0);
        let mut state = RoundState::new(&anchor, representative.clone()).unwrap();
        let prevote = state.accept_proposal(&anchor, &proposal).unwrap();
        let proof = Quorum {
            signatures: keys[..3]
                .iter()
                .map(|key| sign(key, &prevote.signing_payload()))
                .collect(),
            ballot: prevote,
        };
        state.observe_prepared(&anchor, &proposal, &proof).unwrap();
        let manifest = CheckpointManifest {
            checkpoint: proposal.body.checkpoint.clone(),
            heads: Vec::new(),
            target: representative.clone(),
        };
        let mut legacy = Legacy {
            version: 2,
            state: state.clone(),
            manifests: Some(vec![manifest.clone()]),
            checksum: [0; 32],
        };
        legacy.checksum =
            blake2b_256(&bincode::serialize(&(&legacy.state, &legacy.manifests)).unwrap());
        let bytes = bincode::serialize(&legacy).unwrap();
        let retained_key = state_key(&anchor, &representative).unwrap();
        retention::visit_round(
            &retained_key,
            &bytes,
            anchor.network(),
            anchor.genesis_hash(),
            |row| {
                assert_eq!(row.state, &state);
                assert_eq!(
                    row.manifests,
                    Some(&[ReplayManifest::Selection(manifest.clone())][..])
                );
                Ok(())
            },
        )
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let name = state_key(&anchor, &representative).unwrap();
        store.meta_store().put_meta(&name, &bytes).unwrap();
        drop(store);
        let store = open(dir.path());
        assert_eq!(
            read_state(&store, &anchor, &representative).unwrap(),
            Some(state.clone())
        );
        assert_eq!(
            store.meta_store().get_meta(&name).unwrap(),
            bytes,
            "reading must not rewrite a signer journal"
        );
        let output = transition(
            &store,
            &anchor,
            &keys[0].private,
            Command::Resume,
            Some(vec![ReplayManifest::Selection(manifest)]),
            |_| Ok(()),
        )
        .unwrap();
        assert!(output.iter().any(|output| matches!(output, Outgoing::Vote(vote)
            if vote.ballot.phase == Phase::Precommit && vote.ballot.value == Some(proposal.body.checkpoint.hash()))));
        let upgraded = store.meta_store().get_meta(&name).unwrap();
        assert_eq!(upgraded[0], 3);
        assert_eq!(
            decode(&upgraded, &anchor, &representative).unwrap().state,
            state
        );
        let mut corrupt = bytes;
        *corrupt.last_mut().unwrap() ^= 1;
        store.meta_store().put_meta(&name, &corrupt).unwrap();
        assert!(read_state(&store, &anchor, &representative).is_err());
        assert_eq!(store.meta_store().get_meta(&name).unwrap(), corrupt);
    }

    #[test]
    fn four_durable_signers_decide_and_resume_the_same_certificate_after_reopen() {
        let anchor = anchor();
        let keys = keys();
        let dirs: Vec<_> = (0..4).map(|_| tempfile::tempdir().unwrap()).collect();
        let stores: Vec<_> = dirs.iter().map(|d| open(d.path())).collect();
        let proposer = keys
            .iter()
            .position(|key| derive_address(&key.public) == *anchor.proposer(0))
            .unwrap();
        let output = apply(
            &stores[proposer],
            &anchor,
            &keys[proposer].private,
            Command::Propose(checkpoint(&anchor, 10)),
            fixture_valid,
        )
        .unwrap();
        let Outgoing::Proposal(p) = &output[0] else {
            panic!("expected proposal")
        };
        p.verify(&anchor).unwrap();
        let mut prepares = Vec::new();
        for (store, key) in stores.iter().zip(&keys) {
            prepares.push(vote(
                apply(
                    store,
                    &anchor,
                    &key.private,
                    Command::Proposal(p),
                    fixture_valid,
                )
                .unwrap(),
            ));
        }
        let prepare = Quorum {
            ballot: prepares[0].ballot.clone(),
            signatures: prepares[..3].iter().map(|v| v.signer.clone()).collect(),
        };
        let mut commits = Vec::new();
        for (store, key) in stores.iter().zip(&keys) {
            commits.push(vote(
                apply(
                    store,
                    &anchor,
                    &key.private,
                    Command::Prepared(p, &prepare),
                    fixture_valid,
                )
                .unwrap(),
            ));
        }
        let cert = CheckpointCertificate {
            checkpoint: p.body.checkpoint.clone(),
            round: 0,
            signatures: commits[..3].iter().map(|v| v.signer.clone()).collect(),
        };
        anchor.verify_certificate(&cert).unwrap();
        for (store, key) in stores.iter().zip(&keys) {
            assert_eq!(
                apply(
                    store,
                    &anchor,
                    &key.private,
                    Command::Decide(cert.clone()),
                    fixture_valid
                )
                .unwrap(),
                vec![Outgoing::Decision(cert.clone())]
            );
        }
        drop(stores);
        for (dir, key) in dirs.iter().zip(&keys) {
            let store = open(dir.path());
            assert_eq!(
                apply(&store, &anchor, &key.private, Command::Resume, |_| panic!(
                    "resume reuses durable validated decision"
                ))
                .unwrap(),
                vec![Outgoing::Decision(cert.clone())]
            );
            assert!(apply(
                &store,
                &anchor,
                &key.private,
                Command::Timeout {
                    round: 0,
                    step: Step::Precommit,
                    evidence: &commits
                },
                fixture_valid
            )
            .is_err());
        }
    }

    #[test]
    fn a_reopened_lock_rejects_conflicting_proposal_after_a_timeout() {
        let anchor = anchor();
        let key = keys().remove(0);
        let address = derive_address(&key.public);
        let dir = tempfile::tempdir().unwrap();
        let p = proposal(&anchor, 10, 0);
        let mut own;
        {
            let store = open(dir.path());
            own = vote(
                apply(
                    &store,
                    &anchor,
                    &key.private,
                    Command::Proposal(&p),
                    fixture_valid,
                )
                .unwrap(),
            );
            let proof = Quorum {
                ballot: own.ballot.clone(),
                signatures: keys()[..3]
                    .iter()
                    .map(|k| sign(k, &own.ballot.signing_payload()))
                    .collect(),
            };
            own = vote(
                apply(
                    &store,
                    &anchor,
                    &key.private,
                    Command::Prepared(&p, &proof),
                    fixture_valid,
                )
                .unwrap(),
            );
        }
        let store = open(dir.path());
        let resumed = apply(
            &store,
            &anchor,
            &key.private,
            Command::Resume,
            fixture_valid,
        )
        .unwrap();
        assert!(resumed.contains(&Outgoing::Vote(own.clone())));
        let commits: Vec<_> = keys()[..3]
            .iter()
            .map(|k| SignedBallot {
                ballot: own.ballot.clone(),
                signer: sign(k, &own.ballot.signing_payload()),
            })
            .collect();
        apply(
            &store,
            &anchor,
            &key.private,
            Command::Timeout {
                round: 0,
                step: Step::Precommit,
                evidence: &commits,
            },
            fixture_valid,
        )
        .unwrap();
        let before = read_state(&store, &anchor, &address).unwrap().unwrap();
        assert_eq!(before.round(), 1);
        drop(store);
        let store = open(dir.path());
        // This predicate accepts both fixture values. The consensus lock still
        // requires nil for a conflicting proposal without a higher prepare QC.
        let conflicting = proposal(&anchor, 11, 1);
        let ballot = vote(
            apply(
                &store,
                &anchor,
                &key.private,
                Command::Proposal(&conflicting),
                |_| Ok(()),
            )
            .unwrap(),
        );
        assert_eq!(ballot.ballot.value, None);
        assert_eq!(ballot.ballot.phase, Phase::Prevote);
    }

    #[test]
    fn replay_failure_aborts_state_and_releases_no_signature() {
        let anchor = anchor();
        let key = keys().remove(0);
        let address = derive_address(&key.public);
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        assert!(apply(
            &store,
            &anchor,
            &key.private,
            Command::Proposal(&proposal(&anchor, 11, 0)),
            fixture_valid
        )
        .is_err());
        assert!(read_state(&store, &anchor, &address).unwrap().is_none());
        apply(
            &store,
            &anchor,
            &key.private,
            Command::Proposal(&proposal(&anchor, 10, 0)),
            fixture_valid,
        )
        .unwrap();
        let before = read_state(&store, &anchor, &address).unwrap();
        let mut cp = checkpoint(&anchor, 11);
        let ballot = Ballot::precommit(&cp, 5);
        let cert = CheckpointCertificate {
            checkpoint: cp.clone(),
            round: 5,
            signatures: keys()[..3]
                .iter()
                .map(|k| sign(k, &ballot.signing_payload()))
                .collect(),
        };
        assert!(apply(
            &store,
            &anchor,
            &key.private,
            Command::Decide(cert),
            fixture_valid
        )
        .is_err());
        assert_eq!(read_state(&store, &anchor, &address).unwrap(), before);
        cp.parent = BlockHash::new([8; 32]);
        assert!(anchor.validate_successor(&cp).is_err());
    }

    #[test]
    fn concurrent_conflicting_proposals_cannot_double_vote_one_signer() {
        let anchor = anchor();
        let key = keys().remove(0);
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let a = proposal(&anchor, 10, 0);
        let b = proposal(&anchor, 11, 0);
        let results = std::thread::scope(|scope| {
            let left = scope.spawn(|| {
                apply(&store, &anchor, &key.private, Command::Proposal(&a), |_| {
                    Ok(())
                })
            });
            let right = scope.spawn(|| {
                apply(&store, &anchor, &key.private, Command::Proposal(&b), |_| {
                    Ok(())
                })
            });
            [left.join().unwrap(), right.join().unwrap()]
        });
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        let committed = vote(results.into_iter().find_map(Result::ok).unwrap());
        assert_eq!(
            read_state(&store, &anchor, &derive_address(&key.public))
                .unwrap()
                .unwrap()
                .prevote(),
            Some(&committed.ballot)
        );
    }

    #[test]
    fn corrupt_and_legacy_signer_state_fail_closed_without_replacement() {
        let anchor = anchor();
        let key = keys().remove(0);
        let representative = derive_address(&key.public);
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let name = state_key(&anchor, &representative).unwrap();
        store.meta_store().put_meta(&name, &[1, 2, 3]).unwrap();
        assert!(apply(
            &store,
            &anchor,
            &key.private,
            Command::Resume,
            fixture_valid
        )
        .is_err());
        assert_eq!(store.meta_store().get_meta(&name).unwrap(), [1, 2, 3]);
        let other_dir = tempfile::tempdir().unwrap();
        let other_store = open(other_dir.path());
        other_store
            .meta_store()
            .reserve_checkpoint_vote(
                anchor.network(),
                &anchor.genesis_hash(),
                &representative,
                1,
                &checkpoint(&anchor, 10).hash(),
            )
            .unwrap();
        assert!(apply(
            &other_store,
            &anchor,
            &key.private,
            Command::Resume,
            fixture_valid
        )
        .is_err());
        assert!(read_state(&other_store, &anchor, &representative)
            .unwrap()
            .is_none());
    }
}
