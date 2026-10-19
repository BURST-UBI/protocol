use super::*;
use bincode::Options;
use burst_consensus::{
    checkpoint::{CheckpointAnchor, CheckpointCertificate},
    snapshot::SnapshotAuthorityStep,
};
use serde::{Deserialize, Serialize};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

/// Temporary proof storage, rebuilt from durable records on restart. It carries
/// certificates, never deserialized trusted anchors or pruning authority.
pub(in crate::confirmed_replay::snapshot) struct AuthorityHistory {
    file: Arc<Mutex<std::fs::File>>,
    base: CheckpointAnchor,
    tip: CheckpointAnchor,
    bytes: u64,
    max_frame: u64,
}

struct ProofReader {
    file: Arc<Mutex<std::fs::File>>,
    offset: u64,
}

impl Read for ProofReader {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let mut file = self
            .file
            .lock()
            .map_err(|_| std::io::Error::other("authority file lock poisoned"))?;
        file.seek(SeekFrom::Start(self.offset))?;
        let count = file.read(bytes)?;
        self.offset = self
            .offset
            .checked_add(count as u64)
            .ok_or_else(|| std::io::Error::other("authority read offset overflow"))?;
        Ok(count)
    }
}

#[derive(Serialize, Deserialize)]
enum Entry {
    Unchanged(CheckpointCertificate),
    Changed(SnapshotAuthorityStep),
}

impl Entry {
    fn authenticate(&self, parent: &CheckpointAnchor) -> Result<CheckpointAnchor, StoreError> {
        match self {
            Self::Unchanged(certificate) => parent
                .advance(certificate, parent.roster().clone())
                .map_err(invalid),
            Self::Changed(step) => step.authenticate(parent).map_err(invalid),
        }
    }
}

impl AuthorityHistory {
    pub(super) fn fork(&self, directory: &std::path::Path) -> Result<Self, StoreError> {
        let mut source = self.file.lock().map_err(invalid)?;
        if source.metadata().map_err(invalid)?.len() != self.bytes {
            return Err(invalid("recovery authority file changed before fork"));
        }
        let mut copy = tempfile::tempfile_in(directory).map_err(invalid)?;
        source.seek(SeekFrom::Start(0)).map_err(invalid)?;
        if std::io::copy(&mut *source, &mut copy).map_err(invalid)? != self.bytes {
            return Err(invalid("recovery authority file changed during fork"));
        }
        Ok(Self {
            file: Arc::new(Mutex::new(copy)),
            base: self.base.clone(),
            tip: self.tip.clone(),
            bytes: self.bytes,
            max_frame: self.max_frame,
        })
    }

    pub(in crate::confirmed_replay::snapshot) fn new(
        base: CheckpointAnchor,
        directory: &std::path::Path,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            file: Arc::new(Mutex::new(
                tempfile::tempfile_in(directory).map_err(invalid)?,
            )),
            tip: base.clone(),
            base,
            bytes: 0,
            max_frame: 0,
        })
    }

    pub(in crate::confirmed_replay::snapshot) fn append(
        &mut self,
        certificate: &CheckpointCertificate,
        next: &CheckpointAnchor,
    ) -> Result<(), StoreError> {
        let entry = if self.tip.roster().commitment() == next.roster().commitment() {
            Entry::Unchanged(certificate.clone())
        } else {
            Entry::Changed(SnapshotAuthorityStep {
                certificate: certificate.clone(),
                roster: next.roster().humans().to_vec(),
            })
        };
        let authenticated = entry.authenticate(&self.tip)?;
        if authenticated.hash() != next.hash() {
            return Err(invalid("recovery authority proof differs from replay"));
        }
        let length = bincode::serialized_size(&entry).map_err(invalid)?;
        let end = self
            .bytes
            .checked_add(8)
            .and_then(|n| n.checked_add(length))
            .ok_or_else(|| invalid("recovery authority file length overflow"))?;
        let mut file = self.file.lock().map_err(invalid)?;
        if file.metadata().map_err(invalid)?.len() != self.bytes {
            return Err(invalid("recovery authority file changed before append"));
        }
        file.seek(SeekFrom::Start(self.bytes)).map_err(invalid)?;
        file.write_all(&length.to_be_bytes()).map_err(invalid)?;
        bincode::serialize_into(&mut *file, &entry).map_err(invalid)?;
        file.flush().map_err(invalid)?;
        self.bytes = end;
        self.max_frame = self.max_frame.max(length);
        self.tip = authenticated;
        Ok(())
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = Result<CheckpointAnchor, StoreError>> {
        self.proofs().map(|proof| proof.map(|(_, anchor)| anchor))
    }

    pub(in crate::confirmed_replay::snapshot) fn step_count(&self) -> Result<usize, StoreError> {
        usize::try_from(self.tip.height() - self.base.height()).map_err(invalid)
    }

    pub(in crate::confirmed_replay::snapshot) fn steps(
        &self,
    ) -> impl Iterator<Item = Result<SnapshotAuthorityStep, StoreError>> {
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
    ) -> impl Iterator<Item = Result<(Option<CheckpointCertificate>, CheckpointAnchor), StoreError>>
    {
        let opened = self.file.lock().map_err(invalid).and_then(|file| {
            if file.metadata().map_err(invalid)?.len() != self.bytes {
                return Err(invalid("recovery authority file length changed"));
            }
            Ok(BufReader::new(ProofReader {
                file: Arc::clone(&self.file),
                offset: 0,
            }))
        });
        let mut reader = opened;
        let mut current = self.base.clone();
        let expected = (self.tip.height(), self.tip.hash());
        let max_frame = self.max_frame;
        let mut remaining = self.bytes;
        let mut first = true;
        let mut finished = false;
        std::iter::from_fn(move || {
            if finished {
                return None;
            }
            let result = (|| {
                let reader = reader.as_mut().map_err(invalid)?;
                if first {
                    first = false;
                    return Ok(Some((None, current.clone())));
                }
                if remaining == 0 {
                    if (current.height(), current.hash()) != expected {
                        return Err(invalid("recovery authority history does not reach its tip"));
                    }
                    return Ok(None);
                }
                let mut encoded_length = [0; 8];
                reader.read_exact(&mut encoded_length).map_err(invalid)?;
                let length = u64::from_be_bytes(encoded_length);
                remaining = remaining
                    .checked_sub(8)
                    .and_then(|n| n.checked_sub(length))
                    .filter(|_| length > 0 && length <= max_frame)
                    .ok_or_else(|| invalid("invalid recovery authority frame length"))?;
                let mut frame = reader.take(length);
                let entry: Entry = bincode::DefaultOptions::new()
                    .with_fixint_encoding()
                    .with_limit(length)
                    .deserialize_from(&mut frame)
                    .map_err(invalid)?;
                if frame.limit() != 0 {
                    return Err(invalid("trailing recovery authority frame data"));
                }
                current = entry.authenticate(&current)?;
                let certificate = match entry {
                    Entry::Unchanged(certificate) => certificate,
                    Entry::Changed(step) => step.certificate,
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
        let key = burst_crypto::keypair_from_seed(&[signer; 32]);
        let next_address = burst_crypto::derive_address(
            &burst_crypto::keypair_from_seed(&[next_signer; 32]).public,
        );
        let roster = HumanRoster::new(vec![HumanDelegation {
            human: next_address.clone(),
            representative: next_address,
        }])
        .unwrap();
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
        let next = parent.advance(&certificate, roster).unwrap();
        history.append(&certificate, &next).unwrap();
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
        let mut fork = history.fork(directory.path()).unwrap();
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
        let mut original = Vec::new();
        ProofReader {
            file: Arc::clone(&history.file),
            offset: 0,
        }
        .read_to_end(&mut original)
        .unwrap();
        {
            let mut file = history.file.lock().unwrap();
            file.seek(SeekFrom::Start(0)).unwrap();
            file.write_all(&u64::MAX.to_be_bytes()).unwrap();
        }
        assert!(history.iter().collect::<Result<Vec<_>, _>>().is_err());
        assert_eq!(fork.iter().collect::<Result<Vec<_>, _>>().unwrap().len(), 5);
        {
            let mut file = history.file.lock().unwrap();
            file.seek(SeekFrom::Start(0)).unwrap();
            file.write_all(&original).unwrap();
        }
        assert!(history.iter().collect::<Result<Vec<_>, _>>().is_ok());
        {
            let mut file = history.file.lock().unwrap();
            file.seek(SeekFrom::Start(history.bytes - 1)).unwrap();
            file.write_all(&[original.last().unwrap() ^ 1]).unwrap();
        }
        assert!(history.iter().collect::<Result<Vec<_>, _>>().is_err());
        history
            .file
            .lock()
            .unwrap()
            .set_len(history.bytes - 1)
            .unwrap();
        let mut damaged = history.iter();
        assert!(damaged.next().unwrap().is_err());
        assert!(damaged.next().is_none());
        assert!(history.fork(directory.path()).is_err());
    }
}
