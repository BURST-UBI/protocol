#[cfg(test)]
use burst_governance::signing_delegation::Grant;
use super::*;
use burst_governance::signing_delegation::{
    operation::DelegationOperation, scoped, DelegatedBallot, DelegationAuthority, KeyScope,
};
use burst_types::{BlockHash, Timestamp, TxHash, WalletAddress};
use std::collections::{HashMap, HashSet};

mod grants;
mod walk;

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::measure_projection_work;

fn parameter(content: &ProposalContent) -> Option<&GovernableParam> {
    match content {
        ProposalContent::ParameterChange { param, .. }
        | ProposalContent::Emergency { param, .. } => Some(param),
        _ => None,
    }
}

impl CandidateGovernance {
    pub(super) fn apply_key_delegation(
        &mut self,
        batch: &mut impl ledger::Ledger,
        block: &StateBlock,
    ) -> Result<(), String> {
        if !cfg!(test) && !crate::release_policy::delegation(self.network) {
            return Err("scoped key delegation is behind the release gate".into());
        }
        block.validate_format().map_err(str::to_owned)?;
        let operation = DelegationOperation::decode(&block.delegation_payload)?;
        if let DelegationOperation::Ballot {
            ballot,
            grant_block,
        } = operation
        {
            return self.apply_key_ballot(batch, block, &ballot, grant_block);
        }
        let (network, genesis, owner, scope) =
            operation.identity().ok_or("delegation identity missing")?;
        if network != self.network || genesis != self.genesis || owner != &block.account {
            return Err("delegation operation belongs to another owner or chain".into());
        }
        if matches!(operation, DelegationOperation::Grant(_)) {
            if !batch
                .get_account(owner)
                .map_err(|error| error.to_string())?
                .state
                .is_verified()
            {
                return Err("delegation requires a verified owner".into());
            }
            if let Some(proposal) = operation.proposal() {
                if self.governance.get_proposal(&proposal).is_none() {
                    return Err("delegation proposal is unknown".into());
                }
            }
        }
        let key = scoped::key(self.network, self.genesis, owner, scope)?;
        let mut authority = match batch.get_meta(&key).map_err(|error| error.to_string())? {
            Some(bytes) => scoped::restore(self.network, self.genesis, owner, scope, &bytes)?,
            None => DelegationAuthority::new(self.network, self.genesis)?,
        };
        let old = scoped::grant(&authority, owner, scope);
        let owner = owner.clone();
        let scope = scope.clone();
        match operation {
            DelegationOperation::Grant(grant) => authority.apply_grant(grant)?,
            DelegationOperation::Revoke(revocation) => authority.apply_revocation(&revocation)?,
            DelegationOperation::Ballot { .. } => unreachable!("ballots handled above"),
        }
        let bytes = authority.save()?;
        if bytes.len() > scoped::MAX_SCOPE_BYTES {
            return Err("delegation scope record is oversized".into());
        }
        batch
            .put_meta(&key, &bytes)
            .map_err(|error| error.to_string())?;
        use burst_governance::signing_delegation::view::{index_key, Direction};
        if let Some(old) = old {
            let recipient = burst_crypto::derive_address(&old.delegate);
            batch
                .put_meta(
                    &index_key(Direction::Incoming, &recipient, &key)?,
                    BlockHash::ZERO.as_bytes(),
                )
                .map_err(|e| e.to_string())?;
        }
        batch
            .put_meta(
                &index_key(Direction::Outgoing, &owner, &key)?,
                block.hash.as_bytes(),
            )
            .map_err(|e| e.to_string())?;
        if let Some(grant) = scoped::grant(&authority, &owner, &scope) {
            let recipient = burst_crypto::derive_address(&grant.delegate);
            batch
                .put_meta(
                    &index_key(Direction::Incoming, &recipient, &key)?,
                    block.hash.as_bytes(),
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn open_vote_window(&mut self, proposal: TxHash, now: Timestamp) -> Result<(), String> {
        let saved = self
            .governance
            .get_proposal(&proposal)
            .ok_or("delegation proposal is unknown")?;
        if saved.phase == GovernancePhase::Proposal {
            let mut next = saved.clone();
            self.governance
                .try_advance(&mut next, now, &self.params, None)
                .map_err(|error| error.to_string())?;
            *self
                .governance
                .get_proposal_mut(&proposal)
                .ok_or("delegation proposal is missing")? = next;
        }
        Ok(())
    }

    fn apply_key_ballot(
        &mut self,
        batch: &impl ledger::Ledger,
        block: &StateBlock,
        ballot: &DelegatedBallot,
        grant_block: BlockHash,
    ) -> Result<(), String> {
        let source: StateBlock = bincode::deserialize(
            &batch
                .get_block(&grant_block)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        if source.hash != grant_block
            || source.compute_hash() != grant_block
            || source.block_type != BlockType::Delegate
            || source.timestamp > block.timestamp
        {
            return Err("invalid ballot grant dependency".into());
        }
        let DelegationOperation::Grant(grant) =
            DelegationOperation::decode(&source.delegation_payload)?
        else {
            return Err("ballot source is not a grant".into());
        };
        if grant.network != self.network
            || grant.genesis != self.genesis
            || grant.owner != source.account
            || burst_crypto::derive_address(&grant.delegate) != block.account
        {
            return Err("ballot relayer or grant chain mismatch".into());
        }
        for person in [&ballot.owner, &block.account] {
            if !batch
                .get_account(person)
                .map_err(|error| error.to_string())?
                .state
                .can_vote()
            {
                return Err("delegated ballot requires verified owner and recipient".into());
            }
        }
        let proposal = self
            .governance
            .get_proposal(&ballot.proposal)
            .ok_or("ballot proposal is unknown")?;
        // Admission authenticates historical authority. Looking up the current
        // grant here would make independent-chain revocation arrival order
        // decide whether this account block is valid. The certified count cut
        // resolves revocations and rotations before assigning any vote weight.
        grant.verify_candidate_ballot(ballot, parameter(&proposal.content))?;
        self.open_vote_window(ballot.proposal, block.timestamp)?;
        self.governance
            .record_key_ballot(ballot, block.timestamp, &self.params)
            .map_err(|error| error.to_string())
    }

    #[cfg(test)]
    fn scoped_grant(&self, owner: &WalletAddress, proposal: TxHash,
        param: Option<&GovernableParam>, metadata: &HashMap<String, Vec<u8>>,
    ) -> Result<grants::ScopedGrant, String> {
        self.projection_cut(metadata).scoped_grant(owner, proposal, param)
    }

    pub(crate) fn projection_cut<'a>(&'a self, metadata: &'a HashMap<String, Vec<u8>>) -> ProjectionCut<'a> {
        ProjectionCut { state: self, metadata, parameters: Some(grants::ParameterCache::default()), owners: Some(HashMap::new()), eligible: None, records: None }
    }

    pub(crate) fn single_projection<'a>(&'a self, metadata: &'a HashMap<String, Vec<u8>>) -> ProjectionCut<'a> {
        ProjectionCut { state: self, metadata, parameters: None, owners: None, eligible: None, records: None }
    }

    #[cfg(test)]
    pub(crate) fn delegation_at_cut(&self, proposal: TxHash, verified: &[WalletAddress],
        metadata: &HashMap<String, Vec<u8>>,
    ) -> Result<DelegationEngine, String> {
        self.single_projection(metadata).delegation(proposal, verified)
    }
}

/// Borrows immutable candidate state and metadata for delegation projection.
/// Parameter grants, owner keys, eligible voters and the optional record index
/// can be reused; proposal grant results and traversal state are per projection.
pub(crate) struct ProjectionCut<'a> {
    state: &'a CandidateGovernance,
    metadata: &'a HashMap<String, Vec<u8>>,
    parameters: Option<grants::ParameterCache>,
    owners: Option<HashMap<WalletAddress, scoped::OwnerKeys>>,
    eligible: Option<(&'a [WalletAddress], std::sync::Arc<HashSet<&'a WalletAddress>>)>,
    records: Option<Option<scoped::RecordIndex<'a>>>,
}

impl<'a> ProjectionCut<'a> {
    fn scoped_grant(&mut self, owner: &WalletAddress, proposal: TxHash,
        param: Option<&GovernableParam>,
    ) -> Result<grants::ScopedGrant, String> {
        let keys = std::cell::OnceCell::new();
        for scope in std::iter::once(KeyScope::Proposal(proposal))
            .chain(param.cloned().map(KeyScope::Parameter)) {
            let cache_param = match (&self.parameters, &scope) {
                (Some(_), KeyScope::Parameter(param)) => Some(param),
                _ => None,
            };
            if let Some(known) = self.parameters.as_ref().and_then(|parameters|
                cache_param.and_then(|param| parameters.get(owner, param))) {
                return Ok(known);
            }
            let bytes = {
                #[cfg(test)]
                let _span = tests::projection_span(0);
                let load = || {
                    #[cfg(test)]
                    let _span = tests::projection_span(5);
                    scoped::OwnerKeys::new(self.state.network, self.state.genesis, owner)
                };
                let owner_keys = if let Some(owners) = &mut self.owners {
                    if !owners.contains_key(owner) {
                        // Cache only validated owner keys; propagate invalid identities.
                        let validated = load()?;
                        owners.insert(owner.clone(), validated);
                    }
                    owners.get(owner).expect("validated owner inserted")
                } else {
                    keys.get_or_init(load).as_ref().map_err(Clone::clone)?
                };
                if let Some(Some(index)) = &self.records {
                    owner_keys.validate_scope(&scope)?;
                    index.get(owner, &scope)
                } else {
                    #[cfg(test)]
                    let _encode_span = tests::projection_span(6);
                    let key = owner_keys.key(&scope)?;
                    self.metadata.get(&key).map(Vec::as_slice)
                }
            };
            let result = if let Some(bytes) = bytes {
                let grant = {
                    #[cfg(test)]
                    let _span = tests::projection_span(1);
                    scoped::restore_authenticated_grant(self.state.network, self.state.genesis, owner, &scope, bytes)?
                };
                Some(grant.map(|grant| grants::CutGrant::Authenticated(std::sync::Arc::new(grant))))
            } else { None };
            if let (Some(parameters), Some(param)) = (&mut self.parameters, cache_param) { parameters.insert(owner, param, result.clone()); }
            if result.is_some() { return Ok(result) }
        }
        Ok(None)
    }

    pub(crate) fn delegation(
        &mut self,
        proposal: TxHash,
        verified: &'a [WalletAddress],
    ) -> Result<DelegationEngine, String> {
        let state = self.state;
        let target = state
            .governance
            .get_proposal(&proposal)
            .ok_or("ballot proposal missing")?;
        let param = parameter(&target.content);
        if self.records.is_none() {
            self.records = Some(if self.parameters.is_some()
                && self.metadata.len() <= verified.len()
                && self.metadata.len() <= scoped::RecordIndex::MAX_ROWS
            {
                scoped::RecordIndex::build(state.network, state.genesis,
                    self.metadata.iter().map(|(key, bytes)| (key.as_str(), bytes.as_slice())))
            } else { None });
        }
        // Retaining the borrowed slice makes pointer identity safe: its contents
        // cannot change or its allocation be reused while this cut holds it.
        if self.eligible.as_ref().is_none_or(|(prior, _)| !std::ptr::eq(*prior, verified)) {
            #[cfg(test)]
            let _span = tests::projection_span(7);
            self.eligible = Some((verified, std::sync::Arc::new(verified.iter().collect())));
        }
        let eligible = self.eligible.as_ref().unwrap().1.clone();
        let mut projection = state.delegation.clone();
        let mut grants = grants::CutGrants::new(|owner: &WalletAddress| {
            self.scoped_grant(owner, proposal, param)
        });
        let mut walk = walk::Walk::new();
        for owner in verified {
            if grants.get(owner)?.is_none() {
                continue;
            }
            let vote = walk.resolve(owner, state.delegation.max_depth(), |current| {
                if !eligible.contains(current) {
                    return Ok::<_, String>(walk::Step::Stop(None));
                }
                let Some((recipient, grant_id)) = grants.hop(current, |person| eligible.contains(person))? else {
                    return Ok(walk::Step::Stop(None));
                };
                if let Some(choice) = state.governance.key_ballot(proposal, current, grant_id) {
                    return Ok(walk::Step::Stop(Some(choice)));
                }
                Ok(walk::Step::Next(recipient.clone()))
            })?;
            {
                #[cfg(test)]
                let _span = tests::projection_span(4);
                projection.project_key_ballot(owner.clone(), vote);
            }
        }
        Ok(projection)
    }
}
