use super::*;

// Responses contain a prefix of whole block bodies within the 1 MiB body budget.
pub(crate) use burst_messages::checkpoint_sync::MAX_HEADS;


pub(super) fn identity(starts: &[BlockHash]) -> BlockHash {
    let mut bytes = b"BURST/witness-heads/v1\0".to_vec();
    for hash in starts {
        bytes.extend_from_slice(hash.as_bytes());
    }
    BlockHash::new(burst_crypto::blake2b_256(&bytes))
}

pub(super) fn validate(cursors: &[HistoryCursor], wanted: &[BlockHash]) -> Result<(), StoreError> {
    if cursors.is_empty() || cursors.len() > MAX_HEADS || cursors.len() != wanted.len() {
        return Err(invalid("invalid exact witness count"));
    }
    let mut heads = HashSet::new();
    for cursor in cursors {
        let head = cursor
            .next()
            .ok_or_else(|| invalid("empty exact witness cursor"))?;
        if cursor.context() != cursors[0].context() || !heads.insert(head.hash) {
            return Err(invalid("duplicate or foreign exact witness cursor"));
        }
    }
    if wanted.iter().any(|hash| !heads.remove(hash)) || !heads.is_empty() {
        return Err(invalid("exact witnesses differ from requested heads"));
    }
    Ok(())
}

pub(crate) async fn fetch(
    registry: &Arc<RwLock<ConnectionRegistry>>,
    cursors: Vec<HistoryCursor>,
) -> Result<Vec<HistoricalBlock>, StoreError> {
    let wanted = cursors
        .iter()
        .map(|cursor| {
            cursor
                .next()
                .map(|head| head.hash)
                .ok_or_else(|| invalid("empty exact witness cursor"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    request(registry, Roots::Exact(cursors), wanted).await
}

pub(super) fn read(store: &LmdbStore, starts: &[BlockHash]) -> Result<Vec<Vec<u8>>, StoreError> {
    let mut blocks = Vec::with_capacity(starts.len());
    let mut bytes = 0usize;
    for hash in starts {
        let block = history::read_chunk(store, *hash, 1)
            .map_err(invalid)?
            .pop()
            .ok_or_else(|| invalid("missing exact witness body"))?;
        let next = bytes
            .checked_add(block.len())
            .ok_or_else(|| invalid("exact witness byte overflow"))?;
        if next > history::MAX_WITNESS_BYTES {
            break;
        }
        bytes = next;
        blocks.push(block);
    }
    Ok(blocks)
}

pub(super) fn accept(
    cursors: &HashMap<BlockHash, HistoryCursor>,
    starts: &[BlockHash],
    blocks: Vec<Vec<u8>>,
) -> Result<Vec<HistoricalBlock>, StoreError> {
    if blocks.is_empty() || blocks.len() > starts.len() || starts.len() != cursors.len() {
        return Err(invalid("incomplete exact witness batch"));
    }
    let mut proofs = Vec::with_capacity(starts.len());
    for (hash, bytes) in starts.iter().zip(blocks) {
        let mut cursor = cursors
            .get(hash)
            .ok_or_else(|| invalid("unrequested exact witness"))?
            .clone();
        proofs.extend(cursor.accept(&[bytes]).map_err(invalid)?);
    }
    Ok(proofs)
}

impl Transfer {
    pub(super) fn serve(
        &mut self,
        store: &LmdbStore,
        peer: String,
        request: Request<'_>,
        now: Instant,
    ) -> Result<Option<Incoming>, StoreError> {
        let Request {
            start,
            offset,
            digest,
            heads,
        } = request;
        if now.saturating_duration_since(self.serve_window_start) >= RETRY_INTERVAL {
            self.serve_window_start = now;
            self.response_attempts = 0;
            self.load_attempts = 0;
        }
        if self.response_attempts >= 64 {
            tracing::debug!(%start, %peer, "historical witness response budget exhausted");
            return Ok(None);
        }
        self.response_attempts += 1;
        if self.export.as_ref().is_none_or(|cached| {
            cached.start != start
                || cached.heads != heads.is_some()
                || (offset == 0 && now.saturating_duration_since(cached.loaded_at) >= RETRY_INTERVAL)
        }) {
            if self.load_attempts >= 4 {
                tracing::debug!(%start, %peer, "historical witness database load budget exhausted");
                return Ok(None);
            }
            self.load_attempts += 1;
            let blocks = match heads {
                Some(starts) => read(store, starts)?,
                None => history::read_chunk(store, start, history::MAX_WITNESS_BLOCKS)
                    .map_err(invalid)?,
            };
            let bytes = encode(&blocks)?;
            self.export = Some(Export {
                start,
                heads: heads.is_some(),
                digest: BlockHash::new(burst_crypto::blake2b_256(&bytes)),
                bytes,
                loaded_at: now,
            });
        }
        let cached = self
            .export
            .as_ref()
            .ok_or_else(|| invalid("witness export missing"))?;
        if digest.is_some_and(|digest| digest != cached.digest)
            || offset as usize >= cached.bytes.len()
        {
            return Ok(None);
        }
        let end = (offset as usize + CHUNK_BYTES).min(cached.bytes.len());
        Ok(Some(Incoming {
            peer,
            message: Message::WitnessChunk {
                start,
                offset,
                total: cached.bytes.len() as u32,
                digest: cached.digest,
                data: cached.bytes[offset as usize..end].to_vec(),
            },
        }))
    }
}

pub(super) struct Request<'a> {
    pub start: BlockHash,
    pub offset: u32,
    pub digest: Option<BlockHash>,
    pub heads: Option<&'a [BlockHash]>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::block::BlockStore;

    #[test]
    fn exact_head_requests_reject_noncanonical_roots_and_offsets() {
        let roots = vec![BlockHash::new([1; 32]), BlockHash::new([2; 32])];
        let valid = |starts, offset, digest| {
            is_bounded(&Message::WitnessHeadsRequest {
                starts,
                offset,
                digest,
            })
        };
        assert!(valid(roots.clone(), 0, None));
        assert!(valid(
            roots.clone(),
            CHUNK_BYTES as u32,
            Some(BlockHash::new([3; 32]))
        ));
        assert!(!valid(vec![], 0, None));
        assert!(!valid(vec![BlockHash::ZERO], 0, None));
        assert!(!valid(vec![roots[0], roots[0]], 0, None));
        assert!(!valid(vec![roots[1], roots[0]], 0, None));
        assert!(!valid(
            (1..=65).map(|n| BlockHash::new([n; 32])).collect(),
            0,
            None
        ));
        assert!(!valid(roots.clone(), 1, None));
        assert!(!valid(roots.clone(), CHUNK_BYTES as u32, None));
        assert!(!valid(roots.clone(), 0, Some(BlockHash::ZERO)));
        assert_ne!(identity(&roots), identity(&roots[..1]));
    }

    #[test]
    fn exact_head_server_splits_large_bodies_without_raising_payload_or_load_limits() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let mut starts = Vec::new();
        for index in 0..MAX_HEADS {
            let mut block = crate::genesis_state::block(
                burst_types::NetworkId::Dev,
                &burst_types::ProtocolParams::default(),
            );
            block.release_sources = (1u16..=1024)
                .map(|n| {
                    let mut bytes = [0; 32];
                    bytes[..2].copy_from_slice(&n.to_be_bytes());
                    BlockHash::new(bytes)
                })
                .collect();
            block.timestamp = burst_types::Timestamp::new(block.timestamp.as_secs() + index as u64);
            block.hash = block.compute_hash();
            store
                .block_store()
                .put_block(&block.hash, &bincode::serialize(&block).unwrap())
                .unwrap();
            starts.push(block.hash);
        }
        starts.sort_by_key(|hash| *hash.as_bytes());
        let blocks = read(&store, &starts).unwrap();
        assert!(!blocks.is_empty() && blocks.len() < starts.len());
        assert!(blocks.iter().map(Vec::len).sum::<usize>() <= history::MAX_WITNESS_BYTES);
        assert!(encode(&blocks).unwrap().len() <= MAX_BATCH_BYTES);
        for (bytes, expected) in blocks.iter().zip(&starts) {
            let block: burst_ledger::StateBlock = bincode::deserialize(bytes).unwrap();
            assert_eq!(block.hash, *expected);
        }
        let now = Instant::now();
        let mut server = Transfer::new(now);
        for (index, hash) in starts.iter().take(5).enumerate() {
            let response = server
                .handle(
                    &store,
                    Incoming {
                        peer: "peer".into(),
                        message: Message::WitnessHeadsRequest {
                            starts: vec![*hash],
                            offset: 0,
                            digest: None,
                        },
                    },
                    now,
                )
                .unwrap();
            assert_eq!(response.is_some(), index < 4);
        }
        assert_eq!(server.load_attempts, 4);
        // The fourth export remains usable without another database load.
        let request = |start| Incoming {
            peer: "peer".into(),
            message: Message::WitnessHeadsRequest {
                starts: vec![start],
                offset: 0,
                digest: None,
            },
        };
        assert!(server
            .handle(&store, request(starts[3]), now)
            .unwrap()
            .is_some());
        assert_eq!(server.load_attempts, 4);
        assert!(server
            .handle(&store, request(starts[4]), now + RETRY_INTERVAL / 2)
            .unwrap()
            .is_none());
        let reply = server
            .handle(&store, request(starts[4]), now + RETRY_INTERVAL)
            .unwrap()
            .expect("deferred valid request must succeed in the next load window");
        assert!(
            matches!(reply.message, Message::WitnessChunk { start, offset: 0, .. }
            if start == identity(&[starts[4]]))
        );
        assert_eq!(server.load_attempts, 1);
    }
}
