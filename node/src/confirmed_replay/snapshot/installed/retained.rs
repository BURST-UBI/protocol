use super::super::account_inventory::AccountInventory;
use super::super::{history::HistoryCursor, SnapshotContinuation};
use super::{invalid, *};
use crate::confirmed_replay::blocks::{staged::StagedBlocks, AppliedBlocks};
use crate::{checkpoint_sync::witness, connection_registry::ConnectionRegistry};
use burst_store_lmdb::block::IndexedBlock;
use burst_types::{BlockHash, WalletAddress};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};
use tokio::sync::RwLock;

const MAX_ROWS: usize = 131_072;
const MAX_BYTES: usize = 64 << 20;

#[derive(Clone, PartialEq, Eq)]
struct Observation {
    hash: BlockHash,
    account: WalletAddress,
    height: u64,
    digest: [u8; 32],
}

impl From<&IndexedBlock<'_>> for Observation {
    fn from(row: &IndexedBlock<'_>) -> Self {
        Self {
            hash: row.hash,
            account: row.account.clone(),
            height: row.height,
            digest: burst_crypto::blake2b_256(row.bytes),
        }
    }
}

pub(super) struct RetainedInventory {
    rows: Vec<Observation>,
    accounts: AccountInventory,
}

impl RetainedInventory {
    pub(super) fn capture_successor(
        &self,
        batch: &LmdbWriteTransaction<'_>,
        previous: &crate::confirmed_replay::ConfirmedReplay,
        supplied: &HashMap<BlockHash, (WalletAddress, u64, [u8; 32])>,
    ) -> Result<Self, StoreError> {
        let old: HashMap<_, _> = self.rows.iter().map(|row| (row.hash, row)).collect();
        let rows = batch
            .indexed_block_entries(MAX_ROWS, MAX_BYTES)?
            .map(|row| {
                let row = Observation::from(&row?);
                let inherited = old.get(&row.hash).is_some_and(|old| **old == row)
                    && previous
                        .account(&row.account)
                        .is_some_and(|account| row.height <= account.block_count);
                let installed = supplied
                    .get(&row.hash)
                    .is_some_and(|(account, height, digest)| {
                        *account == row.account && *height == row.height && *digest == row.digest
                    });
                if !inherited && !installed {
                    return Err(invalid(
                        "post-install retained row has no authenticated origin",
                    ));
                }
                Ok(row)
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok(Self {
            rows,
            accounts: AccountInventory::read(batch.account_entries()?)?,
        })
    }

    pub(super) fn verify_unchanged(
        &self,
        batch: &LmdbWriteTransaction<'_>,
    ) -> Result<(), StoreError> {
        let mut rows = batch.indexed_block_entries(MAX_ROWS, MAX_BYTES)?;
        for expected in &self.rows {
            let actual = rows
                .next()
                .transpose()?
                .ok_or_else(|| invalid("retained history changed during recovery"))?;
            if Observation::from(&actual) != *expected {
                return Err(invalid("retained history changed during recovery"));
            }
        }
        if rows.next().transpose()?.is_some()
            || AccountInventory::read(batch.account_entries()?)? != self.accounts
        {
            return Err(invalid("retained history changed during recovery"));
        }
        Ok(())
    }
}

struct AccountWalk {
    cursor: HistoryCursor,
    rows: VecDeque<Observation>,
    bodies: Arc<StagedBlocks>,
}

impl AccountWalk {
    fn local_prefix(&mut self) -> Result<(), StoreError> {
        while let Some(row) = self.rows.front() {
            if self.cursor.next().is_none_or(|head| head.hash != row.hash) {
                break;
            }
            let row = self.rows.pop_front().expect("checked front");
            if self
                .cursor
                .next()
                .is_none_or(|head| head.height != row.height)
            {
                return Err(invalid(
                    "retained block index differs from authenticated height",
                ));
            }
            self.cursor
                .accept(&[self.bodies.encoded(&row.hash)?])
                .map_err(invalid)?;
        }
        Ok(())
    }
}

fn plan(
    store: &LmdbStore,
    base: &SnapshotContinuation,
) -> Result<(RetainedInventory, Vec<AccountWalk>), StoreError> {
    let txn = store.tx_begin_read()?;
    let account_store = store.account_store();
    let accounts = AccountInventory::read(account_store.account_entries(&txn)?)?;
    let block_store = store.block_store();
    let rows = block_store
        .indexed_block_entries(&txn, MAX_ROWS, MAX_BYTES)?
        .map(|row| row.map(|row| Observation::from(&row)))
        .collect::<Result<Vec<_>, _>>()?;
    let inventory = RetainedInventory { rows, accounts };
    let mut groups: HashMap<WalletAddress, Vec<Observation>> = HashMap::new();
    let mut bodies = StagedBlocks::new(store.env().path())?;
    let mut committed: HashMap<_, _> = base
        .committed_history_cursors(
            &inventory
                .rows
                .iter()
                .map(|row| row.hash)
                .collect::<Vec<_>>(),
        )
        .map_err(invalid)?
        .into_iter()
        .map(|cursor| (cursor.next().expect("committed cursor").hash, cursor))
        .collect();
    for row in block_store.indexed_block_entries(&txn, MAX_ROWS, MAX_BYTES)? {
        let row = row?;
        let account = account_store.get_account_txn(&txn, &row.account)?;
        if row.height > account.block_count {
            return Err(invalid("retained block exceeds local account height"));
        }
        if let Some(mut cursor) = committed.remove(&row.hash) {
            if cursor
                .next()
                .is_none_or(|head| head.account != row.account || head.height != row.height)
            {
                return Err(invalid(
                    "retained block differs from committed history head",
                ));
            }
            cursor.accept(&[row.bytes.to_vec()]).map_err(invalid)?;
            continue;
        }
        if base
            .state()
            .account(&row.account)
            .is_some_and(|account| row.height <= account.block_count)
        {
            bodies.insert_encoded(row.hash, row.bytes)?;
            groups
                .entry(row.account.clone())
                .or_default()
                .push(Observation::from(&row));
        }
    }
    let mut walks = Vec::new();
    let bodies = Arc::new(bodies);
    for (account, mut rows) in groups {
        rows.sort_by_key(|row| std::cmp::Reverse(row.height));
        walks.push(AccountWalk {
            cursor: base
                .history_cursor(&account)
                .map_err(invalid)?
                .ok_or_else(|| invalid("retained history has no authenticated account"))?,
            rows: rows.into(),
            bodies: bodies.clone(),
        });
    }
    Ok((inventory, walks))
}

pub(super) async fn verify(
    store: Arc<LmdbStore>,
    recovered: RecoveredSnapshot,
    registry: &Arc<RwLock<ConnectionRegistry>>,
) -> Result<(RecoveredSnapshot, RetainedInventory), StoreError> {
    let (recovered, inventory, walks) = tokio::task::spawn_blocking(move || {
        let (inventory, walks) = plan(&store, &recovered.continuation)?;
        Ok::<_, StoreError>((recovered, inventory, walks))
    })
    .await
    .map_err(invalid)??;
    for mut walk in walks {
        loop {
            walk = tokio::task::spawn_blocking(move || {
                walk.local_prefix()?;
                Ok::<_, StoreError>(walk)
            })
            .await
            .map_err(invalid)??;
            if walk.rows.is_empty() {
                break;
            }
            // Keep each retained response below the existing witness budgets.
            let wanted: Vec<_> = walk.rows.iter().take(128).map(|row| row.hash).collect();
            let proofs = witness::fetch(registry, walk.cursor, wanted.clone()).await?;
            let mut cursor = recovered
                .continuation
                .history_cursor(&walk.rows[0].account)
                .map_err(invalid)?
                .ok_or_else(|| invalid("missing retained history root"))?;
            walk = tokio::task::spawn_blocking(move || {
                let mut proofs: HashMap<_, _> = proofs
                    .into_iter()
                    .map(|proof| (proof.block().hash, proof))
                    .collect();
                if proofs.len() != wanted.len() {
                    return Err(invalid("incomplete retained history proofs"));
                }
                for hash in wanted {
                    let row = walk.rows.pop_front().expect("requested retained row");
                    let proof = proofs
                        .remove(&hash)
                        .ok_or_else(|| invalid("missing retained history proof"))?;
                    if proof.height() != row.height || proof.block().account != row.account {
                        return Err(invalid("retained history index differs from proof"));
                    }
                    cursor.seek_proven(&proof).map_err(invalid)?;
                    // Validate the local signature too: membership alone does not
                    // authenticate bytes excluded from the signed block hash.
                    cursor
                        .accept(&[walk.bodies.encoded(&row.hash)?])
                        .map_err(invalid)?;
                }
                Ok::<_, StoreError>(AccountWalk {
                    cursor,
                    rows: walk.rows,
                    bodies: walk.bodies,
                })
            })
            .await
            .map_err(invalid)??;
        }
    }
    Ok((recovered, inventory))
}
