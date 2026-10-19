//! Target and verifier records for incremental durable verification state.
//! Decoding records does not authenticate them or authorize publication.

use super::*;
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

/// One key-bound record. Its fields cannot be changed independently of its key.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerificationRecord(Record);

impl VerificationRecord {
    pub const STORAGE_PREFIX: &'static str = "verification_state/v1/";

    fn key(&self) -> Result<String, String> {
        let (kind, address) = match &self.0 {
            Record::Rules { .. } => return Ok(format!("{}0", Self::STORAGE_PREFIX)),
            Record::Target(target) => (1, &target.address),
            Record::Verifier { address, .. } => (2, address),
        };
        if !address.is_valid() {
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
        visit(self.rules_record())?;
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
            visit(
                self.target_record(&WalletAddress::new(target))
                    .ok_or("missing verification target record")?,
            )?;
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
            visit(
                self.verifier_record(&WalletAddress::new(verifier))
                    .ok_or("missing verification verifier record")?,
            )?;
        }
        Ok(())
    }
}

/// Builds one consistent engine. Any rejected row poisons the reader.
pub struct VerificationRecordReader {
    state: VerificationOrchestrator,
    rules: bool,
    targets: HashSet<WalletAddress>,
    verifiers: HashSet<WalletAddress>,
    failed: bool,
}

impl Default for VerificationRecordReader {
    fn default() -> Self {
        Self {
            state: VerificationOrchestrator::new(),
            rules: false,
            targets: HashSet::new(),
            verifiers: HashSet::new(),
            failed: false,
        }
    }
}

impl VerificationRecordReader {
    pub fn push(&mut self, record: VerificationRecord) -> Result<(), String> {
        if self.failed {
            return Err("verification reader already failed".into());
        }
        let result = self.push_inner(record);
        self.failed |= result.is_err();
        result
    }

    fn push_inner(&mut self, record: VerificationRecord) -> Result<(), String> {
        record.key()?;
        if bincode::serialized_size(&(1u8, &record)).map_err(|e| e.to_string())?
            > burst_types::MAX_P2P_MESSAGE_BYTES as u64
        {
            return Err("verification record exceeds row limit".into());
        }
        match record.0 {
            Record::Rules { threshold, window } => {
                if self.rules {
                    return Err("duplicate verification rules".into());
                }
                self.rules = true;
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
        if self.failed || !self.rules {
            return Err("incomplete verification records".into());
        }
        self.state
            .neither_tracker
            .validate()
            .map_err(|e| e.to_string())?;
        super::appeal::validate_appeals(
            &self.state.active_appeals,
            &self.state.attempts,
            &self.state.active_challenges,
            &self.state.states,
        )
        .map_err(|e| e.to_string())?;
        Ok(self.state)
    }
}

#[cfg(test)]
pub(super) fn assert_record_roundtrip(state: &VerificationOrchestrator) {
    let mut reader = VerificationRecordReader::default();
    state
        .visit_records(|record| {
            let (key, bytes) = record.storage_entry()?;
            reader.push(VerificationRecord::from_storage_entry(&key, &bytes)?)
        })
        .unwrap();
    assert_eq!(
        reader.finish().unwrap().snapshot().encode().unwrap(),
        state.snapshot().encode().unwrap()
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
            state.attempts.votes.insert(address.clone(), HashMap::new());
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
}
