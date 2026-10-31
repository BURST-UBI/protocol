//! Explicit, on-demand archive transfer over existing authenticated peers.
//! A successful download publishes a file, never live economic state.

use super::{
    invalid, CheckpointSyncMessage as Message, Incoming, CHUNK_BYTES, PEER_TIMEOUT, RETRY_INTERVAL,
};
use crate::confirmed_replay::snapshot::archive::{authenticate_prefix, AuthenticatedSnapshot};
use blake2::{digest::consts::U32, Blake2b, Digest};
use burst_consensus::checkpoint::CheckpointAnchor;
use burst_store::StoreError;
use burst_types::BlockHash;
use serde::{Deserialize, Serialize};
use std::{
    io::{BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Local archive admission budget, not a consensus or lifetime history limit.
    #[serde(default = "default_archive_budget")]
    pub max_archive_bytes: u64,
    /// A locally exported quorum-authenticated archive. Loaded once at startup.
    pub serve: Option<PathBuf>,
    /// Optional epoch pin, freshness floor and destination. Never installs state.
    pub fetch: Option<Fetch>,
}

fn default_archive_budget() -> u64 {
    24 + 80 * 1024 * 1024
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_archive_bytes: default_archive_budget(),
            serve: None,
            fetch: None,
        }
    }
}

impl Config {
    pub(crate) fn validate(&self) -> Result<(), StoreError> {
        if self.max_archive_bytes < 25 || usize::try_from(self.max_archive_bytes).is_err() {
            return Err(invalid(
                "snapshot archive budget is outside supported file bounds",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fetch {
    /// Independently obtained checkpoint hash. Omit to query connected peers.
    pub epoch: Option<String>,
    /// Independently chosen freshness floor; peer claims do not change it.
    #[serde(default = "default_minimum_height")]
    pub minimum_height: u64,
    pub output: PathBuf,
}

fn default_minimum_height() -> u64 {
    1
}

pub(crate) use burst_messages::checkpoint_sync::snapshot_is_bounded as is_bounded;

struct Published {
    epoch: BlockHash,
    height: u64,
    digest: BlockHash,
    file: std::fs::File,
    length: usize,
}

mod startup;
pub(crate) use startup::fetch_before_admission;

fn load(
    path: &Path,
    base: &CheckpointAnchor,
    staging: &Path,
    budget: u64,
) -> Result<Published, StoreError> {
    let metadata = std::fs::metadata(path).map_err(invalid)?;
    if !metadata.is_file() || metadata.len() > budget {
        return Err(invalid("snapshot must be a bounded regular archive file"));
    }
    let mut file = std::fs::File::open(path).map_err(invalid)?;
    if !file.metadata().map_err(invalid)?.is_file() {
        return Err(invalid("snapshot must be a regular archive file"));
    }
    let mut staged = tempfile::tempfile_in(staging).map_err(invalid)?;
    let mut hash = Blake2b::<U32>::new();
    let mut length = 0usize;
    let mut buffer = [0; CHUNK_BYTES];
    loop {
        let count = file.read(&mut buffer).map_err(invalid)?;
        if count == 0 {
            break;
        }
        length = length
            .checked_add(count)
            .filter(|length| *length as u64 <= budget)
            .ok_or_else(|| invalid("snapshot exceeds archive byte limit"))?;
        hash.update(&buffer[..count]);
        staged.write_all(&buffer[..count]).map_err(invalid)?;
    }
    staged.seek(SeekFrom::Start(0)).map_err(invalid)?;
    let snapshot = AuthenticatedSnapshot::read_in(base, &mut BufReader::new(&mut staged), staging)
        .map_err(invalid)?;
    Ok(Published {
        epoch: snapshot.anchor().hash(),
        height: snapshot.anchor().height(),
        digest: BlockHash::new(hash.finalize().into()),
        file: staged,
        length,
    })
}

struct Download {
    peer: String,
    failed_peers: Vec<String>,
    epoch: BlockHash,
    offered_height: Option<u64>,
    file: Option<std::fs::File>,
    received: usize,
    hash: Blake2b<U32>,
    total: Option<u64>,
    digest: Option<BlockHash>,
    header_authenticated: bool,
    last_progress_at: Instant,
    last_request_at: Instant,
}

impl Download {
    fn expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_progress_at) >= PEER_TIMEOUT
    }

    fn request(&self) -> Incoming {
        Incoming {
            peer: self.peer.clone(),
            message: Message::SnapshotRequest {
                epoch: self.epoch,
                offset: self.received as u64,
                digest: self.digest,
            },
        }
    }
}

struct FetchPlan {
    epoch: Option<BlockHash>,
    minimum_height: u64,
    output: PathBuf,
}

struct Query {
    peer: String,
    started_at: Instant,
    last_request_at: Instant,
}

impl Query {
    fn request(&self, minimum_height: u64) -> Incoming {
        Incoming {
            peer: self.peer.clone(),
            message: Message::SnapshotQuery { minimum_height },
        }
    }
}

pub(crate) struct Transfer {
    max_archive_bytes: u64,
    base: CheckpointAnchor,
    published: Option<Published>,
    fetch: Option<FetchPlan>,
    query: Option<Query>,
    download: Option<Download>,
    cursor: usize,
    next_poll: Instant,
    serve_window_start: Instant,
    responses_served: usize,
    #[cfg(test)]
    publication_probe: Option<fn(&str)>,
}

impl Transfer {
    pub(crate) fn new(
        config: Config,
        base: CheckpointAnchor,
        staging: &Path,
    ) -> Result<Self, StoreError> {
        config.validate()?;
        let published = config
            .serve
            .as_deref()
            .map(|path| load(path, &base, staging, config.max_archive_bytes))
            .transpose()?;
        let fetch = if let Some(fetch) = config.fetch {
            let epoch = fetch
                .epoch
                .as_ref()
                .map(|encoded| {
                    let mut bytes = [0; 32];
                    hex::decode_to_slice(encoded, &mut bytes).map_err(invalid)?;
                    let epoch = BlockHash::new(bytes);
                    if epoch.is_zero() {
                        return Err(invalid("snapshot fetch requires a nonzero epoch"));
                    }
                    Ok(epoch)
                })
                .transpose()?;
            if fetch.minimum_height == 0 {
                return Err(invalid("snapshot minimum height must be positive"));
            }
            // Existing output is never replaced. A prior completed download is
            // authenticated again on restart and checked against the optional
            // epoch pin and minimum height.
            match std::fs::symlink_metadata(&fetch.output) {
                Ok(metadata) => {
                    if !metadata.is_file() {
                        return Err(invalid("snapshot destination is not a regular file"));
                    }
                    let existing = load(&fetch.output, &base, staging, config.max_archive_bytes)?;
                    if epoch.is_some_and(|epoch| epoch != existing.epoch)
                        || existing.height < fetch.minimum_height
                    {
                        return Err(invalid(
                            "snapshot destination already exists with different state",
                        ));
                    }
                    None
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if !parent(&fetch.output).is_dir() {
                        return Err(invalid("snapshot destination directory does not exist"));
                    }
                    Some(FetchPlan {
                        epoch,
                        minimum_height: fetch.minimum_height,
                        output: fetch.output,
                    })
                }
                Err(error) => return Err(invalid(error)),
            }
        } else {
            None
        };
        let now = Instant::now();
        Ok(Self {
            max_archive_bytes: config.max_archive_bytes,
            base,
            published,
            fetch,
            query: None,
            download: None,
            cursor: 0,
            next_poll: now,
            serve_window_start: now,
            responses_served: 0,
            #[cfg(test)]
            publication_probe: None,
        })
    }

    pub(crate) fn poll(&mut self, mut peers: Vec<String>, now: Instant) -> Option<Incoming> {
        let fetch = self.fetch.as_ref()?;
        peers.sort();
        if let Some(download) = &mut self.download {
            if download.expired(now) || !peers.contains(&download.peer) {
                // Resume on another peer only after authenticating the header.
                // Keep the peer-advertised digest to request identical bytes;
                // verify it and the full archive when the download completes.
                if download.header_authenticated && download.received > 0 {
                    download.failed_peers.retain(|peer| peers.contains(peer));
                    if peers.contains(&download.peer) {
                        download.failed_peers.push(download.peer.clone());
                    }
                    if let Some(peer) = peers
                        .iter()
                        .find(|peer| !download.failed_peers.contains(peer))
                    {
                        download.peer = peer.clone();
                        download.last_progress_at = now;
                        download.last_request_at = now;
                        return Some(download.request());
                    }
                }
                self.download = None;
            } else if now.saturating_duration_since(download.last_request_at) >= RETRY_INTERVAL {
                download.last_request_at = now;
                return Some(download.request());
            } else {
                return None;
            }
        }
        if let Some(query) = &mut self.query {
            if now.saturating_duration_since(query.started_at) >= PEER_TIMEOUT
                || !peers.contains(&query.peer)
            {
                self.query = None;
            } else if now.saturating_duration_since(query.last_request_at) >= RETRY_INTERVAL {
                query.last_request_at = now;
                return Some(query.request(fetch.minimum_height));
            } else {
                return None;
            }
        }
        if now < self.next_poll || peers.is_empty() {
            return None;
        }
        let peer = peers[self.cursor % peers.len()].clone();
        self.cursor = self.cursor.wrapping_add(1);
        self.next_poll = now + PEER_TIMEOUT;
        let Some(epoch) = fetch.epoch else {
            let query = Query {
                peer,
                started_at: now,
                last_request_at: now,
            };
            let request = query.request(fetch.minimum_height);
            self.query = Some(query);
            return Some(request);
        };
        let download = Download {
            peer,
            failed_peers: Vec::new(),
            epoch,
            offered_height: None,
            file: None,
            received: 0,
            hash: Blake2b::new(),
            total: None,
            digest: None,
            header_authenticated: false,
            last_progress_at: now,
            last_request_at: now,
        };
        let request = download.request();
        self.download = Some(download);
        self.next_poll = now + PEER_TIMEOUT;
        Some(request)
    }

    fn allow_serve(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.serve_window_start) >= RETRY_INTERVAL {
            self.serve_window_start = now;
            self.responses_served = 0;
        }
        if self.responses_served >= 64 {
            return false;
        }
        self.responses_served += 1;
        true
    }

    pub(crate) fn handle(
        &mut self,
        incoming: Incoming,
        now: Instant,
    ) -> Result<Option<Incoming>, StoreError> {
        let processing_started = Instant::now();
        if !is_bounded(&incoming.message) {
            return Err(invalid("snapshot packet exceeds transport bounds"));
        }
        match incoming.message {
            Message::SnapshotQuery { minimum_height } => {
                if !self.allow_serve(now) {
                    return Ok(None);
                }
                let Some(published) = &self.published else {
                    return Ok(None);
                };
                if published.height < minimum_height {
                    return Ok(None);
                }
                Ok(Some(Incoming {
                    peer: incoming.peer,
                    message: Message::SnapshotOffer {
                        epoch: published.epoch,
                        height: published.height,
                        total: published.length as u64,
                        digest: published.digest,
                    },
                }))
            }
            Message::SnapshotOffer {
                epoch,
                height,
                total,
                digest,
            } => {
                let Some(fetch) = &self.fetch else {
                    return Ok(None);
                };
                let Some(query) = &self.query else {
                    return Ok(None);
                };
                if fetch.epoch.is_some()
                    || query.peer != incoming.peer
                    || now.saturating_duration_since(query.started_at) >= PEER_TIMEOUT
                {
                    return Ok(None);
                }
                self.query = None;
                self.next_poll = now + PEER_TIMEOUT;
                if total > self.max_archive_bytes {
                    return Err(invalid(
                        "snapshot offer exceeds local archive admission limit",
                    ));
                }
                if height < fetch.minimum_height {
                    return Err(invalid("snapshot offer is below freshness floor"));
                }
                let download = Download {
                    peer: incoming.peer,
                    failed_peers: Vec::new(),
                    epoch,
                    offered_height: Some(height),
                    file: None,
                    received: 0,
                    hash: Blake2b::new(),
                    total: Some(total),
                    digest: Some(digest),
                    header_authenticated: false,
                    last_progress_at: now,
                    last_request_at: now,
                };
                let request = download.request();
                self.download = Some(download);
                Ok(Some(request))
            }
            Message::SnapshotRequest {
                epoch,
                offset,
                digest,
            } => {
                if !self.allow_serve(now) {
                    return Ok(None);
                }
                let Some(published) = &mut self.published else {
                    return Ok(None);
                };
                if epoch != published.epoch
                    || offset >= published.length as u64
                    || digest.is_some_and(|hash| hash != published.digest)
                {
                    return Ok(None);
                }
                let start = usize::try_from(offset).map_err(invalid)?;
                let end = (start + CHUNK_BYTES).min(published.length);
                let mut data = vec![0; end - start];
                published
                    .file
                    .seek(SeekFrom::Start(start as u64))
                    .map_err(invalid)?;
                published.file.read_exact(&mut data).map_err(invalid)?;
                Ok(Some(Incoming {
                    peer: incoming.peer,
                    message: Message::SnapshotChunk {
                        epoch,
                        offset,
                        total: published.length as u64,
                        digest: published.digest,
                        data,
                    },
                }))
            }
            Message::SnapshotChunk {
                epoch,
                offset,
                total,
                digest,
                data,
            } => {
                let Some(fetch) = &self.fetch else {
                    return Ok(None);
                };
                let Some(active) = &self.download else {
                    return Ok(None);
                };
                if epoch != active.epoch
                    || active.peer != incoming.peer
                    || offset != active.received as u64
                {
                    return Ok(None); // unsolicited or duplicate response
                }
                let mut download = self
                    .download
                    .take()
                    .ok_or_else(|| invalid("missing snapshot download"))?;
                self.next_poll = now + PEER_TIMEOUT;
                if total > self.max_archive_bytes {
                    return Err(invalid(
                        "snapshot chunk exceeds local archive admission limit",
                    ));
                }
                if download.expired(now)
                    || download.total.is_some_and(|value| value != total)
                    || download.digest.is_some_and(|value| value != digest)
                {
                    return Err(invalid("snapshot download expired or changed context"));
                }
                download.total = Some(total);
                download.digest = Some(digest);
                if download.file.is_none() {
                    download.file =
                        Some(tempfile::tempfile_in(parent(&fetch.output)).map_err(invalid)?);
                }
                download
                    .file
                    .as_mut()
                    .ok_or_else(|| invalid("missing snapshot staging file"))?
                    .write_all(&data)
                    .map_err(invalid)?;
                download.received += data.len();
                download.hash.update(&data);
                if !download.header_authenticated {
                    let file = download
                        .file
                        .as_mut()
                        .ok_or_else(|| invalid("missing snapshot staging file"))?;
                    file.seek(SeekFrom::Start(0)).map_err(invalid)?;
                    let height = authenticate_prefix(
                        &self.base,
                        file,
                        download.received,
                        epoch,
                        total as usize,
                        parent(&fetch.output),
                    )
                    .map_err(invalid)?;
                    file.seek(SeekFrom::End(0)).map_err(invalid)?;
                    if let Some(height) = height {
                        if height < fetch.minimum_height
                            || download
                                .offered_height
                                .is_some_and(|offered| offered != height)
                        {
                            return Err(invalid("authenticated snapshot height differs from offer or freshness floor"));
                        }
                        download.header_authenticated = true;
                    }
                }
                if download.received == total as usize {
                    if !download.header_authenticated
                        || BlockHash::new(download.hash.finalize().into()) != digest
                    {
                        return Err(invalid("snapshot transfer digest mismatch"));
                    }
                    let mut file = download
                        .file
                        .take()
                        .ok_or_else(|| invalid("missing snapshot staging file"))?;
                    file.seek(SeekFrom::Start(0)).map_err(invalid)?;
                    let snapshot = AuthenticatedSnapshot::read_in(
                        &self.base,
                        &mut BufReader::new(&mut file),
                        parent(&fetch.output),
                    )
                    .map_err(invalid)?;
                    if snapshot.anchor().hash() != epoch
                        || snapshot.anchor().height() < fetch.minimum_height
                    {
                        return Err(invalid("snapshot epoch mismatch"));
                    }
                    #[cfg(test)]
                    if let Some(probe) = self.publication_probe {
                        probe("snapshot_download_prepared");
                    }
                    file.seek(SeekFrom::Start(0)).map_err(invalid)?;
                    publish(&fetch.output, &mut file)?;
                    #[cfg(test)]
                    if let Some(probe) = self.publication_probe {
                        probe("snapshot_download_published");
                    }
                    tracing::info!(%epoch, output = %fetch.output.display(), "authenticated snapshot staged; ledger unchanged");
                    self.fetch = None;
                    Ok(None)
                } else {
                    #[cfg(test)]
                    if let Some(probe) = self.publication_probe {
                        probe("snapshot_download_partial");
                    }
                    download.last_progress_at = now
                        .checked_add(processing_started.elapsed())
                        .ok_or_else(|| invalid("snapshot progress clock overflow"))?;
                    download.failed_peers.clear();
                    download.last_request_at = download.last_progress_at;
                    let request = download.request();
                    self.download = Some(download);
                    Ok(Some(request))
                }
            }
            _ => Err(invalid("expected snapshot packet")),
        }
    }
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn publish(path: &Path, source: &mut impl Read) -> Result<(), StoreError> {
    let directory = parent(path);
    let mut file = tempfile::NamedTempFile::new_in(directory).map_err(invalid)?;
    std::io::copy(source, &mut file).map_err(invalid)?;
    file.as_file().sync_all().map_err(invalid)?;
    file.persist_noclobber(path)
        .map_err(|error| invalid(error.error))?;
    #[cfg(unix)]
    std::fs::File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(invalid)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receiver(dir: &Path) -> Transfer {
        let base = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
            burst_types::NetworkId::Dev,
            &burst_types::ProtocolParams::default(),
        )
        .unwrap();
        Transfer::new(
            Config {
                max_archive_bytes: Config::default().max_archive_bytes,
                serve: None,
                fetch: Some(Fetch {
                    epoch: Some(BlockHash::new([1; 32]).to_string()),
                    minimum_height: 1,
                    output: dir.join("snapshot"),
                }),
            },
            base,
            dir,
        )
        .unwrap()
    }

    #[test]
    fn snapshot_discovery_queries_one_peer_and_rejects_unsolicited_or_stale_offers() {
        let dir = tempfile::tempdir().unwrap();
        let mut transfer = receiver(dir.path());
        let fetch = transfer.fetch.as_mut().unwrap();
        fetch.epoch = None;
        fetch.minimum_height = 2;
        let now = Instant::now();
        let peers = vec!["a".into(), "b".into()];
        let request = transfer.poll(peers.clone(), now).unwrap();
        assert!(matches!(
            request.message,
            Message::SnapshotQuery { minimum_height: 2 }
        ));
        assert_eq!(request.peer, "a");
        assert_eq!(transfer.poll(peers.clone(), now + RETRY_INTERVAL).unwrap().peer, "a");
        let offer = |peer: &str, height| Incoming {
            peer: peer.into(),
            message: Message::SnapshotOffer {
                epoch: BlockHash::new([1; 32]),
                height,
                total: 25,
                digest: BlockHash::new([2; 32]),
            },
        };
        assert!(transfer
            .handle(offer("b", 2), now + RETRY_INTERVAL)
            .unwrap()
            .is_none());
        assert!(transfer.download.is_none());
        assert!(transfer.handle(offer("a", 1), now + RETRY_INTERVAL).is_err());
        assert!(transfer.download.is_none());
        assert!(transfer.query.is_none());
        let later = now + RETRY_INTERVAL + PEER_TIMEOUT;
        assert_eq!(transfer.poll(peers.clone(), later).unwrap().peer, "b");
        let request = transfer.handle(offer("b", 2), later).unwrap().unwrap();
        assert!(matches!(
            request.message,
            Message::SnapshotRequest {
                offset: 0,
                digest: Some(_),
                ..
            }
        ));
        assert!(transfer.query.is_none());
        assert!(transfer.handle(offer("b", 3), later).unwrap().is_none());
        // No chunk progress: discard the offer and discover on the next peer.
        let request = transfer.poll(peers, later + PEER_TIMEOUT).unwrap();
        assert_eq!(request.peer, "a");
        assert!(matches!(request.message, Message::SnapshotQuery { .. }));
        assert!(!dir.path().join("snapshot").exists());
    }

    #[test]
    fn snapshot_discovery_without_response_rotates_and_ignores_late_offer() {
        let dir = tempfile::tempdir().unwrap();
        let mut transfer = receiver(dir.path());
        transfer.fetch.as_mut().unwrap().epoch = None;
        let now = Instant::now();
        let peers = vec!["a".into(), "b".into()];
        assert_eq!(transfer.poll(peers.clone(), now).unwrap().peer, "a");
        assert_eq!(transfer.poll(peers, now + PEER_TIMEOUT).unwrap().peer, "b");
        assert!(transfer
            .handle(
                Incoming {
                    peer: "a".into(),
                    message: Message::SnapshotOffer {
                        epoch: BlockHash::new([1; 32]),
                        height: 1,
                        total: 25,
                        digest: BlockHash::new([2; 32]),
                    }
                },
                now + PEER_TIMEOUT
            )
            .unwrap()
            .is_none());
        assert!(transfer.download.is_none());
        assert_eq!(transfer.query.as_ref().unwrap().peer, "b");
    }

    #[test]
    fn snapshot_discovery_config_roundtrips_and_zero_freshness_is_refused() {
        let config: Config =
            toml::from_str("[fetch]\nminimum_height = 17\noutput = 'snapshot'").unwrap();
        assert!(config.fetch.as_ref().unwrap().epoch.is_none());
        assert_eq!(config.fetch.as_ref().unwrap().minimum_height, 17);
        let roundtrip: Config = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(roundtrip.fetch.unwrap().minimum_height, 17);
        let dir = tempfile::tempdir().unwrap();
        let base = receiver(dir.path()).base;
        let mut zero = config;
        zero.fetch.as_mut().unwrap().minimum_height = 0;
        zero.fetch.as_mut().unwrap().output = dir.path().join("snapshot");
        assert!(Transfer::new(zero, base, dir.path()).is_err());
        assert!(!is_bounded(&Message::SnapshotQuery { minimum_height: 0 }));
    }

    #[test]
    fn snapshot_download_retries_rotates_stalled_peers_but_keeps_long_progress() {
        let dir = tempfile::tempdir().unwrap();
        let mut transfer = receiver(dir.path());
        let now = Instant::now();
        let peers = vec!["a".into(), "b".into()];
        assert_eq!(transfer.poll(peers.clone(), now).unwrap().peer, "a");
        assert!(transfer.poll(peers.clone(), now).is_none());
        assert_eq!(transfer.poll(peers.clone(), now + RETRY_INTERVAL).unwrap().peer, "a");
        assert_eq!(
            transfer
                .poll(peers.clone(), now + PEER_TIMEOUT)
                .unwrap()
                .peer,
            "b"
        );
        // Advancing chunks remain eligible after the old absolute deadline.
        // Only progress time is advanced; the request clock stays unchanged.
        let later = now + PEER_TIMEOUT + super::super::TRANSFER_TIMEOUT;
        transfer.download.as_mut().unwrap().last_progress_at = later;
        assert_eq!(transfer.poll(peers.clone(), later).unwrap().peer, "b");
        assert_eq!(
            transfer.poll(peers, later + PEER_TIMEOUT).unwrap().peer,
            "a"
        );
        assert!(!dir.path().join("snapshot").exists());
    }

    #[test]
    fn snapshot_bad_header_and_unsolicited_data_never_publish() {
        let dir = tempfile::tempdir().unwrap();
        let mut transfer = receiver(dir.path());
        let now = Instant::now();
        transfer.poll(vec!["a".into()], now).unwrap();
        let message = Message::SnapshotChunk {
            epoch: BlockHash::new([1; 32]),
            offset: 0,
            total: (CHUNK_BYTES * 2) as u64,
            digest: BlockHash::new([2; 32]),
            data: vec![0; CHUNK_BYTES],
        };
        assert!(transfer
            .handle(
                Incoming {
                    peer: "unsolicited".into(),
                    message: message.clone()
                },
                now
            )
            .unwrap()
            .is_none());
        assert_eq!(transfer.download.as_ref().unwrap().received, 0);
        assert!(transfer
            .handle(
                Incoming {
                    peer: "a".into(),
                    message
                },
                now
            )
            .is_err());
        assert!(transfer.download.is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn wide_wire_lengths_do_not_bypass_local_archive_admission_or_wrap_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let mut transfer = receiver(dir.path());
        let now = Instant::now();
        transfer.poll(vec!["a".into()], now).unwrap();
        let chunk = |offset| Message::SnapshotChunk {
            epoch: BlockHash::new([1; 32]),
            offset,
            total: u64::MAX,
            digest: BlockHash::new([2; 32]),
            data: vec![0; CHUNK_BYTES],
        };
        assert!(is_bounded(&chunk(1 << 32)));
        assert!(transfer
            .handle(
                Incoming {
                    peer: "a".into(),
                    message: chunk(1 << 32)
                },
                now
            )
            .unwrap()
            .is_none());
        assert_eq!(transfer.download.as_ref().unwrap().received, 0);
        assert!(transfer
            .handle(
                Incoming {
                    peer: "a".into(),
                    message: chunk(0)
                },
                now
            )
            .err()
            .unwrap()
            .to_string()
            .contains("local archive admission"));
        assert!(transfer.download.is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn configured_budget_rejects_chunks_before_staging_and_defaults_compatibly() {
        let config: Config = serde_json::from_str(r#"{"serve":null,"fetch":null}"#).unwrap();
        assert_eq!(config.max_archive_bytes, default_archive_budget());
        let invalid = Config {
            max_archive_bytes: 24,
            ..Config::default()
        };
        assert!(invalid.validate().is_err());
        let dir = tempfile::tempdir().unwrap();
        let mut transfer = receiver(dir.path());
        transfer.max_archive_bytes = CHUNK_BYTES as u64;
        let now = Instant::now();
        transfer.poll(vec!["a".into()], now).unwrap();
        let result = transfer.handle(
            Incoming {
                peer: "a".into(),
                message: Message::SnapshotChunk {
                    epoch: BlockHash::new([1; 32]),
                    offset: 0,
                    total: CHUNK_BYTES as u64 + 1,
                    digest: BlockHash::new([2; 32]),
                    data: vec![0; CHUNK_BYTES],
                },
            },
            now,
        );
        assert!(result
            .err()
            .unwrap()
            .to_string()
            .contains("local archive admission"));
        assert!(transfer.download.is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn snapshot_bounds_and_serve_budget_are_fixed() {
        let dir = tempfile::tempdir().unwrap();
        let mut transfer = receiver(dir.path());
        let epoch = BlockHash::new([1; 32]);
        let digest = BlockHash::new([2; 32]);
        let mut file = tempfile::tempfile_in(dir.path()).unwrap();
        file.write_all(&vec![3; CHUNK_BYTES * 2]).unwrap();
        transfer.published = Some(Published {
            epoch,
            height: 1,
            digest,
            file,
            length: CHUNK_BYTES * 2,
        });
        let now = Instant::now();
        let request = || Incoming {
            peer: "a".into(),
            message: Message::SnapshotRequest {
                epoch,
                offset: 0,
                digest: None,
            },
        };
        for _ in 0..64 {
            assert!(transfer.handle(request(), now).unwrap().is_some());
        }
        assert!(transfer.handle(request(), now).unwrap().is_none());
        assert!(transfer.handle(request(), now + RETRY_INTERVAL).unwrap().is_some());
        for message in [
            Message::SnapshotRequest {
                epoch,
                offset: 1,
                digest: Some(digest),
            },
            Message::SnapshotRequest {
                epoch,
                offset: CHUNK_BYTES as u64,
                digest: None,
            },
            Message::SnapshotChunk {
                epoch,
                offset: 0,
                total: 24,
                digest,
                data: vec![0; CHUNK_BYTES],
            },
            Message::SnapshotChunk {
                epoch,
                offset: 0,
                total: 25,
                digest,
                data: vec![0; 24],
            },
        ] {
            assert!(!is_bounded(&message));
        }
    }

    #[test]
    fn snapshot_publication_does_not_replace_existing_output() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("snapshot");
        publish(&output, &mut b"original".as_slice()).unwrap();
        assert!(publish(&output, &mut b"replacement".as_slice()).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        // Restart refuses the unauthenticated file instead of replacing it.
        let base = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
            burst_types::NetworkId::Dev,
            &burst_types::ProtocolParams::default(),
        )
        .unwrap();
        assert!(Transfer::new(
            Config {
                max_archive_bytes: Config::default().max_archive_bytes,
                serve: None,
                fetch: Some(Fetch {
                    epoch: Some(BlockHash::new([1; 32]).to_string()),
                    minimum_height: 1,
                    output,
                })
            },
            base,
            dir.path(),
        )
        .is_err());
    }
}
