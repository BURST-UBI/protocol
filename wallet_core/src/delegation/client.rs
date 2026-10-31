use super::*;
use crate::{signed_client::parse_hash, wallet::ProcessResponse, NodeClient};
use burst_governance::signing_delegation::{
    operation::DelegationOperation, DelegatedBallot, Grant, Revocation,
};
use burst_types::KeyPair;

impl NodeClient {
    /// Read a delegation page and check its shape, grant signatures and query bindings.
    /// Confirmation flags and grant-block references are supplied by the node,
    /// not independently established by this response validation.
    pub async fn delegation_keys(&self, query: &view::Query) -> Result<view::Page, WalletError> {
        let page: view::Page = serde_json::from_value(
            self.rpc_call(
                "delegation_keys",
                serde_json::to_value(query).map_err(|e| WalletError::Node(e.to_string()))?,
            )
            .await?,
        )
        .map_err(|e| WalletError::Node(e.to_string()))?;
        if page.entries.len() > 50 || (query.scope.is_some() && page.entries.len() > 1) {
            return Err(WalletError::Node("invalid delegation page size".into()));
        }
        let genesis = parse_hash(&page.genesis)?;
        for entry in &page.entries {
            let scope = entry.scope.resolve().map_err(WalletError::Node)?;
            let generation = entry
                .generation
                .parse::<u64>()
                .map_err(|_| WalletError::Node("invalid delegation generation".into()))?;
            if generation == 0
                || query
                    .scope
                    .as_ref()
                    .is_some_and(|requested| requested.resolve().ok().as_ref() != Some(&scope))
                || matches!(query.direction, view::Direction::Outgoing)
                    && entry.owner != query.account
            {
                return Err(WalletError::Node("delegation query mismatch".into()));
            }
            if let Some(grant) = &entry.grant {
                grant.verify().map_err(WalletError::Node)?;
                if grant.network != page.network
                    || grant.genesis != genesis
                    || grant.owner.as_str() != entry.owner
                    || grant.scope != scope
                    || grant.generation != generation
                    || entry.delegate.as_deref()
                        != Some(burst_crypto::derive_address(&grant.delegate).as_str())
                    || matches!(query.direction, view::Direction::Incoming)
                        && entry.delegate.as_deref() != Some(query.account.as_str())
                {
                    return Err(WalletError::Node("delegation grant mismatch".into()));
                }
                parse_hash(
                    entry
                        .grant_block
                        .as_deref()
                        .ok_or_else(|| WalletError::Node("grant source is missing".into()))?,
                )?;
            } else if entry.grant_block.is_some()
                || entry.delegate.is_some()
                || matches!(query.direction, view::Direction::Incoming)
            {
                return Err(WalletError::Node("invalid revoked delegation".into()));
            }
        }
        Ok(page)
    }

    pub async fn set_scoped_delegation(
        &self,
        keys: &KeyPair,
        scope: &view::ScopeChoice,
        delegate: Option<&WalletAddress>,
        expected_generation: &str,
        expected_genesis: &str,
    ) -> Result<ProcessResponse, WalletError> {
        let page = self
            .delegation_keys(&view::Query {
                account: burst_crypto::derive_address(&keys.public).to_string(),
                direction: view::Direction::Outgoing,
                scope: Some(scope.clone()),
                cursor: None,
            })
            .await?;
        require_review(&page, expected_generation)?;
        if parse_hash(&page.genesis)? != parse_hash(expected_genesis)? {
            return Err(WalletError::TransactionBuild(
                "the node's network changed; review delegation again".into(),
            ));
        }
        let operation = build_scoped_operation(keys, scope, delegate, expected_generation, &page)?;
        self.submit_delegation_key_operation_signed(keys, &operation)
            .await
    }

    pub async fn vote_with_delegation(
        &self,
        keys: &KeyPair,
        entry: &view::Entry,
        proposal: burst_types::TxHash,
        vote: burst_transactions::governance::GovernanceVote,
    ) -> Result<ProcessResponse, WalletError> {
        let page = self
            .delegation_keys(&view::Query {
                account: entry.owner.clone(),
                direction: view::Direction::Outgoing,
                scope: Some(entry.scope.clone()),
                cursor: None,
            })
            .await?;
        require_review(&page, &entry.generation)?;
        let current = page
            .entries
            .first()
            .ok_or_else(|| WalletError::TransactionBuild("delegation was removed".into()))?;
        if current.grant_block != entry.grant_block || !current.confirmed {
            return Err(WalletError::TransactionBuild(
                "delegation changed or is not confirmed; review it again".into(),
            ));
        }
        let operation = build_delegated_ballot(keys, current, proposal, vote)?;
        self.submit_delegation_key_operation_signed(keys, &operation)
            .await
    }
}

pub(crate) fn build_scoped_operation(
    keys: &KeyPair,
    scope: &view::ScopeChoice,
    delegate: Option<&WalletAddress>,
    expected_generation: &str,
    page: &view::Page,
) -> Result<DelegationOperation, WalletError> {
    require_review(page, expected_generation)?;
    if page.entries.first().is_some_and(|entry| {
        entry.owner != burst_crypto::derive_address(&keys.public).as_str() || entry.scope != *scope
    }) {
        return Err(WalletError::TransactionBuild(
            "delegation owner or scope mismatch".into(),
        ));
    }
    let generation = expected_generation
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_add(1))
        .ok_or_else(|| WalletError::TransactionBuild("invalid delegation generation".into()))?;
    let scope = scope.resolve().map_err(WalletError::TransactionBuild)?;
    let genesis = parse_hash(&page.genesis)?;
    let operation = if let Some(delegate) = delegate {
        let public = burst_crypto::decode_address(delegate.as_str())
            .ok_or_else(|| WalletError::TransactionBuild("invalid delegate address".into()))?;
        DelegationOperation::Grant(
            Grant::create(
                keys,
                burst_types::PublicKey(public),
                scope,
                generation,
                page.network,
                genesis,
            )
            .map_err(WalletError::TransactionBuild)?,
        )
    } else {
        DelegationOperation::Revoke(
            Revocation::create(keys, scope, generation, page.network, genesis)
                .map_err(WalletError::TransactionBuild)?,
        )
    };
    Ok(operation)
}

pub(crate) fn build_delegated_ballot(
    keys: &KeyPair,
    current: &view::Entry,
    proposal: burst_types::TxHash,
    vote: burst_transactions::governance::GovernanceVote,
) -> Result<DelegationOperation, WalletError> {
    if !current.confirmed {
        return Err(WalletError::TransactionBuild(
            "delegation is not confirmed".into(),
        ));
    }
    let grant = current
        .grant
        .as_ref()
        .ok_or_else(|| WalletError::TransactionBuild("delegation was revoked".into()))?;
    let secondary = grant
        .decrypt(&keys.private)
        .map_err(WalletError::TransactionBuild)?;
    let ballot = DelegatedBallot::sign(grant, &secondary, proposal, vote)
        .map_err(WalletError::TransactionBuild)?;
    let grant_block = parse_hash(
        current
            .grant_block
            .as_deref()
            .ok_or_else(|| WalletError::Node("missing grant block".into()))?,
    )?;
    Ok(DelegationOperation::Ballot {
        ballot,
        grant_block,
    })
}

pub(crate) fn require_review(page: &view::Page, expected: &str) -> Result<(), WalletError> {
    if !page.enabled {
        return Err(WalletError::TransactionBuild(
            "scoped delegation is not enabled on this node".into(),
        ));
    }
    if page
        .entries
        .first()
        .map_or("0", |entry| entry.generation.as_str())
        != expected
        || page.entries.first().is_some_and(|entry| !entry.confirmed)
    {
        return Err(WalletError::TransactionBuild(
            "delegation changed or is awaiting confirmation; review it again".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scoped_review_rejects_disabled_changed_or_unconfirmed_authority() {
        let mut page = view::Page {
            enabled: false,
            network: burst_types::NetworkId::Dev,
            genesis: "01".repeat(32),
            entries: vec![],
            cursor: None,
            parameters: vec![],
        };
        assert!(require_review(&page, "0").is_err());
        page.enabled = true;
        require_review(&page, "0").unwrap();
        page.entries.push(view::Entry {
            owner: String::new(),
            scope: view::ScopeChoice::Parameter("brn_rate".into()),
            generation: "1".into(),
            delegate: None,
            grant_block: None,
            grant: None,
            confirmed: false,
        });
        assert!(require_review(&page, "1").is_err());
        page.entries[0].confirmed = true;
        assert!(require_review(&page, "0").is_err());
        require_review(&page, "1").unwrap();
    }
}
