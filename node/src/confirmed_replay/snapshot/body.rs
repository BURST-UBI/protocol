use super::{CheckpointAnchor, ReplayError, SnapshotCertificate};
use blake2::{digest::consts::U32, Blake2b, Digest};
use std::io::{BufReader, BufWriter, Cursor, Read, Seek, SeekFrom, Write};

const MEMORY_BYTES: u64 = 1024 * 1024;
const BUFFER_BYTES: usize = 16 * 1024;

fn invalid(error: impl ToString) -> ReplayError {
    super::invalid(error.to_string())
}

pub(super) trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

pub(crate) struct StagedBody {
    pub(super) file: std::sync::Mutex<crate::confirmed_replay::scratch_file::ScratchFile>,
    pub(super) directory: std::path::PathBuf,
}

impl StagedBody {
    pub(super) fn prepare(
        anchor: &CheckpointAnchor,
        directory: &std::path::Path,
        encode: impl FnOnce(&mut BufWriter<crate::confirmed_replay::scratch_file::ScratchFile>) -> Result<(), ReplayError>,
    ) -> Result<(Self, super::SnapshotManifest), ReplayError> {
        let file = crate::confirmed_replay::scratch_file::ScratchFile::new_in(directory).map_err(invalid)?;
        let mut writer = BufWriter::with_capacity(BUFFER_BYTES, file);
        encode(&mut writer)?;
        let mut file = writer.into_inner().map_err(invalid)?;
        let length = file.metadata().map_err(invalid)?.len();
        let hash = commitment(&mut file, length)?;
        let manifest =
            super::SnapshotManifest::from_commitment(anchor, length, hash).map_err(invalid)?;
        Ok((
            Self { file: std::sync::Mutex::new(file), directory: directory.to_path_buf() },
            manifest,
        ))
    }
}

fn commitment(
    reader: &mut dyn ReadSeek,
    length: u64,
) -> Result<burst_types::BlockHash, ReplayError> {
    reader.rewind().map_err(invalid)?;
    let mut digest = Blake2b::<U32>::new();
    let mut remaining = length;
    let mut buffer = [0; BUFFER_BYTES];
    while remaining > 0 {
        let count = remaining.min(BUFFER_BYTES as u64) as usize;
        reader.read_exact(&mut buffer[..count]).map_err(invalid)?;
        digest.update(&buffer[..count]);
        remaining -= count as u64;
    }
    if reader.read(&mut buffer[..1]).map_err(invalid)? != 0 {
        return Err(invalid("snapshot body exceeds its certified length"));
    }
    reader.rewind().map_err(invalid)?;
    Ok(burst_types::BlockHash::new(digest.finalize().into()))
}

enum Source<'a> {
    Borrowed(Cursor<&'a [u8]>),
    Stream(BufReader<&'a mut dyn ReadSeek>),
    Memory(Cursor<Vec<u8>>),
    File(BufReader<crate::confirmed_replay::scratch_file::ScratchFile>),
}

/// Body bytes checked against a snapshot certificate before state decoding.
/// A borrowed seekable stream must remain unchanged across verification, decoding
/// and canonical comparison; this wrapper does not lock its underlying storage.
pub(super) struct VerifiedBody<'a> {
    source: Source<'a>,
    length: u64,
    consumed: u64,
    state_format: u16,
    directory: Option<std::path::PathBuf>,
}

impl<'a> VerifiedBody<'a> {
    #[cfg(test)]
    pub(super) fn stream(
        anchor: &CheckpointAnchor,
        certificate: &SnapshotCertificate,
        reader: &'a mut dyn ReadSeek,
    ) -> Result<Self, ReplayError> {
        Self::stream_in(anchor, certificate, reader, None)
    }

    pub(super) fn stream_in(
        anchor: &CheckpointAnchor,
        certificate: &SnapshotCertificate,
        reader: &'a mut dyn ReadSeek,
        directory: Option<&std::path::Path>,
    ) -> Result<Self, ReplayError> {
        certificate.verify(anchor).map_err(invalid)?;
        let length = certificate.manifest.state_bytes;
        if commitment(reader, length)? != certificate.manifest.state_hash {
            return Err(invalid("snapshot body differs from its certified hash"));
        }
        Ok(Self {
            source: Source::Stream(BufReader::with_capacity(BUFFER_BYTES, reader)),
            length,
            consumed: 0,
            state_format: certificate.manifest.state_format,
            directory: directory.map(std::path::Path::to_path_buf),
        })
    }

    pub(super) fn borrowed(
        anchor: &CheckpointAnchor,
        certificate: &SnapshotCertificate,
        bytes: &'a [u8],
    ) -> Result<Self, ReplayError> {
        certificate.verify_state(anchor, bytes).map_err(invalid)?;
        Ok(Self {
            source: Source::Borrowed(Cursor::new(bytes)),
            length: bytes.len() as u64,
            consumed: 0,
            state_format: certificate.manifest.state_format,
            directory: None,
        })
    }

    pub(super) fn consumed_all(&self) -> bool {
        self.consumed == self.length
    }

    pub(super) fn state_format(&self) -> u16 {
        self.state_format
    }

    fn rewind(&mut self) -> Result<(), ReplayError> {
        match &mut self.source {
            Source::Borrowed(source) => source.seek(SeekFrom::Start(0)),
            Source::Stream(source) => source.seek(SeekFrom::Start(0)),
            Source::Memory(source) => source.seek(SeekFrom::Start(0)),
            Source::File(source) => source.seek(SeekFrom::Start(0)),
        }
        .map_err(invalid)?;
        self.consumed = 0;
        Ok(())
    }

    pub(super) fn matches_state_with_poll(&mut self, state: &super::State<'_>,
        poll: &dyn Fn(&'static str) -> Result<(), ReplayError>,
    ) -> Result<bool, ReplayError> {
        self.rewind()?;
        let directory = self.directory.clone();
        let mut comparator = Comparator {
            body: self,
            buffer: [0; BUFFER_BYTES],
            poll,
            bytes_until_poll: 0,
        };
        state.encode_into_in(&mut comparator, directory.as_deref())?;
        Ok(self.consumed_all())
    }

    #[cfg(test)]
    pub(super) fn matches_canonical<T: serde::Serialize>(
        &mut self,
        state: &T,
    ) -> Result<bool, ReplayError> {
        self.rewind()?;
        let mut comparator = Comparator {
            body: self,
            buffer: [0; BUFFER_BYTES],
            poll: &|_| Ok(()),
            bytes_until_poll: 0,
        };
        super::canonical::encode_into(state, &mut comparator)?;
        Ok(self.consumed_all())
    }

    #[cfg(test)]
    fn matches(&mut self, bytes: &[u8]) -> Result<bool, ReplayError> {
        if bytes.len() as u64 != self.length {
            return Ok(false);
        }
        self.rewind()?;
        let mut buffer = [0; BUFFER_BYTES];
        for expected in bytes.chunks(BUFFER_BYTES) {
            self.read_exact(&mut buffer[..expected.len()])
                .map_err(invalid)?;
            if &buffer[..expected.len()] != expected {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

pub(super) struct PollingReader<'a, R> {
    pub inner: &'a mut R,
    pub poll: &'a dyn Fn(&'static str) -> Result<(), ReplayError>,
    pub bytes_until_poll: usize,
}

impl<R: Read> Read for PollingReader<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if self.bytes_until_poll == 0 {
            (self.poll)("snapshot:decode-bytes").map_err(std::io::Error::other)?;
            self.bytes_until_poll = BUFFER_BYTES;
        }
        let length = bytes.len().min(self.bytes_until_poll);
        let count = self.inner.read(&mut bytes[..length])?;
        self.bytes_until_poll -= count;
        Ok(count)
    }
}

struct Comparator<'a, 'b> {
    body: &'a mut VerifiedBody<'b>,
    buffer: [u8; BUFFER_BYTES],
    poll: &'a dyn Fn(&'static str) -> Result<(), ReplayError>,
    bytes_until_poll: usize,
}

impl Write for Comparator<'_, '_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let mut remaining = bytes;
        while !remaining.is_empty() {
            if self.bytes_until_poll == 0 {
                (self.poll)("snapshot:canonical-bytes").map_err(std::io::Error::other)?;
                self.bytes_until_poll = BUFFER_BYTES;
            }
            let (expected, rest) = remaining.split_at(remaining.len().min(self.bytes_until_poll));
            self.bytes_until_poll -= expected.len();
            self.body.read_exact(&mut self.buffer[..expected.len()])?;
            if &self.buffer[..expected.len()] != expected {
                return Err(std::io::Error::other("noncanonical snapshot bytes"));
            }
            remaining = rest;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl VerifiedBody<'static> {
    /// Copy exactly the certified body length, using memory up to 1 MiB and
    /// scratch storage above it. The caller checks for trailing archive bytes.
    pub(super) fn stage(
        anchor: &CheckpointAnchor,
        certificate: &SnapshotCertificate,
        reader: &mut impl Read,
        directory: Option<&std::path::Path>,
    ) -> Result<Self, ReplayError> {
        certificate.verify(anchor).map_err(invalid)?;
        let length = certificate.manifest.state_bytes;
        let mut digest = Blake2b::<U32>::new();
        let mut remaining = length;
        let mut buffer = [0; BUFFER_BYTES];
        let mut memory = Vec::new();
        let mut file = if length > MEMORY_BYTES {
            Some(
                match directory {
                    Some(directory) => crate::confirmed_replay::scratch_file::ScratchFile::new_in(directory),
                    None => crate::confirmed_replay::scratch_file::ScratchFile::new(),
                }
                .map_err(invalid)?,
            )
        } else {
            memory.reserve_exact(length as usize);
            None
        };
        while remaining > 0 {
            let count = remaining.min(BUFFER_BYTES as u64) as usize;
            reader.read_exact(&mut buffer[..count]).map_err(invalid)?;
            digest.update(&buffer[..count]);
            if let Some(file) = &mut file {
                file.write_all(&buffer[..count]).map_err(invalid)?;
            } else {
                memory.extend_from_slice(&buffer[..count]);
            }
            remaining -= count as u64;
        }
        let hash: [u8; 32] = digest.finalize().into();
        if burst_types::BlockHash::new(hash) != certificate.manifest.state_hash {
            return Err(invalid("snapshot body differs from its certified hash"));
        }
        let source = match file {
            Some(mut file) => {
                file.rewind().map_err(invalid)?;
                Source::File(BufReader::with_capacity(BUFFER_BYTES, file))
            }
            None => Source::Memory(Cursor::new(memory)),
        };
        Ok(Self {
            source,
            length,
            consumed: 0,
            state_format: certificate.manifest.state_format,
            directory: directory.map(std::path::Path::to_path_buf),
        })
    }
}

impl Read for VerifiedBody<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let count = match &mut self.source {
            Source::Borrowed(source) => source.read(bytes),
            Source::Stream(source) => source.read(bytes),
            Source::Memory(source) => source.read(bytes),
            Source::File(source) => source.read(bytes),
        }?;
        self.consumed += count as u64;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_consensus::{checkpoint::CheckpointSignature, snapshot::SnapshotManifest};
    use burst_types::{NetworkId, ProtocolParams};

    #[test]
    fn staging_authenticates_before_exposing_small_and_disk_backed_bodies() {
        let directory = tempfile::tempdir().unwrap();
        let anchor = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
            NetworkId::Dev,
            &ProtocolParams::default(),
        )
        .unwrap();
        let key = burst_crypto::keypair_from_seed(&[0; 32]);
        for size in [37, MEMORY_BYTES as usize + 123] {
            let value = super::super::Blob(vec![0x5a; size].into());
            let bytes = super::super::canonical::encode(&value).unwrap();
            let manifest = SnapshotManifest::new(&anchor, &bytes).unwrap();
            let certificate = SnapshotCertificate {
                signatures: vec![CheckpointSignature {
                    representative: burst_crypto::derive_address(&key.public),
                    signature: burst_crypto::sign_message(
                        &manifest.signing_payload(),
                        &key.private,
                    ),
                }],
                manifest,
            };
            // Interrupt after a complete chunk of a valid, authenticated large
            // body; neither decoding nor comparison may consume the remaining bytes.
            if size > MEMORY_BYTES as usize {
                for comparing in [false, true] {
                    let mut body = VerifiedBody::borrowed(&anchor, &certificate, &bytes).unwrap();
                    let calls = std::cell::Cell::new(0);
                    let poll = |_: &'static str| {
                        calls.set(calls.get() + 1);
                        if calls.get() == 2 { Err(invalid("body cancellation")) } else { Ok(()) }
                    };
                    let error = if comparing {
                        let mut comparator = Comparator {
                            body: &mut body, buffer: [0; BUFFER_BYTES], poll: &poll, bytes_until_poll: 0,
                        };
                        super::super::canonical::encode_into(&value, &mut comparator).unwrap_err().to_string()
                    } else {
                        ciborium::from_reader::<super::super::Blob<'_>, _>(PollingReader {
                            inner: &mut body, poll: &poll, bytes_until_poll: 0,
                        }).err().unwrap().to_string()
                    };
                    assert!(error.contains("body cancellation"));
                    assert_eq!(calls.get(), 2);
                    assert_eq!(body.consumed, BUFFER_BYTES as u64);
                    assert!(!body.consumed_all());
                }
            }
            let (disk, manifest) = StagedBody::prepare(&anchor, directory.path(), |writer| {
                writer.write_all(&bytes).map_err(invalid)
            })
            .unwrap();
            assert_eq!(manifest, certificate.manifest);
            {
                let mut file = disk.file.lock().unwrap();
                #[cfg(target_os = "linux")]
                {
                    use std::os::fd::AsRawFd;
                    let path =
                        std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).unwrap();
                    assert!(path.starts_with(directory.path()));
                }
                let mut stream = VerifiedBody::stream(&anchor, &certificate, &mut *file).unwrap();
                assert!(stream.matches_canonical(&value).unwrap());
                assert!(stream.matches(&bytes).unwrap());
                drop(stream);
                file.rewind().unwrap();
                file.write_all(&[bytes[0] ^ 1]).unwrap();
                assert!(VerifiedBody::stream(&anchor, &certificate, &mut *file).is_err());
                file.rewind().unwrap();
                file.write_all(&bytes).unwrap();
                file.write_all(&[0]).unwrap();
                assert!(VerifiedBody::stream(&anchor, &certificate, &mut *file).is_err());
                file.set_len(bytes.len() as u64 - 1).unwrap();
                assert!(VerifiedBody::stream(&anchor, &certificate, &mut *file).is_err());
                file.rewind().unwrap();
                file.write_all(&bytes).unwrap();
                let mut stream = VerifiedBody::stream(&anchor, &certificate, &mut *file).unwrap();
                assert!(stream.matches(&bytes).unwrap());
            }
            drop(disk);
            assert!(StagedBody::prepare(&anchor, directory.path(), |writer| {
                writer.write_all(&bytes[..7]).map_err(invalid)?;
                Err(invalid("injected state encoding failure"))
            })
            .is_err());
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
            let mut input = bytes.as_slice();
            let mut body =
                VerifiedBody::stage(&anchor, &certificate, &mut input, Some(directory.path()))
                    .unwrap();
            if size > MEMORY_BYTES as usize {
                let missing = directory.path().join("missing");
                let mut unread = bytes.as_slice();
                assert!(
                    VerifiedBody::stage(&anchor, &certificate, &mut unread, Some(&missing))
                        .is_err()
                );
                assert_eq!(unread.len(), bytes.len());
                assert!(!missing.exists());
                #[cfg(target_os = "linux")]
                if let Source::File(source) = &body.source {
                    use std::os::fd::AsRawFd;
                    let path = std::fs::read_link(format!(
                        "/proc/self/fd/{}",
                        source.get_ref().as_raw_fd()
                    ))
                    .unwrap();
                    assert!(path.starts_with(directory.path()), "{}", path.display());
                }
            }
            assert_eq!(
                matches!(body.source, Source::File(_)),
                size > MEMORY_BYTES as usize
            );
            assert!(!body.consumed_all());
            assert!(body.matches(&bytes).unwrap());
            assert!(body.consumed_all());
            assert!(body.matches_canonical(&value).unwrap());
            assert!(body.consumed_all());
            let decoded: super::super::Blob = {
                body.rewind().unwrap();
                ciborium::from_reader(&mut body).unwrap()
            };
            assert_eq!(decoded.0, value.0);
            assert!(body
                .matches_canonical(&super::super::Blob(vec![0x5b; size].into()))
                .is_err());
            assert!(body
                .matches_canonical(&super::super::Blob(vec![0x5a; size - 1].into()))
                .is_err());
            assert!(body
                .matches_canonical(&super::super::Blob(vec![0x5a; size + 1].into()))
                .is_err());
            // A failed comparison must not prevent a subsequent exact comparison.
            assert!(body.matches_canonical(&value).unwrap());
            let mut changed = bytes.clone();
            changed[size - 1] ^= 1;
            assert!(!body.matches(&changed).unwrap());
            assert!(VerifiedBody::stage(
                &anchor,
                &certificate,
                &mut changed.as_slice(),
                Some(directory.path())
            )
            .is_err());
            assert!(VerifiedBody::stage(
                &anchor,
                &certificate,
                &mut &bytes[..size - 1],
                Some(directory.path())
            )
            .is_err());
            let mut invalid = certificate.clone();
            invalid.signatures.clear();
            let mut untouched = bytes.as_slice();
            assert!(
                VerifiedBody::stage(&anchor, &invalid, &mut untouched, Some(directory.path()))
                    .is_err()
            );
            assert_eq!(untouched.len(), bytes.len());
            drop(body);
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
            let mut legacy =
                VerifiedBody::stage(&anchor, &certificate, &mut bytes.as_slice(), None).unwrap();
            assert!(legacy.matches(&bytes).unwrap());
        }
    }
}
