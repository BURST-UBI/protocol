//! On-demand ancestry proofs over existing authenticated peer connections.
//! The receiver owns the authenticated cursor. Transport never installs blocks.

use super::{
    invalid, CheckpointSyncMessage as Message, Incoming, CHUNK_BYTES, PEER_TIMEOUT, RETRY,
    TRANSFER_TIMEOUT,
};
use crate::{
    confirmed_replay::snapshot::{
        history::{self, HistoricalBlock, HistoryCursor},
        tail::{SnapshotTailPlan, MAX_TAIL_BLOCKS, MAX_TAIL_BYTES},
        SnapshotContinuation,
    },
    connection_registry::ConnectionRegistry,
};
use bincode::Options;
use burst_store::StoreError;
use burst_store_lmdb::LmdbStore;
use burst_types::{BlockHash, WalletAddress};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Instant,
};
use tokio::sync::{oneshot, RwLock};
mod cache;
mod exact;
mod service;
pub(crate) use service::start;
pub(crate) use exact::{fetch as fetch_exact_heads, MAX_HEADS as MAX_EXACT_HEADS};

const MAX_BATCH: usize = history::MAX_WITNESS_BYTES + 4 * history::MAX_WITNESS_BLOCKS + 4;
const MAX_WALK_BYTES: usize = 64 * 1024 * 1024;
const MAX_WALK_BLOCKS: usize = 131_072;
const LOCAL_WITNESS_PEER: &str = "local-witness-replay";

pub(crate) struct Command {
    pub roots: Roots,
    pub wanted: Vec<BlockHash>,
    pub result: oneshot::Sender<Result<Vec<HistoricalBlock>, StoreError>>,
}

pub(crate) enum Roots {
    Account(HistoryCursor),
    Plan(Vec<HistoryCursor>),
    Exact(Vec<HistoryCursor>),
    Suffix { cursor: HistoryCursor, floor: u64 },
}

pub(crate) async fn fetch_suffix(
    registry: &Arc<RwLock<ConnectionRegistry>>,
    cursor: &mut HistoryCursor,
    floor: u64,
) -> Result<Vec<burst_ledger::StateBlock>, StoreError> {
    let head = cursor
        .next()
        .ok_or_else(|| invalid("empty suffix cursor"))?;
    if floor >= head.height {
        return Err(invalid("invalid suffix floor"));
    }
    let floor = floor.max(
        head.height
            .saturating_sub(history::MAX_WITNESS_BLOCKS as u64),
    );
    let proofs = request(
        registry,
        Roots::Suffix {
            cursor: cursor.clone(),
            floor,
        },
        vec![head.hash],
    )
    .await?;
    if proofs.len() as u64 != head.height - floor {
        return Err(invalid("incomplete suffix batch"));
    }
    cursor.resume_suffix(proofs).map_err(invalid)
}

impl Roots {
    pub(crate) fn for_plan(
        base: &SnapshotContinuation,
        plan: &SnapshotTailPlan,
    ) -> Result<Self, StoreError> {
        if plan.context() != base.history_context() {
            return Err(invalid(
                "witness plan belongs to a different authenticated base",
            ));
        }
        let mut cursors = Vec::new();
        for head in plan.heads() {
            if let Some(cursor) = base.history_cursor(&head.account).map_err(invalid)? {
                cursors.push(cursor);
            }
        }
        Ok(Self::Plan(cursors))
    }
}

/// Fetch selected ancestors of one independently authenticated account head.
/// One request is active per node; cancellation drops it at the next poll.
/// A caller must create separate cursors for separate accounts. This API does
/// not change payment authority, ledger rows, signer promises or admission.
pub async fn fetch(
    registry: &Arc<RwLock<ConnectionRegistry>>,
    cursor: HistoryCursor,
    wanted: Vec<BlockHash>,
) -> Result<Vec<HistoricalBlock>, StoreError> {
    request(registry, Roots::Account(cursor), wanted).await
}

/// Retrieve the exact old-body dependencies of a checked suffix. Account hints
/// are discovered from hash-matching peer bodies, then membership is proved by
/// walking independently authenticated heads. Hints never become witnesses.
pub async fn fetch_plan(
    registry: &Arc<RwLock<ConnectionRegistry>>,
    base: &SnapshotContinuation,
    plan: &SnapshotTailPlan,
) -> Result<Vec<HistoricalBlock>, StoreError> {
    fetch_plan_reusing(registry, base, plan, Vec::new()).await
}

pub(crate) async fn fetch_plan_reusing(
    registry: &Arc<RwLock<ConnectionRegistry>>,
    base: &SnapshotContinuation,
    plan: &SnapshotTailPlan,
    available: Vec<HistoricalBlock>,
) -> Result<Vec<HistoricalBlock>, StoreError> {
    let roots = Roots::for_plan(base, plan)?;
    if plan.witness_hashes().is_empty() {
        return Ok(Vec::new());
    }
    validate_request(&roots, plan.witness_hashes())?;
    let mut missing: HashSet<_> = plan.witness_hashes().iter().copied().collect();
    let mut retained = Vec::new();
    for proof in available {
        if !missing.contains(&proof.block().hash) {
            continue;
        }
        let cursor = base.history_cursor(&proof.block().account).map_err(invalid)?;
        let belongs = proof.context() == plan.context()
            && cursor.and_then(|cursor| cursor.next()).is_some_and(|head| {
                proof.height() <= head.height
                    && (proof.height() != head.height || proof.block().hash == head.hash)
            });
        if belongs {
            missing.remove(&proof.block().hash);
            retained.push(proof);
        }
    }
    if missing.is_empty() {
        return Ok(retained);
    }
    let wanted: Vec<_> = plan.witness_hashes().iter().copied()
        .filter(|hash| missing.contains(hash)).collect();
    let committed = base
        .committed_history_cursors(&wanted)
        .map_err(invalid)?;
    retained.extend(request_with_committed(registry, roots, wanted, committed).await?);
    Ok(retained)
}

/// Offline export uses the same membership verifier and walk budgets as peer
/// recovery. Missing local ancestry is an error, never an unsigned shortcut.
pub(crate) fn read_plan(
    store: &LmdbStore,
    base: &SnapshotContinuation,
    plan: &SnapshotTailPlan,
) -> Result<Vec<HistoricalBlock>, StoreError> {
    let roots = Roots::for_plan(base, plan)?;
    if plan.witness_hashes().is_empty() {
        return Ok(Vec::new());
    }
    validate_request(&roots, plan.witness_hashes())?;
    let mut proofs = Vec::new();
    let mut bytes = 0usize;
    let mut retain = |incoming: Vec<HistoricalBlock>| -> Result<(), StoreError> {
        for proof in incoming {
            bytes = bytes
                .checked_add(bincode::serialized_size(proof.block()).map_err(invalid)? as usize)
                .filter(|bytes| *bytes <= MAX_TAIL_BYTES)
                .ok_or_else(|| invalid("planned witnesses exceed byte budget"))?;
            if proofs.len() >= MAX_TAIL_BLOCKS {
                return Err(invalid("planned witnesses exceed block budget"));
            }
            proofs.push(proof);
        }
        Ok(())
    };
    let mut exact: HashMap<_, _> = base
        .committed_history_cursors(plan.witness_hashes())
        .map_err(invalid)?
        .into_iter()
        .map(|cursor| (cursor.next().expect("committed cursor").hash, cursor))
        .collect();
    let mut remaining = Vec::new();
    for hash in plan.witness_hashes() {
        if let Some(cursor) = exact.remove(hash) {
            retain(read_request(store, Roots::Account(cursor), vec![*hash])?)?;
        } else {
            remaining.push(*hash);
        }
    }
    if !remaining.is_empty() {
        retain(read_request(store, roots, remaining)?)?;
    }
    Ok(proofs)
}

fn read_request(
    store: &LmdbStore,
    roots: Roots,
    wanted: Vec<BlockHash>,
) -> Result<Vec<HistoricalBlock>, StoreError> {
    let (result, mut received) = oneshot::channel();
    let mut transfer = Transfer::new(Instant::now());
    transfer.start(
        Command {
            roots,
            wanted,
            result,
        },
        Instant::now(),
    );
    loop {
        match received.try_recv() {
            Ok(result) => return result,
            Err(oneshot::error::TryRecvError::Closed) => {
                return Err(invalid("local historical witness recovery stopped"));
            }
            Err(oneshot::error::TryRecvError::Empty) => (),
        }
        if !transfer.local_progress(store, Instant::now()) {
            return Err(invalid(
                "snapshot export requires unavailable historical witnesses",
            ));
        }
    }
}

pub(crate) async fn request_with_committed(
    registry: &Arc<RwLock<ConnectionRegistry>>,
    roots: Roots,
    wanted: Vec<BlockHash>,
    committed: Vec<HistoryCursor>,
) -> Result<Vec<HistoricalBlock>, StoreError> {
    validate_request(&roots, &wanted)?;
    let context = match &roots {
        Roots::Account(cursor) => cursor.context(),
        Roots::Suffix { cursor, .. } => cursor.context(),
        Roots::Plan(cursors) => cursors[0].context(),
        Roots::Exact(cursors) => cursors[0].context(),
    };
    let mut exact = HashMap::new();
    let requested: HashSet<_> = wanted.iter().copied().collect();
    for cursor in committed {
        let head = cursor
            .next()
            .ok_or_else(|| invalid("empty committed witness cursor"))?;
        if cursor.context() != context
            || !requested.contains(&head.hash)
            || exact.insert(head.hash, cursor).is_some()
        {
            return Err(invalid("invalid committed witness cursor"));
        }
    }
    let mut proofs = Vec::new();
    let mut retained_bytes = 0usize;
    let mut retain = |incoming: Vec<HistoricalBlock>| -> Result<(), StoreError> {
        for proof in incoming {
            retained_bytes = retained_bytes
                .checked_add(bincode::serialized_size(proof.block()).map_err(invalid)? as usize)
                .filter(|bytes| *bytes <= MAX_TAIL_BYTES)
                .ok_or_else(|| invalid("planned witnesses exceed byte budget"))?;
            if proofs.len() >= MAX_TAIL_BLOCKS {
                return Err(invalid("planned witnesses exceed block budget"));
            }
            proofs.push(proof);
        }
        Ok(())
    };
    let mut remaining = Vec::new();
    let mut batch = Vec::new();
    for hash in wanted {
        if let Some(cursor) = exact.remove(&hash) {
            batch.push(cursor);
            if batch.len() == exact::MAX_HEADS {
                retain(exact::fetch(registry, std::mem::take(&mut batch)).await?)?;
            }
        } else {
            remaining.push(hash);
        }
    }
    if !batch.is_empty() {
        retain(exact::fetch(registry, batch).await?)?;
    }
    if !remaining.is_empty() {
        retain(request(registry, roots, remaining).await?)?;
    }
    Ok(proofs)
}

pub(crate) async fn request(
    registry: &Arc<RwLock<ConnectionRegistry>>,
    roots: Roots,
    wanted: Vec<BlockHash>,
) -> Result<Vec<HistoricalBlock>, StoreError> {
    validate_request(&roots, &wanted)?;
    let sender = registry
        .read()
        .await
        .witness_requests()
        .ok_or_else(|| invalid("historical witness service is not running"))?;
    let (result, received) = oneshot::channel();
    tokio::time::timeout(TRANSFER_TIMEOUT + PEER_TIMEOUT, async {
        sender
            .send(Command {
                roots,
                wanted,
                result,
            })
            .await
            .map_err(|_| invalid("historical witness service stopped"))?;
        received
            .await
            .map_err(|_| invalid("historical witness service stopped"))?
    })
    .await
    .map_err(|_| invalid("historical witness request timed out"))?
}

fn validate_request(roots: &Roots, wanted: &[BlockHash]) -> Result<(), StoreError> {
    if let Roots::Exact(cursors) = roots {
        return exact::validate(cursors, wanted);
    }
    if let Roots::Suffix { cursor, floor } = roots {
        let head = cursor
            .next()
            .ok_or_else(|| invalid("empty suffix cursor"))?;
        if head.height <= *floor
            || head.height - floor > MAX_TAIL_BLOCKS as u64
            || wanted != [head.hash]
        {
            return Err(invalid("invalid suffix request bounds"));
        }
    }
    let cursors = match roots {
        Roots::Account(cursor) => std::slice::from_ref(cursor),
        Roots::Suffix { cursor, .. } => std::slice::from_ref(cursor),
        Roots::Plan(cursors) => cursors.as_slice(),
        Roots::Exact(cursors) => cursors.as_slice(),
    };
    let mut accounts = HashSet::new();
    if cursors.is_empty()
        || cursors.len() > 131_072
        || cursors.iter().any(|cursor| {
            cursor.context() != cursors[0].context()
                || cursor
                    .next()
                    .is_none_or(|head| !accounts.insert(head.account))
        })
        || wanted.is_empty()
        || wanted.len() > MAX_TAIL_BLOCKS
        || wanted.iter().any(BlockHash::is_zero)
        || wanted.iter().collect::<HashSet<_>>().len() != wanted.len()
    {
        return Err(invalid("invalid historical witness request"));
    }
    Ok(())
}

pub(super) fn is_bounded(message: &Message) -> bool {
    match message {
        Message::WitnessHeadsRequest {
            starts,
            offset,
            digest,
        } => {
            exact::valid_starts(starts)
                && (*offset as usize) < MAX_BATCH
                && (*offset as usize).is_multiple_of(CHUNK_BYTES)
                && (*offset == 0 || digest.is_some())
                && digest.is_none_or(|hash| !hash.is_zero())
        }
        Message::WitnessRequest {
            start,
            offset,
            digest,
        } => {
            !start.is_zero()
                && (*offset as usize) < MAX_BATCH
                && (*offset as usize).is_multiple_of(CHUNK_BYTES)
                && (*offset == 0 || digest.is_some())
                && digest.is_none_or(|hash| !hash.is_zero())
        }
        Message::WitnessChunk {
            start,
            offset,
            total,
            digest,
            data,
        } => {
            !start.is_zero()
                && !digest.is_zero()
                && (5..=MAX_BATCH).contains(&(*total as usize))
                && offset < total
                && (*offset as usize).is_multiple_of(CHUNK_BYTES)
                && data.len() == CHUNK_BYTES.min((*total - *offset) as usize)
        }
        _ => false,
    }
}

fn encode(blocks: &[Vec<u8>]) -> Result<Vec<u8>, StoreError> {
    if blocks.is_empty() || blocks.len() > history::MAX_WITNESS_BLOCKS {
        return Err(invalid("invalid witness batch count"));
    }
    let mut raw = 0usize;
    let mut bytes = (blocks.len() as u32).to_be_bytes().to_vec();
    for block in blocks {
        raw = raw
            .checked_add(block.len())
            .filter(|size| *size <= history::MAX_WITNESS_BYTES)
            .ok_or_else(|| invalid("witness batch exceeds byte limit"))?;
        if block.is_empty() || block.len() > burst_types::MAX_P2P_MESSAGE_BYTES {
            return Err(invalid("invalid witness block size"));
        }
        bytes.extend_from_slice(&(block.len() as u32).to_be_bytes());
        bytes.extend_from_slice(block);
    }
    Ok(bytes)
}

fn decode(mut bytes: &[u8]) -> Result<Vec<Vec<u8>>, StoreError> {
    fn length(bytes: &mut &[u8]) -> Result<usize, StoreError> {
        let field = bytes
            .get(..4)
            .ok_or_else(|| invalid("truncated witness length"))?;
        let value = u32::from_be_bytes(field.try_into().map_err(invalid)?) as usize;
        *bytes = &bytes[4..];
        Ok(value)
    }
    if bytes.len() > MAX_BATCH {
        return Err(invalid("witness batch exceeds byte limit"));
    }
    let count = length(&mut bytes)?;
    if count == 0 || count > history::MAX_WITNESS_BLOCKS {
        return Err(invalid("invalid witness batch count"));
    }
    let mut blocks = Vec::with_capacity(count);
    let mut used = 0usize;
    for _ in 0..count {
        let size = length(&mut bytes)?;
        used = used
            .checked_add(size)
            .filter(|size| *size <= history::MAX_WITNESS_BYTES)
            .ok_or_else(|| invalid("witness batch exceeds byte limit"))?;
        if size == 0 || size > burst_types::MAX_P2P_MESSAGE_BYTES || size > bytes.len() {
            return Err(invalid("invalid witness block size"));
        }
        blocks.push(bytes[..size].to_vec());
        bytes = &bytes[size..];
    }
    if !bytes.is_empty() {
        return Err(invalid("trailing witness bytes"));
    }
    Ok(blocks)
}

struct Export {
    start: BlockHash,
    heads: bool,
    digest: BlockHash,
    bytes: Vec<u8>,
    loaded: Instant,
}
struct Download {
    peer: String,
    start: BlockHash,
    heads: Option<Vec<BlockHash>>,
    digest: Option<BlockHash>,
    total: Option<u32>,
    bytes: Vec<u8>,
    progress: Instant,
    requested: Instant,
}
impl Download {
    fn new(peer: String, start: BlockHash, now: Instant) -> Self {
        Self {
            peer,
            start,
            heads: None,
            digest: None,
            total: None,
            bytes: Vec::new(),
            progress: now,
            requested: now,
        }
    }
    fn request(&self) -> Incoming {
        Incoming {
            peer: self.peer.clone(),
            message: if let Some(starts) = &self.heads {
                Message::WitnessHeadsRequest {
                    starts: starts.clone(),
                    offset: self.bytes.len() as u32,
                    digest: self.digest,
                }
            } else {
                Message::WitnessRequest {
                    start: self.start,
                    offset: self.bytes.len() as u32,
                    digest: self.digest,
                }
            },
        }
    }
}
struct Job {
    exact: Option<HashMap<BlockHash, HistoryCursor>>,
    floor: Option<u64>,
    cursor: Option<HistoryCursor>,
    roots: HashMap<WalletAddress, HistoryCursor>,
    discover: bool,
    wanted: HashSet<BlockHash>,
    retained: Vec<HistoricalBlock>,
    retained_from_peer: Vec<bool>,
    result: oneshot::Sender<Result<Vec<HistoricalBlock>, StoreError>>,
    started: Instant,
    walked_bytes: usize,
    walked_blocks: usize,
    retained_bytes: usize,
}

impl Job {
    fn finish(self, cache: &mut cache::Cache) {
        // Traversed ancestors can exceed the cache even when the requested
        // proof set fits. Keep peer-fetched results most recent, without
        // promoting disk-resident bodies over proofs that needed transport.
        for (proof, from_peer) in self.retained.iter().zip(&self.retained_from_peer) {
            if *from_peer {
                cache.remember_requested(std::slice::from_ref(proof));
            }
        }
        let _ = self.result.send(Ok(self.retained));
    }

    fn download(&self, peer: String, now: Instant) -> Option<Download> {
        if let Some(exact) = &self.exact {
            let mut starts: Vec<_> = exact.keys().copied().collect();
            starts.sort_by_key(|hash| *hash.as_bytes());
            let mut download = Download::new(peer, exact::identity(&starts), now);
            download.heads = Some(starts);
            Some(download)
        } else {
            self.next_hash().map(|hash| Download::new(peer, hash, now))
        }
    }
    fn next_hash(&self) -> Option<BlockHash> {
        match &self.cursor {
            Some(cursor) => cursor.next().map(|head| head.hash),
            None => self
                .wanted
                .iter()
                .min_by_key(|hash| *hash.as_bytes())
                .copied(),
        }
    }
}

pub(crate) struct Transfer {
    cache: cache::Cache,
    job: Option<Job>,
    download: Option<Download>,
    export: Option<Export>,
    peer_cursor: usize,
    preferred_peer: Option<(String, Instant)>,
    window: Instant,
    served: usize,
    loaded: usize,
}
impl Transfer {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            cache: cache::Cache::default(),
            job: None,
            download: None,
            export: None,
            peer_cursor: 0,
            preferred_peer: None,
            window: now,
            served: 0,
            loaded: 0,
        }
    }
    pub(crate) fn start(&mut self, command: Command, now: Instant) {
        if self.job.is_some() {
            let _ = command
                .result
                .send(Err(invalid("historical witness recovery is busy")));
            return;
        }
        if let Err(error) = validate_request(&command.roots, &command.wanted) {
            let _ = command.result.send(Err(error));
            return;
        }
        let proofs = self.cache.available(&command.roots, &command.wanted);
        let retained_bytes = match proofs.iter().try_fold(0usize, |bytes, proof| {
            bytes.checked_add(bincode::serialized_size(proof.block()).map_err(invalid)? as usize)
                .filter(|bytes| *bytes <= MAX_TAIL_BYTES)
                .ok_or_else(|| invalid("cached witnesses exceed byte budget"))
        }) {
            Ok(bytes) => bytes,
            Err(error) => {
                let _ = command.result.send(Err(error));
                return;
            }
        };
        if proofs.len() == command.wanted.len() {
            tracing::debug!(wanted = command.wanted.len(), "historical witness cache satisfied request");
            let _ = command.result.send(Ok(proofs));
            return;
        }
        tracing::debug!(cached = proofs.len(), wanted = command.wanted.len(), "historical witness partial cache reuse");
        let mut wanted: HashSet<_> = command.wanted.into_iter().collect();
        for proof in &proofs {
            wanted.remove(&proof.block().hash);
        }
        let floor = match &command.roots {
            Roots::Suffix { floor, .. } => Some(*floor),
            _ => None,
        };
        let mut exact = None;
        let (cursor, roots, discover) = match command.roots {
            Roots::Exact(cursors) => {
                exact = Some(
                    cursors
                        .into_iter()
                        .map(|cursor| (cursor.next().expect("validated cursor").hash, cursor))
                        .filter(|(hash, _)| wanted.contains(hash))
                        .collect(),
                );
                (None, HashMap::new(), false)
            }
            Roots::Account(cursor) => (Some(cursor), HashMap::new(), false),
            Roots::Suffix { cursor, .. } => (Some(cursor), HashMap::new(), false),
            Roots::Plan(cursors) => (
                None,
                cursors
                    .into_iter()
                    .map(|cursor| {
                        (
                            cursor.next().expect("validated nonempty cursor").account,
                            cursor,
                        )
                    })
                    .collect(),
                true,
            ),
        };
        self.job = Some(Job {
            exact,
            floor,
            cursor,
            roots,
            discover,
            wanted,
            retained_from_peer: vec![true; proofs.len()],
            retained: proofs,
            result: command.result,
            started: now,
            walked_bytes: 0,
            walked_blocks: 0,
            retained_bytes,
        });
    }
    fn expire(&mut self, now: Instant) {
        if self.job.as_ref().is_some_and(|job| {
            job.result.is_closed() || now.saturating_duration_since(job.started) >= TRANSFER_TIMEOUT
        }) {
            self.download = None;
            if let Some(job) = self.job.take() {
                let _ = job
                    .result
                    .send(Err(invalid("historical witness request expired")));
            }
        }
    }
    pub(crate) fn poll(&mut self, mut peers: Vec<String>, now: Instant) -> Option<Incoming> {
        self.expire(now);
        self.job.as_ref()?;
        if let Some(download) = &mut self.download {
            if now.saturating_duration_since(download.progress) >= PEER_TIMEOUT {
                self.download = None;
                self.preferred_peer = None;
            } else if now.saturating_duration_since(download.requested) >= RETRY {
                download.requested = now;
                return Some(download.request());
            } else {
                return None;
            }
        }
        let peer = self.select_peer(&mut peers, now)?;
        let download = self.job.as_ref()?.download(peer, now)?;
        let reply = download.request();
        self.download = Some(download);
        Some(reply)
    }
    fn select_peer(&mut self, peers: &mut [String], now: Instant) -> Option<String> {
        peers.sort();
        if peers.is_empty() {
            self.preferred_peer = None;
            return None;
        }
        let preferred = self.preferred_peer.as_ref().and_then(|(peer, accepted)| {
            if now.saturating_duration_since(*accepted) < PEER_TIMEOUT * 3 {
                peers.binary_search(peer).ok()
            } else {
                None
            }
        });
        let index = match preferred {
            Some(index) => index,
            None => {
                self.preferred_peer = None;
                self.peer_cursor % peers.len()
            }
        };
        self.peer_cursor = index + 1;
        Some(peers[index].clone())
    }
    pub(crate) fn local_progress(&mut self, store: &LmdbStore, now: Instant) -> bool {
        self.expire(now);
        if self
            .download
            .as_ref()
            .is_some_and(|download| download.peer != LOCAL_WITNESS_PEER)
        {
            return false;
        }
        let Some(download) = self
            .job
            .as_ref()
            .and_then(|job| job.download(LOCAL_WITNESS_PEER.into(), now))
        else {
            return false;
        };
        let start = download.start;
        let exact_heads = download.heads.as_ref().map_or(0, Vec::len);
        let result = (|| {
            let blocks = match &download.heads {
                Some(starts) => exact::read(store, starts)?,
                None => match history::read_chunk(store, start, history::MAX_WITNESS_BLOCKS) {
                    Ok(blocks) => blocks,
                    Err(error) => self.cache.read_chunk(start)?.ok_or_else(|| invalid(error))?,
                },
            };
            let bytes = encode(&blocks)?;
            let digest = BlockHash::new(burst_crypto::blake2b_256(&bytes));
            self.download = Some(download);
            for (index, data) in bytes.chunks(CHUNK_BYTES).enumerate() {
                self.handle(
                    store,
                    Incoming {
                        peer: LOCAL_WITNESS_PEER.into(),
                        message: Message::WitnessChunk {
                            start,
                            offset: (index * CHUNK_BYTES) as u32,
                            total: bytes.len() as u32,
                            digest,
                            data: data.to_vec(),
                        },
                    },
                    now,
                )?;
            }
            Ok::<_, StoreError>(())
        })();
        if let Err(error) = result {
            self.download = None;
            tracing::debug!(%error, %start, exact_heads, "local witness unavailable; trying peers");
            return false;
        }
        true
    }
    pub(crate) fn handle(
        &mut self,
        store: &LmdbStore,
        incoming: Incoming,
        now: Instant,
    ) -> Result<Option<Incoming>, StoreError> {
        if !is_bounded(&incoming.message) {
            return Err(invalid("invalid witness transport bounds"));
        }
        self.expire(now);
        match incoming.message {
            Message::WitnessHeadsRequest {
                starts,
                offset,
                digest,
            } => self.serve(
                store,
                incoming.peer,
                exact::Request {
                    start: exact::identity(&starts),
                    offset,
                    digest,
                    heads: Some(&starts),
                },
                now,
            ),
            Message::WitnessRequest {
                start,
                offset,
                digest,
            } => self.serve(
                store,
                incoming.peer,
                exact::Request {
                    start,
                    offset,
                    digest,
                    heads: None,
                },
                now,
            ),
            Message::WitnessChunk {
                start,
                offset,
                total,
                digest,
                data,
            } => {
                let Some(active) = &self.download else {
                    return Ok(None);
                };
                if active.peer != incoming.peer
                    || active.start != start
                    || active.bytes.len() != offset as usize
                {
                    return Ok(None);
                }
                // Restore the hint only after cursor authentication. A matching
                // transport digest alone does not make a peer a useful provider.
                self.preferred_peer = None;
                let mut download = self
                    .download
                    .take()
                    .ok_or_else(|| invalid("witness download missing"))?;
                let job = self
                    .job
                    .as_mut()
                    .ok_or_else(|| invalid("witness request missing"))?;
                job.walked_bytes = job.walked_bytes.saturating_add(data.len());
                if job.walked_bytes > MAX_WALK_BYTES {
                    let job = self.job.take().expect("active job");
                    let _ = job
                        .result
                        .send(Err(invalid("witness recovery exceeds byte budget")));
                    return Ok(None);
                }
                if download.digest.is_some_and(|hash| hash != digest)
                    || download.total.is_some_and(|size| size != total)
                {
                    return Err(invalid("mixed witness chunks"));
                }
                download.digest = Some(digest);
                download.total = Some(total);
                download.bytes.extend_from_slice(&data);
                download.progress = now;
                if download.bytes.len() < total as usize {
                    download.requested = now;
                    let reply = download.request();
                    self.download = Some(download);
                    return Ok(Some(reply));
                }
                if BlockHash::new(burst_crypto::blake2b_256(&download.bytes)) != digest {
                    return Err(invalid("witness transport digest mismatch"));
                }
                let mut blocks = decode(&download.bytes)?;
                tracing::debug!(
                    %start,
                    peer = %incoming.peer,
                    blocks = blocks.len(),
                    bytes = download.bytes.len(),
                    exact_heads = download.heads.as_ref().map_or(0, Vec::len),
                    "historical witness batch received"
                );
                job.walked_blocks = job.walked_blocks.saturating_add(blocks.len());
                if job.walked_blocks > MAX_WALK_BLOCKS {
                    let job = self.job.take().expect("active job");
                    let _ = job
                        .result
                        .send(Err(invalid("witness recovery exceeds block budget")));
                    return Ok(None);
                }
                if let Some(cursors) = &job.exact {
                    let starts = download
                        .heads
                        .as_ref()
                        .ok_or_else(|| invalid("exact witness request lost its roots"))?;
                    let proofs = exact::accept(cursors, starts, blocks)?;
                    if !proofs.is_empty() && incoming.peer != LOCAL_WITNESS_PEER {
                        self.preferred_peer = Some((incoming.peer.clone(), now));
                    }
                    for proof in proofs {
                        job.exact
                            .as_mut()
                            .expect("exact roots")
                            .remove(&proof.block().hash);
                        job.wanted.remove(&proof.block().hash);
                        job.retained.push(proof);
                        job.retained_from_peer.push(incoming.peer != LOCAL_WITNESS_PEER);
                    }
                    if job.wanted.is_empty() {
                        let job = self.job.take().expect("active exact witness job");
                        job.finish(&mut self.cache);
                        return Ok(None);
                    }
                    let download = job
                        .download(incoming.peer, now)
                        .ok_or_else(|| invalid("remaining exact roots missing"))?;
                    let reply = download.request();
                    self.download = Some(download);
                    return Ok(Some(reply));
                }
                if job.cursor.is_none() {
                    let first: burst_ledger::StateBlock = bincode::DefaultOptions::new()
                        .with_fixint_encoding()
                        .reject_trailing_bytes()
                        .with_limit(burst_types::MAX_P2P_MESSAGE_BYTES as u64)
                        .deserialize(&blocks[0])
                        .map_err(invalid)?;
                    first.validate_format().map_err(invalid)?;
                    if first.hash != start || first.compute_hash() != start {
                        return Err(invalid(
                            "witness account hint does not match the requested hash",
                        ));
                    }
                    let Some(cursor) = job.roots.remove(&first.account) else {
                        let job = self.job.take().expect("active job");
                        let _ = job.result.send(Err(invalid(
                            "requested witness is outside the authenticated account prefixes",
                        )));
                        return Ok(None);
                    };
                    let next = cursor.next().expect("validated nonempty cursor").hash;
                    job.cursor = Some(cursor);
                    if next != start {
                        let next = Download::new(incoming.peer, next, now);
                        let reply = next.request();
                        self.download = Some(next);
                        return Ok(Some(reply));
                    }
                    // The hint already starts at the authenticated head. Run
                    // normal cursor validation below instead of downloading it twice.
                }
                if let Some(floor) = job.floor {
                    let height = job
                        .cursor
                        .as_ref()
                        .and_then(HistoryCursor::next)
                        .ok_or_else(|| invalid("suffix cursor exhausted"))?
                        .height;
                    blocks.truncate((height - floor) as usize);
                }
                let witnesses = job
                    .cursor
                    .as_mut()
                    .expect("active cursor")
                    .accept(&blocks)
                    .map_err(invalid)?;
                if incoming.peer != LOCAL_WITNESS_PEER {
                    if !witnesses.is_empty() {
                        self.preferred_peer = Some((incoming.peer.clone(), now));
                    }
                    self.cache.remember(&witnesses);
                }
                for witness in witnesses {
                    if job.floor.is_some() || job.wanted.remove(&witness.block().hash) {
                        job.retained_bytes = job.retained_bytes.saturating_add(
                            bincode::serialized_size(witness.block()).map_err(invalid)? as usize,
                        );
                        if job.retained_bytes > MAX_TAIL_BYTES
                            || job.retained.len() >= MAX_TAIL_BLOCKS
                        {
                            let job = self.job.take().expect("active job");
                            let _ = job
                                .result
                                .send(Err(invalid("retained witnesses exceed byte budget")));
                            return Ok(None);
                        }
                        job.retained.push(witness);
                        job.retained_from_peer.push(incoming.peer != LOCAL_WITNESS_PEER);
                    }
                }
                let complete = match job.floor {
                    Some(floor) => job
                        .cursor
                        .as_ref()
                        .and_then(HistoryCursor::next)
                        .is_none_or(|head| head.height <= floor),
                    None => job.wanted.is_empty(),
                };
                if complete {
                    let job = self.job.take().expect("active job");
                    job.finish(&mut self.cache);
                    return Ok(None);
                }
                let remaining_heads = job.discover
                    && job.wanted.iter().all(|hash| {
                        job.roots
                            .values()
                            .any(|cursor| cursor.next().is_some_and(|head| head.hash == *hash))
                    });
                if remaining_heads || job.cursor.as_ref().expect("active cursor").next().is_none() {
                    if !job.discover {
                        let job = self.job.take().expect("active job");
                        let _ = job.result.send(Err(invalid(
                            "requested witnesses are absent from the account prefix",
                        )));
                        return Ok(None);
                    }
                    job.cursor = None;
                }
                let next = Download::new(
                    incoming.peer,
                    job.next_hash().expect("remaining witnesses"),
                    now,
                );
                let reply = next.request();
                self.download = Some(next);
                Ok(Some(reply))
            }
            _ => Err(invalid("unexpected witness message")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn witness_peer_preference_expires_and_falls_back_after_failure_or_disconnect() {
        let now = Instant::now();
        let mut transfer = Transfer::new(now);
        let mut peers = vec!["b".into(), "a".into(), "c".into()];
        assert_eq!(transfer.select_peer(&mut peers, now).as_deref(), Some("a"));
        transfer.preferred_peer = Some(("b".into(), now));
        for _ in 0..3 {
            assert_eq!(transfer.select_peer(&mut peers, now).as_deref(), Some("b"));
        }
        assert_eq!(transfer.select_peer(&mut peers, now + PEER_TIMEOUT * 3).as_deref(), Some("c"));
        assert!(transfer.preferred_peer.is_none());
        transfer.preferred_peer = Some(("b".into(), now));
        assert_eq!(transfer.select_peer(&mut peers, now).as_deref(), Some("b"));
        transfer.preferred_peer = None;
        assert_eq!(transfer.select_peer(&mut peers, now).as_deref(), Some("c"));
        transfer.preferred_peer = Some(("b".into(), now));
        let mut disconnected = vec!["a".into(), "c".into()];
        assert!(transfer.select_peer(&mut disconnected, now).is_some());
        assert!(transfer.preferred_peer.is_none());
        assert!(transfer.select_peer(&mut [], now).is_none());
    }

    #[test]
    fn witness_batch_codec_bounds_lengths_before_allocating_bodies() {
        let blocks = vec![vec![1; burst_types::MAX_P2P_MESSAGE_BYTES], vec![2; 9_216]];
        let valid = encode(&blocks).unwrap();
        assert_eq!(decode(&valid).unwrap(), blocks);
        for end in [0, 3, 4, 7, valid.len() - 1] {
            assert!(decode(&valid[..end]).is_err());
        }
        let mut appended = valid.clone();
        appended.push(0);
        assert!(decode(&appended).is_err());
        let mut count = valid.clone();
        count[..4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(decode(&count).is_err());
        let mut length = valid;
        length[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(decode(&length).is_err());
        assert!(encode(&[]).is_err());
        assert!(encode(&[vec![]]).is_err());
        assert!(encode(&vec![vec![1]; history::MAX_WITNESS_BLOCKS + 1]).is_err());
    }

    #[test]
    fn witness_server_limits_responses_and_database_loads() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let now = Instant::now();
        let mut server = Transfer::new(now);
        let start = BlockHash::new([1; 32]);
        let bytes = encode(&[vec![1]]).unwrap();
        server.export = Some(Export {
            start,
            heads: false,
            digest: BlockHash::new(burst_crypto::blake2b_256(&bytes)),
            bytes,
            loaded: now,
        });
        for index in 0..65 {
            let reply = server
                .handle(
                    &store,
                    Incoming {
                        peer: "peer".into(),
                        message: Message::WitnessRequest {
                            start,
                            offset: 0,
                            digest: None,
                        },
                    },
                    now,
                )
                .unwrap();
            assert_eq!(reply.is_some(), index < 64);
        }
        for index in 0..5 {
            let result = server.handle(
                &store,
                Incoming {
                    peer: "peer".into(),
                    message: Message::WitnessRequest {
                        start: BlockHash::new([index + 2; 32]),
                        offset: 0,
                        digest: None,
                    },
                },
                now + RETRY,
            );
            if index < 4 {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_none());
            }
        }
        assert_eq!(server.loaded, 4);
    }
}
