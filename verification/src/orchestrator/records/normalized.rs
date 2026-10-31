//! Verification targets split into bounded storage rows.
//!
//! Each target has a header with member counts and eight member domains:
//! endorsements, endorser identities, selected verifiers, current votes, excluded
//! verifiers, prior votes, selection pool and vote contexts. Selected verifiers
//! and pool members use ordinal keys; other members use wallet-address keys.
//! Reconstruction checks row structure and counts, not source authenticity.
//! Database format selection and atomic writes belong to the caller.
use super::*;
use crate::state::Endorsement;
mod delta;
mod state;
pub use state::{visit_state_rows, StateReader, StateVisitor};
pub use delta::{visit_target_delta, visit_state_delta};
#[cfg(test)]
pub(crate) use delta::VISITS;

pub const PREFIX: &str = "verification_state/v2/target/";
const DOMAINS: usize = 8;
const MAX_TARGET_BYTES: u64 = 64 << 20;

#[derive(Serialize, Deserialize)]
struct Header {
    target: Target,
    counts: [u64; DOMAINS],
}

#[derive(Serialize, Deserialize)]
enum Member {
    Endorsement(Endorsement),
    Endorser(WalletAddress),
    Selected(u64, WalletAddress),
    Vote(VerifierVote),
    Excluded(WalletAddress),
    PriorVote(VerifierVote),
    Pool(u64, WalletAddress),
    Context(WalletAddress, BlockHash),
}

impl Member {
    fn domain(&self) -> usize {
        match self {
            Self::Endorsement(_) => 0, Self::Endorser(_) => 1,
            Self::Selected(..) => 2, Self::Vote(_) => 3, Self::Excluded(_) => 4,
            Self::PriorVote(_) => 5, Self::Pool(..) => 6, Self::Context(..) => 7,
        }
    }
    fn suffix(&self) -> Result<String, String> {
        let identity = match self {
            Self::Selected(index, _) | Self::Pool(index, _) => format!("{index:016x}"),
            Self::Endorsement(v) => address_hex(&v.endorser)?,
            Self::Vote(v) | Self::PriorVote(v) => address_hex(&v.verifier)?,
            Self::Endorser(a) | Self::Excluded(a) | Self::Context(a, _) => address_hex(a)?,
        };
        Ok(format!("{}/{identity}", self.domain()))
    }
}

fn address_hex(address: &WalletAddress) -> Result<String, String> {
    if !address.has_prefix_and_payload() { return Err("invalid normalized target address".into()); }
    Ok(address.as_str().bytes().map(|b| format!("{b:02x}")).collect())
}
fn root(address: &WalletAddress) -> Result<String, String> {
    Ok(format!("{PREFIX}{}/", address_hex(address)?))
}
/// Header-only lookup for display consumers. This does not authenticate the
/// target or validate its member rows; full reconstruction remains StateReader's job.
pub fn target_header_key(address: &WalletAddress) -> Result<String, String> {
    let key = format!("{}0", root(address)?);
    if key.len() > 511 { return Err("normalized verification key exceeds LMDB limit".into()); }
    Ok(key)
}

pub fn header_has_active_challenge(address: &WalletAddress, bytes: &[u8]) -> Result<bool, String> {
    let reader = TargetReader::new(address, bytes, MAX_TARGET_BYTES)?;
    Ok(reader.header.target.challenge.is_some())
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    let size = bincode::serialized_size(&(1u8, value)).map_err(|e| e.to_string())?;
    if size > burst_types::MAX_P2P_MESSAGE_BYTES as u64 {
        return Err("normalized verification row exceeds byte limit".into());
    }
    bincode::serialize(&(1u8, value)).map_err(|e| e.to_string())
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    if bytes.len() > burst_types::MAX_P2P_MESSAGE_BYTES {
        return Err("normalized verification row exceeds byte limit".into());
    }
    let (version, value): (u8, T) = bincode::DefaultOptions::new().with_fixint_encoding()
        .with_limit(bytes.len() as u64).reject_trailing_bytes().deserialize(bytes)
        .map_err(|e| e.to_string())?;
    if version != 1 { return Err("unsupported normalized verification row version".into()); }
    Ok(value)
}
fn put(rows: &mut BTreeMap<String, Vec<u8>>, key: String, bytes: Vec<u8>) -> Result<(), String> {
    if key.len() > 511 { return Err("normalized verification key exceeds LMDB limit".into()); }
    if rows.insert(key, bytes).is_some() { return Err("duplicate normalized verification member".into()); }
    Ok(())
}

/// Export a logical target into independently keyed fields. No database mutation.
/// Traverses and encodes every member. Incremental writers use
/// `visit_target_delta` to avoid exporting unrelated members.
pub fn target_rows(record: &VerificationRecord) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let Record::Target(target) = &record.0 else { return Err("expected target record".into()); };
    if record.snapshot_entry_bytes()? > MAX_TARGET_BYTES {
        return Err("normalized target exceeds snapshot byte limit".into());
    }
    if target.state.as_ref().is_some_and(|state| state.target != target.address)
        || target.challenge.as_ref().is_some_and(|challenge| challenge.target != target.address) {
        return Err("normalized target identity mismatch".into());
    }
    let prefix = root(&target.address)?;
    let mut target = (**target).clone();
    let mut rows = BTreeMap::new();
    let mut counts = [0; DOMAINS];
    let mut emit = |member: Member| -> Result<(), String> {
        counts[member.domain()] += 1;
        put(&mut rows, format!("{prefix}1/{}", member.suffix()?), encode(&member)?)
    };
    if let Some(state) = &mut target.state {
        for value in std::mem::take(&mut state.endorsements) { emit(Member::Endorsement(value))?; }
        for value in std::mem::take(&mut state.endorser_set) { emit(Member::Endorser(value))?; }
        for (index, value) in std::mem::take(&mut state.selected_verifiers).into_iter().enumerate() {
            emit(Member::Selected(index as u64, value))?;
        }
        for value in std::mem::take(&mut state.votes) { emit(Member::Vote(value))?; }
        for value in std::mem::take(&mut state.excluded_verifiers) { emit(Member::Excluded(value))?; }
    }
    if let Some(votes) = &mut target.votes {
        for value in std::mem::take(&mut votes.0) { emit(Member::PriorVote(value))?; }
    }
    if let Some(selection) = &mut target.selection {
        for (index, value) in selection.replace_storage_pool(Vec::new()).into_iter().enumerate() {
            emit(Member::Pool(index as u64, value))?;
        }
    }
    if let Some(contexts) = &mut target.vote_contexts {
        for (address, context) in std::mem::take(contexts) {
            emit(Member::Context(WalletAddress::new(address), context))?;
        }
    }
    put(&mut rows, format!("{prefix}0"), encode(&Header { target, counts })?)?;
    Ok(rows)
}

/// Validate a historical target key without reading any engine state.
pub fn legacy_target_address(key: &str) -> Result<WalletAddress, String> {
    let encoded = key.strip_prefix(VerificationRecord::STORAGE_PREFIX)
        .and_then(|key| key.strip_prefix("1/")).ok_or("expected legacy target key")?;
    if encoded.len() % 2 != 0 || key.len() > 511 { return Err("invalid legacy target key".into()); }
    let bytes = encoded.as_bytes().chunks_exact(2).map(|pair| {
        let text = std::str::from_utf8(pair).map_err(|_| "invalid legacy target address")?;
        u8::from_str_radix(text, 16).map_err(|_| "invalid legacy target address")
    }).collect::<Result<Vec<_>, _>>()?;
    let address = WalletAddress::new(std::str::from_utf8(&bytes).map_err(|_| "invalid legacy target address")?);
    if VerificationRecord::target_storage_key(&address)? != key {
        return Err("noncanonical legacy target key".into());
    }
    Ok(address)
}

/// Stream one target from a pinned sorted prefix/overlay. An empty stream means
/// absence. The caller's canonical byte budget is checked before member retention.
pub fn target_from_entries<'a>(
    address: &WalletAddress,
    entries: impl IntoIterator<Item = Result<(&'a str, &'a [u8]), String>>,
    max_bytes: u64,
) -> Result<Option<VerificationRecord>, String> {
    let mut entries = entries.into_iter();
    let Some(first) = entries.next() else { return Ok(None); };
    let (key, bytes) = first?;
    if key != target_header_key(address)? { return Err("missing normalized target header".into()); }
    let mut reader = TargetReader::new(address, bytes, max_bytes)?;
    for entry in entries {
        let (key, bytes) = entry?;
        reader.push(key, bytes)?;
    }
    reader.finish().map(Some)
}

/// Reconstruct one complete target from sorted physical rows. Header, identity,
/// member counts and ordinal continuity are checked before returning the record.
/// This does not check voter eligibility, signatures or the selection certificate.
pub fn target_from_rows(address: &WalletAddress, rows: &BTreeMap<String, Vec<u8>>)
    -> Result<VerificationRecord, String>
{
    let header_key = format!("{}0", root(address)?);
    let mut reader = TargetReader::new(address,
        rows.get(&header_key).ok_or("missing normalized target header")?, MAX_TARGET_BYTES)?;
    for (key, value) in rows {
        if key != &header_key { reader.push(key, value)?; }
    }
    reader.finish()
}

/// Retains decoded members only, not a second encoded row map.
struct TargetReader {
    prefix: String,
    header: Header,
    bytes: u64,
    limit: u64,
    seen: [u64; DOMAINS],
    pool: Vec<WalletAddress>,
    last_key: String,
}
impl TargetReader {
    fn new(address: &WalletAddress, value: &[u8], limit: u64) -> Result<Self, String> {
        let prefix = root(address)?;
        let header: Header = decode(value)?;
        if header.target.address != *address { return Err("normalized target identity mismatch".into()); }
        let stripped = VerificationRecord(Record::Target(Box::new(header.target.clone())));
        if target_rows(&stripped)?.len() != 1 { return Err("normalized header contains member payloads".into()); }
        let bytes = stripped.snapshot_entry_bytes()?;
        let limit = limit.min(MAX_TARGET_BYTES);
        if bytes > limit { return Err("normalized target exceeds snapshot byte limit".into()); }
        Ok(Self { last_key: format!("{prefix}0"), prefix, header, bytes, limit,
            seen: [0; DOMAINS], pool: Vec::new() })
    }
    fn push(&mut self, key: &str, value: &[u8]) -> Result<(), String> {
        if key.len() > 511 || key <= self.last_key.as_str() {
            return Err("normalized target rows are not strictly ordered".into());
        }
        let header = &mut self.header;
        let member: Member = decode(value)?;
        if key != format!("{}1/{}", self.prefix, member.suffix()?) {
            return Err("normalized member key mismatch".into());
        }
        let domain = member.domain();
        self.seen[domain] = self.seen[domain].checked_add(1).ok_or("normalized member count overflow")?;
        if self.seen[domain] > header.counts[domain] { return Err("extra normalized target member".into()); }
        // Conservative member payload accounting excludes its enum/row framing.
        let size = match &member {
            Member::Endorsement(v) => bincode::serialized_size(v),
            Member::Vote(v) | Member::PriorVote(v) => bincode::serialized_size(v),
            Member::Endorser(a) | Member::Excluded(a) | Member::Selected(_, a) | Member::Pool(_, a) => bincode::serialized_size(a),
            Member::Context(a, hash) => bincode::serialized_size(&(a, hash)),
        }.map_err(|e| e.to_string())?;
        self.bytes = self.bytes.checked_add(size).ok_or("normalized target size overflow")?;
        if self.bytes > self.limit { return Err("normalized target exceeds snapshot byte limit".into()); }
        match member {
            Member::Endorsement(v) => header.target.state.as_mut().ok_or("member without state")?.endorsements.push(v),
            Member::Endorser(a) => { header.target.state.as_mut().ok_or("member without state")?.endorser_set.insert(a); }
            Member::Selected(index, a) => {
                let selected = &mut header.target.state.as_mut().ok_or("member without state")?.selected_verifiers;
                if index != selected.len() as u64 { return Err("noncontiguous selected members".into()); }
                selected.push(a);
            }
            Member::Vote(v) => header.target.state.as_mut().ok_or("member without state")?.votes.push(v),
            Member::Excluded(a) => { header.target.state.as_mut().ok_or("member without state")?.excluded_verifiers.insert(a); }
            Member::PriorVote(v) => header.target.votes.as_mut().ok_or("member without prior votes")?.0.push(v),
            Member::Pool(index, a) => {
                if index != self.pool.len() as u64 { return Err("noncontiguous selection pool".into()); }
                if header.target.selection.is_none() { return Err("pool without selection".into()); }
                self.pool.push(a);
            }
            Member::Context(a, hash) => { header.target.vote_contexts.as_mut().ok_or("member without vote contexts")?.insert(a.to_string(), hash); }
        }
        self.last_key = key.to_owned();
        Ok(())
    }
    fn finish(mut self) -> Result<VerificationRecord, String> {
        if self.seen != self.header.counts { return Err("missing normalized target members".into()); }
        if let Some(selection) = &mut self.header.target.selection {
            selection.replace_storage_pool(self.pool);
        }
        let record = VerificationRecord(Record::Target(Box::new(self.header.target)));
        if record.snapshot_entry_bytes()? != self.bytes { return Err("normalized target byte accounting mismatch".into()); }
        record.key()?;
        Ok(record)
    }
}

/// Decode one physical row. Header/member consistency and complete membership
/// remain the authenticated inventory reader's job; this does not assemble it.
pub fn visit_retention_references(
    address: &WalletAddress, key: &str, bytes: &[u8],
    mut visit: impl FnMut(super::retention::Reference<'_>) -> Result<(), String>,
) -> Result<(), String> {
    use super::retention::{Reference, target_roots, vote_roots};
    if key.len() > 511 { return Err("normalized verification key exceeds LMDB limit".into()); }
    if key == target_header_key(address)? {
        let reader = TargetReader::new(address, bytes, MAX_TARGET_BYTES)?;
        return target_roots(&reader.header.target, &mut visit);
    }
    let member: Member = decode(bytes)?;
    if key != format!("{}1/{}", root(address)?, member.suffix()?) {
        return Err("normalized member key mismatch".into());
    }
    match &member {
        Member::Endorsement(value) => {
            visit(Reference::Account(&value.endorser))?;
            visit(Reference::Boundary(value.timestamp))
        }
        Member::Vote(value) | Member::PriorVote(value) => vote_roots(value, &mut visit),
        Member::Endorser(account) | Member::Selected(_, account) | Member::Excluded(account) | Member::Pool(_, account) => visit(Reference::Account(account)),
        Member::Context(account, context) => {
            visit(Reference::Account(account))?;
            visit(Reference::CommitteeContext(*context))
        }
    }
}

/// Extract references from a canonical normalized key using the same address
/// decoder as whole-state recovery. Does not authenticate inventory membership.
pub fn visit_row_retention_references(
    key: &str, bytes: &[u8],
    visit: impl FnMut(super::retention::Reference<'_>) -> Result<(), String>,
) -> Result<(), String> {
    let address = state::address_from_key(key)?;
    visit_retention_references(&address, key, bytes, visit)
}
