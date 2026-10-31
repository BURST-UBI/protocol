//! Complete economic replay state bound to a trusted checkpoint epoch.
//! This format excludes private keys, local consensus signer journals, peers,
//! provisional state and undo. Certified verification votes remain in state.
//! Decoding does not install a database or authorize pruning retained history.

use super::*;
use burst_consensus::{
    checkpoint::{CheckpointAnchor, CheckpointSignature},
    snapshot::{SnapshotCertificate, SnapshotManifest},
};
use burst_crypto::{derive_address, public_from_private, sign_message};
use burst_types::PrivateKey;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

mod account_inventory;
pub mod archive;
mod binary;
mod body;
mod brn_wallets;
mod canonical;
mod compact;
mod continuation;
pub mod export;
pub mod history;
pub mod installed;
pub mod reconciliation;
pub mod tail;
mod verification_state;
pub use crate::checkpoint_store::chain::CertifiedEvent;
pub use continuation::{
    CompactActivationCoverage, CompactDeadlineCoverage, CompactGovernanceCoverage,
    CompactOutcomeCoverage, CompactRosterCoverage, CompactSelectionCoverage, SelectionSource,
    SnapshotContinuation,
};

fn body_version(format: u16) -> Result<u8, ReplayError> {
    match format {
        burst_consensus::snapshot::LEGACY_STATE_FORMAT => Ok(5),
        burst_consensus::snapshot::PREVIOUS_STATE_FORMAT => Ok(7),
        burst_consensus::snapshot::STATE_FORMAT => Ok(8),
        _ => Err(invalid("unsupported snapshot state format")),
    }
}

struct Blob<'a>(Cow<'a, [u8]>);

impl Serialize for Blob<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for Blob<'_> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Bytes;
        impl<'de> serde::de::Visitor<'de> for Bytes {
            type Value = Vec<u8>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a snapshot byte string")
            }
            fn visit_bytes<E: serde::de::Error>(self, bytes: &[u8]) -> Result<Vec<u8>, E> {
                Ok(bytes.to_vec())
            }
            fn visit_byte_buf<E: serde::de::Error>(self, bytes: Vec<u8>) -> Result<Vec<u8>, E> {
                Ok(bytes)
            }
        }
        deserializer
            .deserialize_byte_buf(Bytes)
            .map(|bytes| Blob(Cow::Owned(bytes)))
    }
}

macro_rules! snapshot_state {
    (@versioned $version:expr, $value:expr, accounts) => { &compact::WalletMap { map: $value.as_ref(), compact: $version >= 8 } as &dyn canonical::Field };
    (@versioned $version:expr, $value:expr, wallets) => { &compact::WalletMap { map: &$value, compact: $version >= 8 } as &dyn canonical::Field };
    (@versioned $version:expr, $value:expr, lifecycle) => { &compact::Metadata { map: &$value, compact: $version >= 8 } as &dyn canonical::Field };
    (@versioned $version:expr, $value:expr, verification) => { &compact::Verification { state: &$value, compact: $version >= 8 } as &dyn canonical::Field };
    (@versioned $version:expr, $value:expr $(, $encoding:ident)?) => { snapshot_state!(@field $value $(, $encoding)?) };
    (@field $value:expr, rates) => { &canonical::RateHistoryField($value.as_ref()) as &dyn canonical::Field };
    (@field $value:expr, governance) => { &canonical::GovernanceField($value.as_ref()) as &dyn canonical::Field };
    (@field $value:expr, verification) => { &canonical::VerificationField($value.snapshot_view(), &|| Ok(())) as &dyn canonical::Field };
    (@field $value:expr, binary) => { &canonical::BinaryField(&$value) as &dyn canonical::Field };
    (@field $value:expr, bytes) => { &canonical::Bytes(&$value.0) as &dyn canonical::Field };
    (@field $value:expr, blob_map) => { &canonical::BlobMap(&$value) as &dyn canonical::Field };
    (@field $value:expr) => { &$value as &dyn canonical::Field };
    (@field $value:expr, map) => { &canonical::Map(&$value) as &dyn canonical::Field };
    (@field $value:expr, borrowed_map) => { &canonical::Map($value.as_ref()) as &dyn canonical::Field };
    (@field $value:expr, sequence) => { &canonical::Sequence($value.as_ref()) as &dyn canonical::Field };
    (@field $value:expr, ordered) => { &canonical::OrderedSequence($value.as_ref()) as &dyn canonical::Field };
    ($lifetime:lifetime; $($(#[$attribute:meta])* $field:ident: $kind:ty $(=> $encoding:ident)?),* $(,)?) => {
        // Serde serialization is the legacy-format oracle in tests. Production
        // encoding uses the versioned field writers below for every format.
        #[derive(Deserialize)]
        #[cfg_attr(test, derive(Serialize))]
        struct State<$lifetime> { $($(#[$attribute])* $field: $kind),* }

        impl State<'_> {
            fn encode_into(&self, output: &mut impl std::io::Write) -> Result<(), ReplayError> {
                self.encode_into_in(output, None)
            }

            fn encode_into_in(&self, output: &mut impl std::io::Write, directory: Option<&std::path::Path>) -> Result<(), ReplayError> {
                canonical::encode_record_in(&[
                    $((stringify!($field), snapshot_state!(@versioned self.version, self.$field $(, $encoding)?))),*
                ], output, directory)
            }
        }
    };
}

snapshot_state! {
    'a;
    version: u8,
    #[serde(deserialize_with = "compact::read_accounts")]
    accounts: Cow<'a, HashMap<WalletAddress, AccountInfo>> => accounts,
    pending: Cow<'a, HashMap<(WalletAddress, TxHash), PendingInfo>> => borrowed_map,
    brn_wallets: brn_wallets::Wallets<'a> => wallets,
    brn_rates: Cow<'a, burst_brn::RateHistory> => rates,
    next_stake_id: u64,
    trst_wallets: binary::Binary<'a> => binary,
    merger_graph: binary::Binary<'a> => binary,
    cutoff: Timestamp,
    payment_epoch: BlockHash,
    verification: verification_state::Verification<'a> => verification,
    bootstrap_phase: burst_verification::bootstrap::BootstrapPhase,
    #[serde(deserialize_with = "compact::read_metadata")]
    lifecycle_metadata: HashMap<String, Blob<'a>> => lifecycle,
    params: Cow<'a, ProtocolParams>,
    endorsement_blocks: Cow<'a, HashMap<WalletAddress, Vec<(Timestamp, BlockHash)>>> => borrowed_map,
    completed_endorsements: Vec<(WalletAddress, BlockHash)> => sequence,
    verification_triggers: Cow<'a, HashMap<WalletAddress, (Timestamp, BlockHash)>> => borrowed_map,
    counterparties: HashMap<WalletAddress, Vec<WalletAddress>> => map,
    governance: Cow<'a, Option<crate::governance_commit::CandidateGovernance>> => governance,
    proposal_contents: HashMap<BlockHash, Blob<'a>> => blob_map,
    closed_governance: Vec<TxHash> => sequence,
    selection_checkpoints: Cow<'a, super::receipts::CheckpointHistory> => ordered,
    selection_receipts: Cow<'a, super::receipts::ReceiptHistory> => ordered,
    replay_dependencies: Vec<BlockHash> => sequence,
    minted_burns: Cow<'a, HashMap<TxHash, Timestamp>> => borrowed_map,
}

/// Snapshot bytes and their manifest prepared from replay state or an
/// authenticated snapshot. Both fields are private, so callers cannot replace
/// the body or manifest before signing.
pub struct PreparedSnapshot<B = Vec<u8>> {
    manifest: SnapshotManifest,
    bytes: B,
}

impl<B> PreparedSnapshot<B> {
    pub fn manifest(&self) -> &SnapshotManifest {
        &self.manifest
    }

    pub fn sign(
        &self,
        anchor: &CheckpointAnchor,
        key: &PrivateKey,
    ) -> Result<CheckpointSignature, ReplayError> {
        self.manifest
            .validate(anchor)
            .map_err(|e| invalid(e.to_string()))?;
        let representative = derive_address(&public_from_private(key));
        if anchor.roster().weight(&representative) == 0 {
            return Err(invalid(
                "snapshot signer is not in the certified electorate",
            ));
        }
        Ok(CheckpointSignature {
            representative,
            signature: sign_message(&self.manifest.signing_payload(), key),
        })
    }
}

impl PreparedSnapshot {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// An operator-requested snapshot uses the exact last certified cut. Live
    /// account afterimages and a later provisional tail are not exported.
    pub fn at_tip(
        store: &burst_store_lmdb::LmdbStore,
        network: NetworkId,
        initial_params: &ProtocolParams,
    ) -> Result<Self, ReplayError> {
        let events = crate::checkpoint_store::chain::load(store, network, initial_params)
            .map_err(|e| invalid(e.to_string()))?;
        Self::from_events(store, network, initial_params, &events)
    }

    fn from_events(
        store: &burst_store_lmdb::LmdbStore,
        network: NetworkId,
        initial_params: &ProtocolParams,
        events: &[crate::checkpoint_store::chain::CertifiedEvent],
    ) -> Result<Self, ReplayError> {
        if events.is_empty() {
            return Err(invalid("snapshot requires a certified checkpoint"));
        }
        let mut continuation = SnapshotContinuation::from_genesis(network, initial_params)?;
        let mut selections = Vec::new();
        for event in events {
            let plan = continuation.plan_tail(
                store,
                event.certificate().checkpoint.cutoff,
                event.heads(),
            )?;
            let retained = crate::checkpoint_sync::witness::read_plan(store, &continuation, &plan)
                .map_err(|error| invalid(error.to_string()))?;
            let tail = plan.finish(retained)?;
            continuation = continuation.replay_checkpoint(tail, event, &selections)?;
            if let Some(selection) = event.selection() {
                selections.push(selection);
            }
        }
        continuation.state().prepare_snapshot(continuation.anchor())
    }
}

impl ConfirmedReplay {
    /// Resume an authenticated economic state in the same payment epoch. The
    /// current history reader still supplies complete ancestry for dependency
    /// checks; this avoids executing the prefix again, but does not permit
    /// pruning that ancestry or bypass its size limits.
    pub fn replay_snapshot_tail(
        mut self,
        anchor: &CheckpointAnchor,
        checkpoint: &burst_consensus::checkpoint::Checkpoint,
        previous: &ConfirmedHistory,
        history: &ConfirmedHistory,
        selections: &[crate::checkpoint_store::CertifiedCheckpoint],
    ) -> Result<Self, ReplayError> {
        self.validate_snapshot_context(anchor)?;
        if checkpoint.hash() != anchor.hash()
            || checkpoint.cutoff != previous.cutoff()
            || checkpoint.ledger_root != previous.commitment()
            || checkpoint.params_hash != self.params.params_hash()
            || previous.network() != anchor.network()
            || previous.genesis() != anchor.genesis_hash()
            || history.cutoff() < previous.cutoff()
        {
            return Err(invalid(
                "snapshot continuation does not match its certified history",
            ));
        }
        let refs: Vec<_> = selections.iter().collect();
        self.append_epoch(
            previous,
            history,
            anchor.hash(),
            &self.params.clone(),
            &refs,
            false,
        )?;
        self.require_closed_governance(Timestamp::new(history.cutoff()))?;
        Ok(self)
    }

    pub fn prepare_snapshot(
        &self,
        anchor: &CheckpointAnchor,
    ) -> Result<PreparedSnapshot, ReplayError> {
        self.validate_snapshot_context(anchor)?;
        let bytes = self.snapshot_bytes()?;
        let manifest = SnapshotManifest::new(anchor, &bytes).map_err(|e| invalid(e.to_string()))?;
        Ok(PreparedSnapshot { manifest, bytes })
    }

    pub(super) fn snapshot_bytes(&self) -> Result<Vec<u8>, ReplayError> {
        let mut bytes = Vec::new();
        self.snapshot_state().encode_into(&mut bytes)?;
        Ok(bytes)
    }

    #[cfg(test)]
    pub(super) fn legacy_snapshot_bytes(&self, format: u16) -> Result<Vec<u8>, ReplayError> {
        let mut bytes = Vec::new();
        self.snapshot_state_version(body_version(format)?)
            .encode_into(&mut bytes)?;
        Ok(bytes)
    }

    pub(super) fn prepare_snapshot_in(
        &self,
        anchor: &CheckpointAnchor,
        directory: &std::path::Path,
    ) -> Result<PreparedSnapshot<body::StagedBody>, ReplayError> {
        self.prepare_snapshot_format_in(anchor, directory, burst_consensus::snapshot::STATE_FORMAT)
    }

    pub(super) fn prepare_snapshot_format_in(
        &self,
        anchor: &CheckpointAnchor,
        directory: &std::path::Path,
        format: u16,
    ) -> Result<PreparedSnapshot<body::StagedBody>, ReplayError> {
        let version = body_version(format)?;
        self.validate_snapshot_context(anchor)?;
        let (bytes, mut manifest) = body::StagedBody::prepare(anchor, directory, |writer| {
            self.snapshot_state_version(version)
                .encode_into_in(writer, Some(directory))
        })?;
        manifest.state_format = format;
        manifest
            .validate(anchor)
            .map_err(|error| invalid(error.to_string()))?;
        Ok(PreparedSnapshot { manifest, bytes })
    }

    pub(super) fn snapshot_manifest(
        &self,
        anchor: &CheckpointAnchor,
    ) -> Result<SnapshotManifest, ReplayError> {
        use blake2::{digest::consts::U32, Blake2b, Digest};
        self.validate_snapshot_context(anchor)?;
        struct Commitment {
            hash: Blake2b<U32>,
            length: u64,
        }
        impl std::io::Write for Commitment {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.length = self
                    .length
                    .checked_add(bytes.len() as u64)
                    .ok_or_else(|| std::io::Error::other("snapshot length overflow"))?;
                self.hash.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut commitment = Commitment {
            hash: Blake2b::new(),
            length: 0,
        };
        self.snapshot_state().encode_into(&mut commitment)?;
        SnapshotManifest::from_commitment(
            anchor,
            commitment.length,
            BlockHash::new(commitment.hash.finalize().into()),
        )
        .map_err(|e| invalid(e.to_string()))
    }

    #[cfg(test)]
    pub(super) fn assert_snapshot_encoding_compatibility(&self) {
        let state = self.snapshot_state();
        assert!(matches!(state.trst_wallets, binary::Binary::Wallets(_)));
        assert!(matches!(state.merger_graph, binary::Binary::Graph(_)));
        assert!(matches!(state.accounts, Cow::Borrowed(_)));
        assert!(matches!(
            state.brn_wallets,
            brn_wallets::Wallets::Borrowed(_)
        ));
        assert!(matches!(state.selection_receipts, Cow::Borrowed(_)));
        assert!(state
            .lifecycle_metadata
            .values()
            .all(|blob| matches!(blob.0, Cow::Borrowed(_))));
        assert!(state
            .proposal_contents
            .values()
            .all(|blob| matches!(blob.0, Cow::Borrowed(_))));
        for version in [5, 7] {
            let mut state = self.snapshot_state_version(version);
            let verification_state::Verification::Borrowed(borrowed) = &state.verification else {
                panic!("checkpoint encoding cloned verification state");
            };
            assert!(std::ptr::eq(*borrowed, &self.verification));
            let borrowed_bytes = canonical::encode(&state).unwrap();
            let mut streamed = Vec::new();
            state.encode_into(&mut streamed).unwrap();
            state.verification =
                verification_state::Verification::Owned(self.verification.snapshot());
            let legacy = canonical::encode(&state).unwrap();
            assert_eq!(borrowed_bytes, legacy);
            assert_eq!(streamed, legacy);
        }
    }

    fn snapshot_state(&self) -> State<'_> {
        self.snapshot_state_version(8)
    }

    fn snapshot_state_version(&self, version: u8) -> State<'_> {
        let mut completed_endorsements: Vec<_> =
            self.completed_endorsements.iter().cloned().collect();
        completed_endorsements.sort_by(|a, b| {
            a.0.as_str()
                .cmp(b.0.as_str())
                .then_with(|| a.1.as_bytes().cmp(b.1.as_bytes()))
        });
        let mut closed_governance: Vec<_> = self.closed_governance.iter().copied().collect();
        closed_governance.sort_by_key(|hash| *hash.as_bytes());
        let counterparties = self
            .counterparties
            .iter()
            .map(|(wallet, peers)| {
                let mut peers: Vec<_> = peers.iter().cloned().collect();
                peers.sort_by(|a, b| a.as_str().cmp(b.as_str()));
                (wallet.clone(), peers)
            })
            .collect();
        let mut replay_dependencies: Vec<_> = self.replay_dependencies.iter().copied().collect();
        replay_dependencies.sort_by_key(|hash| *hash.as_bytes());
        State {
            version,
            minted_burns: Cow::Borrowed(&self.minted_burns),
            selection_checkpoints: Cow::Borrowed(&self.selection_checkpoints),
            selection_receipts: Cow::Borrowed(&self.selection_receipts),
            replay_dependencies,
            accounts: Cow::Borrowed(&self.accounts),
            pending: Cow::Borrowed(&self.pending),
            brn_wallets: brn_wallets::Wallets::Borrowed(&self.brn.wallets),
            brn_rates: Cow::Borrowed(&self.brn.rate_history),
            next_stake_id: self.brn.next_stake_id(),
            trst_wallets: if version == 5 {
                binary::Binary::LegacyWallets(&self.trst)
            } else {
                binary::Binary::Wallets(&self.trst)
            },
            merger_graph: if version == 5 {
                binary::Binary::LegacyGraph(&self.trst.merger_graph)
            } else {
                binary::Binary::Graph(&self.trst.merger_graph)
            },
            cutoff: self.cutoff,
            payment_epoch: self.payment_epoch,
            verification: verification_state::Verification::Borrowed(&self.verification),
            bootstrap_phase: self.bootstrap_phase,
            lifecycle_metadata: self
                .lifecycle_metadata
                .iter()
                .map(|(key, bytes)| (key.clone(), Blob(Cow::Borrowed(bytes))))
                .collect(),
            params: Cow::Borrowed(&self.params),
            endorsement_blocks: Cow::Borrowed(&self.endorsement_blocks),
            completed_endorsements,
            verification_triggers: Cow::Borrowed(&self.verification_triggers),
            counterparties,
            governance: Cow::Borrowed(&self.governance),
            proposal_contents: self
                .proposal_contents
                .iter()
                .map(|(key, bytes)| (*key, Blob(Cow::Borrowed(bytes))))
                .collect(),
            closed_governance,
        }
    }

    /// The anchor must come from configured genesis and authenticated authority
    /// transitions, never from fields inside this snapshot or a peer roster.
    pub fn from_authenticated_snapshot(
        anchor: &CheckpointAnchor,
        certificate: &SnapshotCertificate,
        bytes: &[u8],
    ) -> Result<Self, ReplayError> {
        let body = body::VerifiedBody::borrowed(anchor, certificate, bytes)?;
        Self::from_verified_snapshot_body(anchor, body)
    }

    fn from_verified_snapshot_body(
        anchor: &CheckpointAnchor,
        reader: body::VerifiedBody<'_>,
    ) -> Result<Self, ReplayError> {
        Self::from_verified_snapshot_body_until(anchor, reader, &|_| Ok(()))
    }

    fn from_verified_snapshot_body_until(
        anchor: &CheckpointAnchor,
        mut reader: body::VerifiedBody<'_>,
        check: &dyn Fn(&'static str) -> Result<(), ReplayError>,
    ) -> Result<Self, ReplayError> {
        let state: State = ciborium::from_reader(body::PollingReader {
            inner: &mut reader,
            poll: check,
            bytes_until_poll: 0,
        })
        .map_err(|e| invalid(e.to_string()))?;
        check("snapshot:decoded-state")?;
        let expected_version = body_version(reader.state_format())?;
        if !reader.consumed_all() || state.version != expected_version {
            return Err(invalid("unsupported or noncanonical economic snapshot"));
        }
        check("snapshot:restore-brn")?;
        let brn = BrnEngine::from_state(
            state.next_stake_id,
            state.brn_rates.into_owned(),
            state.brn_wallets.into_owned(),
        )
        .map_err(|e| invalid(e.to_string()))?;
        let version = state.version;
        check("snapshot:restore-wallets")?;
        let mut trst = state
            .trst_wallets
            .into_wallets(state.params.trst_expiry_secs, version == 5)?;
        check("snapshot:restore-graph")?;
        trst.merger_graph = state.merger_graph.into_graph(version == 5)?;
        trst.validate_bases_with_poll(&|stage| check(stage).map_err(|error| error.to_string()))
            .map_err(invalid)?;
        check("snapshot:restore-continuation")?;
        let replay = Self {
            mutations: None,
            network: anchor.network(),
            minted_burns: state.minted_burns.into_owned(),
            selection_checkpoints: state.selection_checkpoints.into_owned(),
            selection_receipts: state.selection_receipts.into_owned(),
            replay_dependencies: state.replay_dependencies.into_iter().collect(),
            accounts: state.accounts.into_owned(),
            pending: state.pending.into_owned(),
            brn,
            trst,
            cutoff: state.cutoff,
            payment_epoch: state.payment_epoch,
            verification: VerificationOrchestrator::restore(state.verification.into_owned()),
            bootstrap_phase: state.bootstrap_phase,
            lifecycle_metadata: state
                .lifecycle_metadata
                .into_iter()
                .map(|(key, bytes)| (key, bytes.0.into_owned()))
                .collect(),
            params: state.params.into_owned(),
            endorsement_blocks: state.endorsement_blocks.into_owned(),
            completed_endorsements: state.completed_endorsements.into_iter().collect(),
            verification_triggers: state.verification_triggers.into_owned(),
            counterparties: state
                .counterparties
                .into_iter()
                .map(|(wallet, peers)| (wallet, peers.into_iter().collect()))
                .collect(),
            governance: state.governance.into_owned(),
            proposal_contents: state
                .proposal_contents
                .into_iter()
                .map(|(key, bytes)| (key, bytes.0.into_owned()))
                .collect(),
            closed_governance: state.closed_governance.into_iter().collect(),
        };
        replay.validate_snapshot_context_until(anchor, check)?;
        check("snapshot:canonical-state")?;
        // Comparing the reconstructed canonical state with the original signed
        // bytes checks both canonical encoding and lossless reconstruction in
        // one pass. Comparing the decoded State separately repeats that work.
        if !reader.matches_state_with_poll(&replay.snapshot_state_version(version), check)? {
            return Err(invalid(
                "snapshot contains duplicate or inconsistent continuation state",
            ));
        }
        check("snapshot:canonical-finished")?;
        Ok(replay)
    }

    fn validate_snapshot_context(&self, anchor: &CheckpointAnchor) -> Result<(), ReplayError> {
        self.validate_snapshot_context_until(anchor, &|_| Ok(()))
    }

    fn validate_snapshot_context_until(
        &self,
        anchor: &CheckpointAnchor,
        check: &dyn Fn(&'static str) -> Result<(), ReplayError>,
    ) -> Result<(), ReplayError> {
        for (hash, at) in &self.minted_burns {
            check("snapshot:mint-context")?;
            if hash.is_zero() || *at > self.cutoff {
                return Err(invalid(
                    "snapshot contains invalid mint records or non-lifecycle metadata",
                ));
            }
        }
        for key in self.lifecycle_metadata.keys() {
            check("snapshot:lifecycle-context")?;
            if !burst_store_lmdb::write_batch::checkpoint_projection::is_lifecycle_key(key) {
                return Err(invalid(
                    "snapshot contains invalid mint records or non-lifecycle metadata",
                ));
            }
        }
        if self.selection_receipts.len() != self.selection_checkpoints.len() {
            return Err(invalid(
                "snapshot selection inventory differs from its receipts",
            ));
        }
        let mut previous_height = 0;
        for (receipt, hash) in self
            .selection_receipts
            .iter()
            .zip(self.selection_checkpoints.iter())
        {
            check("snapshot:receipt-context")?;
            let receipt = receipt?;
            receipt
                .validate_at(anchor)
                .map_err(|error| invalid(error.to_string()))?;
            if receipt.checkpoint().hash() != *hash?
                || receipt.checkpoint().height <= previous_height
            {
                return Err(invalid(
                    "snapshot selection receipts are missing or out of order",
                ));
            }
            previous_height = receipt.checkpoint().height;
        }
        check("snapshot:roster-context")?;
        if self.network != anchor.network()
            || self.payment_epoch != anchor.hash()
            || self.cutoff.as_secs() != anchor.cutoff()
            || self.human_roster()? != *anchor.roster()
        {
            return Err(invalid("snapshot state differs from certified epoch"));
        }
        check("snapshot:governance-context")?;
        let governance = self
            .governance
            .as_ref()
            .ok_or_else(|| invalid("snapshot has no governance state"))?;
        if governance.network != anchor.network()
            || governance.genesis != anchor.genesis_hash()
            || governance.params.params_hash() != self.params.params_hash()
        {
            return Err(invalid(
                "snapshot governance context differs from its epoch",
            ));
        }
        Ok(())
    }
}
