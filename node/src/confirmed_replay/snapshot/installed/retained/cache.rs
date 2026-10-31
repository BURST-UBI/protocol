use super::*;
use crate::confirmed_replay::snapshot::history::DurableInventoryInputs;
use bincode::Options;
use burst_store_lmdb::recovery_inputs::InputWriteError;
use serde::{Deserialize, Serialize};

const MAX_PAGE_BYTES: u64 = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Header {
    pub version: u16,
    // Invalidation hint only. Equality across opens does not restore a storage
    // cut or authenticate any row; final writer-side comparison stays required.
    pub revision: u64,
    pub rows: u64,
}

#[derive(Serialize, Deserialize)]
pub(super) struct Page {
    #[serde(deserialize_with = "bounded_rows")]
    pub rows: Vec<Observation>,
    pub end: bool,
}
fn bounded_rows<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Observation>, D::Error> {
    struct Rows;
    impl<'de> serde::de::Visitor<'de> for Rows {
        type Value = Vec<Observation>;
        fn expecting(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            out.write_str("at most 100 inventory observations")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let limit = burst_store_lmdb::retention::MAX_PAGE_ROWS;
            if seq.size_hint().is_some_and(|size| size > limit) {
                return Err(serde::de::Error::custom(
                    "inventory cache page exceeds row budget",
                ));
            }
            let mut rows = Vec::new();
            while let Some(row) = seq.next_element()? {
                if rows.len() == limit {
                    return Err(serde::de::Error::custom(
                        "inventory cache page exceeds row budget",
                    ));
                }
                rows.push(row);
            }
            Ok(rows)
        }
    }
    deserializer.deserialize_seq(Rows)
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, StoreError> {
    if bytes.len() as u64 > MAX_PAGE_BYTES {
        return Err(invalid("inventory cache record exceeds byte budget"));
    }
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(bytes.len() as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(invalid)
}
fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, StoreError> {
    let length = bincode::serialized_size(value).map_err(invalid)?;
    if length > MAX_PAGE_BYTES {
        return Err(invalid("inventory cache record exceeds byte budget"));
    }
    bincode::serialize(value).map_err(invalid)
}

pub(super) struct InventoryCache {
    inputs: DurableInventoryInputs,
    next: u64,
    available: u64,
    sealed: bool,
    pending: Vec<Vec<u8>>,
    pending_bytes: usize,
}
impl InventoryCache {
    /// `None` means cleanup was scheduled or advanced by one bounded page.
    /// Check cancellation and select a fresh handle before retrying.
    /// Unrelated streams survive.
    pub(super) fn prepare(
        inputs: DurableInventoryInputs,
        expected: Header,
    ) -> Result<Option<Self>, InputWriteError> {
        if inputs.raw.stream_reset_pending()? {
            inputs
                .raw
                .reset_stream_page(128, MAX_PAGE_BYTES as usize + 64)?;
            return Ok(None);
        }
        let mut progress = inputs.raw.progress()?;
        if progress.positions == 0 {
            inputs.raw.append_checked(0, &encode(&expected)?)?;
            progress = inputs.raw.progress()?;
        } else {
            let saved: Header = decode(
                &inputs
                    .raw
                    .read_position(0)?
                    .ok_or_else(|| invalid("missing inventory cache header"))?,
            )?;
            if saved.version != 1 {
                return Err(invalid("unsupported inventory cache version").into());
            }
            if saved != expected {
                inputs.raw.begin_stream_reset()?;
                return Ok(None);
            }
        }
        if progress.positions > expected.rows.saturating_add(2)
            || (progress.complete && progress.positions == 1)
        {
            return Err(invalid("inventory cache progress exceeds capture bounds").into());
        }
        Ok(Some(Self {
            inputs,
            next: 1,
            available: progress.positions,
            sealed: progress.complete,
            pending: Vec::new(),
            pending_bytes: 0,
        }))
    }

    pub(super) fn page(&self) -> Result<Option<Page>, StoreError> {
        if self.next >= self.available {
            return Ok(None);
        }
        let page: Page = decode(
            &self
                .inputs
                .raw
                .read_position(self.next)?
                .ok_or_else(|| invalid("missing inventory cache page"))?,
        )?;
        if (page.end && self.next + 1 != self.available)
            || (!page.end && self.sealed && self.next + 1 == self.available)
        {
            return Err(invalid("inventory cache completion differs from pages"));
        }
        Ok(Some(page))
    }

    fn flush(&mut self) -> Result<(), InputWriteError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        self.inputs
            .raw
            .append_batch_checked(self.available, &self.pending)?;
        self.available += self.pending.len() as u64;
        self.pending.clear();
        self.pending_bytes = 0;
        Ok(())
    }

    /// Called only after the consumer checks order, accounting and coverage.
    /// Completion still describes raw acquisition, never publication authority.
    pub(super) fn accept(&mut self, page: &Page, cached: bool) -> Result<(), InputWriteError> {
        if cached {
            if self.next >= self.available {
                return Err(invalid("inventory cache cursor exceeded saved progress").into());
            }
        } else {
            if self.next != self.available + self.pending.len() as u64 || self.sealed {
                return Err(invalid("inventory cache append differs from progress").into());
            }
            let bytes = encode(page)?;
            if self.pending_bytes + bytes.len() > MAX_PAGE_BYTES as usize {
                self.flush()?;
            }
            self.pending_bytes += bytes.len();
            self.pending.push(bytes);
            // Save the first page immediately; batch later pages until a size,
            // count or end boundary. Unflushed pages must be fetched again.
            if self.available == 1 || self.pending.len() == 128 || page.end {
                self.flush()?;
            }
        }
        self.next += 1;
        if page.end && !self.sealed {
            self.inputs.raw.seal_checked(self.next)?;
            self.sealed = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
