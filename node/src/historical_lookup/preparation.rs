//! Single-entry cache of authenticated history cursors. Store identities allow
//! reuse; fetched bodies still require cursor authentication.
use super::*;
use crate::confirmed_replay::snapshot::history::HistoryCursor;

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Request {
    Account(WalletAddress),
    Batch(Vec<BlockHash>),
}

pub(crate) enum Prepared {
    Account(Option<HistoryCursor>),
    Batch(Option<(Vec<HistoryCursor>, Vec<HistoryCursor>)>),
}

struct Entry {
    identity: usize,
    network: NetworkId,
    params: ProtocolParams,
    request: Request,
    prepared: Arc<Prepared>,
}

#[derive(Default)]
pub(crate) struct Cache {
    entry: Option<Entry>,
}

impl Cache {
    /// Reuse only an exact store-identity, network, parameter and request match.
    /// Batch preparation visits all confirmed heads. The cache budget is checked
    /// after construction; it does not bound preparation time or peak memory.
    pub(crate) fn prepare(
        &mut self, store: &LmdbStore, network: NetworkId, params: &ProtocolParams, request: Request,
    ) -> Result<Arc<Prepared>, String> {
        let view = store.tx_begin_read().map_err(|error| error.to_string())?;
        let identity = store.read_snapshot_base_identity(&view);
        if let Some(entry) = &self.entry {
            if identity == Some(entry.identity) && network == entry.network
                && params == &entry.params && request == entry.request {
                return Ok(entry.prepared.clone());
            }
        }
        // Drop the cache reference; existing Arc holders may retain the old cursors.
        self.entry = None;
        let base = installed::read_base(
            |key| store.meta_store().get_meta_txn(&view, key),
            network, params, store.env().path(),
        ).map_err(|error| error.to_string())?.map(|(base, _)| base);
        let prepared = match &request {
            Request::Account(account) => Prepared::Account(base.as_ref()
                .map(|base| base.history_cursor(account)).transpose()
                .map_err(|error| error.to_string())?.flatten()),
            Request::Batch(wanted) => Prepared::Batch(match &base {
                None => None,
                Some(base) => {
                    let mut cursors = Vec::new();
                    for head in base.confirmed_heads() {
                        if let Some(cursor) = base.history_cursor(&head.account)
                            .map_err(|error| error.to_string())? { cursors.push(cursor); }
                    }
                    let committed = base.committed_history_cursors(wanted)
                        .map_err(|error| error.to_string())?;
                    Some((cursors, committed))
                }
            }),
        };
        drop(base);
        let prepared = Arc::new(prepared);
        if let Some(identity) = identity.filter(|_| retained_bytes(&request, &prepared)
            .is_some_and(|bytes| bytes <= witness::MAX_WALK_BYTES)) {
            self.entry = Some(Entry { identity, network, params: params.clone(), request,
                prepared: prepared.clone() });
        }
        Ok(prepared)
    }
}

// Account address payload plus allocated vector/inline storage. This is a cache
// retention bound, not a total process-memory cap. Oversized results work uncached.
fn retained_bytes(request: &Request, prepared: &Prepared) -> Option<usize> {
    let mut bytes = std::mem::size_of::<Entry>() + std::mem::size_of::<Prepared>();
    bytes = bytes.checked_add(match request {
        Request::Account(account) => account.as_str().len(),
        Request::Batch(hashes) => hashes.capacity().checked_mul(std::mem::size_of::<BlockHash>())?,
    })?;
    let mut cursor_payload = |cursor: &HistoryCursor| -> Option<()> {
        bytes = bytes.checked_add(cursor.next()?.account.as_str().len())?;
        Some(())
    };
    match prepared {
        Prepared::Account(Some(cursor)) => cursor_payload(cursor)?,
        Prepared::Batch(Some((cursors, committed))) => {
            for cursor in cursors.iter().chain(committed) { cursor_payload(cursor)?; }
            bytes = bytes.checked_add(cursors.capacity().checked_add(committed.capacity())?
                .checked_mul(std::mem::size_of::<HistoryCursor>())?)?;
        }
        _ => {}
    }
    Some(bytes)
}
