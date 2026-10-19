//! Bounded snapshot file/stream format. Authenticate the authority chain and
//! snapshot certificate before reading the economic body. Import is read-only;
//! callers must separately coordinate durable installation and cache publication.

use super::{ConfirmedReplay, PreparedSnapshot, ReplayError};
use bincode::Options;
use burst_consensus::{
    checkpoint::CheckpointAnchor,
    snapshot::{SnapshotAuthorityStep, SnapshotCertificate},
};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
mod framed;
mod source;
mod staged;
use source::{AuthoritySource, Sequence};
pub(super) use staged::Authority;

const MAGIC: &[u8; 16] = b"BURST-SNAPSHOT\0\x02";
pub(super) const MAX_HEADER_BYTES: u64 = 16 * 1024 * 1024;
const HEADER_BUFFER_BYTES: usize = 16 * 1024;

/// A transfer can authenticate a completed header before requesting more body
/// chunks. At most one chunk's body bytes can accompany the header.
pub(crate) fn authenticate_prefix(
    base: &CheckpointAnchor,
    reader: &mut impl Read,
    received: usize,
    epoch: burst_types::BlockHash,
    total: usize,
    directory: &std::path::Path,
) -> Result<Option<u64>, ReplayError> {
    if received < 24 {
        return Ok(None);
    }
    let mut prefix = [0; 24];
    reader.read_exact(&mut prefix).map_err(invalid)?;
    if &prefix[..16] != MAGIC && &prefix[..16] != framed::MAGIC {
        return Err(invalid("unsupported snapshot archive format"));
    }
    let length = u64::from_be_bytes(prefix[16..24].try_into().map_err(invalid)?);
    if length == 0 || (&prefix[..16] == MAGIC && length > MAX_HEADER_BYTES) {
        return Err(invalid("snapshot archive header exceeds its limit"));
    }
    let end = usize::try_from(length)
        .ok()
        .and_then(|length| length.checked_add(24))
        .ok_or_else(|| invalid("snapshot header length overflow"))?;
    if total <= end {
        return Err(invalid("snapshot archive has no state body"));
    }
    if received < end {
        return Ok(None);
    }
    let (authority, certificate, beacons) = if &prefix[..16] == MAGIC {
        staged::read_header(base, reader, length, directory)?
    } else {
        framed::read(base, reader, length, directory)?
    };
    let anchor = authenticate(base, &authority, &certificate)?;
    validate_beacon_bounds(authority.count()?, &beacons)?;
    if anchor.hash() != epoch
        || (end as u64).checked_add(certificate.manifest.state_bytes) != Some(total as u64)
    {
        return Err(invalid(
            "snapshot header differs from requested epoch or length",
        ));
    }
    Ok(Some(anchor.height()))
}

#[derive(Serialize, Deserialize)]
struct Header {
    authority: Vec<SnapshotAuthorityStep>,
    certificate: SnapshotCertificate,
    beacons: Vec<(burst_types::BlockHash, burst_vrf::RandomOutput)>,
}

fn validate_beacon_bounds(
    authority_len: usize,
    beacons: &[(burst_types::BlockHash, burst_vrf::RandomOutput)],
) -> Result<(), ReplayError> {
    if beacons.len() > authority_len {
        return Err(invalid("snapshot has too many beacon supplements"));
    }
    for (_, beacon) in beacons {
        if bincode::serialized_size(beacon).map_err(invalid)? > 4096 {
            return Err(invalid("snapshot beacon exceeds byte limit"));
        }
    }
    Ok(())
}

fn read_header(reader: &mut impl Read, length: u64) -> Result<Header, ReplayError> {
    if length == 0 || length > MAX_HEADER_BYTES {
        return Err(invalid("snapshot archive header exceeds its limit"));
    }
    let mut framed = std::io::BufReader::with_capacity(HEADER_BUFFER_BYTES, reader.take(length));
    let header = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(length)
        .deserialize_from(&mut framed)
        .map_err(invalid)?;
    if !framed.buffer().is_empty() || framed.get_ref().limit() != 0 {
        return Err(invalid("trailing snapshot header data"));
    }
    Ok(header)
}

fn invalid(error: impl ToString) -> ReplayError {
    ReplayError::Invalid(error.to_string())
}

fn authenticate<A: AuthoritySource + ?Sized>(
    base: &CheckpointAnchor,
    authority: &A,
    certificate: &SnapshotCertificate,
) -> Result<CheckpointAnchor, ReplayError> {
    let mut anchor = base.clone();
    for step in authority.steps() {
        anchor = step?.authenticate(&anchor).map_err(invalid)?;
    }
    certificate.verify(&anchor).map_err(invalid)?;
    Ok(anchor)
}

/// A verified archive is not an installed node and grants no pruning authority.
/// The base must come from local configuration or an already authenticated tip.
pub struct AuthenticatedSnapshot {
    anchor: CheckpointAnchor,
    certificate: SnapshotCertificate,
    pub(super) authority_base: CheckpointAnchor,
    pub(super) authority: Authority,
    pub(super) beacons: Vec<(burst_types::BlockHash, burst_vrf::RandomOutput)>,
    pub(super) state: ConfirmedReplay,
}

/// Consumers must exhaust the stream to check the terminal snapshot certificate.
fn authority_path<'a, A: AuthoritySource + ?Sized>(
    base: &CheckpointAnchor,
    authority: &'a A,
    certificate: &'a SnapshotCertificate,
) -> impl Iterator<Item = Result<CheckpointAnchor, ReplayError>> + 'a {
    let mut current = base.clone();
    let mut steps = authority.steps();
    let mut first = true;
    let mut finished = false;
    std::iter::from_fn(move || {
        if finished {
            return None;
        }
        if first {
            first = false;
            return Some(Ok(current.clone()));
        }
        match steps.next() {
            Some(step) => {
                match step.and_then(|step| step.authenticate(&current).map_err(invalid)) {
                    Ok(next) => {
                        current = next;
                        Some(Ok(current.clone()))
                    }
                    Err(error) => {
                        finished = true;
                        Some(Err(invalid(error)))
                    }
                }
            }
            None => {
                finished = true;
                certificate
                    .verify(&current)
                    .err()
                    .map(|error| Err(invalid(error)))
            }
        }
    })
}

impl AuthenticatedSnapshot {
    pub(in crate::confirmed_replay::snapshot) fn authority_path(
        &self,
    ) -> impl Iterator<Item = Result<CheckpointAnchor, ReplayError>> + '_ {
        authority_path(&self.authority_base, &self.authority, &self.certificate)
    }

    pub(crate) fn committed_history_cursors(
        &self,
        hashes: &[burst_types::BlockHash],
    ) -> Result<Vec<super::history::HistoryCursor>, ReplayError> {
        super::SnapshotContinuation::committed_cursors_for(
            &self.anchor,
            self.history_context(),
            &self.state,
            hashes,
        )
    }
    pub(crate) fn confirmed_heads(&self) -> Vec<crate::confirmed_history::HistoryHead> {
        self.state
            .accounts
            .values()
            .filter(|account| account.block_count > 0)
            .map(|account| crate::confirmed_history::HistoryHead {
                account: account.address.clone(),
                height: account.block_count,
                hash: account.head,
            })
            .collect()
    }

    pub(super) fn history_context(&self) -> super::history::HistoryContext {
        super::history::HistoryContext {
            network: self.anchor.network(),
            genesis: self.anchor.genesis_hash(),
            epoch: self.anchor.hash(),
            state_hash: self.certificate.manifest.state_hash,
        }
    }
    /// Start bounded ancestral recovery only from a head committed by this
    /// authenticated economic state. Peers cannot choose the starting head.
    pub fn history_cursor(
        &self,
        account: &burst_types::WalletAddress,
    ) -> Result<Option<super::history::HistoryCursor>, ReplayError> {
        super::history::HistoryCursor::new(
            &self.anchor,
            self.certificate.manifest.state_hash,
            account,
            self.state.account(account),
        )
    }

    pub fn anchor(&self) -> &CheckpointAnchor {
        &self.anchor
    }

    pub fn certificate(&self) -> &SnapshotCertificate {
        &self.certificate
    }

    pub fn into_state(self) -> ConfirmedReplay {
        self.state
    }

    /// Restore frozen historical selection inputs from the authenticated body
    /// and old-roster certificates from the archive path, without ledger reads.
    /// A path starting after a needed selection cannot supply its authority.
    /// Transported beacons are checked against these frozen requests, not trusted
    /// as part of the economic body.
    pub fn recover_selections(
        &self,
    ) -> Result<Vec<crate::checkpoint_store::CertifiedCheckpoint>, ReplayError> {
        let mut selections = Vec::with_capacity(self.state.selection_receipts.len());
        self.try_visit_selections(|selection| {
            selections.push(selection);
            Ok(())
        })?;
        Ok(selections)
    }

    /// Stream authenticated selections into private staging. Callers must not
    /// publish staged rows until this returns success: a later receipt, beacon
    /// or storage failure invalidates the whole traversal.
    pub(crate) fn try_visit_selections(
        &self,
        visit: impl FnMut(crate::checkpoint_store::CertifiedCheckpoint) -> Result<(), ReplayError>,
    ) -> Result<(), ReplayError> {
        Self::visit_selection_path(
            &self.authority_base,
            &self.authority,
            &self.state,
            &self.beacons,
            visit,
        )
    }

    /// Retain one requested selection while authenticating the entire path.
    /// Finding a match never skips validation of later receipts or beacons.
    pub(crate) fn recover_selection(
        &self,
        height: u64,
    ) -> Result<Option<crate::checkpoint_store::CertifiedCheckpoint>, ReplayError> {
        let mut found = None;
        self.try_visit_selections(|selection| {
            if selection.certificate().checkpoint.height == height {
                found = Some(selection);
            }
            Ok(())
        })?;
        Ok(found)
    }

    pub(in crate::confirmed_replay) fn validate_selections(
        &self,
    ) -> Result<(), ReplayError> {
        self.try_visit_selections(|_| Ok(()))
    }

    fn visit_selection_path<A: AuthoritySource + ?Sized>(
        base: &CheckpointAnchor,
        authority: &A,
        state: &ConfirmedReplay,
        beacons: &[(burst_types::BlockHash, burst_vrf::RandomOutput)],
        mut visit: impl FnMut(crate::checkpoint_store::CertifiedCheckpoint) -> Result<(), ReplayError>,
    ) -> Result<(), ReplayError> {
        let mut parent = base.clone();
        let mut restored = 0;
        let mut next_beacon = 0;
        for step in authority.steps() {
            let step = step?;
            let next_parent = step.authenticate(&parent).map_err(invalid)?;
            if let Some(receipt) = state.selection_receipts.get(restored)? {
                if receipt.checkpoint().hash() == step.certificate.checkpoint.hash() {
                    let replay = receipt.restore(&parent).map_err(invalid)?;
                    let mut selection =
                        crate::checkpoint_store::CertifiedCheckpoint::from_compact(
                            parent.clone(),
                            step.certificate.clone(),
                            replay,
                        )
                        .map_err(invalid)?;
                    if let Some((hash, beacon)) = beacons.get(next_beacon) {
                        if *hash == step.certificate.checkpoint.hash() {
                            selection
                                .attach_compact_beacon(beacon.clone())
                                .map_err(invalid)?;
                            next_beacon += 1;
                        }
                    }
                    visit(selection)?;
                    restored += 1;
                }
            }
            parent = next_parent;
        }
        if restored != state.selection_receipts.len() {
            return Err(invalid(
                "archive authority path omits historical selection parents",
            ));
        }
        if next_beacon != beacons.len() {
            return Err(invalid("unknown, duplicate or unordered snapshot beacon"));
        }
        Ok(())
    }

    /// Read one framed archive, rejecting truncation and appended data. Network
    /// callers must supply a length-delimited stream and enforce an I/O timeout.
    pub fn read(base: &CheckpointAnchor, reader: &mut impl Read) -> Result<Self, ReplayError> {
        Self::read_with_staging(base, reader, None)
    }

    /// Decode authority one certificate at a time into anonymous storage in the
    /// caller's directory. The directory must already exist; no ledger is created.
    pub fn read_in(
        base: &CheckpointAnchor,
        reader: &mut impl Read,
        directory: &std::path::Path,
    ) -> Result<Self, ReplayError> {
        Self::read_with_staging(base, reader, Some(directory))
    }

    pub(crate) fn read_file_with_budget(
        base: &CheckpointAnchor,
        path: &std::path::Path,
        directory: &std::path::Path,
        budget: u64,
    ) -> Result<Self, ReplayError> {
        let mut file = std::fs::File::open(path).map_err(invalid)?;
        let metadata = file.metadata().map_err(invalid)?;
        if !metadata.is_file() || metadata.len() > budget {
            return Err(invalid(
                "snapshot archive exceeds local file admission bounds",
            ));
        }
        let mut input = (&mut file).take(budget);
        let snapshot = Self::read_in(base, &mut input, directory)?;
        if budget - input.limit() != metadata.len()
            || input.get_ref().metadata().map_err(invalid)?.len() != metadata.len()
        {
            return Err(invalid(
                "snapshot file length changed during authentication",
            ));
        }
        Ok(snapshot)
    }

    fn read_with_staging(
        base: &CheckpointAnchor,
        reader: &mut impl Read,
        directory: Option<&std::path::Path>,
    ) -> Result<Self, ReplayError> {
        let mut magic = [0; 16];
        reader.read_exact(&mut magic).map_err(invalid)?;
        if &magic != MAGIC && &magic != framed::MAGIC {
            return Err(invalid("unsupported snapshot archive format"));
        }
        let mut length = [0; 8];
        reader.read_exact(&mut length).map_err(invalid)?;
        let length = u64::from_be_bytes(length);
        let (authority, certificate, beacons) = if &magic == framed::MAGIC {
            framed::read(
                base,
                reader,
                length,
                directory.ok_or_else(|| {
                    invalid("framed snapshot archives require read_in with a scratch directory")
                })?,
            )?
        } else {
            match directory {
                Some(directory) => staged::read_header(base, reader, length, directory)?,
                None => {
                    let header = read_header(reader, length)?;
                    (
                        Authority::Memory(header.authority),
                        header.certificate,
                        header.beacons,
                    )
                }
            }
        };
        if length
            .checked_add(24)
            .and_then(|n| n.checked_add(certificate.manifest.state_bytes))
            .is_none()
        {
            return Err(invalid("snapshot archive length overflow"));
        }
        let anchor = authenticate(base, &authority, &certificate)?;
        validate_beacon_bounds(authority.count()?, &beacons)?;
        // Verification above bounds this authenticated length to 64 MiB. An
        // unsigned length cannot force allocation or consumption of a body.
        let body = super::body::VerifiedBody::stage(&anchor, &certificate, reader, directory)?;
        let mut trailing = [0];
        if reader.read(&mut trailing).map_err(invalid)? != 0 {
            return Err(invalid("trailing snapshot archive data"));
        }
        let state = ConfirmedReplay::from_verified_snapshot_body(&anchor, body)?;
        let snapshot = Self {
            anchor,
            certificate,
            authority_base: base.clone(),
            authority,
            beacons,
            state,
        };
        if !snapshot.beacons.is_empty() {
            snapshot.validate_selections()?;
        }
        Ok(snapshot)
    }
}

impl PreparedSnapshot {
    /// Export only after collecting signatures over this independently replayed
    /// body. The writer owns file creation/atomic rename and error cleanup.
    pub fn write_archive(
        &self,
        base: &CheckpointAnchor,
        authority: &[SnapshotAuthorityStep],
        certificate: &SnapshotCertificate,
        writer: &mut impl Write,
    ) -> Result<(), ReplayError> {
        self.write_archive_with_beacons(base, authority, certificate, &[], writer)
    }

    pub(crate) fn write_archive_with_beacons<A: AuthoritySource + ?Sized>(
        &self,
        base: &CheckpointAnchor,
        authority: &A,
        certificate: &SnapshotCertificate,
        beacons: &[(burst_types::BlockHash, burst_vrf::RandomOutput)],
        writer: &mut impl Write,
    ) -> Result<(), ReplayError> {
        write_prepared_archive(
            &self.manifest,
            (&mut std::io::Cursor::new(&self.bytes), None),
            base,
            authority,
            certificate,
            beacons,
            writer,
        )
    }
}

impl PreparedSnapshot<super::body::StagedBody> {
    pub(crate) fn write_archive_with_beacons<A: AuthoritySource + ?Sized>(
        &self,
        base: &CheckpointAnchor,
        authority: &A,
        certificate: &SnapshotCertificate,
        beacons: &[(burst_types::BlockHash, burst_vrf::RandomOutput)],
        writer: &mut impl Write,
    ) -> Result<(), ReplayError> {
        let mut file = self.bytes.0.lock().map_err(invalid)?;
        write_prepared_archive(
            &self.manifest,
            (&mut *file, Some(&self.bytes.1)),
            base,
            authority,
            certificate,
            beacons,
            writer,
        )
    }
}

fn write_prepared_archive<A: AuthoritySource + ?Sized>(
    manifest: &burst_consensus::snapshot::SnapshotManifest,
    (body, directory): (&mut impl super::body::ReadSeek, Option<&std::path::Path>),
    base: &CheckpointAnchor,
    authority: &A,
    certificate: &SnapshotCertificate,
    beacons: &[(burst_types::BlockHash, burst_vrf::RandomOutput)],
    writer: &mut impl Write,
) -> Result<(), ReplayError> {
    if certificate.manifest != *manifest {
        return Err(invalid("snapshot certificate does not attest this body"));
    }
    // Bincode encodes these fields identically to Header, without cloning
    // the caller's authority vector before its size has been checked.
    let length =
        bincode::serialized_size(&(Sequence(authority), certificate, beacons)).map_err(invalid)?;
    let streamed = length > MAX_HEADER_BYTES;
    let length = if streamed {
        framed::size(authority, certificate, beacons)?
    } else {
        length
    };
    if length
        .checked_add(24)
        .and_then(|n| n.checked_add(manifest.state_bytes))
        .is_none()
    {
        return Err(invalid("snapshot archive length overflow"));
    }
    let anchor = authenticate(base, authority, certificate)?;
    let verified = super::body::VerifiedBody::stream_in(&anchor, certificate, body, directory)?;
    validate_beacon_bounds(authority.count()?, beacons)?;
    if !beacons.is_empty() {
        let state = ConfirmedReplay::from_verified_snapshot_body(&anchor, verified)?;
        AuthenticatedSnapshot::visit_selection_path(base, authority, &state, beacons, |_| Ok(()))?;
    } else {
        drop(verified);
    }
    writer
        .write_all(if streamed { framed::MAGIC } else { MAGIC })
        .map_err(invalid)?;
    writer.write_all(&length.to_be_bytes()).map_err(invalid)?;
    {
        let mut header_writer =
            std::io::BufWriter::with_capacity(HEADER_BUFFER_BYTES, &mut *writer);
        if streamed {
            framed::write(authority, certificate, beacons, &mut header_writer)?;
        } else {
            bincode::serialize_into(
                &mut header_writer,
                &(Sequence(authority), certificate, beacons),
            )
            .map_err(invalid)?;
        }
        header_writer.flush().map_err(invalid)?;
    }
    body.rewind().map_err(invalid)?;
    let written = std::io::copy(body, writer).map_err(invalid)?;
    if written != manifest.state_bytes {
        return Err(invalid("snapshot body length changed during export"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framed_authority_header_can_cross_height_128_without_lifting_byte_bounds() {
        use burst_consensus::checkpoint::{Checkpoint, CheckpointCertificate, CheckpointSignature};
        use burst_consensus::snapshot::SnapshotManifest;
        use burst_types::{BlockHash, NetworkId, ProtocolParams};
        let base = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
            NetworkId::Dev,
            &ProtocolParams::default(),
        )
        .unwrap();
        let key = burst_crypto::keypair_from_seed(&[0; 32]);
        let representative = burst_crypto::derive_address(&key.public);
        let mut anchor = base.clone();
        let mut authority = Vec::new();
        for _ in 0..129 {
            let checkpoint = Checkpoint {
                network: anchor.network(),
                genesis: anchor.genesis_hash(),
                parent: anchor.hash(),
                height: anchor.height() + 1,
                cutoff: anchor.cutoff() + 1,
                ledger_root: BlockHash::new([2; 32]),
                params_hash: BlockHash::new([3; 32]),
                next_roster: anchor.roster().commitment(),
                selection_root: BlockHash::new([4; 32]),
                next_quorum_bps: anchor.quorum_bps(),
            };
            let payload = burst_consensus::checkpoint_round::Ballot::precommit(&checkpoint, 0)
                .signing_payload();
            let step = SnapshotAuthorityStep {
                roster: anchor.roster().humans().to_vec(),
                certificate: CheckpointCertificate {
                    checkpoint,
                    round: 0,
                    signatures: vec![CheckpointSignature {
                        representative: representative.clone(),
                        signature: burst_crypto::sign_message(&payload, &key.private),
                    }],
                },
            };
            anchor = step.authenticate(&anchor).unwrap();
            authority.push(step);
        }
        let manifest = SnapshotManifest::new(&anchor, b"body").unwrap();
        let certificate = SnapshotCertificate {
            signatures: vec![CheckpointSignature {
                representative,
                signature: burst_crypto::sign_message(&manifest.signing_payload(), &key.private),
            }],
            manifest,
        };
        let header = Header {
            authority,
            certificate,
            beacons: vec![],
        };
        let mut path = authority_path(&base, &header.authority, &header.certificate);
        for height in 0..=129 {
            assert_eq!(path.next().unwrap().unwrap().height(), height);
        }
        assert!(path.next().is_none());
        assert!(path.next().is_none());
        let mut invalid_certificate = header.certificate.clone();
        invalid_certificate.signatures.clear();
        let mut path = authority_path(&base, &header.authority, &invalid_certificate);
        for _ in 0..=129 {
            assert!(path.next().unwrap().is_ok());
        }
        assert!(path.next().unwrap().is_err());
        assert!(path.next().is_none());
        let mut broken = header.authority.clone();
        broken[64].certificate.signatures.clear();
        let mut path = authority_path(&base, &broken, &header.certificate);
        for _ in 0..=64 {
            assert!(path.next().unwrap().is_ok());
        }
        assert!(path.next().unwrap().is_err());
        assert!(path.next().is_none());
        let encoded = bincode::serialize(&header).unwrap();
        struct Fragmented<'a>(&'a [u8]);
        impl Read for Fragmented<'_> {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                assert!(
                    output.len() <= HEADER_BUFFER_BYTES,
                    "whole-header buffering requested"
                );
                let count = output.len().min(self.0.len()).min(3);
                output[..count].copy_from_slice(&self.0[..count]);
                self.0 = &self.0[count..];
                Ok(count)
            }
        }
        assert!(encoded.len() > HEADER_BUFFER_BYTES);
        let decoded = read_header(&mut Fragmented(&encoded), encoded.len() as u64).unwrap();
        assert_eq!(bincode::serialize(&decoded).unwrap(), encoded);
        let directory = tempfile::tempdir().unwrap();
        let (authority, certificate, beacons) = staged::read_header(
            &base,
            &mut Fragmented(&encoded),
            encoded.len() as u64,
            directory.path(),
        )
        .unwrap();
        assert!(matches!(authority, Authority::File(_)));
        assert_eq!(
            bincode::serialize(&(Sequence(&authority), &certificate, &beacons)).unwrap(),
            encoded
        );
        assert_eq!(
            authenticate(&base, &authority, &certificate)
                .unwrap()
                .hash(),
            anchor.hash()
        );
        drop(authority);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        let mut framed_bytes = Vec::new();
        framed::write(
            &header.authority,
            &header.certificate,
            &header.beacons,
            &mut framed_bytes,
        )
        .unwrap();
        assert_eq!(
            framed_bytes.len() as u64,
            framed::size(&header.authority, &header.certificate, &header.beacons).unwrap()
        );
        let decode = |bytes: &[u8]| {
            framed::read(&base, &mut &bytes[..], bytes.len() as u64, directory.path())
        };
        assert!(decode(&framed_bytes).is_ok());
        for range in [0..8, 8..16] {
            let mut damaged = framed_bytes.clone();
            damaged[range].copy_from_slice(&u64::MAX.to_be_bytes());
            assert!(decode(&damaged).is_err());
        }
        assert!(decode(&framed_bytes[..framed_bytes.len() - 1]).is_err());
        let mut trailing = framed_bytes.clone();
        trailing.push(0);
        assert!(decode(&trailing).is_err());
        let mut reordered = header.authority.clone();
        reordered.swap(0, 1);
        for steps in [&broken, &reordered] {
            let mut damaged = Vec::new();
            framed::write(steps, &header.certificate, &header.beacons, &mut damaged).unwrap();
            assert!(decode(&damaged).is_err());
        }
        let mut damaged = Vec::new();
        framed::write(
            &header.authority,
            &invalid_certificate,
            &header.beacons,
            &mut damaged,
        )
        .unwrap();
        assert!(decode(&damaged).is_err());
        let header_length = framed_bytes.len() as u64;
        framed_bytes.extend_from_slice(b"unread body");
        let mut input = std::io::Cursor::new(framed_bytes);
        assert!(framed::read(&base, &mut input, header_length, directory.path()).is_ok());
        assert_eq!(input.position(), header_length);
        assert!(read_header(&mut encoded.as_slice(), encoded.len() as u64 - 1).is_err());
        let mut padded = encoded.clone();
        padded.push(0);
        assert!(read_header(&mut padded.as_slice(), padded.len() as u64).is_err());
        assert_eq!(
            encoded.len() as u64,
            bincode::serialized_size(&(
                &header.authority[..],
                &header.certificate,
                &header.beacons[..]
            ))
            .unwrap()
        );
        assert!((encoded.len() as u64) < MAX_HEADER_BYTES);
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&(encoded.len() as u64).to_be_bytes());
        bytes.extend_from_slice(&encoded);
        for received in [0, 23, 24, bytes.len() - 1] {
            let mut partial = std::io::Cursor::new(&bytes[..received]);
            assert_eq!(
                authenticate_prefix(
                    &base,
                    &mut partial,
                    received,
                    anchor.hash(),
                    bytes.len() + 4,
                    directory.path(),
                )
                .unwrap(),
                None
            );
            assert!(
                partial.position() <= 24,
                "incomplete headers must not be repeatedly decoded"
            );
        }
        assert_eq!(
            authenticate_prefix(
                &base,
                &mut bytes.as_slice(),
                bytes.len(),
                anchor.hash(),
                bytes.len() + 4,
                directory.path()
            )
            .unwrap(),
            Some(129)
        );
        assert!(authenticate_prefix(
            &base,
            &mut bytes.as_slice(),
            bytes.len(),
            base.hash(),
            bytes.len() + 4,
            directory.path()
        )
        .is_err());
        // Header authentication alone is not semantic state restoration.
        bytes.extend_from_slice(b"body");
        let mut prefix_reader = std::io::Cursor::new(&bytes);
        assert_eq!(
            authenticate_prefix(
                &base,
                &mut prefix_reader,
                bytes.len(),
                anchor.hash(),
                bytes.len(),
                directory.path()
            )
            .unwrap(),
            Some(129)
        );
        assert_eq!(
            prefix_reader.position(),
            (bytes.len() - 4) as u64,
            "header authentication must not read the economic body"
        );
        assert!(AuthenticatedSnapshot::read(&base, &mut bytes.as_slice()).is_err());
    }

    #[test]
    fn rejects_invalid_framing_without_reading_a_body() {
        let base = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
            burst_types::NetworkId::Dev,
            &burst_types::ProtocolParams::default(),
        )
        .unwrap();
        for length in [0, MAX_HEADER_BYTES + 1, u64::MAX] {
            let mut bytes = MAGIC.to_vec();
            bytes.extend_from_slice(&length.to_be_bytes());
            assert!(AuthenticatedSnapshot::read(&base, &mut bytes.as_slice()).is_err());
        }
        for bytes in [vec![], vec![0; 24], MAGIC.to_vec()] {
            assert!(AuthenticatedSnapshot::read(&base, &mut bytes.as_slice()).is_err());
        }
    }
}
