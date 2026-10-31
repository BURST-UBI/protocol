//! Authenticated secondary-key grants and revocation generations.
//!
//! The node's gated account-block path records grants, revocations, and
//! secondary-signed ballots. Certified counting resolves current authority
//! and verified eligibility. Detached grants never authorize account blocks
//! or spending.

use crate::GovernableParam;
use bincode::Options;
use burst_transactions::governance::GovernanceVote;
use burst_types::{
    BlockHash, KeyPair, NetworkId, PrivateKey, PublicKey, Signature, TxHash, WalletAddress,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use zeroize::Zeroizing;

pub mod operation;
pub mod scoped;
pub mod view;

const MAX_ENTRIES: usize = 16_384;
const MAX_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeyScope {
    Proposal(TxHash),
    Parameter(GovernableParam),
}

impl KeyScope {
    fn validate(&self) -> Result<(), String> {
        if matches!(self, Self::Proposal(hash) if hash.is_zero()) {
            return Err("zero delegation proposal".into());
        }
        Ok(())
    }

    fn encode(&self, bytes: &mut Vec<u8>) -> Result<(), String> {
        self.validate()?;
        match self {
            Self::Proposal(hash) => {
                bytes.push(0);
                bytes.extend_from_slice(hash.as_bytes());
            }
            Self::Parameter(param) => {
                bytes.push(1);
                bytes.push(param.name().len() as u8);
                bytes.extend_from_slice(param.name().as_bytes());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Grant {
    pub network: NetworkId,
    pub genesis: BlockHash,
    pub owner: WalletAddress,
    pub delegate: PublicKey,
    pub secondary: PublicKey,
    pub scope: KeyScope,
    pub generation: u64,
    pub envelope: Vec<u8>,
    pub signature: Signature,
}

fn identity(
    network: NetworkId,
    genesis: BlockHash,
    owner: &WalletAddress,
    scope: &KeyScope,
    generation: u64,
) -> Result<Vec<u8>, String> {
    if genesis.is_zero() || generation == 0 {
        return Err("invalid delegation identity".into());
    }
    let mut bytes = owner_identity(network, genesis, owner)?;
    scope.encode(&mut bytes)?;
    bytes.extend_from_slice(&generation.to_be_bytes());
    Ok(bytes)
}

fn owner_identity(
    network: NetworkId,
    genesis: BlockHash,
    owner: &WalletAddress,
) -> Result<Vec<u8>, String> {
    if genesis.is_zero() { return Err("invalid delegation identity".into()); }
    let key = burst_crypto::decode_address(owner.as_str()).ok_or("invalid delegation owner")?;
    let mut bytes = vec![match network {
        NetworkId::Live => 0,
        NetworkId::Test => 1,
        NetworkId::Dev => 2,
    }];
    bytes.extend_from_slice(genesis.as_bytes());
    bytes.extend_from_slice(&key);
    Ok(bytes)
}

impl Grant {
    pub fn create(
        owner: &KeyPair,
        delegate: PublicKey,
        scope: KeyScope,
        generation: u64,
        network: NetworkId,
        genesis: BlockHash,
    ) -> Result<Self, String> {
        if burst_crypto::public_from_private(&owner.private) != owner.public {
            return Err("owner keypair mismatch".into());
        }
        let secondary = burst_crypto::generate_keypair();
        let mut grant = Self {
            network,
            genesis,
            owner: burst_crypto::derive_address(&owner.public),
            delegate,
            scope,
            secondary: secondary.public,
            generation,
            envelope: vec![],
            signature: Signature([0; 64]),
        };
        let context = grant.context()?;
        let recipient = burst_crypto::ed25519_public_to_x25519(&grant.delegate.0)
            .ok_or("invalid delegate public key")?;
        grant.envelope =
            burst_crypto::encrypt_delegation_key(&secondary.private.0, &recipient, &context)?;
        grant.signature = burst_crypto::sign_message(&grant.signing_payload()?, &owner.private);
        Ok(grant)
    }

    fn context(&self) -> Result<Vec<u8>, String> {
        let mut bytes = b"BURST/delegation-context/v1\0".to_vec();
        bytes.extend_from_slice(&identity(
            self.network,
            self.genesis,
            &self.owner,
            &self.scope,
            self.generation,
        )?);
        if burst_crypto::derive_address(&self.delegate) == self.owner
            || burst_crypto::derive_address(&self.secondary) == self.owner
            || self.secondary == self.delegate
            || burst_crypto::ed25519_public_to_x25519(&self.delegate.0).is_none()
            || burst_crypto::ed25519_public_to_x25519(&self.secondary.0).is_none()
        {
            return Err("invalid or self delegation key".into());
        }
        bytes.extend_from_slice(&self.delegate.0);
        bytes.extend_from_slice(&self.secondary.0);
        Ok(bytes)
    }

    fn signing_payload(&self) -> Result<Vec<u8>, String> {
        burst_crypto::encryption::validate_delegation_envelope(&self.envelope)?;
        let mut bytes = b"BURST/delegation-grant/v1\0".to_vec();
        bytes.extend_from_slice(&self.context()?);
        bytes.extend_from_slice(&self.envelope);
        Ok(bytes)
    }

    /// Check the owner's signature and grant structure. Confirmation, current
    /// generation and voter eligibility require the ledger's authority state.
    pub fn verify(&self) -> Result<(), String> {
        self.verified_payload().map(|_| ())
    }

    fn verified_payload(&self) -> Result<Vec<u8>, String> {
        let key = burst_crypto::decode_address(self.owner.as_str()).ok_or("invalid owner")?;
        let payload = self.signing_payload()?;
        if !burst_crypto::verify_signature(&payload, &self.signature, &PublicKey(key)) {
            return Err("invalid primary-key delegation signature".into());
        }
        Ok(payload)
    }

    /// Hash the structured signing payload without verifying its signature.
    /// A matching ID alone does not authenticate this grant.
    pub fn id(&self) -> Result<BlockHash, String> {
        Ok(BlockHash::new(burst_crypto::blake2b_256(
            &self.signing_payload()?,
        )))
    }

    pub fn decrypt(&self, delegate: &PrivateKey) -> Result<PrivateKey, String> {
        self.verify()?;
        if burst_crypto::public_from_private(delegate) != self.delegate {
            return Err("wrong delegation recipient".into());
        }
        let secret = Zeroizing::new(burst_crypto::ed25519_private_to_x25519(&delegate.0));
        let secondary =
            burst_crypto::decrypt_delegation_key(&self.envelope, &secret, &self.context()?)?;
        if burst_crypto::public_from_private(&secondary) != self.secondary {
            return Err("secondary key does not match grant".into());
        }
        Ok(secondary)
    }

    /// Authenticate a candidate ballot against its historical grant. Current
    /// authority and verified eligibility must still be resolved at the tally cut.
    pub fn verify_candidate_ballot(
        &self,
        ballot: &DelegatedBallot,
        param: Option<&GovernableParam>,
    ) -> Result<(), String> {
        self.verify()?;
        if ballot.owner != self.owner
            || ballot.grant != self.id()?
            || match &self.scope {
                KeyScope::Proposal(proposal) => proposal != &ballot.proposal,
                KeyScope::Parameter(parameter) => Some(parameter) != param,
            }
        {
            return Err("ballot is outside its grant".into());
        }
        if !burst_crypto::verify_signature(
            &ballot.payload(self.network, self.genesis)?,
            &ballot.signature,
            &self.secondary,
        ) {
            return Err("invalid delegated ballot signature".into());
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.verify()?;
        bincode::serialize(self).map_err(|e| e.to_string())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 1024 {
            return Err("oversized delegation grant".into());
        }
        let grant: Self = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(1024)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map_err(|e| e.to_string())?;
        grant.verify()?;
        Ok(grant)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Revocation {
    pub network: NetworkId,
    pub genesis: BlockHash,
    pub owner: WalletAddress,
    pub scope: KeyScope,
    pub generation: u64,
    pub signature: Signature,
}

impl Revocation {
    pub fn create(
        owner: &KeyPair,
        scope: KeyScope,
        generation: u64,
        network: NetworkId,
        genesis: BlockHash,
    ) -> Result<Self, String> {
        let mut value = Self {
            network,
            genesis,
            owner: burst_crypto::derive_address(&owner.public),
            scope,
            generation,
            signature: Signature([0; 64]),
        };
        value.signature = burst_crypto::sign_message(&value.payload()?, &owner.private);
        value.verify()?;
        Ok(value)
    }
    fn payload(&self) -> Result<Vec<u8>, String> {
        let mut bytes = b"BURST/delegation-revoke/v1\0".to_vec();
        bytes.extend_from_slice(&identity(
            self.network,
            self.genesis,
            &self.owner,
            &self.scope,
            self.generation,
        )?);
        Ok(bytes)
    }
    fn verify(&self) -> Result<(), String> {
        let key = burst_crypto::decode_address(self.owner.as_str()).ok_or("invalid owner")?;
        if !burst_crypto::verify_signature(&self.payload()?, &self.signature, &PublicKey(key)) {
            return Err("invalid revocation signature".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    generation: u64,
    grant: Option<Grant>,
}

/// Apply only ledger-ordered, confirmed grants/revocations. Keep tombstones so
/// neither restart nor receipt of an old grant can restore revoked authority.
#[derive(Clone)]
pub struct DelegationAuthority {
    network: NetworkId,
    genesis: BlockHash,
    entries: HashMap<(WalletAddress, KeyScope), Entry>,
}

impl DelegationAuthority {
    pub fn new(network: NetworkId, genesis: BlockHash) -> Result<Self, String> {
        if genesis.is_zero() {
            return Err("zero delegation genesis".into());
        }
        Ok(Self {
            network,
            genesis,
            entries: HashMap::new(),
        })
    }
    fn check_next(
        &self,
        network: NetworkId,
        genesis: BlockHash,
        owner: &WalletAddress,
        scope: &KeyScope,
        generation: u64,
    ) -> Result<(), String> {
        if network != self.network || genesis != self.genesis {
            return Err("delegation belongs to another chain".into());
        }
        let old = self
            .entries
            .get(&(owner.clone(), scope.clone()))
            .map_or(0, |e| e.generation);
        if old == 0 && self.entries.len() >= MAX_ENTRIES {
            return Err("delegation authority capacity reached".into());
        }
        if old.checked_add(1) != Some(generation) {
            return Err("delegation generation is stale or has a gap".into());
        }
        Ok(())
    }
    pub fn apply_grant(&mut self, grant: Grant) -> Result<(), String> {
        grant.verify()?;
        self.check_next(
            grant.network,
            grant.genesis,
            &grant.owner,
            &grant.scope,
            grant.generation,
        )?;
        self.entries.insert(
            (grant.owner.clone(), grant.scope.clone()),
            Entry {
                generation: grant.generation,
                grant: Some(grant),
            },
        );
        Ok(())
    }
    pub fn apply_revocation(&mut self, value: &Revocation) -> Result<(), String> {
        value.verify()?;
        self.check_next(
            value.network,
            value.genesis,
            &value.owner,
            &value.scope,
            value.generation,
        )?;
        self.entries.insert(
            (value.owner.clone(), value.scope.clone()),
            Entry {
                generation: value.generation,
                grant: None,
            },
        );
        Ok(())
    }
    pub fn active_grant(
        &self,
        owner: &WalletAddress,
        proposal: &TxHash,
        param: Option<&GovernableParam>,
    ) -> Option<&Grant> {
        // An explicit proposal tombstone blocks broader parameter authority.
        if let Some(entry) = self
            .entries
            .get(&(owner.clone(), KeyScope::Proposal(*proposal)))
        {
            return entry.grant.as_ref();
        }
        self.entries
            .get(&(owner.clone(), KeyScope::Parameter(param?.clone())))?
            .grant
            .as_ref()
    }

    pub fn generation(&self, owner: &WalletAddress, scope: &KeyScope) -> u64 {
        self.entries
            .get(&(owner.clone(), scope.clone()))
            .map_or(0, |entry| entry.generation)
    }

    /// The parameter must come from the ledger's proposal, never the sender.
    /// Verify only this typed ballot. There is no API that authorizes spending,
    /// representative changes, registration or revocation with a secondary key.
    pub fn verify_ballot(
        &self,
        ballot: &DelegatedBallot,
        param: Option<&GovernableParam>,
    ) -> Result<(), String> {
        let grant = self
            .active_grant(&ballot.owner, &ballot.proposal, param)
            .ok_or("no active scoped grant")?;
        grant.verify_candidate_ballot(ballot, param)
    }

    pub fn save(&self) -> Result<Vec<u8>, String> {
        let mut entries = self
            .entries
            .iter()
            .map(|((owner, scope), entry)| {
                Ok((
                    identity(self.network, self.genesis, owner, scope, 1)?,
                    owner,
                    scope,
                    entry,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let values: Vec<_> = entries
            .into_iter()
            .map(|(_, owner, scope, entry)| (owner, scope, entry))
            .collect();
        let bytes = bincode::serialize(&(1u8, self.network, self.genesis, values))
            .map_err(|e| e.to_string())?;
        if bytes.len() > MAX_SNAPSHOT_BYTES {
            return Err("delegation snapshot exceeds size limit".into());
        }
        Ok(bytes)
    }

    /// Restore trusted local state and verify active grant signatures. Tombstones
    /// store a generation without the revocation signature, so these bytes alone
    /// cannot prove revocation, ledger order or eligibility. Network state needs
    /// authenticated replay.
    pub fn restore(network: NetworkId, genesis: BlockHash, bytes: &[u8]) -> Result<Self, String> {
        Self::restore_using(network, genesis, bytes, Grant::verify)
    }

    fn restore_using(
        network: NetworkId,
        genesis: BlockHash,
        bytes: &[u8],
        mut verify: impl FnMut(&Grant) -> Result<(), String>,
    ) -> Result<Self, String> {
        if bytes.len() > MAX_SNAPSHOT_BYTES {
            return Err("oversized delegation snapshot".into());
        }
        type Snapshot = (
            u8,
            NetworkId,
            BlockHash,
            Vec<(WalletAddress, KeyScope, Entry)>,
        );
        let (version, stored_network, stored_genesis, entries): Snapshot =
            bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .with_limit(MAX_SNAPSHOT_BYTES as u64)
                .reject_trailing_bytes()
                .deserialize(bytes)
                .map_err(|e| e.to_string())?;
        if version != 1
            || stored_network != network
            || stored_genesis != genesis
            || entries.len() > MAX_ENTRIES
        {
            return Err("invalid delegation snapshot anchor or size".into());
        }
        let mut authority = Self::new(network, genesis)?;
        for (owner, scope, entry) in entries {
            identity(network, genesis, &owner, &scope, entry.generation)?;
            if let Some(grant) = &entry.grant {
                verify(grant)?;
                if grant.owner != owner
                    || grant.scope != scope
                    || grant.generation != entry.generation
                    || grant.network != network
                    || grant.genesis != genesis
                {
                    return Err("delegation snapshot entry mismatch".into());
                }
            }
            if authority.entries.insert((owner, scope), entry).is_some() {
                return Err("duplicate delegation snapshot entry".into());
            }
        }
        Ok(authority)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DelegatedBallot {
    pub owner: WalletAddress,
    pub proposal: TxHash,
    pub grant: BlockHash,
    pub vote: GovernanceVote,
    pub signature: Signature,
}
impl DelegatedBallot {
    /// Sign a candidate using the grant's secondary key. Proposal scopes are
    /// checked here; parameter scopes require the ledger's proposal at validation.
    /// Signing does not establish that the grant is still active.
    pub fn sign(
        grant: &Grant,
        key: &PrivateKey,
        proposal: TxHash,
        vote: GovernanceVote,
    ) -> Result<Self, String> {
        grant.verify()?;
        if burst_crypto::public_from_private(key) != grant.secondary {
            return Err("wrong secondary key".into());
        }
        if matches!(&grant.scope, KeyScope::Proposal(hash) if *hash != proposal) {
            return Err("proposal outside grant scope".into());
        }
        let mut ballot = Self {
            owner: grant.owner.clone(),
            proposal,
            grant: grant.id()?,
            vote,
            signature: Signature([0; 64]),
        };
        ballot.signature =
            burst_crypto::sign_message(&ballot.payload(grant.network, grant.genesis)?, key);
        Ok(ballot)
    }
    fn payload(&self, network: NetworkId, genesis: BlockHash) -> Result<Vec<u8>, String> {
        if self.proposal.is_zero() || self.grant.is_zero() {
            return Err("invalid ballot reference".into());
        }
        let key =
            burst_crypto::decode_address(self.owner.as_str()).ok_or("invalid ballot owner")?;
        let mut bytes = b"BURST/delegated-ballot/v1\0".to_vec();
        bytes.push(match network {
            NetworkId::Live => 0,
            NetworkId::Test => 1,
            NetworkId::Dev => 2,
        });
        bytes.extend_from_slice(genesis.as_bytes());
        bytes.extend_from_slice(&key);
        bytes.extend_from_slice(self.proposal.as_bytes());
        bytes.extend_from_slice(self.grant.as_bytes());
        bytes.push(match self.vote {
            GovernanceVote::Yea => 0,
            GovernanceVote::Nay => 1,
            GovernanceVote::Abstain => 2,
        });
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn keys(n: u8) -> KeyPair {
        burst_crypto::keypair_from_seed(&[n; 32])
    }
    fn genesis() -> BlockHash {
        BlockHash::new([7; 32])
    }
    fn proposal() -> TxHash {
        TxHash::new([8; 32])
    }
    fn grant(scope: KeyScope, generation: u64) -> Grant {
        Grant::create(
            &keys(1),
            keys(2).public,
            scope,
            generation,
            NetworkId::Dev,
            genesis(),
        )
        .unwrap()
    }

    #[test]
    fn only_the_named_delegate_can_import_an_owner_signed_key() {
        let grant = grant(KeyScope::Proposal(proposal()), 1);
        let restored = Grant::decode(&grant.encode().unwrap()).unwrap();
        assert_eq!(restored.id().unwrap(), grant.id().unwrap());
        let secret = restored.decrypt(&keys(2).private).unwrap();
        assert_eq!(burst_crypto::public_from_private(&secret), grant.secondary);
        assert!(restored.decrypt(&keys(3).private).is_err());
        for mutate in 0..8 {
            let mut bad = grant.clone();
            match mutate {
                0 => bad.owner = burst_crypto::derive_address(&keys(3).public),
                1 => bad.delegate = keys(3).public,
                2 => bad.secondary = keys(3).public,
                3 => bad.scope = KeyScope::Proposal(TxHash::new([9; 32])),
                4 => bad.generation += 1,
                5 => bad.network = NetworkId::Live,
                6 => bad.genesis = BlockHash::new([9; 32]),
                _ => bad.envelope[60] ^= 1,
            }
            assert!(bad.verify().is_err());
            assert!(bad.decrypt(&keys(2).private).is_err());
        }
        let mut encoded = grant.encode().unwrap();
        encoded.push(0);
        assert!(Grant::decode(&encoded).is_err());
        assert!(Grant::decode(&[0; 1025]).is_err());
    }

    #[test]
    fn primary_keys_and_unknown_envelope_versions_are_not_secondary_grants() {
        let original = grant(KeyScope::Proposal(proposal()), 1);
        for primary in [keys(1).public, keys(2).public] {
            let mut bad = original.clone();
            bad.secondary = primary;
            assert!(bad.signing_payload().is_err());
            assert!(bad.verify().is_err());
        }
        let mut bad = original.clone();
        bad.envelope[3] = 2;
        assert!(bad.signing_payload().is_err());
        assert!(bad.verify().is_err());
        let mut authority = DelegationAuthority::new(NetworkId::Dev, genesis()).unwrap();
        assert!(authority.apply_grant(bad).is_err());
        assert!(authority
            .active_grant(&original.owner, &proposal(), None)
            .is_none());
        authority.apply_grant(original).unwrap();
    }

    #[test]
    fn revoked_and_replaced_keys_cannot_reappear_after_restart() {
        let scope = KeyScope::Proposal(proposal());
        let old = grant(scope.clone(), 1);
        let key = old.decrypt(&keys(2).private).unwrap();
        let ballot = DelegatedBallot::sign(&old, &key, proposal(), GovernanceVote::Yea).unwrap();
        let mut authority = DelegationAuthority::new(NetworkId::Dev, genesis()).unwrap();
        authority.apply_grant(old.clone()).unwrap();
        authority.verify_ballot(&ballot, None).unwrap();
        assert!(authority.apply_grant(old.clone()).is_err());
        let revoke =
            Revocation::create(&keys(1), scope.clone(), 2, NetworkId::Dev, genesis()).unwrap();
        authority.apply_revocation(&revoke).unwrap();
        let mut restored =
            DelegationAuthority::restore(NetworkId::Dev, genesis(), &authority.save().unwrap())
                .unwrap();
        assert!(restored.verify_ballot(&ballot, None).is_err());
        assert!(restored.apply_grant(old).is_err());
        assert!(restored.apply_grant(grant(scope.clone(), 4)).is_err());
        restored.apply_grant(grant(scope, 3)).unwrap();
        assert!(restored.verify_ballot(&ballot, None).is_err());
        assert!(DelegationAuthority::restore(
            NetworkId::Live,
            genesis(),
            &restored.save().unwrap()
        )
        .is_err());
    }

    #[test]
    fn narrower_grant_and_revocation_override_parameter_authority() {
        let broad = grant(KeyScope::Parameter(GovernableParam::BrnRate), 1);
        let narrow = grant(KeyScope::Proposal(proposal()), 1);
        let broad_key = broad.decrypt(&keys(2).private).unwrap();
        let ballot =
            DelegatedBallot::sign(&broad, &broad_key, proposal(), GovernanceVote::Yea).unwrap();
        let mut authority = DelegationAuthority::new(NetworkId::Dev, genesis()).unwrap();
        authority.apply_grant(broad).unwrap();
        authority
            .verify_ballot(&ballot, Some(&GovernableParam::BrnRate))
            .unwrap();
        assert!(authority
            .verify_ballot(&ballot, Some(&GovernableParam::TrstExpirySecs))
            .is_err());
        assert!(authority.verify_ballot(&ballot, None).is_err());
        authority.apply_grant(narrow.clone()).unwrap();
        assert!(authority
            .verify_ballot(&ballot, Some(&GovernableParam::BrnRate))
            .is_err());
        let narrow_key = narrow.decrypt(&keys(2).private).unwrap();
        let specific =
            DelegatedBallot::sign(&narrow, &narrow_key, proposal(), GovernanceVote::Nay).unwrap();
        authority
            .verify_ballot(&specific, Some(&GovernableParam::BrnRate))
            .unwrap();
        assert!(DelegatedBallot::sign(
            &narrow,
            &narrow_key,
            TxHash::new([9; 32]),
            GovernanceVote::Yea
        )
        .is_err());
        authority
            .apply_revocation(
                &Revocation::create(
                    &keys(1),
                    KeyScope::Proposal(proposal()),
                    2,
                    NetworkId::Dev,
                    genesis(),
                )
                .unwrap(),
            )
            .unwrap();
        assert!(authority
            .verify_ballot(&ballot, Some(&GovernableParam::BrnRate))
            .is_err());
        assert!(authority
            .verify_ballot(&specific, Some(&GovernableParam::BrnRate))
            .is_err());
    }

    #[test]
    fn ballot_signature_cannot_be_reused_for_another_vote_owner_or_chain() {
        let grant = grant(KeyScope::Proposal(proposal()), 1);
        let key = grant.decrypt(&keys(2).private).unwrap();
        let ballot = DelegatedBallot::sign(&grant, &key, proposal(), GovernanceVote::Yea).unwrap();
        let mut authority = DelegationAuthority::new(NetworkId::Dev, genesis()).unwrap();
        authority.apply_grant(grant.clone()).unwrap();
        for mutate in 0..4 {
            let mut bad = ballot.clone();
            match mutate {
                0 => bad.owner = burst_crypto::derive_address(&keys(3).public),
                1 => bad.proposal = TxHash::new([9; 32]),
                2 => bad.grant = BlockHash::new([9; 32]),
                _ => bad.vote = GovernanceVote::Nay,
            }
            assert!(authority.verify_ballot(&bad, None).is_err());
        }
        assert!(!burst_crypto::verify_signature(
            b"spend all funds",
            &ballot.signature,
            &grant.secondary
        ));
        let mut foreign = DelegationAuthority::new(NetworkId::Live, genesis()).unwrap();
        assert!(foreign.apply_grant(grant).is_err());
        let mut forged = Revocation::create(
            &keys(3),
            KeyScope::Proposal(proposal()),
            2,
            NetworkId::Dev,
            genesis(),
        )
        .unwrap();
        forged.owner = ballot.owner;
        assert!(authority.apply_revocation(&forged).is_err());
    }

    #[test]
    fn snapshots_are_canonical_and_reject_duplicate_or_mismatched_entries() {
        let first = grant(KeyScope::Proposal(proposal()), 1);
        let second = grant(KeyScope::Parameter(GovernableParam::BrnRate), 1);
        let mut a = DelegationAuthority::new(NetworkId::Dev, genesis()).unwrap();
        a.apply_grant(first.clone()).unwrap();
        a.apply_grant(second.clone()).unwrap();
        let mut b = DelegationAuthority::new(NetworkId::Dev, genesis()).unwrap();
        b.apply_grant(second).unwrap();
        b.apply_grant(first.clone()).unwrap();
        assert_eq!(a.save().unwrap(), b.save().unwrap());
        let entry = (
            first.owner.clone(),
            first.scope.clone(),
            Entry {
                generation: 1,
                grant: Some(first.clone()),
            },
        );
        let duplicate = bincode::serialize(&(
            1u8,
            NetworkId::Dev,
            genesis(),
            vec![entry.clone(), entry.clone()],
        ))
        .unwrap();
        assert!(DelegationAuthority::restore(NetworkId::Dev, genesis(), &duplicate).is_err());
        let mut wrong = entry;
        wrong.2.generation = 2;
        let mismatch = bincode::serialize(&(1u8, NetworkId::Dev, genesis(), vec![wrong])).unwrap();
        assert!(DelegationAuthority::restore(NetworkId::Dev, genesis(), &mismatch).is_err());
        let mut trailing = a.save().unwrap();
        trailing.push(0);
        assert!(DelegationAuthority::restore(NetworkId::Dev, genesis(), &trailing).is_err());
        assert!(DelegationAuthority::restore(NetworkId::Dev, genesis(), b"corrupt").is_err());
    }
}
