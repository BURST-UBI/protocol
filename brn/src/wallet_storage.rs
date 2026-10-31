//! Local BRN wallet rows for storage and replay undo.
//! Keys within a wallet namespace sort the header first, then indexed pauses,
//! then outcome-keyed stake releases. Whole-wallet serialization uses a separate
//! format. Callers select the format and commit related rows atomically.

use crate::{
    releases::{PendingReleases, StakeRelease},
    BrnError, BrnWalletState,
};
use bincode::Options;
use burst_types::{BlockHash, Timestamp};
use serde::{Deserialize, Serialize};

pub const HEADER: &[u8] = &[0];
pub const FORMAT_KEY: &[u8] = b"wallet_records/v1/format";
pub const ROW_PREFIX: &[u8] = b"wallet_records/v1/row/";

/// Length framing keeps address namespaces disjoint even for noncanonical local
/// fixture addresses. A row suffix must still pass the wallet record decoder.
pub fn row_prefix(address: &burst_types::WalletAddress) -> Result<Vec<u8>, BrnError> {
    let address = address.as_str().as_bytes();
    let len = u32::try_from(address.len()).map_err(|_| invalid())?;
    let mut prefix = ROW_PREFIX.to_vec();
    prefix.extend_from_slice(&len.to_be_bytes());
    prefix.extend_from_slice(address);
    Ok(prefix)
}

/// Split length-framed address bytes from a nonempty row suffix. This does not
/// validate UTF-8, the wallet address or the row suffix.
pub fn split_row_key(key: &[u8]) -> Result<(&[u8], &[u8]), BrnError> {
    let key = key.strip_prefix(ROW_PREFIX).ok_or_else(invalid)?;
    let len = u32::from_be_bytes(
        key.get(..4)
            .ok_or_else(invalid)?
            .try_into()
            .map_err(|_| invalid())?,
    ) as usize;
    let rest = &key[4..];
    let address = rest.get(..len).ok_or_else(invalid)?;
    let suffix = rest
        .get(len..)
        .filter(|suffix| !suffix.is_empty())
        .ok_or_else(invalid)?;
    Ok((address, suffix))
}
const PAUSE: u8 = 1;
const RELEASE: u8 = 2;

/// Visit dependency references in one row without rebuilding the wallet history.
/// The caller supplies the decoded namespace owner and authenticates the complete
/// inventory separately. Header counts and row order are not validated here.
/// Reports unproven outcomes and stopped-accrual boundaries too. This is a
/// dependency inventory, not permission to delete any referenced record.
pub fn visit_retention_references(
    owner: &burst_types::WalletAddress,
    key: &[u8],
    value: &[u8],
    mut visit: impl FnMut(crate::retention::Reference<'_>) -> Result<(), BrnError>,
) -> Result<(), BrnError> {
    use crate::retention::Reference::{Account, Block, Boundary, Outcome};
    match key.first().copied() {
        Some(0) if key == HEADER => {
            let header: Header = decode_value(value)?;
            if header.version != 1 {
                return Err(invalid());
            }
            visit(Account(owner))?;
            visit(Boundary(header.verified_at))?;
            if let Some(at) = header.accrual_stopped_at {
                visit(Boundary(at))?;
            }
        }
        Some(PAUSE) if key.len() == 9 => {
            let (start, end): (Timestamp, Timestamp) = decode_value(value)?;
            visit(Account(owner))?;
            visit(Boundary(start))?;
            visit(Boundary(end))?;
        }
        Some(RELEASE) if key.len() == 33 => {
            let release: StakeRelease = decode_value(value)?;
            let outcome = BlockHash::new(key[1..].try_into().map_err(|_| invalid())?);
            visit(Account(owner))?;
            visit(Outcome(outcome))?;
            visit(Boundary(release.completed_at))?;
            for source in release.sources {
                visit(Block(source))?;
            }
        }
        _ => return Err(invalid()),
    }
    Ok(())
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct Header {
    version: u8,
    verified_at: Timestamp,
    total_burned: u128,
    total_staked: u128,
    accrual_active: bool,
    accrual_stopped_at: Option<Timestamp>,
    pauses: u64,
    releases: u64,
}

impl From<&BrnWalletState> for Header {
    fn from(state: &BrnWalletState) -> Self {
        Self {
            version: 1,
            verified_at: state.verified_at,
            total_burned: state.total_burned,
            total_staked: state.total_staked,
            accrual_active: state.accrual_active,
            accrual_stopped_at: state.accrual_stopped_at,
            pauses: state.paused_intervals.len() as u64,
            releases: state.pending_releases.len() as u64,
        }
    }
}

fn invalid() -> BrnError {
    BrnError::Other("invalid persisted BRN wallet records".into())
}
fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, BrnError> {
    bincode::serialize(value).map_err(|error| BrnError::Other(error.to_string()))
}
fn decode_value<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, BrnError> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .reject_trailing_bytes()
        .with_limit(bytes.len() as u64)
        .deserialize(bytes)
        .map_err(|error| BrnError::Other(error.to_string()))
}
fn pause_key(index: usize) -> Vec<u8> {
    let mut key = vec![PAUSE];
    key.extend_from_slice(&(index as u64).to_be_bytes());
    key
}
fn release_key(hash: &BlockHash) -> Vec<u8> {
    let mut key = vec![RELEASE];
    key.extend_from_slice(hash.as_bytes());
    key
}

/// Emit exact before/after physical rows for a wallet transition. The caller
/// supplies the wallet namespace and owns transaction atomicity and undo budgets.
/// A callback error stops emission; partial output must never be committed.
pub fn visit_delta(
    before: Option<&BrnWalletState>,
    after: Option<&BrnWalletState>,
    mut emit: impl FnMut(&[u8], Option<Vec<u8>>, Option<Vec<u8>>) -> Result<(), BrnError>,
) -> Result<(), BrnError> {
    let old = before.map(Header::from);
    let new = after.map(Header::from);
    if old != new {
        emit(
            HEADER,
            old.as_ref().map(encode).transpose()?,
            new.as_ref().map(encode).transpose()?,
        )?;
    }
    let empty = crate::pauses::PauseHistory::default();
    let old = before.map_or(&empty, |state| &state.paused_intervals);
    let new = after.map_or(&empty, |state| &state.paused_intervals);
    new.visit_changes(old, |index, old, new| {
        emit(
            &pause_key(index),
            old.map(encode).transpose()?,
            new.map(encode).transpose()?,
        )
    })?;
    let empty = PendingReleases::default();
    let old = before.map_or(&empty, |state| &state.pending_releases);
    let new = after.map_or(&empty, |state| &state.pending_releases);
    new.visit_changes(old, |hash, old, new| {
        emit(
            &release_key(hash),
            old.map(encode).transpose()?,
            new.map(encode).transpose()?,
        )
    })
}

/// Whether the supplied wallet state contains the row key. This checks key
/// membership only, not stored bytes or wallet authority. Replacement uses it
/// to identify obsolete keys before writing the canonical values.
pub fn contains_row(state: &BrnWalletState, key: &[u8]) -> bool {
    match key.first().copied() {
        Some(0) => key == HEADER,
        Some(PAUSE) if key.len() == 9 => {
            let index = u64::from_be_bytes(key[1..].try_into().unwrap());
            usize::try_from(index).is_ok_and(|index| index < state.paused_intervals.len())
        }
        Some(RELEASE) if key.len() == 33 => state
            .pending_releases
            .contains_key(&BlockHash::new(key[1..].try_into().unwrap())),
        _ => false,
    }
}

/// Read one complete, ordered wallet inventory. Header counts never allocate
/// capacity; missing, duplicate, unknown, out-of-order and trailing data fail.
/// This validates the physical layout, not economic state: pause intervals,
/// balances and release evidence still need caller validation. Raw release rows
/// are retained while their derived lookup indexes are rebuilt.
pub fn decode<K: AsRef<[u8]>, V: AsRef<[u8]>>(
    rows: impl IntoIterator<Item = Result<(K, V), BrnError>>,
) -> Result<Option<BrnWalletState>, BrnError> {
    let mut rows = rows.into_iter();
    let Some((key, value)) = rows.next().transpose()? else {
        return Ok(None);
    };
    if key.as_ref() != HEADER {
        return Err(invalid());
    }
    let header: Header = decode_value(value.as_ref())?;
    if header.version != 1 {
        return Err(invalid());
    }
    let mut state = BrnWalletState::new(header.verified_at);
    state.total_burned = header.total_burned;
    state.total_staked = header.total_staked;
    state.accrual_active = header.accrual_active;
    state.accrual_stopped_at = header.accrual_stopped_at;
    let mut releases = std::collections::BTreeMap::new();
    let mut last_release = None;
    for row in rows {
        let (key, value) = row?;
        let key = key.as_ref();
        let value = value.as_ref();
        match key.first().copied() {
            Some(PAUSE) if key.len() == 9 && last_release.is_none() => {
                let index = u64::from_be_bytes(key[1..].try_into().map_err(|_| invalid())?);
                if index != state.paused_intervals.len() as u64 || index >= header.pauses {
                    return Err(invalid());
                }
                state.paused_intervals.push(decode_value(value)?);
            }
            Some(RELEASE)
                if key.len() == 33 && state.paused_intervals.len() as u64 == header.pauses =>
            {
                let hash = BlockHash::new(key[1..].try_into().map_err(|_| invalid())?);
                if last_release.is_some_and(|last| last >= hash)
                    || releases.len() as u64 >= header.releases
                {
                    return Err(invalid());
                }
                let release: StakeRelease = decode_value(value)?;
                releases.insert(hash, release);
                last_release = Some(hash);
            }
            _ => return Err(invalid()),
        }
    }
    if state.paused_intervals.len() as u64 != header.pauses
        || releases.len() as u64 != header.releases
    {
        return Err(invalid());
    }
    state.pending_releases = releases.into();
    Ok(Some(state))
}

#[cfg(test)]
mod tests;
