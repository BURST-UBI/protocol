//! All checkpoint event types share one canonical height sequence.
use crate::checkpoint_store::record_storage;

use super::*;
use crate::checkpoint_activation::{
    ActivationManifest, ActivationProof, CertifiedActivation, PreparedActivation,
};
use crate::checkpoint_agreement::ReplayManifest;
use crate::checkpoint_deadline::{CertifiedDeadline, DeadlineManifest, PreparedDeadline};
use crate::checkpoint_governance::{CertifiedGovernance, GovernanceManifest, PreparedGovernance};
use crate::checkpoint_outcome::{
    CertifiedChallengeOutcome, ChallengeOutcomeManifest, PreparedChallengeOutcome,
};
use crate::checkpoint_roster::{CertifiedRoster, PreparedRoster, RosterManifest};
use crate::confirmed_replay::ConfirmedReplay;
use burst_consensus::checkpoint::HumanRoster;

pub enum CertifiedEvent {
    Selection(Box<CertifiedCheckpoint>),
    Outcome(Box<CertifiedChallengeOutcome>),
    Roster(Box<CertifiedRoster>),
    Activation(Box<CertifiedActivation>),
    Deadline(Box<CertifiedDeadline>),
    Governance(Box<CertifiedGovernance>),
}

pub(crate) fn record_matches(bytes: &[u8], event: &CertifiedEvent) -> Result<bool, StoreError> {
    let record = decode_record(bytes)?;
    Ok(record.certificate() == event.certificate()
        && record.heads() == event.heads()
        && record.manifest() == event.manifest())
}

/// Persist a replay-validated certificate, not the resulting live economic state.
pub(crate) fn accept_manifest(
    store: &LmdbStore,
    network: NetworkId,
    params: &ProtocolParams,
    certificate: CheckpointCertificate,
    manifest: &ReplayManifest,
) -> Result<(), StoreError> {
    if manifest.checkpoint() != &certificate.checkpoint || certificate.checkpoint.network != network
    {
        return Err(invalid("decision manifest or network mismatch"));
    }
    match manifest {
        ReplayManifest::Selection(value) => {
            CertifiedCheckpoint::accept(
                store,
                network,
                params,
                certificate,
                value.heads.clone(),
                value.target.clone(),
            )?;
        }
        ReplayManifest::ChallengeOutcome(value) => {
            require_lifecycle_enabled(network)?;
            PreparedChallengeOutcome::replay_manifest(store, params, value)?
                .certify(certificate)?
                .persist(store, network, params)?;
        }
        ReplayManifest::Roster(value) => {
            require_lifecycle_enabled(network)?;
            accept(
                store,
                network,
                params,
                ParsedRecord::Roster(RosterRecord {
                    version: 3,
                    certificate,
                    heads: value.heads.clone(),
                }),
            )?;
        }
        ReplayManifest::Activation(value) => {
            require_lifecycle_enabled(network)?;
            accept(
                store,
                network,
                params,
                ParsedRecord::Activation(ActivationRecord {
                    version: 4,
                    certificate,
                    heads: value.heads.clone(),
                    activation: value.activation.clone(),
                }),
            )?;
        }
        ReplayManifest::Deadline(value) => {
            require_lifecycle_enabled(network)?;
            accept(
                store,
                network,
                params,
                ParsedRecord::Deadline(DeadlineRecord {
                    version: 5,
                    certificate,
                    heads: value.heads.clone(),
                }),
            )?;
        }
        ReplayManifest::Governance(value) => {
            require_lifecycle_enabled(network)?;
            accept(
                store,
                network,
                params,
                ParsedRecord::Governance(GovernanceRecord {
                    version: 6,
                    certificate,
                    heads: value.heads.clone(),
                    proposals: value.proposals.clone(),
                }),
            )?;
        }
    }
    Ok(())
}

impl CertifiedEvent {
    pub(crate) fn certificate(&self) -> &CheckpointCertificate {
        match self {
            Self::Selection(value) => value.certificate(),
            Self::Outcome(value) => value.certificate(),
            Self::Roster(value) => value.certificate(),
            Self::Activation(value) => value.certificate(),
            Self::Deadline(value) => value.certificate(),
            Self::Governance(value) => value.certificate(),
        }
    }

    pub(crate) fn heads(&self) -> &[HistoryHead] {
        match self {
            Self::Selection(value) => value.heads(),
            Self::Outcome(value) => &value.manifest().heads,
            Self::Roster(value) => &value.manifest().heads,
            Self::Activation(value) => &value.manifest().heads,
            Self::Deadline(value) => &value.manifest().heads,
            Self::Governance(value) => &value.manifest().heads,
        }
    }

    pub(crate) fn parent(&self) -> &CheckpointAnchor {
        match self {
            Self::Selection(value) => value.parent(),
            Self::Outcome(value) => value.parent(),
            Self::Roster(value) => value.parent(),
            Self::Activation(value) => value.parent(),
            Self::Deadline(value) => value.parent(),
            Self::Governance(value) => value.parent(),
        }
    }

    pub(crate) fn next_roster(&self) -> &HumanRoster {
        match self {
            Self::Selection(value) => value.replayed().roster(),
            Self::Outcome(value) => value.next_roster(),
            Self::Roster(value) => value.next_roster(),
            Self::Activation(value) => value.next_roster(),
            Self::Deadline(value) => value.next_roster(),
            Self::Governance(value) => value.next_roster(),
        }
    }

    pub(crate) fn selection(&self) -> Option<&CertifiedCheckpoint> {
        match self {
            Self::Selection(value) => Some(value),
            Self::Outcome(_)
            | Self::Roster(_)
            | Self::Activation(_)
            | Self::Deadline(_)
            | Self::Governance(_) => None,
        }
    }

    pub(crate) fn manifest(&self) -> ReplayManifest {
        match self {
            Self::Selection(value) => ReplayManifest::Selection(value.replayed().manifest()),
            Self::Outcome(value) => ReplayManifest::ChallengeOutcome(value.manifest().clone()),
            Self::Roster(value) => ReplayManifest::Roster(value.manifest().clone()),
            Self::Activation(value) => ReplayManifest::Activation(value.manifest().clone()),
            Self::Deadline(value) => ReplayManifest::Deadline(value.manifest().clone()),
            Self::Governance(value) => ReplayManifest::Governance(value.manifest().clone()),
        }
    }

    fn encode(&self) -> Result<Vec<u8>, StoreError> {
        match self {
            Self::Selection(value) => super::encode(&value.record),
            Self::Outcome(value) => encode_outcome(&OutcomeRecord {
                version: 2,
                certificate: value.certificate().clone(),
                heads: value.manifest().heads.clone(),
                target: value.manifest().target.clone(),
            }),
            Self::Roster(value) => encode_roster(&RosterRecord {
                version: 3,
                certificate: value.certificate().clone(),
                heads: value.manifest().heads.clone(),
            }),
            Self::Activation(value) => encode_activation(&ActivationRecord {
                version: 4,
                certificate: value.certificate().clone(),
                heads: value.manifest().heads.clone(),
                activation: value.manifest().activation.clone(),
            }),
            Self::Deadline(value) => encode_deadline(&DeadlineRecord {
                version: 5,
                certificate: value.certificate().clone(),
                heads: value.manifest().heads.clone(),
            }),
            Self::Governance(value) => encode_governance(&GovernanceRecord {
                version: 6,
                certificate: value.certificate().clone(),
                heads: value.manifest().heads.clone(),
                proposals: value.manifest().proposals.clone(),
            }),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct OutcomeRecord {
    version: u8,
    certificate: CheckpointCertificate,
    heads: Vec<HistoryHead>,
    target: WalletAddress,
}

pub(super) enum ParsedRecord {
    Selection(Record),
    Outcome(OutcomeRecord),
    Roster(RosterRecord),
    Activation(ActivationRecord),
    Deadline(DeadlineRecord),
    Governance(GovernanceRecord),
}

#[derive(Serialize, Deserialize)]
pub(super) struct GovernanceRecord {
    version: u8,
    certificate: CheckpointCertificate,
    heads: Vec<HistoryHead>,
    proposals: Vec<burst_types::TxHash>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct DeadlineRecord {
    version: u8,
    certificate: CheckpointCertificate,
    heads: Vec<HistoryHead>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ActivationRecord {
    version: u8,
    certificate: CheckpointCertificate,
    heads: Vec<HistoryHead>,
    activation: ActivationProof,
}

#[derive(Serialize, Deserialize)]
pub(super) struct RosterRecord {
    version: u8,
    certificate: CheckpointCertificate,
    heads: Vec<HistoryHead>,
}

impl ParsedRecord {
    pub(super) fn selection_epoch(&self) -> Option<burst_types::BlockHash> {
        match self {
            Self::Selection(record) => Some(record.certificate.checkpoint.hash()),
            Self::Activation(record) => Some(record.activation.selection),
            _ => None,
        }
    }

    pub(super) fn certificate(&self) -> &CheckpointCertificate {
        match self {
            Self::Selection(value) => &value.certificate,
            Self::Outcome(value) => &value.certificate,
            Self::Roster(value) => &value.certificate,
            Self::Activation(value) => &value.certificate,
            Self::Deadline(value) => &value.certificate,
            Self::Governance(value) => &value.certificate,
        }
    }

    pub(super) fn heads(&self) -> &[HistoryHead] {
        match self {
            Self::Selection(value) => &value.heads,
            Self::Outcome(value) => &value.heads,
            Self::Roster(value) => &value.heads,
            Self::Activation(value) => &value.heads,
            Self::Deadline(value) => &value.heads,
            Self::Governance(value) => &value.heads,
        }
    }

    pub(super) fn manifest(&self) -> ReplayManifest {
        match self {
            Self::Selection(value) => ReplayManifest::Selection(CheckpointManifest {
                checkpoint: value.certificate.checkpoint.clone(),
                heads: value.heads.clone(),
                target: value.target.clone(),
            }),
            Self::Outcome(value) => ReplayManifest::ChallengeOutcome(ChallengeOutcomeManifest {
                checkpoint: value.certificate.checkpoint.clone(),
                heads: value.heads.clone(),
                target: value.target.clone(),
            }),
            Self::Roster(value) => ReplayManifest::Roster(RosterManifest {
                checkpoint: value.certificate.checkpoint.clone(),
                heads: value.heads.clone(),
            }),
            Self::Activation(value) => ReplayManifest::Activation(ActivationManifest {
                checkpoint: value.certificate.checkpoint.clone(),
                heads: value.heads.clone(),
                activation: value.activation.clone(),
            }),
            Self::Deadline(value) => ReplayManifest::Deadline(DeadlineManifest {
                checkpoint: value.certificate.checkpoint.clone(),
                heads: value.heads.clone(),
            }),
            Self::Governance(value) => ReplayManifest::Governance(GovernanceManifest {
                checkpoint: value.certificate.checkpoint.clone(),
                heads: value.heads.clone(),
                proposals: value.proposals.clone(),
            }),
        }
    }
}

pub(crate) fn require_lifecycle_enabled(network: NetworkId) -> Result<(), StoreError> {
    if !cfg!(test) && !burst_verification::checkpoint_selection_enabled(network) {
        return Err(invalid(
            "mixed checkpoint records are behind the release gate",
        ));
    }
    Ok(())
}

pub(super) fn decode_record(bytes: &[u8]) -> Result<ParsedRecord, StoreError> {
    if bytes.first() == Some(&1) {
        return super::decode(bytes).map(ParsedRecord::Selection);
    }
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(invalid("checkpoint record exceeds byte limit"));
    }
    if bytes.first() == Some(&6) {
        let record: GovernanceRecord = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .with_limit(MAX_RECORD_BYTES)
            .deserialize(bytes)
            .map_err(invalid)?;
        require_lifecycle_enabled(record.certificate.checkpoint.network)?;
        if record.certificate.checkpoint.height == 0 || record.proposals.is_empty() {
            return Err(invalid(
                "unsupported governance checkpoint height or empty ballot set",
            ));
        }
        return Ok(ParsedRecord::Governance(record));
    }
    if bytes.first() == Some(&4) {
        let record: ActivationRecord = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .with_limit(MAX_RECORD_BYTES)
            .deserialize(bytes)
            .map_err(invalid)?;
        require_lifecycle_enabled(record.certificate.checkpoint.network)?;
        if record.certificate.checkpoint.height < 2 {
            return Err(invalid("unsupported activation checkpoint height"));
        }
        record.activation.commitment()?;
        return Ok(ParsedRecord::Activation(record));
    }
    if bytes.first() == Some(&5) {
        let record: DeadlineRecord = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .with_limit(MAX_RECORD_BYTES)
            .deserialize(bytes)
            .map_err(invalid)?;
        require_lifecycle_enabled(record.certificate.checkpoint.network)?;
        if record.certificate.checkpoint.height == 0 {
            return Err(invalid("unsupported deadline checkpoint height"));
        }
        return Ok(ParsedRecord::Deadline(record));
    }
    if bytes.first() == Some(&3) {
        let record: RosterRecord = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .with_limit(MAX_RECORD_BYTES)
            .deserialize(bytes)
            .map_err(invalid)?;
        require_lifecycle_enabled(record.certificate.checkpoint.network)?;
        if record.certificate.checkpoint.height == 0 {
            return Err(invalid("unsupported roster checkpoint height"));
        }
        return Ok(ParsedRecord::Roster(record));
    }
    let record: OutcomeRecord = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .reject_trailing_bytes()
        .with_limit(MAX_RECORD_BYTES)
        .deserialize(bytes)
        .map_err(invalid)?;
    if record.version != 2 || record.certificate.checkpoint.height == 0 {
        return Err(invalid("unsupported checkpoint record version or height"));
    }
    require_lifecycle_enabled(record.certificate.checkpoint.network)?;
    Ok(ParsedRecord::Outcome(record))
}

fn encode_outcome(record: &OutcomeRecord) -> Result<Vec<u8>, StoreError> {
    if bincode::serialized_size(record).map_err(invalid)? > MAX_RECORD_BYTES {
        return Err(invalid("checkpoint record exceeds byte limit"));
    }
    bincode::serialize(record).map_err(invalid)
}

fn encode_roster(record: &RosterRecord) -> Result<Vec<u8>, StoreError> {
    if bincode::serialized_size(record).map_err(invalid)? > MAX_RECORD_BYTES {
        return Err(invalid("checkpoint record exceeds byte limit"));
    }
    bincode::serialize(record).map_err(invalid)
}

fn encode_activation(record: &ActivationRecord) -> Result<Vec<u8>, StoreError> {
    if bincode::serialized_size(record).map_err(invalid)? > MAX_RECORD_BYTES {
        return Err(invalid("checkpoint record exceeds byte limit"));
    }
    bincode::serialize(record).map_err(invalid)
}

pub(super) fn encode_governance_manifest(
    certificate: &CheckpointCertificate,
    manifest: &GovernanceManifest,
) -> Result<Vec<u8>, StoreError> {
    if certificate.checkpoint != manifest.checkpoint {
        return Err(invalid("governance manifest differs from certificate"));
    }
    encode_governance(&GovernanceRecord {
        version: 6,
        certificate: certificate.clone(),
        heads: manifest.heads.clone(),
        proposals: manifest.proposals.clone(),
    })
}

pub(super) fn encode_activation_manifest(
    certificate: &CheckpointCertificate,
    manifest: &ActivationManifest,
) -> Result<Vec<u8>, StoreError> {
    if certificate.checkpoint != manifest.checkpoint {
        return Err(invalid("activation manifest differs from certificate"));
    }
    encode_activation(&ActivationRecord {
        version: 4,
        certificate: certificate.clone(),
        heads: manifest.heads.clone(),
        activation: manifest.activation.clone(),
    })
}

pub(super) fn encode_roster_manifest(
    certificate: &CheckpointCertificate,
    manifest: &RosterManifest,
) -> Result<Vec<u8>, StoreError> {
    if certificate.checkpoint != manifest.checkpoint {
        return Err(invalid("roster manifest differs from certificate"));
    }
    encode_roster(&RosterRecord {
        version: 3,
        certificate: certificate.clone(),
        heads: manifest.heads.clone(),
    })
}

pub(super) fn encode_outcome_manifest(
    certificate: &CheckpointCertificate,
    manifest: &ChallengeOutcomeManifest,
) -> Result<Vec<u8>, StoreError> {
    if certificate.checkpoint != manifest.checkpoint {
        return Err(invalid("outcome manifest differs from certificate"));
    }
    encode_outcome(&OutcomeRecord {
        version: 2,
        certificate: certificate.clone(),
        heads: manifest.heads.clone(),
        target: manifest.target.clone(),
    })
}

pub(super) fn encode_deadline_manifest(
    certificate: &CheckpointCertificate,
    manifest: &DeadlineManifest,
) -> Result<Vec<u8>, StoreError> {
    if certificate.checkpoint != manifest.checkpoint {
        return Err(invalid("deadline manifest differs from certificate"));
    }
    encode_deadline(&DeadlineRecord {
        version: 5,
        certificate: certificate.clone(),
        heads: manifest.heads.clone(),
    })
}

fn encode_deadline(record: &DeadlineRecord) -> Result<Vec<u8>, StoreError> {
    if bincode::serialized_size(record).map_err(invalid)? > MAX_RECORD_BYTES {
        return Err(invalid("checkpoint record exceeds byte limit"));
    }
    bincode::serialize(record).map_err(invalid)
}

fn encode_governance(record: &GovernanceRecord) -> Result<Vec<u8>, StoreError> {
    if bincode::serialized_size(record).map_err(invalid)? > MAX_RECORD_BYTES {
        return Err(invalid("checkpoint record exceeds byte limit"));
    }
    bincode::serialize(record).map_err(invalid)
}

pub(crate) fn next_anchor(
    chain: &[CertifiedEvent],
    network: NetworkId,
    params: &ProtocolParams,
) -> Result<CheckpointAnchor, StoreError> {
    match chain.last() {
        Some(parent) => parent
            .parent()
            .advance(parent.certificate(), parent.next_roster().clone())
            .map_err(invalid),
        None => ReplayedCheckpoint::genesis_anchor(network, params),
    }
}

pub(super) fn selection_from_history(
    history: &ConfirmedHistory,
    anchor: &CheckpointAnchor,
    params: &ProtocolParams,
    target: &WalletAddress,
    ancestors: &[CertifiedEvent],
) -> Result<ReplayedCheckpoint, StoreError> {
    let replay = ConfirmedReplay::replay_with_events(history, anchor.network(), params, ancestors)
        .map_err(invalid)?;
    let selections: Vec<_> = ancestors
        .iter()
        .filter_map(CertifiedEvent::selection)
        .map(CertifiedCheckpoint::replayed)
        .collect();
    ReplayedCheckpoint::from_replay(history, anchor, params, target, replay, &selections)
}

fn validate(
    store: &LmdbStore,
    anchor: &CheckpointAnchor,
    params: &ProtocolParams,
    record: ParsedRecord,
    ancestors: &[CertifiedEvent],
) -> Result<CertifiedEvent, StoreError> {
    anchor
        .verify_certificate(record.certificate())
        .map_err(invalid)?;
    let history = ConfirmedHistory::read_certified(
        store,
        anchor.network(),
        anchor.genesis_hash(),
        record.certificate().checkpoint.cutoff,
        record.heads(),
        ancestors.last().map_or(&[][..], CertifiedEvent::heads),
    )?;
    replay_record(&history, anchor, params, record, ancestors)
}

/// The caller must verify the certificate against its parent before replay.
/// State is reconstructed from signed history, not peer account snapshots.
pub(super) fn replay_record(
    history: &ConfirmedHistory,
    anchor: &CheckpointAnchor,
    params: &ProtocolParams,
    record: ParsedRecord,
    ancestors: &[CertifiedEvent],
) -> Result<CertifiedEvent, StoreError> {
    if history.heads() != record.heads() {
        return Err(invalid("checkpoint heads are not canonical"));
    }
    match record {
        ParsedRecord::Selection(record) => {
            let replay =
                selection_from_history(history, anchor, params, &record.target, ancestors)?;
            if replay.checkpoint() != &record.certificate.checkpoint {
                return Err(invalid("selection certificate differs from mixed replay"));
            }
            if let Some(beacon) = &record.beacon {
                replay
                    .request()
                    .select(anchor, &record.certificate, beacon)
                    .map_err(invalid)?;
            }
            Ok(CertifiedEvent::Selection(Box::new(CertifiedCheckpoint {
                record,
                replay,
                parent: anchor.clone(),
            })))
        }
        ParsedRecord::Outcome(record) => {
            let replay =
                ConfirmedReplay::replay_with_events(history, anchor.network(), params, ancestors)
                    .map_err(invalid)?;
            let prepared = PreparedChallengeOutcome::from_replay(
                history,
                anchor,
                params,
                &record.target,
                replay,
            )?;
            Ok(CertifiedEvent::Outcome(Box::new(
                prepared.certify(record.certificate)?,
            )))
        }
        ParsedRecord::Roster(record) => {
            let replay =
                ConfirmedReplay::replay_with_events(history, anchor.network(), params, ancestors)
                    .map_err(invalid)?;
            let prepared = PreparedRoster::from_replay(history, anchor, params, replay)?;
            Ok(CertifiedEvent::Roster(Box::new(
                prepared.certify(record.certificate)?,
            )))
        }
        ParsedRecord::Activation(record) => {
            let selection = ancestors
                .iter()
                .filter_map(CertifiedEvent::selection)
                .find(|entry| entry.certificate().checkpoint.hash() == record.activation.selection)
                .ok_or_else(|| invalid("activation has no ancestor selection"))?;
            let state =
                ConfirmedReplay::replay_with_events(history, anchor.network(), params, ancestors)
                    .map_err(invalid)?;
            let prepared = PreparedActivation::from_replay(
                history,
                anchor,
                params,
                &record.activation,
                selection,
                state,
            )?;
            Ok(CertifiedEvent::Activation(Box::new(
                prepared.certify(record.certificate)?,
            )))
        }
        ParsedRecord::Deadline(record) => {
            let replay = ConfirmedReplay::replay_deadline_candidate(
                history,
                anchor.network(),
                params,
                ancestors,
            )
            .map_err(invalid)?;
            let prepared = PreparedDeadline::from_replay(history, anchor, params, replay)?;
            Ok(CertifiedEvent::Deadline(Box::new(
                prepared.certify(record.certificate)?,
            )))
        }
        ParsedRecord::Governance(record) => {
            let replay = ConfirmedReplay::replay_governance_candidate(
                history,
                anchor.network(),
                params,
                ancestors,
            )
            .map_err(invalid)?;
            let prepared = PreparedGovernance::from_replay(history, anchor, params, replay)?;
            if prepared.manifest().proposals != record.proposals {
                return Err(invalid("governance ballot set differs from replay"));
            }
            Ok(CertifiedEvent::Governance(Box::new(
                prepared.certify(record.certificate)?,
            )))
        }
    }
}

pub(crate) fn load(
    store: &LmdbStore,
    network: NetworkId,
    params: &ProtocolParams,
) -> Result<Vec<CertifiedEvent>, StoreError> {
    let mut anchor = ReplayedCheckpoint::genesis_anchor(network, params)?;
    let prefix = prefix(&anchor);
    let meta = store.meta_store();
    let txn = store.tx_begin_read()?;
    let tip = decode_tip(
        record_storage::get_txn(&meta, &txn, &format!("{prefix}/head"))?
            .as_deref(),
    )?;
    if tip > MAX_CHECKPOINTS {
        return Err(invalid(
            "full checkpoint history requires incremental replay",
        ));
    }
    let mut inventory = super::inventory::read(store, &txn, &prefix, tip)?;
    let mut records = Vec::new();
    let mut total = 0usize;
    for height in 1..=tip {
        let bytes = inventory
            .remove(&height)
            .ok_or_else(|| invalid("checkpoint chain has a missing certificate"))?;
        total = total
            .checked_add(bytes.len())
            .ok_or_else(|| invalid("checkpoint chain size overflow"))?;
        if total > MAX_CHAIN_BYTES {
            return Err(invalid("checkpoint chain exceeds replay byte limit"));
        }
        records.push(decode_record(&bytes)?);
    }
    drop(txn);
    let mut chain = Vec::new();
    for record in records {
        hydrate_record(&mut chain, &record)?;
        let event = validate(store, &anchor, params, record, &chain)?;
        anchor = event
            .parent()
            .advance(event.certificate(), event.next_roster().clone())
            .map_err(invalid)?;
        chain.push(event);
    }
    Ok(chain)
}

fn accept(
    store: &LmdbStore,
    network: NetworkId,
    params: &ProtocolParams,
    record: ParsedRecord,
) -> Result<CertifiedEvent, StoreError> {
    if !matches!(record, ParsedRecord::Selection(_)) {
        require_lifecycle_enabled(network)?;
    }
    let mut chain = load(store, network, params)?;
    let height = record.certificate().checkpoint.height;
    if height == 0 || height > MAX_CHECKPOINTS {
        return Err(invalid("checkpoint height outside replay bounds"));
    }
    if height <= chain.len() as u64 {
        let existing = chain.remove(height as usize - 1);
        existing
            .parent()
            .verify_certificate(record.certificate())
            .map_err(invalid)?;
        if existing.manifest() != record.manifest() {
            return Err(invalid(
                "checkpoint height already contains a different decision",
            ));
        }
        // The selection API supplies no beacon here; raw import validates it separately.
        store.force_sync().map_err(invalid)?;
        return Ok(existing);
    }
    let anchor = next_anchor(&chain, network, params)?;
    hydrate_record(&mut chain, &record)?;
    let validated = validate(store, &anchor, params, record, &chain)?;
    let history = ConfirmedHistory::read_certified(
        store,
        network,
        anchor.genesis_hash(),
        validated.certificate().checkpoint.cutoff,
        validated.heads(),
        chain.last().map_or(&[][..], CertifiedEvent::heads),
    )?;
    let bytes = validated.encode()?;
    let used = chain
        .iter()
        .try_fold(bytes.len() + MAX_BEACON_BYTES, |sum, entry| {
            sum.checked_add(entry.encode()?.len() + MAX_BEACON_BYTES)
                .ok_or_else(|| invalid("checkpoint chain size overflow"))
        })?;
    if used > MAX_CHAIN_BYTES {
        return Err(invalid("checkpoint chain exceeds replay byte limit"));
    }
    let prefix = prefix(&anchor);
    let tip_key = format!("{prefix}/head");
    let key = format!("{prefix}/{height}");
    let mut batch = store.tx_begin_write()?;
    if decode_tip(record_storage::get_batch(&batch, &tip_key)?.as_deref())? != chain.len() as u64 {
        return Err(invalid(
            "checkpoint chain advanced during validation; retry from its new head",
        ));
    }
    if record_storage::get_batch(&batch, &key)?.is_some() {
        return Err(invalid(
            "checkpoint record already exists beyond its chain head",
        ));
    }
    super::historical::persist_history(&mut batch, &history)?;
    if let CertifiedEvent::Activation(activation) = &validated {
        let selection = chain
            .iter()
            .filter_map(CertifiedEvent::selection)
            .find(|entry| {
                entry.certificate().checkpoint.hash() == activation.manifest().activation.selection
            })
            .ok_or_else(|| invalid("activation selection disappeared"))?;
        let selection_key = format!("{prefix}/{}", selection.certificate().checkpoint.height);
        let mut stored = super::decode(
            &record_storage::get_batch(&batch, &selection_key)?
                .ok_or_else(|| invalid("activation selection is not persisted"))?,
        )?;
        if stored.certificate != *selection.certificate()
            || stored.heads != selection.heads()
            || stored.target != *selection.replayed().request().target()
        {
            return Err(invalid("activation selection changed before commit"));
        }
        if stored.beacon.is_none() {
            stored.beacon = Some(activation.manifest().activation.beacon());
            record_storage::put(&mut batch, &selection_key, &super::encode(&stored)?)?;
        }
    }
    record_storage::put(&mut batch, &key, &bytes)?;
    record_storage::put(&mut batch, &tip_key, &height.to_be_bytes())?;
    batch.commit()?;
    store.force_sync().map_err(invalid)?;
    Ok(validated)
}

pub(crate) fn accept_selection(
    store: &LmdbStore,
    network: NetworkId,
    params: &ProtocolParams,
    certificate: CheckpointCertificate,
    mut heads: Vec<HistoryHead>,
    target: WalletAddress,
) -> Result<CertifiedCheckpoint, StoreError> {
    heads.sort_by(|a, b| a.account.as_str().cmp(b.account.as_str()));
    match accept(
        store,
        network,
        params,
        ParsedRecord::Selection(Record {
            version: 1,
            certificate,
            heads,
            target,
            beacon: None,
        }),
    )? {
        CertifiedEvent::Selection(value) => Ok(*value),
        CertifiedEvent::Outcome(_)
        | CertifiedEvent::Roster(_)
        | CertifiedEvent::Activation(_)
        | CertifiedEvent::Deadline(_)
        | CertifiedEvent::Governance(_) => {
            Err(invalid("selection acceptance returned a different event"))
        }
    }
}

pub(crate) fn accept_outcome(
    store: &LmdbStore,
    network: NetworkId,
    params: &ProtocolParams,
    certificate: CheckpointCertificate,
    heads: Vec<HistoryHead>,
    target: WalletAddress,
) -> Result<CertifiedEvent, StoreError> {
    accept(
        store,
        network,
        params,
        ParsedRecord::Outcome(OutcomeRecord {
            version: 2,
            certificate,
            heads,
            target,
        }),
    )
}

pub(crate) fn prepare_selection(
    store: &LmdbStore,
    network: NetworkId,
    params: &ProtocolParams,
    cutoff: u64,
    heads: &[HistoryHead],
    target: &WalletAddress,
) -> Result<ReplayedCheckpoint, StoreError> {
    let chain = load(store, network, params)?;
    if chain.len() as u64 >= MAX_CHECKPOINTS {
        return Err(invalid("checkpoint chain requires incremental replay"));
    }
    let anchor = next_anchor(&chain, network, params)?;
    let history = ConfirmedHistory::read(
        store,
        network,
        anchor.genesis_hash(),
        cutoff,
        heads,
        chain.last().map_or(&[][..], CertifiedEvent::heads),
    )?;
    selection_from_history(&history, &anchor, params, target, &chain)
}

pub(crate) fn import_record(
    store: &LmdbStore,
    network: NetworkId,
    params: &ProtocolParams,
    height: u64,
    bytes: &[u8],
) -> Result<CertifiedEvent, StoreError> {
    let record = decode_record(bytes)?;
    if record.certificate().checkpoint.network != network
        || record.certificate().checkpoint.height != height
    {
        return Err(invalid(
            "checkpoint response has a different network or height",
        ));
    }
    let mut chain = load(store, network, params)?;
    let parent_height = height as usize - 1;
    if parent_height > chain.len() {
        return Err(invalid("checkpoint response skips certified ancestry"));
    }
    chain.truncate(parent_height);
    hydrate_record(&mut chain, &record)?;
    let anchor = next_anchor(&chain, network, params)?;
    let validated = validate(store, &anchor, params, record, &chain)?;
    let beacon = validated
        .selection()
        .and_then(CertifiedCheckpoint::beacon)
        .cloned();
    let mut accepted = accept(store, network, params, decode_record(&validated.encode()?)?)?;
    if let (Some(beacon), CertifiedEvent::Selection(selection)) = (beacon, &mut accepted) {
        selection.attach_beacon(store, params, beacon)?;
    }
    Ok(accepted)
}

pub(crate) fn hydrate_activation(
    events: &mut [CertifiedEvent],
    proof: &ActivationProof,
) -> Result<(), StoreError> {
    proof.commitment()?;
    let selection = events
        .iter_mut()
        .find_map(|event| match event {
            CertifiedEvent::Selection(value)
                if value.certificate().checkpoint.hash() == proof.selection =>
            {
                Some(value)
            }
            _ => None,
        })
        .ok_or_else(|| invalid("activation selection is absent from its ancestry"))?;
    let beacon = proof.beacon();
    selection
        .replayed()
        .request()
        .select(selection.parent(), selection.certificate(), &beacon)
        .map_err(invalid)?;
    if let Some(existing) = selection.beacon() {
        if existing.value != beacon.value || existing.round != beacon.round {
            return Err(invalid("activation conflicts with the selected beacon"));
        }
    } else {
        selection.record.beacon = Some(beacon);
    }
    Ok(())
}

fn hydrate_record(events: &mut [CertifiedEvent], record: &ParsedRecord) -> Result<(), StoreError> {
    if let ParsedRecord::Activation(record) = record {
        hydrate_activation(events, &record.activation)?;
    }
    Ok(())
}
