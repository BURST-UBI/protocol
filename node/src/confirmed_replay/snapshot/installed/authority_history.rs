use super::*;
use crate::confirmed_replay::scratch_records::ScratchSequence;
use burst_consensus::{
    checkpoint::{CheckpointAnchor, CheckpointCertificate, HumanDelegation},
    snapshot::SnapshotAuthorityStep,
};
use serde::{Deserialize, Serialize};

/// Temporary proof storage, rebuilt from durable records on restart. It carries
/// certificates, never deserialized trusted anchors or pruning authority.
pub(in crate::confirmed_replay::snapshot) struct AuthorityHistory {
    entries: ScratchSequence<Entry>,
    base: CheckpointAnchor,
    tip: CheckpointAnchor,
}

#[derive(Serialize, Deserialize)]
pub(crate) enum Entry {
    Unchanged(CheckpointCertificate),
    Changed { certificate: CheckpointCertificate, changes: Vec<RosterChange> },
    Full(SnapshotAuthorityStep),
}

// Shared with archive framing version 4. Changing this encoding requires a new
// archive version; decoded entries still require certificate authentication.
#[derive(Serialize, Deserialize)]
pub(crate) struct RosterChange {
    human: burst_types::WalletAddress,
    representative: Option<burst_types::WalletAddress>,
}

fn roster_changes(previous: &[HumanDelegation], next: &[HumanDelegation]) -> Result<Option<Vec<RosterChange>>, StoreError> {
    let full_bytes = bincode::serialized_size(next).map_err(invalid)?;
    let mut changes = Vec::new();
    let mut bytes = bincode::serialized_size(&changes).map_err(invalid)?;
    let mut push = |human: &burst_types::WalletAddress, representative: Option<&burst_types::WalletAddress>| {
        let length = bincode::serialized_size(&(human, representative)).map_err(invalid)?;
        bytes = bytes.checked_add(length).ok_or_else(|| invalid("roster change size overflow"))?;
        // Use the full roster when the encoded delta would be as large or larger.
        if bytes >= full_bytes { return Ok::<_, StoreError>(false); }
        changes.push(RosterChange { human: human.clone(), representative: representative.cloned() });
        Ok(true)
    };
    let mut next = next.iter().peekable();
    for old in previous {
        while next.peek().is_some_and(|row| row.human.as_str() < old.human.as_str()) {
            let row = next.next().expect("checked next roster row");
            if !push(&row.human, Some(&row.representative))? { return Ok(None); }
        }
        if next.peek().is_some_and(|row| row.human == old.human) {
            let row = next.next().expect("matching next roster row");
            if row.representative != old.representative {
                if !push(&row.human, Some(&row.representative))? { return Ok(None); }
            }
        } else {
            if !push(&old.human, None)? { return Ok(None); }
        }
    }
    for row in next {
        if !push(&row.human, Some(&row.representative))? { return Ok(None); }
    }
    Ok(Some(changes))
}

fn changed_roster(previous: &[HumanDelegation], changes: &[RosterChange]) -> Result<Vec<HumanDelegation>, StoreError> {
    let mut rows = Vec::with_capacity(previous.len());
    let mut previous = previous.iter().peekable();
    let mut last: Option<&burst_types::WalletAddress> = None;
    for change in changes {
        if last.is_some_and(|last| last.as_str() >= change.human.as_str()) {
            return Err(invalid("unordered recovery roster changes"));
        }
        last = Some(&change.human);
        while previous.peek().is_some_and(|row| row.human.as_str() < change.human.as_str()) {
            rows.push(previous.next().expect("checked previous roster row").clone());
        }
        let old = if previous.peek().is_some_and(|row| row.human == change.human) {
            previous.next()
        } else { None };
        match &change.representative {
            Some(representative) => {
                if old.is_some_and(|row| row.representative == *representative) {
                    return Err(invalid("redundant recovery roster change"));
                }
                rows.push(HumanDelegation { human: change.human.clone(), representative: representative.clone() });
            }
            None if old.is_none() => return Err(invalid("recovery roster removes missing human")),
            None => {}
        }
    }
    rows.extend(previous.cloned());
    Ok(rows)
}

impl Entry {
    pub(crate) fn between(parent: &CheckpointAnchor, certificate: &CheckpointCertificate,
        next: &CheckpointAnchor) -> Result<Self, StoreError> {
        let entry = if parent.roster().commitment() == next.roster().commitment() {
            Self::Unchanged(certificate.clone())
        } else {
            match roster_changes(parent.roster().humans(), next.roster().humans())? {
                Some(changes) => Self::Changed { certificate: certificate.clone(), changes },
                None => Self::Full(SnapshotAuthorityStep {
                    certificate: certificate.clone(), roster: next.roster().humans().to_vec(),
                }),
            }
        };
        Ok(entry)
    }

    pub(crate) fn certificate(&self) -> &CheckpointCertificate {
        match self {
            Self::Unchanged(certificate) | Self::Changed { certificate, .. } => certificate,
            Self::Full(step) => &step.certificate,
        }
    }

    pub(crate) fn authenticate(&self, parent: &CheckpointAnchor) -> Result<CheckpointAnchor, StoreError> {
        match self {
            Self::Unchanged(certificate) => parent
                .advance(certificate, parent.roster().clone())
                .map_err(invalid),
            Self::Changed { certificate, changes } => {
                // Verify the certificate before applying deltas; authentication
                // below checks the resulting roster against its commitment.
                parent.verify_certificate(certificate).map_err(invalid)?;
                let roster = changed_roster(parent.roster().humans(), changes)?;
                SnapshotAuthorityStep { certificate: certificate.clone(), roster }
                    .authenticate(parent).map_err(invalid)
            }
            Self::Full(step) => step.authenticate(parent).map_err(invalid),
        }
    }
}

impl AuthorityHistory {
    #[cfg(test)]
    pub(super) fn base(&self) -> &CheckpointAnchor {
        &self.base
    }

    pub(super) fn fork(&self) -> Result<Self, StoreError> {
        Ok(Self {
            entries: self.entries.fork()?,
            base: self.base.clone(),
            tip: self.tip.clone(),
        })
    }

    pub(in crate::confirmed_replay::snapshot) fn new(
        base: CheckpointAnchor,
        directory: &std::path::Path,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            entries: ScratchSequence::new(directory)?,
            tip: base.clone(),
            base,
        })
    }

    pub(in crate::confirmed_replay::snapshot) fn append(
        &mut self,
        certificate: &CheckpointCertificate,
        next: &CheckpointAnchor,
    ) -> Result<(), StoreError> {
        let entry = Entry::between(&self.tip, certificate, next)?;
        let authenticated = entry.authenticate(&self.tip)?;
        if authenticated.hash() != next.hash() {
            return Err(invalid("recovery authority proof differs from replay"));
        }
        self.entries.push(&entry)?;
        self.tip = authenticated;
        Ok(())
    }

    pub(in crate::confirmed_replay::snapshot) fn encoded_entries(
        &self, base: &CheckpointAnchor,
    ) -> Box<dyn Iterator<Item = Result<Entry, StoreError>> + '_> {
        let identity = |anchor: &CheckpointAnchor| (anchor.network(), anchor.genesis_hash(),
            anchor.hash(), anchor.height(), anchor.cutoff(), anchor.roster().commitment(), anchor.quorum_bps());
        if identity(base) != identity(&self.base) {
            return Box::new(std::iter::once(Err(invalid("authority delta base differs"))));
        }
        if let Err(error) = self.entries.check_storage() {
            return Box::new(std::iter::once(Err(error)));
        }
        Box::new(self.entries.iter())
    }

    pub(in crate::confirmed_replay::snapshot) fn authenticated_steps(
        &self, base: &CheckpointAnchor,
    ) -> Box<dyn Iterator<Item = Result<(CheckpointCertificate, CheckpointAnchor), StoreError>> + '_> {
        let identity = |anchor: &CheckpointAnchor| (anchor.network(), anchor.genesis_hash(),
            anchor.hash(), anchor.height(), anchor.cutoff(), anchor.roster().commitment(), anchor.quorum_bps());
        if identity(base) != identity(&self.base) {
            return Box::new(std::iter::once(Err(invalid("authority proof base differs"))));
        }
        Box::new(self.proofs().filter_map(|proof| match proof {
            Ok((Some(certificate), anchor)) => Some(Ok((certificate, anchor))),
            Ok((None, _)) => None,
            Err(error) => Some(Err(error)),
        }))
    }

    pub(in crate::confirmed_replay::snapshot) fn append_entry(
        &mut self, entry: Entry,
    ) -> Result<CheckpointAnchor, StoreError> {
        let next = entry.authenticate(&self.tip)?;
        self.entries.push(&entry)?;
        self.tip = next.clone();
        Ok(next)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = Result<CheckpointAnchor, StoreError>> + '_ {
        self.proofs().map(|proof| proof.map(|(_, anchor)| anchor))
    }

    pub(in crate::confirmed_replay::snapshot) fn step_count(&self) -> Result<usize, StoreError> {
        usize::try_from(self.tip.height() - self.base.height()).map_err(invalid)
    }

    pub(in crate::confirmed_replay::snapshot) fn steps(
        &self,
    ) -> impl Iterator<Item = Result<SnapshotAuthorityStep, StoreError>> + '_ {
        self.proofs().filter_map(|proof| match proof {
            Ok((Some(certificate), anchor)) => Some(Ok(SnapshotAuthorityStep {
                certificate,
                roster: anchor.roster().humans().to_vec(),
            })),
            Ok((None, _)) => None,
            Err(error) => Some(Err(error)),
        })
    }

    fn proofs(
        &self,
    ) -> impl Iterator<Item = Result<(Option<CheckpointCertificate>, CheckpointAnchor), StoreError>> + '_
    {
        let opened = self.entries.check_storage();
        let mut entries = self.entries.iter();
        let mut current = self.base.clone();
        let expected = (self.tip.height(), self.tip.hash());
        let mut first = true;
        let mut finished = false;
        std::iter::from_fn(move || {
            if finished {
                return None;
            }
            let result = (|| {
                opened.as_ref().map_err(invalid)?;
                if first {
                    first = false;
                    return Ok(Some((None, current.clone())));
                }
                let Some(entry) = entries.next() else {
                    if (current.height(), current.hash()) != expected {
                        return Err(invalid("recovery authority history does not reach its tip"));
                    }
                    return Ok(None);
                };
                let entry = entry?;
                current = entry.authenticate(&current)?;
                let certificate = match entry {
                    Entry::Unchanged(certificate) => certificate,
                    Entry::Changed { certificate, .. } => certificate,
                    Entry::Full(step) => step.certificate,
                };
                Ok(Some((Some(certificate), current.clone())))
            })();
            match result {
                Ok(Some(anchor)) => Some(Ok(anchor)),
                Ok(None) => {
                    finished = true;
                    None
                }
                Err(error) => {
                    finished = true;
                    Some(Err(error))
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_consensus::checkpoint::{
        Checkpoint, CheckpointSignature, HumanDelegation, HumanRoster,
    };
    use burst_types::BlockHash;

    fn advance(history: &mut AuthorityHistory, signer: u8, next_signer: u8) {
        let next_address = burst_crypto::derive_address(
            &burst_crypto::keypair_from_seed(&[next_signer; 32]).public,
        );
        let roster = HumanRoster::new(vec![HumanDelegation {
            human: next_address.clone(),
            representative: next_address,
        }])
        .unwrap();
        advance_roster(history, signer, roster);
    }

    fn advance_roster(history: &mut AuthorityHistory, signer: u8, roster: HumanRoster) {
        let key = burst_crypto::keypair_from_seed(&[signer; 32]);
        let parent = &history.tip;
        let checkpoint = Checkpoint {
            network: parent.network(),
            genesis: parent.genesis_hash(),
            parent: parent.hash(),
            height: parent.height() + 1,
            cutoff: parent.cutoff() + 1,
            ledger_root: BlockHash::new([2; 32]),
            params_hash: BlockHash::new([3; 32]),
            selection_root: BlockHash::new([4; 32]),
            next_roster: roster.commitment(),
            next_quorum_bps: parent.quorum_bps(),
        };
        let payload =
            burst_consensus::checkpoint_round::Ballot::precommit(&checkpoint, 0).signing_payload();
        let certificate = CheckpointCertificate {
            checkpoint,
            round: 0,
            signatures: vec![CheckpointSignature {
                representative: burst_crypto::derive_address(&key.public),
                signature: burst_crypto::sign_message(&payload, &key.private),
            }],
        };
        let expected = SnapshotAuthorityStep { certificate: certificate.clone(), roster: roster.humans().to_vec() };
        let next = parent.advance(&certificate, roster).unwrap();
        history.append(&certificate, &next).unwrap();
        let exported = history.steps().last().unwrap().unwrap();
        assert_eq!(bincode::serialize(&exported).unwrap(), bincode::serialize(&expected).unwrap());
        let stored = history.entries.iter().last().unwrap().unwrap();
        assert!(bincode::serialized_size(&stored).unwrap() <= 4 + bincode::serialized_size(&expected).unwrap());
    }

    fn human(number: u32) -> burst_types::WalletAddress {
        let mut seed = [0; 32];
        seed[..4].copy_from_slice(&number.to_le_bytes());
        burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&seed).public)
    }

    #[test]
    fn roster_deltas_preserve_membership_and_reject_invalid_edits() {
        let roster = |numbers: &[u32], representative| HumanRoster::new(numbers.iter().map(|&n|
            HumanDelegation { human: human(n), representative: human(representative) }).collect()).unwrap();
        let previous = roster(&[1, 2, 3, 4, 5, 6, 7, 8], 0);
        let mut next = roster(&[2, 3, 5, 6, 7, 8, 9], 0).humans().to_vec();
        next.iter_mut().find(|row| row.human == human(3)).unwrap().representative = human(9);
        let next = HumanRoster::new(next).unwrap();
        let changes = roster_changes(previous.humans(), next.humans()).unwrap().unwrap();
        assert_eq!(changes.len(), 4); // Two removals, one addition, one redelegation.
        assert_eq!(changed_roster(previous.humans(), &changes).unwrap(), next.humans());
        assert!(roster_changes(next.humans(), next.humans()).unwrap().unwrap().is_empty());
        assert_eq!(changed_roster(next.humans(), &[]).unwrap(), next.humans());

        let duplicate = RosterChange { human: human(9), representative: Some(human(0)) };
        assert!(changed_roster(previous.humans(), &[duplicate, RosterChange {
            human: human(9), representative: Some(human(9)),
        }]).is_err());
        let mut reversed = roster_changes(previous.humans(), next.humans()).unwrap().unwrap();
        reversed.reverse();
        assert!(changed_roster(previous.humans(), &reversed).is_err());
        assert!(changed_roster(previous.humans(), &[RosterChange {
            human: human(99), representative: None,
        }]).is_err());
        assert!(changed_roster(previous.humans(), &[RosterChange {
            human: human(1), representative: Some(human(0)),
        }]).is_err());
        let remove_all: Vec<_> = previous.humans().iter().map(|row| RosterChange {
            human: row.human.clone(), representative: None,
        }).collect();
        assert!(HumanRoster::new(changed_roster(previous.humans(), &remove_all).unwrap()).is_err());
    }

    #[test]
    fn authority_uses_full_rosters_for_replacement_and_shrink() {
        let base = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
            NetworkId::Dev, &ProtocolParams::default(),
        ).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let mut history = AuthorityHistory::new(base, directory.path()).unwrap();
        for (numbers, representative) in [(1..513, 0), (1001..1513, 0), (2001..2002, 0), (2001..2002, 9)] {
            let roster = HumanRoster::new(numbers.map(|number| HumanDelegation {
                human: human(number), representative: human(representative),
            }).collect()).unwrap();
            assert!(roster_changes(history.tip.roster().humans(), roster.humans()).unwrap().is_none());
            advance_roster(&mut history, 0, roster);
            assert!(matches!(history.entries.iter().last().unwrap().unwrap(), Entry::Full(_)));
        }
        assert_eq!(history.iter().last().unwrap().unwrap().hash(), history.tip.hash());
        assert_eq!(history.step_count().unwrap(), 4);
    }

    #[test]
    fn growing_authority_stores_changes_within_shared_scratch_budget() {
        let base = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
            NetworkId::Dev, &ProtocolParams::default(),
        ).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let limit = crate::confirmed_replay::scratch_file::ScratchLimit::acquire(directory.path(), 256 << 10).unwrap();
        let mut history = AuthorityHistory::new(base.clone(), directory.path()).unwrap();
        let mut rows: Vec<_> = (1..=512).map(|n| HumanDelegation {
            human: human(n), representative: human(0),
        }).collect();
        advance_roster(&mut history, 0, HumanRoster::new(rows.clone()).unwrap());
        for n in 513..=544 {
            rows.push(HumanDelegation { human: human(n), representative: human(0) });
            advance_roster(&mut history, 0, HumanRoster::new(rows.clone()).unwrap());
        }
        assert_eq!(history.step_count().unwrap(), 33);
        let mut parent = base.clone();
        for step in history.steps() {
            parent = step.unwrap().authenticate(&parent).unwrap();
        }
        assert_eq!(parent.hash(), history.tip.hash());
        assert_eq!(parent.roster().humans().len(), 544);
        let parent = history.iter().nth(1).unwrap().unwrap();
        let mut entry = history.entries.iter().nth(1).unwrap().unwrap();
        let Entry::Changed { changes, .. } = &mut entry else { panic!("changed initial roster") };
        changes.iter_mut().find(|change| change.representative.is_some()).unwrap().representative = Some(human(99));
        assert!(entry.authenticate(&parent).is_err(), "a valid delta cannot replace the certified roster");

        let mut certificate = entry.certificate().clone();
        let malformed = |certificate| Entry::Changed { certificate, changes: vec![RosterChange {
            human: human(99_991), representative: None,
        }] };
        assert_eq!(malformed(certificate.clone()).authenticate(&parent).err().unwrap().to_string(),
            invalid("recovery roster removes missing human").to_string());
        certificate.signatures.clear();
        let expected = parent.verify_certificate(&certificate).unwrap_err();
        assert_eq!(malformed(certificate).authenticate(&parent).err().unwrap().to_string(),
            invalid(expected).to_string(), "reject missing quorum before interpreting roster edits");
        assert!(limit.used_temporary_bytes().unwrap() < 256 << 10);
        drop(history);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
    }

    #[test]
    fn disk_authority_replays_roster_changes_and_rejects_damaged_frames() {
        let base = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
            NetworkId::Dev,
            &ProtocolParams::default(),
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let mut history = AuthorityHistory::new(base, directory.path()).unwrap();
        advance(&mut history, 0, 1);
        advance(&mut history, 1, 1);
        advance(&mut history, 1, 2);
        let expected: Vec<_> = history
            .iter()
            .map(|anchor| anchor.unwrap().hash())
            .collect();
        assert_eq!(expected.len(), 4);
        assert_eq!(expected[3], history.tip.hash());
        let steps = history.authenticated_steps(&history.base)
            .collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(steps.iter().map(|(_, anchor)| anchor.hash()).collect::<Vec<_>>(), expected[1..]);
        for (certificate, anchor) in steps {
            assert_eq!(certificate.checkpoint.hash(), anchor.hash());
        }
        assert!(history.authenticated_steps(&history.tip).next().unwrap().is_err());
        let mut fork = history.fork().unwrap();
        advance(&mut fork, 2, 3);
        assert_eq!(fork.iter().collect::<Result<Vec<_>, _>>().unwrap().len(), 5);
        assert_eq!(
            history.iter().collect::<Result<Vec<_>, _>>().unwrap().len(),
            4
        );
        let mut first = history.iter();
        let mut second = history.iter();
        for hash in expected {
            assert_eq!(first.next().unwrap().unwrap().hash(), hash);
            assert_eq!(second.next().unwrap().unwrap().hash(), hash);
        }
        assert!(first.next().is_none());
        assert!(second.next().is_none());
        drop(first);
        drop(second);
        // Both branches may append at the same logical position independently.
        advance(&mut history, 2, 4);
        assert_ne!(history.tip.hash(), fork.tip.hash());
        assert_eq!(history.iter().last().unwrap().unwrap().hash(), history.tip.hash());
        assert_eq!(fork.iter().last().unwrap().unwrap().hash(), fork.tip.hash());

        // Authenticated scratch bytes do not replace certificate authentication.
        let mut invalid_chain = history.fork().unwrap();
        let duplicate = history.entries.iter().next().unwrap().unwrap();
        invalid_chain.entries.push(&duplicate).unwrap();
        let mut proofs = invalid_chain.iter();
        for _ in 0..5 {
            proofs.next().unwrap().unwrap();
        }
        assert!(proofs.next().unwrap().is_err());
        assert!(proofs.next().is_none());
        assert!(invalid_chain.authenticated_steps(&invalid_chain.base)
            .collect::<Result<Vec<_>, _>>().is_err());
        assert!(history.iter().collect::<Result<Vec<_>, _>>().is_ok());

        // Shared physical damage fails closed for every branch.
        assert_eq!(history.encoded_entries(&history.base).collect::<Result<Vec<_>, _>>().unwrap().len(), 4);
        assert!(history.encoded_entries(&history.tip).next().unwrap().is_err());
        history.entries.truncate_for_test();
        assert!(history.authenticated_steps(&history.base).next().unwrap().is_err());
        assert!(history.encoded_entries(&history.base).next().unwrap().is_err());
        let mut damaged = history.iter();
        assert!(damaged.next().unwrap().is_err());
        assert!(damaged.next().is_none());
        assert!(history.fork().is_err());
        assert!(fork.iter().next().unwrap().is_err());
    }
}
