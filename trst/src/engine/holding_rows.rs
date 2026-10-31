//! Local portfolio storage keyed by holder and token ID. A header and next-token
//! links preserve canonical token order without using vector positions as keys.
//! Row decoding checks format and key consistency, not ledger authenticity.

use super::{TrstToken, WalletPortfolio};
use bincode::Options;
use burst_types::{Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub mod indexed;
pub mod positional;

pub const PREFIX: &str = "trst_holdings/v2/";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Row {
    Header {
        holder: WalletAddress,
        count: u64,
        head: Option<TxHash>,
        cached_transferable: u128,
        earliest_expiry: Option<Timestamp>,
    },
    Holding {
        holder: WalletAddress,
        next: Option<TxHash>,
        token: Box<TrstToken>,
    },
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|byte| {
            [
                DIGITS[(byte >> 4) as usize] as char,
                DIGITS[(byte & 15) as usize] as char,
            ]
        })
        .collect()
}

pub fn portfolio_keys(holder: &WalletAddress) -> (String, String) {
    let holder = hex(holder.as_str().as_bytes());
    (
        format!("{PREFIX}0/{holder}"),
        format!("{PREFIX}1/{holder}/"),
    )
}

// Bound output while encoding fixed-width Bincode once, without a size prepass.
struct BoundedRow(Vec<u8>);

impl std::io::Write for BoundedRow {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > burst_types::MAX_P2P_MESSAGE_BYTES - self.0.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "stable holding row exceeds byte limit",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Row {
    /// Include canonical list links as well as the token's economic references.
    pub fn visit_retention_references<'a, B>(
        &'a self,
        mut visit: impl FnMut(burst_types::TrstReference<'a>) -> std::ops::ControlFlow<B>,
    ) -> std::ops::ControlFlow<B> {
        use burst_types::TrstReference::{Account, Token};
        match self {
            Self::Header { holder, head, .. } => {
                visit(Account(holder))?;
                if let Some(head) = head {
                    visit(Token(*head))?;
                }
            }
            Self::Holding {
                holder,
                next,
                token,
            } => {
                visit(Account(holder))?;
                if let Some(next) = next {
                    visit(Token(*next))?;
                }
                token.visit_retention_references(&mut visit)?;
            }
        }
        std::ops::ControlFlow::Continue(())
    }

    /// Compare canonical key bytes without allocating the encoded holder, both
    /// portfolio prefixes and token ID strings during every row decode.
    fn matches_key(&self, key: &str) -> bool {
        fn matches_hex(encoded: &str, bytes: &[u8]) -> bool {
            const DIGITS: &[u8; 16] = b"0123456789abcdef";
            if bytes.len().checked_mul(2) != Some(encoded.len()) {
                return false;
            }
            encoded
                .as_bytes()
                .chunks_exact(2)
                .zip(bytes)
                .all(|(pair, byte)| {
                    pair[0] == DIGITS[(byte >> 4) as usize]
                        && pair[1] == DIGITS[(byte & 15) as usize]
                })
        }
        if key.len() > 511 {
            return false;
        }
        let Some(key) = key.strip_prefix(PREFIX) else {
            return false;
        };
        match self {
            Self::Header { holder, .. } => key
                .strip_prefix("0/")
                .is_some_and(|key| matches_hex(key, holder.as_str().as_bytes())),
            Self::Holding { holder, token, .. } => {
                if token.holder != *holder {
                    return false;
                }
                key.strip_prefix("1/")
                    .and_then(|key| key.rsplit_once('/'))
                    .is_some_and(|(owner, id)| {
                        matches_hex(owner, holder.as_str().as_bytes())
                            && matches_hex(id, token.id.as_bytes())
                    })
            }
        }
    }

    fn key(&self) -> Result<String, String> {
        let key = match self {
            Self::Header { holder, .. } => portfolio_keys(holder).0,
            Self::Holding { holder, token, .. } => {
                if token.holder != *holder {
                    return Err("stable holding owner mismatch".into());
                }
                format!("{}{}", portfolio_keys(holder).1, hex(token.id.as_bytes()))
            }
        };
        if key.len() > 511 {
            return Err("stable holding key exceeds LMDB limit".into());
        }
        Ok(key)
    }

    pub fn storage_entry(&self) -> Result<(String, Vec<u8>), String> {
        let key = self.key()?;
        let mut encoded = BoundedRow(Vec::with_capacity(256));
        bincode::serialize_into(&mut encoded, &(2u8, self)).map_err(|e| e.to_string())?;
        Ok((key, encoded.0))
    }

    /// Decode one bounded version-2 row and require its canonical key. Linked-list
    /// structure, cached totals and provenance are checked outside this codec.
    pub fn from_storage_entry(key: &str, bytes: &[u8]) -> Result<Self, String> {
        if !key.starts_with(PREFIX)
            || key.len() > 511
            || bytes.len() > burst_types::MAX_P2P_MESSAGE_BYTES
        {
            return Err("invalid stable holding row bounds".into());
        }
        let (version, row): (u8, Self) = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(burst_types::MAX_P2P_MESSAGE_BYTES as u64)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map_err(|e| e.to_string())?;
        if version != 2 || !row.matches_key(key) {
            return Err("stable holding version or key mismatch".into());
        }
        Ok(row)
    }
}

/// Rewind only the fields changed by this journal, preserving independent live
/// fields. Every changed field must still equal its after-image. All three rows
/// are bound to the same primary key. A holding's token payload is one indivisible
/// field; its next link is separate. Creation/deletion require caller-side exact
/// row comparison. The returned bytes still need whole-portfolio validation.
pub fn reconcile_undo(
    key: &str,
    before: &[u8],
    after: &[u8],
    current: &[u8],
) -> Result<Vec<u8>, String> {
    fn field<T: PartialEq>(old: T, new: T, live: T, name: &str) -> Result<T, String> {
        if old == new {
            return Ok(live);
        }
        if live != new {
            return Err(format!("stable undo conflicts with {name}"));
        }
        Ok(old)
    }
    let old = Row::from_storage_entry(key, before)?;
    let new = Row::from_storage_entry(key, after)?;
    let live = Row::from_storage_entry(key, current)?;
    let restored = match (old, new, live) {
        (
            Row::Holding {
                holder,
                next: old_next,
                token: old_token,
            },
            Row::Holding {
                next: new_next,
                token: new_token,
                ..
            },
            Row::Holding {
                next: live_next,
                token: live_token,
                ..
            },
        ) => Row::Holding {
            holder,
            next: field(old_next, new_next, live_next, "ordering link")?,
            token: field(old_token, new_token, live_token, "token payload")?,
        },
        (
            Row::Header {
                holder,
                count: old_count,
                head: old_head,
                cached_transferable: old_transferable,
                earliest_expiry: old_expiry,
            },
            Row::Header {
                count: new_count,
                head: new_head,
                cached_transferable: new_transferable,
                earliest_expiry: new_expiry,
                ..
            },
            Row::Header {
                count: live_count,
                head: live_head,
                cached_transferable: live_transferable,
                earliest_expiry: live_expiry,
                ..
            },
        ) => Row::Header {
            holder,
            count: field(old_count, new_count, live_count, "holding count")?,
            head: field(old_head, new_head, live_head, "portfolio head")?,
            cached_transferable: field(
                old_transferable,
                new_transferable,
                live_transferable,
                "transferable cache",
            )?,
            earliest_expiry: field(old_expiry, new_expiry, live_expiry, "expiry cache")?,
        },
        _ => return Err("stable undo changed row kind".into()),
    };
    restored.storage_entry().map(|(_, bytes)| bytes)
}

fn validate(holder: &WalletAddress, portfolio: &WalletPortfolio) -> Result<(), String> {
    portfolio.validate_snapshot()?;
    validate_owner(holder, portfolio)
}

fn validate_owner(holder: &WalletAddress, portfolio: &WalletPortfolio) -> Result<(), String> {
    if portfolio.tokens.iter().any(|token| token.holder != *holder) {
        return Err("stable portfolio owner mismatch".into());
    }
    Ok(())
}

fn header(holder: &WalletAddress, portfolio: &WalletPortfolio) -> Result<Row, String> {
    Ok(Row::Header {
        holder: holder.clone(),
        count: portfolio
            .tokens
            .len()
            .try_into()
            .map_err(|_| "holding count overflow")?,
        head: portfolio.tokens.first().map(|token| token.id),
        cached_transferable: portfolio.cached_transferable,
        earliest_expiry: portfolio.earliest_expiry,
    })
}

fn holding(holder: &WalletAddress, token: &TrstToken, next: Option<TxHash>) -> Row {
    Row::Holding {
        holder: holder.clone(),
        next,
        token: Box::new(token.clone()),
    }
}

/// Collect changed rows from visit_delta. Output space is proportional to emitted
/// rows. Compatible indexed portfolios diff retained roots; the vector fallback
/// uses O(old + new holdings) time and temporary lookup space. Emission order
/// depends on the representation and must not be used as canonical token order.
pub fn delta(
    holder: &WalletAddress,
    before: Option<&WalletPortfolio>,
    after: Option<&WalletPortfolio>,
) -> Result<Vec<(Option<Row>, Option<Row>)>, String> {
    let mut changes = Vec::new();
    visit_delta(
        holder,
        before,
        after,
        |error| error,
        |old, new| {
            changes.push((old, new));
            Ok(())
        },
    )?;
    Ok(changes)
}

/// Emit changed rows one pair at a time. Compatible indexed portfolios reuse
/// their validation and diff changed IDs. Otherwise, validate owners and portfolio
/// structure and scan both vectors with O(old + new) temporary lookup space.
/// Neither path authenticates provenance. Callback errors stop emission without
/// undoing earlier effects; callers control buffering and transaction boundaries.
pub fn visit_delta<E>(
    holder: &WalletAddress,
    before: Option<&WalletPortfolio>,
    after: Option<&WalletPortfolio>,
    mut invalid: impl FnMut(String) -> E,
    mut visit: impl FnMut(Option<Row>, Option<Row>) -> Result<(), E>,
) -> Result<(), E> {
    if let Some((old, new)) = before
        .and_then(WalletPortfolio::indexed)
        .zip(after.and_then(WalletPortfolio::indexed))
    {
        if new.can_diff_with(old, holder) {
            return new.visit_delta(old, invalid, visit);
        }
    }
    let old_tokens = before.map_or(&[][..], |p| p.tokens.as_slice());
    let new_tokens = after.map_or(&[][..], |p| p.tokens.as_slice());
    // Some(position) is an old ID not yet seen in the new portfolio. None marks
    // an ID already seen there, including new IDs, so duplicates still fail.
    let mut ids = HashMap::with_capacity(old_tokens.len().max(new_tokens.len()));
    if let Some(portfolio) = before {
        portfolio
            .validate_snapshot_with_ids(|n, token| ids.insert(token.id, Some(n)).is_none())
            .map_err(&mut invalid)?;
        validate_owner(holder, portfolio).map_err(&mut invalid)?;
    }
    let mut previous_positions = Vec::with_capacity(new_tokens.len());
    let mut deletions = old_tokens.len();
    if let Some(portfolio) = after {
        portfolio
            .validate_snapshot_with_ids(|_, token| {
                use std::collections::hash_map::Entry;
                let previous = match ids.entry(token.id) {
                    Entry::Occupied(mut entry) => match entry.get_mut().take() {
                        Some(position) => {
                            deletions -= 1;
                            Some(position)
                        }
                        None => return false,
                    },
                    Entry::Vacant(entry) => {
                        entry.insert(None);
                        None
                    }
                };
                previous_positions.push(previous);
                true
            })
            .map_err(&mut invalid)?;
        validate_owner(holder, portfolio).map_err(&mut invalid)?;
    }
    let old_header = before
        .map(|p| header(holder, p))
        .transpose()
        .map_err(&mut invalid)?;
    let new_header = after
        .map(|p| header(holder, p))
        .transpose()
        .map_err(&mut invalid)?;
    if old_header != new_header {
        visit(old_header, new_header)?;
    }
    for (n, (token, previous)) in new_tokens.iter().zip(previous_positions).enumerate() {
        let next = new_tokens.get(n + 1).map(|t| t.id);
        let previous = previous.map(|p| (&old_tokens[p], old_tokens.get(p + 1).map(|t| t.id)));
        if previous != Some((token, next)) {
            visit(
                previous.map(|(t, next)| holding(holder, t, next)),
                Some(holding(holder, token, next)),
            )?;
        }
    }
    if deletions != 0 {
        for (n, token) in old_tokens.iter().enumerate() {
            if ids[&token.id].is_some() {
                visit(
                    Some(holding(holder, token, old_tokens.get(n + 1).map(|t| t.id))),
                    None,
                )?;
                deletions -= 1;
                if deletions == 0 {
                    break;
                }
            }
        }
    }
    Ok(())
}

/// Reconstruct one scoped portfolio. The count never controls allocation.
/// Every supplied holding must occur exactly once in the linked order. Validate
/// owner, ordering, stored-state totals and the deadline under the supplied expiry
/// period. No local-clock reclassification or graph-provenance check is performed.
pub fn restore(
    expected_holder: &WalletAddress,
    header: Row,
    holdings: impl IntoIterator<Item = Result<Row, String>>,
    expiry_secs: u64,
) -> Result<WalletPortfolio, String> {
    restore_with_poll(expected_holder, header, holdings, expiry_secs, &|_| Ok(()))
}

pub fn restore_with_poll(
    expected_holder: &WalletAddress,
    header: Row,
    holdings: impl IntoIterator<Item = Result<Row, String>>,
    expiry_secs: u64,
    poll: &dyn Fn(&'static str) -> Result<(), String>,
) -> Result<WalletPortfolio, String> {
    let Row::Header {
        holder,
        count,
        mut head,
        cached_transferable,
        earliest_expiry,
    } = header
    else {
        return Err("stable portfolio header missing".into());
    };
    if holder != *expected_holder {
        return Err("stable portfolio header owner mismatch".into());
    }
    let mut remaining = HashMap::new();
    for row in holdings {
        poll("trst:holding-row")?;
        let Row::Holding {
            holder: row_holder,
            next,
            token,
        } = row?
        else {
            return Err("unexpected stable portfolio header".into());
        };
        if row_holder != holder || token.holder != holder {
            return Err("stable holding owner mismatch".into());
        }
        if remaining.insert(token.id, (token, next)).is_some() {
            return Err("duplicate stable holding ID".into());
        }
    }
    if u64::try_from(remaining.len()).ok() != Some(count) {
        return Err("stable holding count mismatch".into());
    }
    let mut portfolio =
        WalletPortfolio::from_parts(Vec::new(), cached_transferable, earliest_expiry);
    while let Some(id) = head {
        poll("trst:holding-link")?;
        let (token, next) = remaining
            .remove(&id)
            .ok_or("stable holding link is missing or cyclic")?;
        portfolio.tokens.push(*token);
        head = next;
    }
    if !remaining.is_empty() {
        return Err("disconnected stable holdings".into());
    }
    portfolio.validate_snapshot_with_poll(poll)?;
    let mut expected_expiry = None;
    for token in &portfolio.tokens {
        poll("trst:holding-expiry")?;
        if token.holder != holder {
            return Err("stable portfolio owner mismatch".into());
        }
        if token.state == burst_types::TrstState::Active {
            let expiry = token.earliest_expiry(expiry_secs);
            expected_expiry =
                Some(expected_expiry.map_or(expiry, |old: Timestamp| old.min(expiry)));
        }
    }
    if expected_expiry != portfolio.earliest_expiry {
        return Err("stable holding expiry differs from governed period".into());
    }
    Ok(portfolio)
}

#[cfg(test)]
mod tests;
