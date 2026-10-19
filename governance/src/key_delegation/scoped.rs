//! One bounded durable record per owner and scope, including revocation tombstones.

use super::*;

pub const PREFIX: &str = "key_delegation/v2/";
pub const MAX_SCOPE_BYTES: usize = 2048;

pub fn key(
    network: NetworkId,
    genesis: BlockHash,
    owner: &WalletAddress,
    scope: &KeyScope,
) -> Result<String, String> {
    let digest = burst_crypto::blake2b_256_multi(&[
        b"BURST/delegation-scope/v1\0",
        &identity(network, genesis, owner, scope, 1)?,
    ]);
    Ok(format!("{PREFIX}{}", BlockHash::new(digest)))
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
