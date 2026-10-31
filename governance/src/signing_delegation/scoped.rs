//! One bounded durable record per owner and scope, including revocation tombstones.

use super::*;

mod index;
pub mod retention;
pub use index::RecordIndex;

pub const PREFIX: &str = "key_delegation/v2/";
pub const MAX_SCOPE_BYTES: usize = 2048;

pub fn key(
    network: NetworkId,
    genesis: BlockHash,
    owner: &WalletAddress,
    scope: &KeyScope,
) -> Result<String, String> {
    OwnerKeys::new(network, genesis, owner)?.key(scope)
}

/// Validated chain/owner bytes reused for a sequence of scoped lookups. This
/// derives storage keys only; it supplies no grant or ballot authority.
pub struct OwnerKeys {
    identity: Vec<u8>,
}

impl OwnerKeys {
    pub fn new(network: NetworkId, genesis: BlockHash, owner: &WalletAddress) -> Result<Self, String> {
        Ok(Self { identity: owner_identity(network, genesis, owner)? })
    }

    /// Validate the remaining key input when a complete record index supplies
    /// the lookup. The owner and chain were checked by this builder's constructor.
    pub fn validate_scope(&self, scope: &KeyScope) -> Result<(), String> {
        scope.validate()
    }

    pub fn key(&self, scope: &KeyScope) -> Result<String, String> {
        let mut identity = Vec::with_capacity(self.identity.len() + 64);
        identity.extend_from_slice(&self.identity);
        scope.encode(&mut identity)?;
        identity.extend_from_slice(&1u64.to_be_bytes());
        let digest = burst_crypto::blake2b_256_multi(&[
            b"BURST/delegation-scope/v1\0", &identity,
        ]);
        Ok(format!("{PREFIX}{}", BlockHash::new(digest)))
    }
}

pub fn restore(
    network: NetworkId,
    genesis: BlockHash,
    owner: &WalletAddress,
    scope: &KeyScope,
    bytes: &[u8],
) -> Result<DelegationAuthority, String> {
    if bytes.len() > MAX_SCOPE_BYTES {
        return Err("oversized delegation scope record".into());
    }
    let value = DelegationAuthority::restore(network, genesis, bytes)?;
    if value.entries.len() != 1 || !value.entries.contains_key(&(owner.clone(), scope.clone())) {
        return Err("delegation scope record belongs to another owner or scope".into());
    }
    Ok(value)
}

/// A restored grant and the identity of the exact payload whose signature was
/// verified. Read-only access prevents field edits from invalidating that identity.
#[derive(Clone, Debug)]
pub struct AuthenticatedGrant {
    grant: Grant,
    id: BlockHash,
}

impl AuthenticatedGrant {
    pub fn grant(&self) -> &Grant { &self.grant }
    pub fn id(&self) -> BlockHash { self.id }
}

pub fn restore_authenticated_grant(
    network: NetworkId,
    genesis: BlockHash,
    owner: &WalletAddress,
    scope: &KeyScope,
    bytes: &[u8],
) -> Result<Option<AuthenticatedGrant>, String> {
    if bytes.len() > MAX_SCOPE_BYTES {
        return Err("oversized delegation scope record".into());
    }
    let mut authenticated = None;
    let value = DelegationAuthority::restore_using(network, genesis, bytes, |grant| {
        let payload = grant.verified_payload()?;
        authenticated = Some(AuthenticatedGrant {
            grant: grant.clone(),
            id: BlockHash::new(burst_crypto::blake2b_256(&payload)),
        });
        Ok(())
    })?;
    if value.entries.len() != 1 || !value.entries.contains_key(&(owner.clone(), scope.clone())) {
        return Err("delegation scope record belongs to another owner or scope".into());
    }
    Ok(authenticated)
}

pub fn grant(
    authority: &DelegationAuthority,
    owner: &WalletAddress,
    scope: &KeyScope,
) -> Option<Grant> {
    authority
        .entries
        .get(&(owner.clone(), scope.clone()))?
        .grant
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_keys_match_legacy_bytes_and_identity_error_precedence() {
        fn legacy(network: NetworkId, genesis: BlockHash, owner: &WalletAddress,
            scope: &KeyScope) -> Result<String, String> {
            if genesis.is_zero() { return Err("invalid delegation identity".into()); }
            let decoded = burst_crypto::decode_address(owner.as_str()).ok_or("invalid delegation owner")?;
            let mut bytes = vec![match network { NetworkId::Live => 0, NetworkId::Test => 1, NetworkId::Dev => 2 }];
            bytes.extend_from_slice(genesis.as_bytes());
            bytes.extend_from_slice(&decoded);
            scope.encode(&mut bytes)?;
            bytes.extend_from_slice(&1u64.to_be_bytes());
            Ok(format!("{PREFIX}{}", BlockHash::new(burst_crypto::blake2b_256_multi(
                &[b"BURST/delegation-scope/v1\0", &bytes]))))
        }
        let owner = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[19;32]).public);
        let scopes = [KeyScope::Proposal(TxHash::new([21;32])),
            KeyScope::Parameter(GovernableParam::BrnRate), KeyScope::Proposal(TxHash::ZERO)];
        for network in [NetworkId::Live, NetworkId::Test, NetworkId::Dev] {
            for genesis in [BlockHash::ZERO, BlockHash::new([23;32])] {
                for person in [&owner, &WalletAddress::new("brst_invalid")] {
                    for scope in &scopes {
                        let expected = legacy(network, genesis, person, scope);
                        assert_eq!(key(network, genesis, person, scope), expected);
                        assert_eq!(OwnerKeys::new(network, genesis, person).and_then(|keys| keys.key(scope)), expected);
                    }
                }
            }
        }
        let keys = OwnerKeys::new(NetworkId::Dev, BlockHash::new([23;32]), &owner).unwrap();
        for _ in 0..3 {
            for scope in &scopes {
                assert_eq!(keys.key(scope), legacy(NetworkId::Dev, BlockHash::new([23;32]), &owner, scope));
            }
        }
    }

    #[test]
    fn authenticated_identity_matches_legacy_and_rejects_corrupt_scope_records() {
        let owner_key = burst_crypto::keypair_from_seed(&[11; 32]);
        let delegate = burst_crypto::keypair_from_seed(&[12; 32]);
        let owner = burst_crypto::derive_address(&owner_key.public);
        let genesis = BlockHash::new([13; 32]);
        for scope in [KeyScope::Parameter(GovernableParam::BrnRate),
            KeyScope::Proposal(TxHash::new([14; 32]))] {
            let mut authority = DelegationAuthority::new(NetworkId::Dev, genesis).unwrap();
            let original = Grant::create(&owner_key, delegate.public.clone(), scope.clone(),
                1, NetworkId::Dev, genesis).unwrap();
            authority.apply_grant(original.clone()).unwrap();
            let bytes = authority.save().unwrap();
            let authenticated = restore_authenticated_grant(NetworkId::Dev, genesis,
                &owner, &scope, &bytes).unwrap().unwrap();
            assert_eq!(authenticated.id(), original.id().unwrap());
            assert_eq!(bincode::serialize(authenticated.grant()).unwrap(),
                bincode::serialize(&original).unwrap());
            assert_eq!(restore(NetworkId::Dev, genesis, &owner, &scope, &bytes)
                .unwrap().save().unwrap(), bytes);
            let mut detached = authenticated.grant().clone();
            detached.generation += 1;
            assert_ne!(detached.id().unwrap(), authenticated.id());
            assert_eq!(authenticated.id(), original.id().unwrap());
            for fault in 0..8 {
                let mut corrupted = authority.clone();
                let grant = corrupted.entries.get_mut(&(owner.clone(), scope.clone()))
                    .unwrap().grant.as_mut().unwrap();
                match fault {
                    0 => grant.signature.0[0] ^= 1,
                    1 => grant.genesis = BlockHash::ZERO,
                    2 => grant.owner = burst_crypto::derive_address(&delegate.public),
                    3 => grant.generation += 1,
                    4 => grant.envelope.clear(),
                    5 => grant.secondary = grant.delegate.clone(),
                    6 => grant.network = NetworkId::Live,
                    _ => grant.scope = KeyScope::Proposal(TxHash::ZERO),
                }
                let bytes = corrupted.save().unwrap();
                let legacy = restore(NetworkId::Dev, genesis, &owner, &scope, &bytes).err();
                assert!(legacy.is_some());
                assert_eq!(restore_authenticated_grant(NetworkId::Dev, genesis,
                    &owner, &scope, &bytes).err(), legacy);
            }
            for (network, chain, person, selected_scope) in [
                (NetworkId::Live, genesis, owner.clone(), scope.clone()),
                (NetworkId::Dev, BlockHash::new([99;32]), owner.clone(), scope.clone()),
                (NetworkId::Dev, genesis, burst_crypto::derive_address(&delegate.public), scope.clone()),
                (NetworkId::Dev, genesis, owner.clone(), KeyScope::Proposal(TxHash::new([99;32]))),
            ] {
                let legacy = restore(network, chain, &person, &selected_scope, &bytes).err();
                assert!(legacy.is_some());
                assert_eq!(restore_authenticated_grant(network, chain, &person, &selected_scope, &bytes).err(), legacy);
            }
            let mut trailing = bytes.clone(); trailing.push(0);
            for invalid in [&trailing[..], &[0xff][..], &vec![0;MAX_SCOPE_BYTES + 1][..]] {
                assert_eq!(restore_authenticated_grant(NetworkId::Dev, genesis, &owner,
                    &scope, invalid).err(), restore(NetworkId::Dev, genesis, &owner, &scope, invalid).err());
            }
            authority.apply_revocation(&Revocation::create(&owner_key, scope.clone(),
                2, NetworkId::Dev, genesis).unwrap()).unwrap();
            assert!(restore_authenticated_grant(NetworkId::Dev, genesis, &owner,
                &scope, &authority.save().unwrap()).unwrap().is_none());
        }
    }

    #[test]
    fn scope_records_bind_owner_scope_chain_and_preserve_tombstones() {
        let owner = burst_crypto::keypair_from_seed(&[1; 32]);
        let recipient = burst_crypto::keypair_from_seed(&[2; 32]);
        let address = burst_crypto::derive_address(&owner.public);
        let other = burst_crypto::derive_address(&recipient.public);
        let genesis = BlockHash::new([3; 32]);
        let scope = KeyScope::Parameter(GovernableParam::BrnRate);
        let narrower = KeyScope::Proposal(TxHash::new([4; 32]));
        let mut authority = DelegationAuthority::new(NetworkId::Dev, genesis).unwrap();
        authority
            .apply_grant(
                Grant::create(
                    &owner,
                    recipient.public.clone(),
                    scope.clone(),
                    1,
                    NetworkId::Dev,
                    genesis,
                )
                .unwrap(),
            )
            .unwrap();
        let bytes = authority.save().unwrap();
        assert!(bytes.len() <= MAX_SCOPE_BYTES);
        let restored = restore(NetworkId::Dev, genesis, &address, &scope, &bytes).unwrap();
        assert!(grant(&restored, &address, &scope).is_some());
        assert!(restore(NetworkId::Dev, genesis, &other, &scope, &bytes).is_err());
        assert!(restore(NetworkId::Dev, genesis, &address, &narrower, &bytes).is_err());
        assert!(restore(NetworkId::Live, genesis, &address, &scope, &bytes).is_err());
        assert!(restore(
            NetworkId::Dev,
            BlockHash::new([5; 32]),
            &address,
            &scope,
            &bytes
        )
        .is_err());
        assert!(restore(
            NetworkId::Dev,
            genesis,
            &address,
            &scope,
            &[0; MAX_SCOPE_BYTES + 1]
        )
        .is_err());
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(restore(NetworkId::Dev, genesis, &address, &scope, &trailing).is_err());
        authority
            .apply_revocation(
                &Revocation::create(&owner, scope.clone(), 2, NetworkId::Dev, genesis).unwrap(),
            )
            .unwrap();
        let restored = restore(
            NetworkId::Dev,
            genesis,
            &address,
            &scope,
            &authority.save().unwrap(),
        )
        .unwrap();
        assert!(grant(&restored, &address, &scope).is_none());
        assert_eq!(restored.generation(&address, &scope), 2);
        authority
            .apply_grant(
                Grant::create(
                    &owner,
                    recipient.public,
                    narrower.clone(),
                    1,
                    NetworkId::Dev,
                    genesis,
                )
                .unwrap(),
            )
            .unwrap();
        assert!(restore(
            NetworkId::Dev,
            genesis,
            &address,
            &scope,
            &authority.save().unwrap()
        )
        .is_err());
        assert_ne!(
            key(NetworkId::Dev, genesis, &address, &scope).unwrap(),
            key(NetworkId::Dev, genesis, &address, &narrower).unwrap()
        );
    }
}
