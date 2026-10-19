use super::*;
use crate::{
    connection_registry::ConnectionRegistry, priority_queue::BlockPriorityQueue,
    shutdown::ShutdownController, wire_message::WireMessage,
};
use burst_governance::content::{ContentChunk, ContentReceiver, CONTENT_CHUNK_BYTES};
use burst_store::{block::BlockStore, StoreError};
use burst_store_lmdb::LmdbStore;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, RwLock};

pub(crate) const INBOX_CAPACITY: usize = 64;
const MAX_TRANSFERS: usize = 16;
const MAX_PER_ACCOUNT: usize = 2;
const TTL: Duration = Duration::from_secs(60);
const RETRY: Duration = Duration::from_secs(2);
type TransferActions = (Vec<(String, Packet)>, Vec<StateBlock>);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Packet {
    Request {
        proposal: BlockHash,
        offset: u32,
    },
    Chunk {
        proposal: BlockHash,
        chunk: ContentChunk,
    },
}

pub(crate) struct Incoming {
    pub peer: String,
    pub packet: Packet,
}

enum Data {
    Pending {
        receiver: ContentReceiver,
        offset: u32,
        peer: Option<String>,
        attempt: usize,
    },
    Ready(Arc<Vec<u8>>),
}

struct Entry {
    block: StateBlock,
    data: Data,
    created: Instant,
    due: Instant,
}

/// Bounded volatile staging only. Authenticated content enters durable storage
/// with the admitted block, never from an unsolicited peer announcement.
#[derive(Default)]
pub struct Transfers {
    entries: Mutex<HashMap<BlockHash, Entry>>,
}

impl Transfers {
    /// Recovery uses a separate pool whose ready blocks are never queued for
    /// admission. The caller owns certificate validation and semantic replay.
    pub(crate) async fn fetch(&self, block: &StateBlock) -> Result<Arc<Vec<u8>>, String> {
        authenticate(block)?;
        tokio::time::timeout(TTL, async {
            let mut tick = tokio::time::interval(Duration::from_millis(100));
            loop {
                tick.tick().await;
                self.defer(block)?;
                if let Some(bytes) = self.ready(block.hash)? {
                    self.remove(block.hash);
                    return Ok(bytes);
                }
            }
        })
        .await
        .map_err(|_| "recovery proposal content timed out".to_owned())?
    }

    fn slot(
        entries: &mut HashMap<BlockHash, Entry>,
        block: &StateBlock,
        now: Instant,
    ) -> Result<(), String> {
        entries.retain(|_, entry| now.duration_since(entry.created) < TTL);
        if !entries.contains_key(&block.hash)
            && (entries.len() >= MAX_TRANSFERS
                || entries
                    .values()
                    .filter(|entry| entry.block.account == block.account)
                    .count()
                    >= MAX_PER_ACCOUNT)
        {
            return Err("proposal content staging is full; retry later".into());
        }
        Ok(())
    }

    pub fn offer(&self, block: &StateBlock, bytes: &[u8]) -> Result<(), String> {
        authenticate(block)?
            .decode_verified(bytes)
            .map_err(|error| error.to_string())?;
        let now = Instant::now();
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| "proposal content pool unavailable")?;
        Self::slot(&mut entries, block, now)?;
        if entries
            .get(&block.hash)
            .is_some_and(|entry| matches!(entry.data, Data::Ready(_)))
        {
            return Ok(());
        }
        entries.insert(
            block.hash,
            Entry {
                block: block.clone(),
                data: Data::Ready(Arc::new(bytes.to_vec())),
                created: now,
                due: now + Duration::from_secs(1),
            },
        );
        Ok(())
    }

    pub(crate) fn defer(&self, block: &StateBlock) -> Result<(), String> {
        let manifest = authenticate(block)?;
        let now = Instant::now();
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| "proposal content pool unavailable")?;
        Self::slot(&mut entries, block, now)?;
        if let std::collections::hash_map::Entry::Vacant(slot) = entries.entry(block.hash) {
            slot.insert(Entry {
                block: block.clone(),
                created: now,
                due: now,
                data: Data::Pending {
                    receiver: ContentReceiver::new(manifest).map_err(|error| error.to_string())?,
                    offset: 0,
                    peer: None,
                    attempt: 0,
                },
            });
        }
        Ok(())
    }

    pub(crate) fn ready(&self, hash: BlockHash) -> Result<Option<Arc<Vec<u8>>>, String> {
        let entries = self
            .entries
            .lock()
            .map_err(|_| "proposal content pool unavailable")?;
        Ok(entries.get(&hash).and_then(|entry| match &entry.data {
            Data::Ready(bytes) if entry.created.elapsed() < TTL => Some(bytes.clone()),
            _ => None,
        }))
    }

    pub(crate) fn remove(&self, hash: BlockHash) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.remove(&hash);
        }
    }

    fn tick(&self, peers: &[String], now: Instant) -> Result<TransferActions, String> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| "proposal content pool unavailable")?;
        entries.retain(|_, entry| now.duration_since(entry.created) < TTL);
        let mut requests = Vec::new();
        let mut ready = Vec::new();
        for entry in entries.values_mut().filter(|entry| now >= entry.due) {
            match &mut entry.data {
                Data::Pending {
                    offset,
                    peer,
                    attempt,
                    ..
                } if !peers.is_empty() => {
                    let next = peers[*attempt % peers.len()].clone();
                    *attempt = attempt.wrapping_add(1);
                    *peer = Some(next.clone());
                    entry.due = now + RETRY;
                    requests.push((
                        next,
                        Packet::Request {
                            proposal: entry.block.hash,
                            offset: *offset,
                        },
                    ));
                }
                Data::Ready(_) => {
                    ready.push(entry.block.clone());
                    entry.due = now + Duration::from_secs(1);
                }
                _ => (),
            }
        }
        Ok((requests, ready))
    }

    fn receive(
        &self,
        sender: &str,
        proposal: BlockHash,
        chunk: &ContentChunk,
    ) -> Result<(), String> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| "proposal content pool unavailable")?;
        let Some(entry) = entries.get_mut(&proposal) else {
            return Ok(());
        };
        if entry.created.elapsed() >= TTL {
            return Ok(());
        }
        let Data::Pending {
            receiver,
            offset,
            peer,
            ..
        } = &mut entry.data
        else {
            return Ok(());
        };
        if peer.as_deref() != Some(sender) || chunk.offset != *offset {
            return Ok(());
        }
        match receiver.push(chunk) {
            Ok(Some(content)) => {
                entry.data = Data::Ready(Arc::new(
                    content
                        .encode_content()
                        .map_err(|error| error.to_string())?,
                ))
            }
            Ok(None) => {
                *offset += chunk.bytes.len() as u32;
                *peer = None;
            }
            Err(_) => {
                *receiver = ContentReceiver::new(authenticate(&entry.block)?)
                    .map_err(|error| error.to_string())?;
                *offset = 0;
                *peer = None;
            }
        }
        entry.due = Instant::now();
        Ok(())
    }
}

fn response(store: &LmdbStore, proposal: BlockHash, offset: u32) -> Result<Packet, String> {
    use bincode::Options;
    let txn = store.tx_begin_read().map_err(|error| error.to_string())?;
    let block_bytes = match store.block_store().get_block_txn(&txn, &proposal) {
        Ok(bytes) => bytes,
        Err(StoreError::NotFound(_)) => store
            .meta_store()
            .get_meta_txn(&txn, &format!("checkpoint_history/v1/{proposal}"))
            .map_err(|error| error.to_string())?
            .or(store
                .meta_store()
                .fork_candidate_txn(&txn, &proposal)
                .map_err(|error| error.to_string())?)
            .ok_or("proposal block is missing")?,
        Err(error) => return Err(error.to_string()),
    };
    let block: StateBlock = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(burst_types::MAX_P2P_MESSAGE_BYTES as u64)
        .reject_trailing_bytes()
        .deserialize(&block_bytes)
        .map_err(|error| error.to_string())?;
    if block.hash != proposal {
        return Err("proposal storage identity mismatch".into());
    }
    let manifest = authenticate(&block)?;
    if offset >= manifest.length || !(offset as usize).is_multiple_of(CONTENT_CHUNK_BYTES) {
        return Err("invalid content request offset".into());
    }
    let bytes = match store
        .meta_store()
        .get_meta_txn(&txn, &key(proposal))
        .map_err(|error| error.to_string())?
    {
        Some(bytes) => bytes,
        None => store
            .meta_store()
            .get_meta_txn(
                &txn,
                &crate::confirmed_history::proposal_archive_key(proposal),
            )
            .map_err(|error| error.to_string())?
            .or(store
                .meta_store()
                .fork_content_txn(&txn, &proposal)
                .map_err(|error| error.to_string())?)
            .ok_or("proposal content is missing")?,
    };
    manifest
        .decode_verified(&bytes)
        .map_err(|error| error.to_string())?;
    let start = offset as usize;
    let end = (start + CONTENT_CHUNK_BYTES).min(bytes.len());
    Ok(Packet::Chunk {
        proposal,
        chunk: ContentChunk {
            manifest,
            offset,
            bytes: bytes[start..end].to_vec(),
        },
    })
}

pub(crate) async fn run(
    transfers: Arc<Transfers>,
    recovery: Arc<Transfers>,
    store: Arc<LmdbStore>,
    queue: Arc<BlockPriorityQueue>,
    registry: Arc<RwLock<ConnectionRegistry>>,
    mut inbox: mpsc::Receiver<Incoming>,
    shutdown: Arc<ShutdownController>,
) {
    let mut stop = shutdown.subscribe();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut served = HashMap::<String, usize>::new();
    let mut window = Instant::now();
    loop {
        tokio::select! {
            biased;
            _ = stop.recv() => break,
            _ = tick.tick() => {
                if window.elapsed() >= Duration::from_secs(1) { served.clear(); window = Instant::now(); }
                let mut peers: Vec<_> = registry.read().await.peer_ids().into_iter().cloned().collect();
                peers.sort();
                if let Ok((requests, ready)) = transfers.tick(&peers, Instant::now()) {
                    for (peer, packet) in requests { send(&registry, &peer, packet).await; }
                    for block in ready {
                        if store.block_store().exists(&block.hash).unwrap_or(false) { transfers.remove(block.hash); }
                        else { queue.push(block).await; }
                    }
                }
                if let Ok((requests, _)) = recovery.tick(&peers, Instant::now()) {
                    for (peer, packet) in requests { send(&registry, &peer, packet).await; }
                }
            }
            incoming = inbox.recv() => {
                let Some(incoming) = incoming else { break; };
                match incoming.packet {
                    Packet::Request { proposal, offset } => {
                        if served.values().sum::<usize>() >= 64 { continue; }
                        let count = served.entry(incoming.peer.clone()).or_default();
                        if *count >= 16 { continue; }
                        *count += 1;
                        if let Ok(packet) = response(&store, proposal, offset) { send(&registry, &incoming.peer, packet).await; }
                    }
                    Packet::Chunk { proposal, chunk } => {
                        if let Err(error) = transfers.receive(&incoming.peer, proposal, &chunk) {
                            tracing::debug!(%error, "proposal content transfer deferred");
                        }
                        if let Err(error) = recovery.receive(&incoming.peer, proposal, &chunk) {
                            tracing::debug!(%error, "recovery proposal content transfer deferred");
                        }
                    }
                }
            }
        }
    }
}

async fn send(registry: &RwLock<ConnectionRegistry>, peer: &str, packet: Packet) {
    if let Ok(bytes) = WireMessage::GovernanceContent(packet).encode() {
        if bytes.len() <= burst_types::MAX_P2P_MESSAGE_BYTES {
            registry.read().await.send(peer, bytes);
        }
    }
}

#[cfg(test)]
mod tests;
