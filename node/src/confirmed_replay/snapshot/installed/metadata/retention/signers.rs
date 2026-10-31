use super::*;
use crate::confirmed_replay::snapshot::reconciliation::promises::{self, PaymentPromise};
use bincode::Options;
use burst_consensus::snapshot::SnapshotManifest;
use burst_types::{BlockHash, NetworkId};

#[cfg(test)]
const PREFIXES: &[&str] = &[
    "consensus_final/",
    "consensus_fence/",
    "consensus_time/",
    "consensus_governance/",
    "consensus_sequence/",
    "payment_epoch/",
    "snapshot_attestation/",
    "checkpoint_vote/",
];

/// Exact observed signer state. Commitments and epoch IDs are not block-body
/// references. Sequence values may advance after this view: publication must
/// preserve the current high-water mark, never restore this older observation.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SignerRecord<'a> {
    Payment(PaymentPromise<'a>),
    Time {
        epoch: BlockHash,
        signer: &'a str,
        value: u64,
    },
    Governance {
        epoch: BlockHash,
        signer: &'a str,
        value: u64,
    },
    Sequence {
        signer: &'a str,
        value: u64,
    },
    Epoch(BlockHash),
    Cutoff(u64),
    VoteLimit {
        epoch: BlockHash,
        limit: Option<u64>,
    },
    CountingLimit {
        epoch: BlockHash,
        limit: Option<u64>,
    },
    ClosedEpoch {
        from: BlockHash,
        to: BlockHash,
    },
    Snapshot {
        signer: &'a str,
        manifest: SnapshotManifest,
    },
}

fn number(value: &[u8]) -> Result<u64, StoreError> {
    Ok(u64::from_be_bytes(value.try_into().map_err(invalid)?))
}
fn signer(value: &str) -> Result<&str, StoreError> {
    burst_crypto::decode_address(value).ok_or_else(|| invalid("invalid retained signer"))?;
    Ok(value)
}
pub(super) fn decode<'a>(
    key: &'a str,
    bytes: &[u8],
    network: NetworkId,
    genesis: BlockHash,
) -> Result<SignerRecord<'a>, StoreError> {
    use burst_store::meta::*;
    if key.starts_with("consensus_final/") || key.starts_with("consensus_fence/") {
        return Ok(SignerRecord::Payment(promises::decode(key, bytes)?));
    }
    for (prefix, governance) in [
        ("consensus_time/v1/", false),
        ("consensus_governance/v1/", true),
    ] {
        if let Some(suffix) = key.strip_prefix(prefix) {
            let (epoch, account) = suffix
                .split_once('/')
                .ok_or_else(|| invalid("invalid counter key"))?;
            let epoch = promises::hash(epoch)?;
            let signer = signer(account)?;
            let value = number(bytes)?;
            return Ok(if governance {
                SignerRecord::Governance {
                    epoch,
                    signer,
                    value,
                }
            } else {
                SignerRecord::Time {
                    epoch,
                    signer,
                    value,
                }
            });
        }
    }
    if let Some(account) = key.strip_prefix("consensus_sequence/v1/") {
        let value = number(bytes)?;
        if value == 0 {
            return Err(invalid("zero persisted signing sequence"));
        }
        return Ok(SignerRecord::Sequence {
            signer: signer(account)?,
            value,
        });
    }
    if key == PAYMENT_EPOCH_KEY {
        return Ok(SignerRecord::Epoch(decode_payment_epoch(bytes)?));
    }
    if key == crate::confirmed_replay::installation::CUTOFF_KEY {
        return Ok(SignerRecord::Cutoff(number(bytes)?));
    }
    if key == PAYMENT_VOTE_LIMIT_KEY || key == GOVERNANCE_COUNTING_LIMIT_KEY {
        let epoch = decode_payment_epoch(
            bytes
                .get(..32)
                .ok_or_else(|| invalid("missing limit epoch"))?,
        )?;
        let limit = decode_payment_vote_limit(bytes, epoch)?;
        return Ok(if key == PAYMENT_VOTE_LIMIT_KEY {
            SignerRecord::VoteLimit { epoch, limit }
        } else {
            SignerRecord::CountingLimit { epoch, limit }
        });
    }
    if let Some(from) = key.strip_prefix("payment_epoch/v1/closed/") {
        let from = promises::hash(from)?;
        if bytes.len() != 64 || bytes[32..] != *from.as_bytes() {
            return Err(invalid("closed epoch differs from key"));
        }
        let to = BlockHash::new(bytes[..32].try_into().map_err(invalid)?);
        return Ok(SignerRecord::ClosedEpoch { from, to });
    }
    let prefix = format!("snapshot_attestation/v1/{}/{genesis}/", network.as_str());
    if let Some(suffix) = key.strip_prefix(&prefix) {
        let (epoch, account) = suffix
            .split_once('/')
            .ok_or_else(|| invalid("invalid snapshot promise key"))?;
        let manifest: SnapshotManifest = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .with_limit(4096)
            .deserialize(bytes)
            .map_err(invalid)?;
        if manifest.epoch != promises::hash(epoch)?
            || manifest.network != network
            || manifest.genesis != genesis
        {
            return Err(invalid("snapshot promise differs from key"));
        }
        return Ok(SignerRecord::Snapshot {
            signer: signer(account)?,
            manifest,
        });
    }
    Err(invalid("unsupported retained signer record"))
}

#[cfg(test)]
pub(crate) struct SignerRecordReader<'scan, 'db> {
    store: &'scan LmdbStore,
    view: &'scan burst_store_lmdb::LmdbReadTransaction<'db>,
    network: NetworkId,
    genesis: BlockHash,
    phase: usize,
    after: Option<String>,
    failed: bool,
}
#[cfg(test)]
impl<'scan, 'db> SignerRecordReader<'scan, 'db> {
    #[cfg(test)]
    pub(super) fn new(
        store: &'scan LmdbStore,
        view: &'scan burst_store_lmdb::LmdbReadTransaction<'db>,
        network: NetworkId,
        genesis: BlockHash,
    ) -> Self {
        Self {
            store,
            view,
            network,
            genesis,
            phase: 0,
            after: None,
            failed: false,
        }
    }
    /// One namespace per page, including empty pages. Context and cross-record
    /// validity come from the checked recovery cut, not this decoder. On error,
    /// discard the results; earlier callbacks are not undone. Completion alone
    /// does not permit pruning.
    #[cfg(test)]
    pub(crate) fn visit_next_page(
        &mut self,
        max_rows: usize,
        max_bytes: usize,
        mut visit: impl FnMut(&str, SignerRecord<'_>) -> Result<(), StoreError>,
    ) -> Result<ReferencePage, StoreError> {
        if self.failed {
            return Err(invalid("signer retention scan already failed"));
        }
        let result = (|| {
            if max_rows == 0
                || max_rows > burst_store_lmdb::retention::MAX_PAGE_ROWS
                || max_bytes == 0
                || max_bytes > burst_store_lmdb::retention::MAX_PAGE_BYTES
            {
                return Err(invalid("invalid signer retention page budget"));
            }
            if self.phase == PREFIXES.len() {
                return Ok(ReferencePage {
                    rows: 0,
                    bytes: 0,
                    done: true,
                });
            }
            let page = self.store.meta_store().visit_prefix_page_txn(
                self.view,
                PREFIXES[self.phase],
                self.after.as_deref(),
                max_rows,
                max_bytes,
                |key, bytes| visit(key, decode(key, bytes, self.network, self.genesis)?),
            )?;
            if let Some(key) = page.last_key {
                self.after = Some(key);
            }
            if page.done {
                self.phase += 1;
                self.after = None;
            }
            Ok(ReferencePage {
                rows: page.rows,
                bytes: page.bytes,
                done: self.phase == PREFIXES.len(),
            })
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}

#[cfg(test)]
mod tests;
