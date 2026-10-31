//! Target and verifier records for incremental durable verification state.
//! Decoding records does not authenticate them or authorize publication.

use super::*;
pub mod normalized;
pub mod retention;
mod visitor;
pub use visitor::VerificationRecordVisitor;
use bincode::Options;
use std::collections::{BTreeMap, BTreeSet, HashSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PriorVotes(#[serde(serialize_with = "crate::state::ordered_votes")] Vec<VerifierVote>);

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Target {
    address: WalletAddress,
    state: Option<VerificationState>,
    challenge: Option<Challenge>,
    appeal: Option<AppealContext>,
    votes: Option<PriorVotes>,
    selection: Option<SelectionRound>,
    vote_contexts: Option<BTreeMap<String, BlockHash>>,
    challenge_context: Option<BlockHash>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum Record {
    Rules {
        threshold: u32,
        window: u32,
    },
    Target(Box<Target>),
    Verifier {
        address: WalletAddress,
        penalty: Option<u64>,
        history: Option<crate::voting::VoteWindow>,
    },
}

/// A logical rules, target or verifier record. Storage decoding checks that its
/// derived key matches the supplied key; ordinary deserialization does not.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerificationRecord(Record);

impl VerificationRecord {
    pub const STORAGE_PREFIX: &'static str = "verification_state/v1/";

    /// Key for a single account lifecycle read; does not scan verifier history.
    pub fn target_storage_key(address: &WalletAddress) -> Result<String, String> {
        Self::address_key(1, address)
    }

    /// Canonical snapshot projection of one logical record plus the actual rules.
    /// Runs record identity, history and appeal checks on this fragment. It does
    /// not establish inventory completeness or authenticate its chain provenance.
    pub fn into_snapshot_fragment(self, rules: &Self, max_snapshot_bytes: u64) -> Result<OrchestratorSnapshot, String> {
        visitor::snapshot_fragment(self, rules, max_snapshot_bytes, false)
    }

    pub fn has_active_challenge(&self) -> Result<bool, String> {
        match &self.0 {
            Record::Target(target) => Ok(target.challenge.is_some()),
            _ => Err("expected verification target record".into()),
        }
    }

    fn key(&self) -> Result<String, String> {
        let (kind, address) = match &self.0 {
            Record::Rules { .. } => return Ok(format!("{}0", Self::STORAGE_PREFIX)),
            Record::Target(target) => (1, &target.address),
            Record::Verifier { address, .. } => (2, address),
        };
        Self::address_key(kind, address)
    }

    fn address_key(kind: u8, address: &WalletAddress) -> Result<String, String> {
        if !address.has_prefix_and_payload() {
            return Err("invalid verification record address".into());
        }
        let mut key = format!("{}{kind}/", Self::STORAGE_PREFIX);
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in address.as_str().as_bytes() {
            key.push(HEX[(byte >> 4) as usize] as char);
            key.push(HEX[(byte & 15) as usize] as char);
        }
        if key.len() > 511 {
            return Err("verification record key exceeds LMDB limit".into());
        }
        Ok(key)
    }

    // Each populated field contributes one entry to an existing snapshot map.
    // Map lengths, rule scalars and the format prefix belong to the empty image.
    fn snapshot_entry_bytes(&self) -> Result<u64, String> {
        fn add<T: Serialize>(
            total: &mut u64,
            address: &WalletAddress,
            value: Option<&T>,
        ) -> Result<(), String> {
            if let Some(value) = value {
                let bytes =
                    bincode::serialized_size(&(address, value)).map_err(|e| e.to_string())?;
                *total = total
                    .checked_add(bytes)
                    .ok_or("verification snapshot size overflow")?;
            }
            Ok(())
        }
        let mut total = 0;
        match &self.0 {
            Record::Rules { .. } => {}
            Record::Target(target) => {
                add(&mut total, &target.address, target.state.as_ref())?;
                add(&mut total, &target.address, target.challenge.as_ref())?;
                add(&mut total, &target.address, target.appeal.as_ref())?;
                add(&mut total, &target.address, target.votes.as_ref())?;
                add(&mut total, &target.address, target.selection.as_ref())?;
                add(&mut total, &target.address, target.vote_contexts.as_ref())?;
                add(
                    &mut total,
                    &target.address,
                    target.challenge_context.as_ref(),
                )?;
            }
            Record::Verifier {
                address,
                penalty,
                history,
            } => {
                add(&mut total, address, penalty.as_ref())?;
                add(&mut total, address, history.as_ref())?;
            }
        }
        Ok(total)
    }

    pub fn storage_entry(&self) -> Result<(String, Vec<u8>), String> {
        let size = bincode::serialized_size(&(1u8, self)).map_err(|e| e.to_string())?;
        if size > burst_types::MAX_P2P_MESSAGE_BYTES as u64 {
            return Err("verification record exceeds row limit".into());
        }
        Ok((
            self.key()?,
            bincode::serialize(&(1u8, self)).map_err(|e| e.to_string())?,
        ))
    }

    pub fn from_storage_entry(key: &str, bytes: &[u8]) -> Result<Self, String> {
        if !key.starts_with(Self::STORAGE_PREFIX)
            || key.len() > 511
            || bytes.len() > burst_types::MAX_P2P_MESSAGE_BYTES
        {
            return Err("invalid verification record bounds or namespace".into());
        }
        let (version, record): (u8, Self) = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(bytes.len() as u64)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map_err(|e| e.to_string())?;
        if version != 1 || record.key()? != key {
            return Err("verification record version or key mismatch".into());
        }
        Ok(record)
    }
}

impl VerificationOrchestrator {
    /// Cold replacement scope for independently reconstructed durable state.
    /// This may visit every key; ordinary admission must retain its write scope.
    pub fn storage_replacement_scope(&self, before: &Self) -> Self {
        let mut next = self.storage_clone();
        next.states.replacement_scope(&before.states);
        next.active_challenges.replacement_scope(&before.active_challenges);
        next.active_appeals.replacement_scope(&before.active_appeals);
        next.prior_votes.replacement_scope(&before.prior_votes);
        next.selections.replacement_scope(&before.selections);
        next.penalized_verifiers.replacement_scope(&before.penalized_verifiers);
        next.attempts.votes.replacement_scope(&before.attempts.votes);
        next.attempts.challenges.replacement_scope(&before.attempts.challenges);
        next.neither_tracker.history.replacement_scope(&before.neither_tracker.history);
        next.record_change_rules = Some((before.neither_tracker.penalty_threshold_bps,
            before.neither_tracker.window_assignments));
        next
    }

    /// Validate Neither history and appeal relationships, as snapshot decoding does.
    /// This does not replay signed operations or authenticate selection certificates.
    pub fn validate_storage_state(&self) -> Result<(), String> {
        self.neither_tracker.validate().map_err(|e| e.to_string())?;
        super::appeal::validate_appeals(&self.active_appeals, &self.attempts,
            |target| self.active_challenges.get(target), |target| self.states.get(target))
            .map_err(|e| e.to_string())
    }

    /// A shared image of durable state only. Pending events and a caller's
    /// uncommitted tracking scope must never survive restoration from storage.
    pub fn storage_clone(&self) -> Self {
        let mut neither_tracker = self.neither_tracker.clone();
        neither_tracker.history = self.neither_tracker.history.untracked_clone();
        Self {
            endorsement: self.endorsement.clone(),
            voting: self.voting.clone(),
            challenges: self.challenges.clone(),
            neither_tracker,
            states: self.states.untracked_clone(),
            active_challenges: self.active_challenges.untracked_clone(),
            active_appeals: self.active_appeals.untracked_clone(),
            prior_votes: self.prior_votes.untracked_clone(),
            selections: self.selections.untracked_clone(),
            penalized_verifiers: self.penalized_verifiers.untracked_clone(),
            attempts: AttemptContexts {
                votes: self.attempts.votes.untracked_clone(),
                challenges: self.attempts.challenges.untracked_clone(),
            },
            pending_events: Vec::new(),
            record_change_rules: None,
        }
    }

    /// Root identity proves unchanged durable state; equal values in different
    /// roots need ordinary cold validation instead of this shortcut.
    pub fn shares_record_storage_with(&self, other: &Self) -> bool {
        self.neither_tracker.same_rules(&other.neither_tracker)
            && self.states.ptr_eq(&other.states)
            && self.active_challenges.ptr_eq(&other.active_challenges)
            && self.active_appeals.ptr_eq(&other.active_appeals)
            && self.prior_votes.ptr_eq(&other.prior_votes)
            && self.selections.ptr_eq(&other.selections)
            && self.penalized_verifiers.ptr_eq(&other.penalized_verifiers)
            && self.attempts.votes.ptr_eq(&other.attempts.votes)
            && self.attempts.challenges.ptr_eq(&other.attempts.challenges)
            && self
                .neither_tracker
                .history
                .ptr_eq(&other.neither_tracker.history)
    }

    /// Begin one candidate's storage write scope after restoring its baseline.
    /// Calling this again starts a new scope; callers must persist the preceding
    /// scope first. Cloned candidates own independent tracking roots.
    pub fn begin_record_changes(&mut self) {
        self.states.begin_changes();
        self.active_challenges.begin_changes();
        self.active_appeals.begin_changes();
        self.prior_votes.begin_changes();
        self.selections.begin_changes();
        self.penalized_verifiers.begin_changes();
        self.attempts.votes.begin_changes();
        self.attempts.challenges.begin_changes();
        self.neither_tracker.history.begin_changes();
        self.record_change_rules = Some((
            self.neither_tracker.penalty_threshold_bps,
            self.neither_tracker.window_assignments,
        ));
    }

    /// Emit final affected rows, including deletion keys, without scanning
    /// unrelated maps. No-op mutations may emit unchanged rows; writers should
    /// compare their existing bytes before adding undo entries. Callback failure
    /// stops iteration but does not undo prior writes or consume the change scope.
    pub fn visit_record_changes(
        &self,
        mut visit: impl FnMut(String, Option<VerificationRecord>) -> Result<(), String>,
    ) -> Result<(), String> {
        let before = self
            .record_change_rules
            .ok_or("verification change scope was not started")?;
        if before
            != (
                self.neither_tracker.penalty_threshold_bps,
                self.neither_tracker.window_assignments,
            )
        {
            let record = self.rules_record();
            visit(record.key()?, Some(record))?;
        }
        let targets: BTreeSet<_> = self
            .states
            .changed_keys()
            .chain(self.active_challenges.changed_keys())
            .chain(self.active_appeals.changed_keys())
            .chain(self.prior_votes.changed_keys())
            .chain(self.selections.changed_keys())
            .chain(self.attempts.votes.changed_keys())
            .chain(self.attempts.challenges.changed_keys())
            .collect();
        for target in targets {
            visit(
                VerificationRecord::address_key(1, target)?,
                self.target_record(target),
            )?;
        }
        let verifiers: BTreeSet<_> = self
            .penalized_verifiers
            .changed_keys()
            .map(WalletAddress::as_str)
            .chain(
                self.neither_tracker
                    .history
                    .changed_keys()
                    .map(String::as_str),
            )
            .collect();
        for verifier in verifiers {
            let verifier = WalletAddress::new(verifier);
            visit(
                VerificationRecord::address_key(2, &verifier)?,
                self.verifier_record(&verifier),
            )?;
        }
        Ok(())
    }

    /// Prove the current change scope began from this exact immutable baseline.
    pub fn record_scope_matches(&self, before: &Self) -> bool {
        self.record_change_rules
            == Some((
                before.neither_tracker.penalty_threshold_bps,
                before.neither_tracker.window_assignments,
            ))
            && self.states.scope_matches(&before.states)
            && self
                .active_challenges
                .scope_matches(&before.active_challenges)
            && self.active_appeals.scope_matches(&before.active_appeals)
            && self.prior_votes.scope_matches(&before.prior_votes)
            && self.selections.scope_matches(&before.selections)
            && self
                .penalized_verifiers
                .scope_matches(&before.penalized_verifiers)
            && self.attempts.votes.scope_matches(&before.attempts.votes)
            && self
                .attempts
                .challenges
                .scope_matches(&before.attempts.challenges)
            && self
                .neither_tracker
                .history
                .scope_matches(&before.neither_tracker.history)
    }

    /// Complete before/after rows for a scope tied to an immutable baseline.
    pub fn visit_record_delta(
        &self,
        before: &Self,
        mut visit: impl FnMut(
            String,
            Option<VerificationRecord>,
            Option<VerificationRecord>,
        ) -> Result<(), String>,
    ) -> Result<(), String> {
        if !self.record_scope_matches(before) {
            return Err("verification change scope differs from its baseline".into());
        }
        self.visit_record_changes(|key, after| {
            let old = before.record_at_storage_key(&key)?;
            visit(key, old, after)
        })
    }

    /// Key-bound point lookup for scoped before-image validation.
    pub fn record_at_storage_key(&self, key: &str) -> Result<Option<VerificationRecord>, String> {
        let suffix = key
            .strip_prefix(VerificationRecord::STORAGE_PREFIX)
            .ok_or("invalid verification record namespace")?;
        if suffix == "0" {
            return Ok(Some(self.rules_record()));
        }
        let (kind, encoded) = suffix
            .split_once('/')
            .ok_or("invalid verification record key")?;
        if encoded.len() % 2 != 0 || key.len() > 511 {
            return Err("invalid verification record key".into());
        }
        let bytes: Vec<u8> = encoded
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let digit = |byte: u8| match byte {
                    b'0'..=b'9' => Ok(byte - b'0'),
                    b'a'..=b'f' => Ok(byte - b'a' + 10),
                    _ => Err("invalid verification record key"),
                };
                Ok::<u8, &str>(digit(pair[0])? * 16 + digit(pair[1])?)
            })
            .collect::<Result<_, _>>()?;
        let address =
            std::str::from_utf8(&bytes).map_err(|_| "invalid verification record address")?;
        if !address.starts_with(WalletAddress::PREFIX) {
            return Err("invalid verification record address".into());
        }
        let address = WalletAddress::new(address);
        let kind = match kind {
            "1" => 1,
            "2" => 2,
            _ => return Err("invalid verification record kind".into()),
        };
        if VerificationRecord::address_key(kind, &address)? != key {
            return Err("invalid verification record key".into());
        }
        Ok(if kind == 1 {
            self.target_record(&address)
        } else {
            self.verifier_record(&address)
        })
    }

    /// A target's attempt bindings and appeal travel with its verification state.
    pub fn target_record(&self, address: &WalletAddress) -> Option<VerificationRecord> {
        let target = Target {
            address: address.clone(),
            state: self.states.get(address).cloned(),
            challenge: self.active_challenges.get(address).cloned(),
            appeal: self.active_appeals.get(address).cloned(),
            votes: self.prior_votes.get(address).cloned().map(PriorVotes),
            selection: self.selections.get(address).cloned(),
            vote_contexts: self
                .attempts
                .votes
                .get(address)
                .map(|votes| votes.iter().map(|(k, v)| (k.to_string(), *v)).collect()),
            challenge_context: self.attempts.challenges.get(address).copied(),
        };
        (target.state.is_some()
            || target.challenge.is_some()
            || target.appeal.is_some()
            || target.votes.is_some()
            || target.selection.is_some()
            || target.vote_contexts.is_some()
            || target.challenge_context.is_some())
        .then(|| VerificationRecord(Record::Target(Box::new(target))))
    }

    pub fn verifier_record(&self, address: &WalletAddress) -> Option<VerificationRecord> {
        let penalty = self.penalized_verifiers.get(address).copied();
        let history = self.neither_tracker.history.get(address.as_str()).cloned();
        (penalty.is_some() || history.is_some()).then(|| {
            VerificationRecord(Record::Verifier {
                address: address.clone(),
                penalty,
                history,
            })
        })
    }

    pub fn rules_record(&self) -> VerificationRecord {
        VerificationRecord(Record::Rules {
            threshold: self.neither_tracker.penalty_threshold_bps,
            window: self.neither_tracker.window_assignments,
        })
    }

    /// Deterministic full export. Transition writers can request individual rows.
    pub fn visit_records(
        &self,
        mut visit: impl FnMut(VerificationRecord) -> Result<(), String>,
    ) -> Result<(), String> {
        self.visit_record_keys(|key| {
            visit(self.record_at_storage_key(&key)?.ok_or("missing verification record")?)
        })
    }

    /// Enumerate the logical inventory without cloning target or verifier values.
    pub fn visit_record_keys(&self, mut visit: impl FnMut(String) -> Result<(), String>) -> Result<(), String> {
        visit(self.rules_record().key()?)?;
        let targets: BTreeSet<_> = self
            .states
            .keys()
            .chain(self.active_challenges.keys())
            .chain(self.active_appeals.keys())
            .chain(self.prior_votes.keys())
            .chain(self.selections.keys())
            .chain(self.attempts.targets())
            .map(|target| target.as_str())
            .collect();
        for target in targets {
            visit(VerificationRecord::target_storage_key(&WalletAddress::new(target))?)?;
        }
        let verifiers: BTreeSet<_> = self
            .penalized_verifiers
            .keys()
            .map(|v| v.as_str())
            .chain(self.neither_tracker.history.keys().map(String::as_str))
            .collect();
        for verifier in verifiers {
            if !verifier.starts_with(WalletAddress::PREFIX) {
                return Err("invalid verification history address".into());
            }
            visit(VerificationRecord::address_key(2, &WalletAddress::new(verifier))?)?;
        }
        Ok(())
    }
}

/// Reconstructs an engine from logical records, checking duplicate keys, record
/// identity, Neither history and appeals. Any rejected row poisons the reader.
/// Callers establish inventory completeness and authenticate the source state.
pub struct VerificationRecordReader {
    state: VerificationOrchestrator,
    has_rules: bool,
    targets: HashSet<WalletAddress>,
    verifiers: HashSet<WalletAddress>,
    failed: bool,
    snapshot_bytes: Option<u64>,
}

impl Default for VerificationRecordReader {
    fn default() -> Self {
        Self {
            state: VerificationOrchestrator::new(),
            has_rules: false,
            targets: HashSet::new(),
            verifiers: HashSet::new(),
            failed: false,
            snapshot_bytes: None,
        }
    }
}

impl VerificationRecordReader {
    pub fn push(&mut self, record: VerificationRecord) -> Result<(), String> {
        self.push_bounded(record, u64::MAX)
    }

    /// Limit the complete canonical snapshot, including its format prefix,
    /// before retaining each row. Storage keys and row framing do not consume
    /// that aggregate budget; individual rows keep their own decoder limit.
    pub fn push_bounded(
        &mut self,
        record: VerificationRecord,
        max_snapshot_bytes: u64,
    ) -> Result<(), String> {
        self.push_record(record, max_snapshot_bytes, true)
    }

    /// Reassemble bounded physical rows, then apply the same global snapshot
    /// accounting and target validation as the logical-record reader.
    pub fn push_normalized_target(
        &mut self,
        address: &WalletAddress,
        rows: &BTreeMap<String, Vec<u8>>,
        max_snapshot_bytes: u64,
    ) -> Result<(), String> {
        if self.failed { return Err("verification reader already failed".into()); }
        let result = normalized::target_from_rows(address, rows)
            .and_then(|record| self.push_record(record, max_snapshot_bytes, false));
        self.failed |= result.is_err();
        result
    }

    fn push_record(
        &mut self,
        record: VerificationRecord,
        max_snapshot_bytes: u64,
        physical_row: bool,
    ) -> Result<(), String> {
        if self.failed {
            return Err("verification reader already failed".into());
        }
        let result = (|| {
            let before = match self.snapshot_bytes {
                Some(bytes) => bytes,
                None => VerificationOrchestrator::new()
                    .snapshot()
                    .encoded_size()
                    .map_err(|e| e.to_string())?,
            };
            let next = before
                .checked_add(record.snapshot_entry_bytes()?)
                .ok_or("verification snapshot size overflow")?;
            if next > max_snapshot_bytes {
                return Err("verification snapshot exceeds byte limit".into());
            }
            self.push_inner(record, physical_row)?;
            self.snapshot_bytes = Some(next);
            Ok(())
        })();
        self.failed |= result.is_err();
        result
    }

    fn push_inner(&mut self, record: VerificationRecord, physical_row: bool) -> Result<(), String> {
        record.key()?;
        if physical_row && bincode::serialized_size(&(1u8, &record)).map_err(|e| e.to_string())?
            > burst_types::MAX_P2P_MESSAGE_BYTES as u64
        {
            return Err("verification record exceeds row limit".into());
        }
        match record.0 {
            Record::Rules { threshold, window } => {
                if self.has_rules {
                    return Err("duplicate verification rules".into());
                }
                self.has_rules = true;
                // Restore exactly; configure would trim histories depending on row order.
                self.state.neither_tracker.penalty_threshold_bps = threshold;
                self.state.neither_tracker.window_assignments = window;
            }
            Record::Target(target) => {
                let Target {
                    address,
                    state,
                    challenge,
                    appeal,
                    votes,
                    selection,
                    vote_contexts,
                    challenge_context,
                } = *target;
                if !self.targets.insert(address.clone())
                    || state.as_ref().is_some_and(|s| s.target != address)
                    || challenge.as_ref().is_some_and(|c| c.target != address)
                {
                    return Err("duplicate or mismatched verification target".into());
                }
                if state.is_none()
                    && challenge.is_none()
                    && appeal.is_none()
                    && votes.is_none()
                    && selection.is_none()
                    && vote_contexts.is_none()
                    && challenge_context.is_none()
                {
                    return Err("empty verification target record".into());
                }
                if let Some(value) = state {
                    self.state.states.insert(address.clone(), value);
                }
                if let Some(value) = challenge {
                    self.state.active_challenges.insert(address.clone(), value);
                }
                if let Some(value) = appeal {
                    self.state.active_appeals.insert(address.clone(), value);
                }
                if let Some(value) = votes {
                    self.state.prior_votes.insert(address.clone(), value.0);
                }
                if let Some(value) = selection {
                    self.state.selections.insert(address.clone(), value);
                }
                if let Some(value) = vote_contexts {
                    if value.keys().any(|key| {
                        !key.starts_with(WalletAddress::PREFIX)
                            || key.len() <= WalletAddress::PREFIX.len()
                    }) {
                        return Err("invalid verification vote context address".into());
                    }
                    self.state.attempts.votes.insert(
                        address.clone(),
                        value
                            .into_iter()
                            .map(|(k, v)| (WalletAddress::new(k), v))
                            .collect(),
                    );
                }
                if let Some(value) = challenge_context {
                    self.state.attempts.challenges.insert(address, value);
                }
            }
            Record::Verifier {
                address,
                penalty,
                history,
            } => {
                if !self.verifiers.insert(address.clone())
                    || (penalty.is_none() && history.is_none())
                {
                    return Err("duplicate or empty verification verifier record".into());
                }
                if let Some(value) = penalty {
                    self.state
                        .penalized_verifiers
                        .insert(address.clone(), value);
                }
                if let Some(value) = history {
                    self.state
                        .neither_tracker
                        .history
                        .insert(address.to_string(), value);
                }
            }
        }
        Ok(())
    }

    pub fn finish(self) -> Result<VerificationOrchestrator, String> {
        if self.failed || !self.has_rules {
            return Err("incomplete verification records".into());
        }
        self.state
            .neither_tracker
            .validate()
            .map_err(|e| e.to_string())?;
        super::appeal::validate_appeals(
            &self.state.active_appeals,
            &self.state.attempts,
            |target| self.state.active_challenges.get(target),
            |target| self.state.states.get(target),
        )
        .map_err(|e| e.to_string())?;
        Ok(self.state)
    }
}

#[cfg(test)]
pub(super) fn assert_record_roundtrip(state: &VerificationOrchestrator) {
    let expected = state.snapshot().encode().unwrap();
    assert_eq!(bincode::serialize(&state.snapshot_view()).unwrap(),
        bincode::serialize(&state.snapshot()).unwrap());
    super::borrowed_snapshot::assert_map_views(state);
    assert_normalized_roundtrip(state);
    assert_visitor_roundtrip(state, false);
    assert_visitor_roundtrip(state, true);

    let mut reader = VerificationRecordReader::default();
    let mut too_small = VerificationRecordReader::default();
    let mut rejected = false;
    state
        .visit_records(|record| {
            let (key, bytes) = record.storage_entry()?;
            let record = VerificationRecord::from_storage_entry(&key, &bytes)?;
            if !rejected {
                rejected = too_small
                    .push_bounded(record.clone(), expected.len() as u64 - 1)
                    .is_err();
            }
            reader.push_bounded(record, expected.len() as u64)
        })
        .unwrap();
    assert!(rejected);
    assert!(too_small.finish().is_err());
    assert_eq!(reader.snapshot_bytes, Some(expected.len() as u64));
    assert_eq!(
        reader.finish().unwrap().snapshot().encode().unwrap(),
        expected
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(state: &VerificationOrchestrator) -> Vec<(String, Vec<u8>)> {
        let mut entries = Vec::new();
        state
            .visit_records(|record| {
                entries.push(record.storage_entry()?);
                Ok(())
            })
            .unwrap();
        entries
    }

    fn restore(entries: &[(String, Vec<u8>)]) -> VerificationOrchestrator {
        let mut reader = VerificationRecordReader::default();
        for (key, bytes) in entries {
            reader
                .push(VerificationRecord::from_storage_entry(key, bytes).unwrap())
                .unwrap();
        }
        reader.finish().unwrap()
    }

    #[test]
    fn records_restore_exact_snapshot_and_preserve_empty_attempt_maps() {
        let mut state = VerificationOrchestrator::new();
        for n in 0..32 {
            let address = WalletAddress::new(format!("brst_target_{n}"));
            state.initialize_genesis(&address);
            state.attempts.votes.insert(address.clone(), Default::default());
            state
                .attempts
                .bind_challenge(&address, BlockHash::new([n + 1; 32]));
            state.neither_tracker.record_vote(&address, Vote::Neither);
            state
                .neither_tracker
                .record_vote(&address, Vote::Legitimate);
            state.penalized_verifiers.insert(address, 900);
        }
        let original = state.snapshot().encode().unwrap();
        let rows = entries(&state);
        assert_eq!(rows.len(), 65);
        assert_eq!(restore(&rows).snapshot().encode().unwrap(), original);
        let mut reversed = rows.clone();
        reversed.reverse();
        assert_eq!(restore(&reversed).snapshot().encode().unwrap(), original);
        assert_eq!(entries(&restore(&reversed)), rows);
    }

    #[test]
    fn record_identity_limits_and_reader_failure_are_enforced() {
        let state = VerificationOrchestrator::new();
        let record = state.rules_record();
        let (key, bytes) = record.storage_entry().unwrap();
        assert!(VerificationRecord::from_storage_entry("wrong", &bytes).is_err());
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(VerificationRecord::from_storage_entry(&key, &trailing).is_err());
        let mut wrong_version = bytes;
        wrong_version[0] = 2;
        assert!(VerificationRecord::from_storage_entry(&key, &wrong_version).is_err());
        assert!(VerificationRecordReader::default().finish().is_err());
        let mut reader = VerificationRecordReader::default();
        reader.push(record.clone()).unwrap();
        assert!(reader.push(record.clone()).is_err());
        assert!(reader.push(record).is_err());
        assert!(reader.finish().is_err());
        let mut state = state;
        state.initialize_genesis(&WalletAddress::new(format!("brst_{}", "x".repeat(512))));
        assert!(state
            .visit_records(|record| record.storage_entry().map(|_| ()))
            .is_err());
    }

    #[test]
    fn target_and_verifier_updates_do_not_rewrite_unrelated_rows() {
        let mut state = VerificationOrchestrator::new();
        let first = WalletAddress::new("brst_first");
        let second = WalletAddress::new("brst_second");
        state.initialize_genesis(&first);
        state.initialize_genesis(&second);
        let before = entries(&state);
        state.states.get_mut(&first).unwrap().phase = VerificationPhase::Unverified;
        state.neither_tracker.record_vote(&first, Vote::Neither);
        let after = entries(&state);
        let second_key = state
            .target_record(&second)
            .unwrap()
            .storage_entry()
            .unwrap()
            .0;
        assert_eq!(
            before.iter().find(|(key, _)| key == &second_key),
            after.iter().find(|(key, _)| key == &second_key)
        );
        assert_eq!(
            after.iter().filter(|entry| !before.contains(entry)).count(),
            2
        );
        assert_eq!(
            restore(&after).snapshot().encode().unwrap(),
            state.snapshot().encode().unwrap()
        );
        let mut calls = 0;
        assert!(state
            .visit_records(|_| {
                calls += 1;
                Err("stop".into())
            })
            .is_err());
        assert_eq!(calls, 1);
    }

    #[test]
    fn scoped_changes_reconstruct_full_records_and_preserve_candidate_isolation() {
        fn apply(state: &VerificationOrchestrator, rows: &mut BTreeMap<String, Vec<u8>>) -> usize {
            let mut emitted = BTreeSet::new();
            state
                .visit_record_changes(|key, record| {
                    assert!(emitted.insert(key.clone()), "duplicate changed record");
                    if let Some(record) = record {
                        let (bound_key, bytes) = record.storage_entry()?;
                        assert_eq!(key, bound_key);
                        rows.insert(key, bytes);
                    } else {
                        rows.remove(&key);
                    }
                    Ok(())
                })
                .unwrap();
            assert_eq!(*rows, entries(state).into_iter().collect());
            assert_eq!(
                restore(
                    &rows
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect::<Vec<_>>()
                )
                .snapshot()
                .encode()
                .unwrap(),
                state.snapshot().encode().unwrap()
            );
            emitted.len()
        }
        let address = |n| WalletAddress::new(format!("brst_scope_{n}"));
        let mut baseline = VerificationOrchestrator::new();
        for n in 0..128 {
            let target = address(n);
            baseline.initialize_genesis(&target);
            baseline
                .attempts
                .bind_vote(&target, &target, BlockHash::new([1; 32]))
                .unwrap();
            baseline.neither_tracker.record_vote(&target, Vote::Neither);
            baseline.penalized_verifiers.insert(target, 900);
        }
        assert!(baseline.visit_record_changes(|_, _| Ok(())).is_err());
        baseline.begin_record_changes();
        let original = entries(&baseline);
        let mut rows: BTreeMap<_, _> = original.clone().into_iter().collect();
        assert_eq!(apply(&baseline, &mut rows), 0);
        let mut candidate = baseline.clone();
        candidate.states.remove(&address(0));
        candidate.attempts.clear(&address(0));
        candidate.states.get_mut(&address(1)).unwrap().phase = VerificationPhase::Unverified;
        candidate.initialize_genesis(&address(200));
        candidate.neither_tracker.reset(&address(0));
        candidate.penalized_verifiers.remove(&address(0));
        candidate
            .neither_tracker
            .record_vote(&address(1), Vote::Legitimate);
        candidate
            .pending_events
            .push(VerificationEvent::WalletUnverified { wallet: address(1) });
        let mut durable = candidate.storage_clone();
        assert!(candidate.shares_record_storage_with(&durable));
        assert!(durable.drain_events().is_empty());
        assert_eq!(candidate.pending_events.len(), 1);
        assert!(durable.visit_record_changes(|_, _| Ok(())).is_err());
        assert_eq!(
            durable.snapshot().encode().unwrap(),
            candidate.snapshot().encode().unwrap()
        );
        assert!(candidate.record_scope_matches(&baseline));
        let wrong = VerificationOrchestrator::restore(baseline.snapshot());
        assert!(!candidate.record_scope_matches(&wrong));
        assert!(candidate
            .visit_record_delta(&wrong, |_, _, _| panic!("wrong baseline emitted rows"))
            .is_err());
        let baseline_rows: BTreeMap<_, _> = original.clone().into_iter().collect();
        candidate
            .visit_record_delta(&baseline, |key, old, _| {
                let old = old.map(|record| record.storage_entry().unwrap().1);
                assert_eq!(old.as_ref(), baseline_rows.get(&key));
                Ok(())
            })
            .unwrap();
        assert!(candidate
            .record_at_storage_key("verification_state/v1/1/FF")
            .is_err());
        assert!(candidate
            .record_at_storage_key("verification_state/v1/3/00")
            .is_err());
        assert_eq!(
            candidate.snapshot().encoded_size().unwrap() as usize,
            candidate.snapshot().encode().unwrap().len()
        );
        assert_eq!(apply(&candidate, &mut rows), 5);
        assert_eq!(entries(&baseline), original);
        assert_eq!(
            apply(&baseline, &mut original.clone().into_iter().collect()),
            0
        );
        let mut calls = 0;
        assert!(candidate
            .visit_record_changes(|_, _| {
                calls += 1;
                Err("stop".into())
            })
            .is_err());
        assert_eq!(calls, 1);
        assert_eq!(apply(&candidate, &mut rows), 5); // Failure did not consume the scope.
        candidate.begin_record_changes();
        assert_eq!(apply(&candidate, &mut rows), 0);
        candidate.cleanup_expired_penalties(901);
        candidate.neither_tracker.configure(5001, 0);
        assert_eq!(apply(&candidate, &mut rows), 128); // Rules + 127 affected verifier rows.
        candidate.begin_record_changes();
        candidate.neither_tracker.configure(5001, 0);
        assert_eq!(apply(&candidate, &mut rows), 0);
    }
}

#[cfg(test)]
pub(crate) fn assert_normalized_roundtrip(state: &VerificationOrchestrator) {
    let expected = state.snapshot().encode().unwrap();
    let mut reader = normalized::StateReader::new(expected.len() as u64);
    let mut short = normalized::StateReader::new(expected.len() as u64 - 1);
    let mut rejected = false;
    normalized::visit_state_rows(state, |key, bytes| {
        reader.push(&key, &bytes)?;
        if !rejected { rejected = short.push(&key, &bytes).is_err(); }
        Ok(())
    }).unwrap();
    assert_eq!(reader.finish().unwrap().snapshot().encode().unwrap(), expected);
    assert!(rejected || short.finish().is_err());
}

#[cfg(test)]
fn assert_visitor_roundtrip(state: &VerificationOrchestrator, normalized_rows: bool) {
    let limit = state.snapshot().encoded_size().unwrap();
    let rules = state.rules_record();
    let mut expected = BTreeMap::new();
    state.visit_records(|record| {
        let key = record.key()?;
        let fragment = record.into_snapshot_fragment(&rules, limit)?.encode().unwrap();
        expected.insert(key, fragment);
        Ok(())
    }).unwrap();
    for budget in [limit, limit - 1] {
        let mut seen = BTreeMap::new();
        let mut callback = |key: &str, snapshot: &OrchestratorSnapshot| {
            assert!(seen.insert(key.to_owned(), snapshot.encode().unwrap()).is_none());
            Ok(())
        };
        let result = if normalized_rows {
            let mut reader = normalized::StateVisitor::new(budget, &mut callback);
            normalized::visit_state_rows(state, |key, bytes| reader.push(&key, &bytes))
                .and_then(|_| reader.finish())
        } else {
            let mut reader = VerificationRecordVisitor::new(&mut callback);
            state.visit_records(|record| reader.push_bounded(record, budget))
                .and_then(|_| reader.finish())
        };
        if budget == limit { result.unwrap(); assert_eq!(seen, expected); }
        else { assert!(result.is_err()); }
    }
}
