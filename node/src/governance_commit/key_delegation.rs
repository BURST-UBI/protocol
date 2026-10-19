use super::*;
use burst_governance::key_delegation::{
    operation::DelegationOperation, scoped, DelegatedBallot, DelegationAuthority, Grant, KeyScope,
};
use burst_types::{BlockHash, Timestamp, TxHash, WalletAddress};
use std::collections::{HashMap, HashSet};

#[cfg(test)]
mod tests;

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
        use burst_governance::key_delegation::view::{index_key, Direction};
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

    fn scoped_grant(
        &self,
        owner: &WalletAddress,
        proposal: TxHash,
        param: Option<&GovernableParam>,
        metadata: &HashMap<String, Vec<u8>>,
    ) -> Result<Option<Option<Grant>>, String> {
        for scope in std::iter::once(KeyScope::Proposal(proposal))
            .chain(param.cloned().map(KeyScope::Parameter))
        {
            if let Some(bytes) =
                metadata.get(&scoped::key(self.network, self.genesis, owner, &scope)?)
            {
                let authority = scoped::restore(self.network, self.genesis, owner, &scope, bytes)?;
                return Ok(Some(scoped::grant(&authority, owner, &scope)));
            }
        }
        Ok(None)
    }

    pub(crate) fn delegation_at_cut(
        &self,
        proposal: TxHash,
        verified: &[WalletAddress],
        metadata: &HashMap<String, Vec<u8>>,
    ) -> Result<DelegationEngine, String> {
        let target = self
            .governance
            .get_proposal(&proposal)
            .ok_or("ballot proposal missing")?;
        let param = parameter(&target.content);
        let eligible: HashSet<_> = verified.iter().collect();
        let mut projection = self.delegation.clone();
        for owner in verified {
            if self
                .scoped_grant(owner, proposal, param, metadata)?
                .is_none()
            {
                continue;
            }
            let mut current = owner.clone();
            let mut seen = HashSet::new();
            let mut vote = None;
            for _ in 0..self.delegation.max_depth() {
                if !eligible.contains(&current) || !seen.insert(current.clone()) {
                    break;
                }
                let Some(Some(grant)) = self.scoped_grant(&current, proposal, param, metadata)?
                else {
                    break;
                };
                let recipient = burst_crypto::derive_address(&grant.delegate);
                if !eligible.contains(&recipient) {
                    break;
                }
                if let Some(choice) = self.governance.key_ballot(proposal, &current, grant.id()?) {
                    vote = Some(choice);
                    break;
                }
                current = recipient;
            }
            projection.project_key_ballot(owner.clone(), vote);
        }
        Ok(projection)
    }
}
