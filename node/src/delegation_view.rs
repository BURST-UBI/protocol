use burst_governance::key_delegation::{operation::DelegationOperation, scoped, view::*};
use burst_types::{BlockHash, NetworkId, WalletAddress};
use std::sync::Arc;

pub(crate) struct Reader {
    pub store: Arc<burst_store_lmdb::LmdbStore>,
    pub network: NetworkId,
    pub genesis: BlockHash,
}

impl Reader {
    pub fn read(&self, query: Query, enabled: bool) -> Result<Page, String> {
        let address = WalletAddress::new(query.account);
        let prefix = index_prefix(query.direction, &address)?;
        if query.cursor.as_ref().is_some_and(|cursor| {
            cursor.len() != 64 || !cursor.bytes().all(|c| c.is_ascii_hexdigit())
        }) {
            return Err("invalid delegation cursor".into());
        }
        let exact = query.scope.as_ref().map(ScopeChoice::resolve).transpose()?;
        if exact.is_some()
            && (!matches!(query.direction, Direction::Outgoing) || query.cursor.is_some())
        {
            return Err("scope lookup requires an outgoing query without a cursor".into());
        }
        let txn = self.store.tx_begin_read().map_err(|e| e.to_string())?;
        let meta = self.store.meta_store();
        let mut rows = if let Some(scope) = &exact {
            let key = index_key(
                Direction::Outgoing,
                &address,
                &scoped::key(self.network, self.genesis, &address, scope)?,
            )?;
            meta.get_meta_txn(&txn, &key)
                .map_err(|e| e.to_string())?
                .map(|bytes| vec![(key, bytes)])
                .unwrap_or_default()
        } else {
            let cursor = query.cursor.map(|suffix| format!("{prefix}{suffix}"));
            meta.prefix_page_txn(&txn, &prefix, cursor.as_deref(), 51)
                .map_err(|e| e.to_string())?
        };
        let has_more = rows.len() > 50;
        rows.truncate(50);
        let cursor = has_more.then(|| rows.last().unwrap().0[prefix.len()..].to_owned());
        let mut entries = Vec::new();
        for (index, bytes) in rows {
            let hash = BlockHash::new(
                bytes
                    .try_into()
                    .map_err(|_| "invalid delegation index value")?,
            );
            if hash.is_zero() {
                continue;
            }
            let block: burst_ledger::StateBlock = bincode::deserialize(
                &self
                    .store
                    .block_store()
                    .get_block_txn(&txn, &hash)
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            block.validate_format().map_err(str::to_owned)?;
            if block.hash != hash || block.compute_hash() != hash {
                return Err("delegation source hash mismatch".into());
            }
            let operation = DelegationOperation::decode(&block.delegation_payload)?;
            let (network, genesis, owner, scope) = operation
                .identity()
                .ok_or("delegation source is a ballot")?;
            if network != self.network || genesis != self.genesis || owner != &block.account {
                return Err("delegation source chain mismatch".into());
            }
            let key = scoped::key(network, genesis, owner, scope)?;
            if index != index_key(query.direction, &address, &key)? {
                return Err("delegation index mismatch".into());
            }
            let authority = scoped::restore(
                network,
                genesis,
                owner,
                scope,
                &meta
                    .get_meta_txn(&txn, &key)
                    .map_err(|e| e.to_string())?
                    .ok_or("delegation scope is missing")?,
            )?;
            let grant = scoped::grant(&authority, owner, scope);
            let generation = match &operation {
                DelegationOperation::Grant(g) => g.generation,
                DelegationOperation::Revoke(r) => r.generation,
                _ => unreachable!(),
            };
            if authority.generation(owner, scope) != generation {
                return Err("stale delegation index".into());
            }
            let delegate = grant
                .as_ref()
                .map(|g| burst_crypto::derive_address(&g.delegate).to_string());
            if match query.direction {
                Direction::Outgoing => owner != &address,
                Direction::Incoming => delegate.as_deref() != Some(address.as_str()),
            } {
                return Err("delegation index owner mismatch".into());
            }
            let account = self
                .store
                .account_store()
                .get_account_txn(&txn, owner)
                .map_err(|e| e.to_string())?;
            let height = self
                .store
                .block_store()
                .height_of_block_txn(&txn, &hash)
                .map_err(|e| e.to_string())?
                .ok_or("delegation source is not indexed")?;
            entries.push(Entry {
                owner: owner.to_string(),
                scope: ScopeChoice::from(scope),
                generation: generation.to_string(),
                delegate,
                grant_block: grant.as_ref().map(|_| hash.to_string()),
                grant,
                confirmed: height <= account.confirmation_height,
            });
        }
        let parameters = burst_governance::GovernableParam::ALL
            .iter()
            .map(|param| param.name().to_owned())
            .collect();
        Ok(Page {
            enabled,
            network: self.network,
            genesis: self.genesis.to_string(),
            entries,
            cursor,
            parameters,
        })
    }
}
