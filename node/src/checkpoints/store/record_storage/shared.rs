//! Local shared-head representation; emitted bytes remain the original record.
use super::{invalid, MAX_RECORD_BYTES};
use crate::paged_metadata::{Commitment, Location, Reader};
use burst_store::StoreError;
use burst_store_lmdb::{checkpoint_heads::{HeadNode, NodeHash}, LmdbWriteTransaction};
use std::io::Read;
use std::borrow::Cow;

const MAGIC: &[u8; 16] = b"BURST-HEAD-REC\0\x01";
const HEADER: usize = 112;
const HEAD_BYTES: usize = 113; // Fixed bincode string length + canonical 65-byte address + height/hash.

pub(super) fn is_shared(bytes: &[u8]) -> Result<bool, StoreError> {
    if bytes.starts_with(MAGIC) { Ok(true) }
    else if bytes.starts_with(b"BURST-HEAD-REC") { Err(invalid("unsupported shared head record version")) }
    else { Ok(false) }
}

struct Header { size: usize, hash: NodeHash, root: NodeHash, count: usize, prefix: usize, suffix: usize }
impl Header {
    fn decode(bytes: &[u8], max: usize) -> Result<Self, StoreError> {
        if bytes.len() != HEADER || !is_shared(bytes)? { return Err(invalid("invalid shared record descriptor")); }
        let integer = |at| -> Result<usize, StoreError> {
            usize::try_from(u64::from_be_bytes(bytes[at..at+8].try_into().unwrap())).map_err(invalid)
        };
        let result = Self { size: integer(16)?, hash: bytes[24..56].try_into().unwrap(),
            root: bytes[56..88].try_into().unwrap(), count: integer(88)?, prefix: integer(96)?, suffix: integer(104)? };
        let size = result.count.checked_mul(HEAD_BYTES).and_then(|n| n.checked_add(8))
            .and_then(|n| n.checked_add(result.prefix)).and_then(|n| n.checked_add(result.suffix));
        if result.count == 0 || result.count > crate::confirmed_history::MAX_BLOCKS as usize
            || result.prefix == 0 || size != Some(result.size) || result.size > max.min(MAX_RECORD_BYTES as usize) {
            return Err(invalid("shared record exceeds logical size/count bound"));
        }
        Ok(result)
    }
    fn encode(&self) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec(); bytes.extend_from_slice(&(self.size as u64).to_be_bytes());
        bytes.extend_from_slice(&self.hash); bytes.extend_from_slice(&self.root);
        for n in [self.count, self.prefix, self.suffix] { bytes.extend_from_slice(&(n as u64).to_be_bytes()); }
        bytes
    }
}

struct Envelope { descriptor: String, pages: String }
impl Envelope {
    fn new(key: &str) -> Self {
        let hash = burst_types::BlockHash::new(burst_crypto::blake2b_256(key.as_bytes()));
        Self { descriptor: format!("checkpoint_heads/envelope/v1/{hash}"),
            pages: format!("checkpoint_heads/envelope-pages/v1/{hash}/") }
    }
}
impl Location for Envelope {
    fn descriptor_key(&self) -> &str { &self.descriptor }
    fn page_key(&self, index: usize) -> String { format!("{}{index:08x}", self.pages) }
}

pub(super) struct Prepared<'a> {
    bytes: Cow<'a, [u8]>, prefix: usize, end: usize,
    heads: Vec<([u8; 32], [u8; 40])>,
}

/// Prepare records larger than one page with canonical, ordered account heads
/// for shared storage. Return `None` for unsupported layouts; no certificate
/// or economic validation occurs here.
pub(super) fn prepare(bytes: &[u8]) -> Result<Option<Prepared<'_>>, StoreError> {
    prepare_with_poll(bytes, &|| Ok(()))
}

fn prepare_with_poll<'a>(bytes: &'a [u8], poll: &dyn Fn() -> Result<(), StoreError>)
    -> Result<Option<Prepared<'a>>, StoreError> {
    poll()?;
    if bytes.len() <= crate::paged_metadata::PAGE_BYTES { return Ok(None); }
    let Ok(record) = super::super::chain::decode_record(bytes) else { return Ok(None); };
    poll()?;
    let heads = record.heads();
    if heads.is_empty() || heads.len() > crate::confirmed_history::MAX_BLOCKS as usize { return Ok(None); }
    let prefix = usize::try_from(bincode::serialized_size(record.certificate()).map_err(invalid)?).map_err(invalid)?
        .checked_add(1).ok_or_else(|| invalid("record prefix overflow"))?;
    let span = heads.len().checked_mul(HEAD_BYTES).and_then(|n| n.checked_add(8))
        .ok_or_else(|| invalid("record head span overflow"))?;
    let end = prefix.checked_add(span).filter(|n| *n <= bytes.len()).ok_or_else(|| invalid("record head span exceeds bytes"))?;
    if bytes[prefix..prefix+8] != (heads.len() as u64).to_le_bytes() { return Ok(None); }
    let mut rows = Vec::with_capacity(heads.len());
    let mut previous = None;
    for (index, head) in heads.iter().enumerate() {
        if index % 128 == 0 { poll()?; }
        let Some(key) = burst_crypto::decode_address(head.account.as_str()) else { return Ok(None); };
        if previous.is_some_and(|old| old >= key) || head.height == 0 || head.hash.is_zero()
            || burst_crypto::derive_address(&burst_types::PublicKey(key)) != head.account {
            return Ok(None);
        }
        let encoded = bincode::serialize(head).map_err(invalid)?;
        let start = prefix + 8 + index * HEAD_BYTES;
        if encoded.len() != HEAD_BYTES || bytes[start..start+HEAD_BYTES] != encoded { return Ok(None); }
        let mut value = [0; 40]; value[..8].copy_from_slice(&head.height.to_be_bytes());
        value[8..].copy_from_slice(head.hash.as_bytes()); rows.push((key, value)); previous = Some(key);
    }
    Ok(Some(Prepared { bytes: Cow::Borrowed(bytes), prefix, end, heads: rows }))
}

fn prepare_owned(bytes: Vec<u8>, poll: &dyn Fn() -> Result<(), StoreError>) -> Result<Option<Prepared<'static>>, StoreError> {
    let Some(Prepared { prefix, end, heads, .. }) = prepare_with_poll(&bytes, poll)? else { return Ok(None); };
    Ok(Some(Prepared { bytes: Cow::Owned(bytes), prefix, end, heads }))
}

fn node(node: crate::ordered_radix::Node<[u8; 40], NodeHash>) -> HeadNode {
    match node {
        crate::ordered_radix::Node::Leaf { key, value } => HeadNode::Leaf { account: key,
            height: u64::from_be_bytes(value[..8].try_into().unwrap()), hash: value[8..].try_into().unwrap() },
        crate::ordered_radix::Node::Branch { bit, left, right } => HeadNode::Branch { bit, left, right },
    }
}

impl Prepared<'_> {
    /// Pin before releasing the old descriptor: an unchanged root may be shared.
    pub(super) fn build_root(&self, batch: &mut LmdbWriteTransaction<'_>) -> Result<NodeHash, StoreError> {
        let mut builder = crate::ordered_radix::Builder::new();
        for (key, value) in &self.heads {
            builder.push(*key, *value, |value| batch.intern_checkpoint_head(&node(value)))?;
        }
        let (root, count) = builder.finish(|value| batch.intern_checkpoint_head(&node(value)))?;
        if count != self.heads.len() { return Err(invalid("shared record builder changed count")); }
        let root = root.ok_or_else(|| invalid("shared record has no root"))?;
        batch.retain_checkpoint_head(&root)?; Ok(root)
    }
    pub(super) fn publish(self, batch: &mut LmdbWriteTransaction<'_>, key: &str, root: NodeHash) -> Result<(), StoreError> {
        use std::io::Write;
        let mut writer = crate::paged_metadata::Writer::new_at(Envelope::new(key), batch)?;
        writer.write_all(&self.bytes[..self.prefix]).map_err(invalid)?;
        writer.write_all(&self.bytes[self.end..]).map_err(invalid)?;
        writer.finish()?;
        let descriptor = Header { size: self.bytes.len(), hash: burst_crypto::blake2b_256(&self.bytes), root,
            count: self.heads.len(), prefix: self.prefix, suffix: self.bytes.len()-self.end };
        batch.put_meta(key, &descriptor.encode())
    }
}

pub(super) fn visit(
    key: &str, stored: &[u8], get: impl FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>,
    mut get_node: impl FnMut(&NodeHash) -> Result<Option<HeadNode>, StoreError>, max: usize,
    mut output: impl FnMut(&[u8]) -> Result<(), StoreError>,
) -> Result<Commitment, StoreError> {
    let header = Header::decode(stored, max)?;
    let mut envelope = Reader::open_at_bounded(Envelope::new(key), get, header.prefix+header.suffix)?
        .ok_or_else(|| invalid("shared record envelope is missing"))?;
    let mut digest = burst_crypto::Blake2b256Hasher::default(); let mut size = 0usize;
    let mut emit = |bytes: &[u8]| -> Result<(), StoreError> {
        size = size.checked_add(bytes.len()).filter(|size| *size <= header.size)
            .ok_or_else(|| invalid("shared record reconstruction exceeds bound"))?;
        digest.update(bytes); output(bytes)
    };
    let copy = |reader: &mut dyn Read, mut count: usize, emit: &mut dyn FnMut(&[u8]) -> Result<(), StoreError>| {
        let mut buffer = [0; 4096];
        while count > 0 { let n = count.min(buffer.len()); reader.read_exact(&mut buffer[..n]).map_err(invalid)?;
            emit(&buffer[..n])?; count -= n; }
        Ok::<_, StoreError>(())
    };
    copy(&mut envelope, header.prefix, &mut emit)?; emit(&(header.count as u64).to_le_bytes())?;
    let mut pending = vec![(header.root, None)]; let mut visited = 0usize; let mut leaves = 0usize;
    let mut rebuilt = crate::ordered_radix::Builder::new();
    while let Some((hash, parent)) = pending.pop() {
        visited += 1;
        if visited > header.count * 2 - 1 { return Err(invalid("shared record tree exceeds node bound")); }
        let value = get_node(&hash)?.ok_or_else(|| invalid("shared record head node is missing"))?;
        if value.digest()? != hash { return Err(invalid("shared record head digest differs")); }
        match value {
            HeadNode::Branch { bit, left, right } => {
                if bit >= 256 || parent.is_some_and(|parent| bit <= parent) {
                    return Err(invalid("shared record branch depth is not increasing"));
                }
                pending.push((right, Some(bit))); pending.push((left, Some(bit)));
            }
            HeadNode::Leaf { account, height, hash } => {
                leaves += 1;
                if leaves > header.count { return Err(invalid("shared record has too many heads")); }
                let head = crate::confirmed_history::HistoryHead {
                    account: burst_crypto::derive_address(&burst_types::PublicKey(account)), height,
                    hash: burst_types::BlockHash::new(hash) };
                emit(&bincode::serialize(&head).map_err(invalid)?)?;
                let mut value = [0; 40]; value[..8].copy_from_slice(&height.to_be_bytes()); value[8..].copy_from_slice(&hash);
                rebuilt.push(account, value, |value| node(value).digest())?;
            }
        }
    }
    let (root, count) = rebuilt.finish(|value| node(value).digest())?;
    if root != Some(header.root) || count != header.count { return Err(invalid("shared record tree is not canonical")); }
    copy(&mut envelope, header.suffix, &mut emit)?;
    if envelope.read(&mut [0; 1]).map_err(invalid)? != 0 { return Err(invalid("shared envelope has trailing bytes")); }
    envelope.commitment()?;
    if size != header.size || digest.finalize() != header.hash { return Err(invalid("shared record logical commitment mismatch")); }
    Ok(Commitment { size, hash: header.hash })
}

/// Caller has already verified the complete record before releasing ownership.
pub(super) fn remove(batch: &mut LmdbWriteTransaction<'_>, key: &str, stored: &[u8]) -> Result<(), StoreError> {
    let header = Header::decode(stored, MAX_RECORD_BYTES as usize)?;
    batch.release_checkpoint_head(&header.root)?;
    crate::paged_metadata::delete_at(Envelope::new(key), batch, MAX_RECORD_BYTES as usize)?;
    batch.delete_meta(key)
}

#[cfg(test)]
mod tests;

pub(super) mod staging;
pub(super) mod publication;
pub(super) mod migration;

#[cfg(test)]
struct WriteProbe {
    stage: &'static str,
    start: Option<std::time::Instant>,
    acquired: Option<std::time::Instant>,
}
#[cfg(test)]
impl WriteProbe {
    fn new(stage: &'static str) -> Self {
        Self { stage, start: std::env::var_os("BURST_RECORD_MIGRATION_TIMING")
            .is_some().then(std::time::Instant::now), acquired: None }
    }
    fn acquired(&mut self) { if self.start.is_some() { self.acquired = Some(std::time::Instant::now()); } }
    fn finish(&mut self) {
        if let Some(start) = self.start.take() {
            if let Some(acquired) = self.acquired {
                println!("HISTORY_MIGRATION_WRITER stage={} wait_us={} scope_us={}", self.stage,
                    acquired.duration_since(start).as_micros(), acquired.elapsed().as_micros());
            }
        }
    }
}
#[cfg(test)]
impl Drop for WriteProbe {
    fn drop(&mut self) { self.finish(); }
}
