//! Scoped authority and discovery references. Tombstones still bind their owner,
//! scope and generation; no reference list replaces those persisted records.
use super::*;
use crate::record_references::Reference;

fn hash_suffix(value: &str) -> Result<BlockHash, String> {
    let mut bytes = [0;32];
    hex::decode_to_slice(value, &mut bytes).map_err(|_| "invalid delegation scope key")?;
    let hash = BlockHash::new(bytes);
    if hash.is_zero() || value != hash.to_string() { return Err("noncanonical delegation scope key".into()); }
    Ok(hash)
}

/// One bounded record from a recovered cut. Active grant signatures are verified
/// by the existing decoder; ledger order and tombstone authority still need replay.
pub fn visit_references(
    network: NetworkId, genesis: BlockHash, stored_key: &str, bytes: &[u8],
    mut visit: impl FnMut(Reference<'_>) -> Result<(), String>,
) -> Result<(), String> {
    if genesis.is_zero() { return Err("zero delegation genesis".into()); }
    let suffix = stored_key.strip_prefix(PREFIX).ok_or("unsupported delegation storage schema")?;
    for (prefix, direction) in [("incoming/", view::Direction::Incoming), ("outgoing/", view::Direction::Outgoing)] {
        if let Some(rest) = suffix.strip_prefix(prefix) {
            let (owner, encoded_scope) = rest.split_once('/').ok_or("invalid delegation discovery key")?;
            let account = WalletAddress::new(owner);
            let scope = hash_suffix(encoded_scope)?;
            if stored_key != view::index_key(direction, &account, &format!("{PREFIX}{scope}"))? {
                return Err("noncanonical delegation discovery key".into());
            }
            let source = BlockHash::new(bytes.try_into().map_err(|_| "invalid delegation discovery source")?);
            if source.is_zero() && matches!(direction, view::Direction::Outgoing) {
                return Err("outgoing delegation discovery has no signed source".into());
            }
            visit(Reference::Account(&account))?;
            visit(Reference::DelegationScope(scope))?;
            if !source.is_zero() { visit(Reference::Block(source))?; }
            return Ok(());
        }
    }
    let digest = hash_suffix(suffix)?;
    if bytes.len() > MAX_SCOPE_BYTES { return Err("oversized delegation scope record".into()); }
    let mut grant_id = None;
    let authority = DelegationAuthority::restore_using(network, genesis, bytes, |grant| {
        grant_id = Some(BlockHash::new(burst_crypto::blake2b_256(&grant.verified_payload()?)));
        Ok(())
    })?;
    if authority.entries.len() != 1 { return Err("delegation retention requires one scoped entry".into()); }
    let ((owner, scope), entry) = authority.entries.iter().next().unwrap();
    if key(network, genesis, owner, scope)? != stored_key { return Err("delegation scope key mismatch".into()); }
    visit(Reference::DelegationScope(digest))?;
    visit(Reference::Genesis(genesis))?;
    visit(Reference::Account(owner))?;
    if let KeyScope::Proposal(proposal) = scope { visit(Reference::Proposal(*proposal))?; }
    if let Some(grant) = &entry.grant {
        let recipient = burst_crypto::derive_address(&grant.delegate);
        visit(Reference::Account(&recipient))?;
        visit(Reference::Grant(grant_id.ok_or("missing verified grant identity")?))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scoped_roots_verify_grants_retain_tombstones_and_distinguish_carriers() {
        let owner = burst_crypto::keypair_from_seed(&[71;32]);
        let recipient = burst_crypto::keypair_from_seed(&[72;32]);
        let account = burst_crypto::derive_address(&owner.public);
        let delegate = burst_crypto::derive_address(&recipient.public);
        let genesis = BlockHash::new([73;32]);
        for scope in [KeyScope::Proposal(TxHash::new([74;32])), KeyScope::Parameter(GovernableParam::BrnRate)] {
            let storage_key = key(NetworkId::Dev, genesis, &account, &scope).unwrap();
            let digest = hash_suffix(storage_key.strip_prefix(PREFIX).unwrap()).unwrap();
            let grant = Grant::create(&owner, recipient.public.clone(), scope.clone(), 1, NetworkId::Dev, genesis).unwrap();
            let grant_id = grant.id().unwrap();
            let mut authority = DelegationAuthority::new(NetworkId::Dev, genesis).unwrap();
            authority.apply_grant(grant).unwrap();
            let mut forged = authority.clone();
            forged.entries.get_mut(&(account.clone(),scope.clone())).unwrap().grant.as_mut().unwrap().signature.0[0] ^= 1;
            assert!(visit_references(NetworkId::Dev,genesis,&storage_key,&forged.save().unwrap(), |_| panic!("forged signature emitted roots")).is_err());
            for revoked in [false, true] {
                if revoked { authority.apply_revocation(&Revocation::create(&owner, scope.clone(), 2, NetworkId::Dev, genesis).unwrap()).unwrap(); }
                let bytes = authority.save().unwrap();
                let mut expected = vec![format!("{:?}", Reference::DelegationScope(digest)), format!("{:?}", Reference::Genesis(genesis)), format!("{:?}", Reference::Account(&account))];
                if let KeyScope::Proposal(hash) = scope { expected.push(format!("{:?}", Reference::Proposal(hash))); }
                if !revoked { expected.extend([format!("{:?}", Reference::Account(&delegate)), format!("{:?}", Reference::Grant(grant_id))]); }
                let mut actual = Vec::new();
                visit_references(NetworkId::Dev, genesis, &storage_key, &bytes, |r| { actual.push(format!("{r:?}")); Ok(()) }).unwrap();
                assert_eq!(actual, expected);
                for stop in 1..=expected.len() {
                    let mut calls = 0;
                    assert!(visit_references(NetworkId::Dev, genesis, &storage_key, &bytes, |_| {
                        calls += 1; if calls == stop { Err("stop".into()) } else { Ok(()) }
                    }).is_err()); assert_eq!(calls,stop);
                }
                let mut trailing = bytes.clone(); trailing.push(0);
                for (network, chain, key, payload) in [
                    (NetworkId::Test, genesis, storage_key.clone(), bytes.clone()),
                    (NetworkId::Dev, BlockHash::new([75;32]), storage_key.clone(), bytes.clone()),
                    (NetworkId::Dev, genesis, format!("{PREFIX}{}", BlockHash::new([76;32])), bytes.clone()),
                    (NetworkId::Dev, genesis, storage_key.clone(), trailing),
                    (NetworkId::Dev, genesis, storage_key.clone(), vec![0;MAX_SCOPE_BYTES+1]),
                ] { assert!(visit_references(network,chain,&key,&payload, |_| panic!("invalid record emitted roots")).is_err()); }
            }
            let incoming = view::index_key(view::Direction::Incoming,&delegate,&storage_key).unwrap();
            let outgoing = view::index_key(view::Direction::Outgoing,&account,&storage_key).unwrap();
            for (key, source, expected_account) in [(&incoming,BlockHash::ZERO,&delegate),(&incoming,BlockHash::new([77;32]),&delegate),(&outgoing,BlockHash::new([78;32]),&account)] {
                let mut actual = Vec::new();
                visit_references(NetworkId::Dev,genesis,key,source.as_bytes(),|r| { actual.push(format!("{r:?}")); Ok(()) }).unwrap();
                let mut expected = vec![format!("{:?}",Reference::Account(expected_account)),format!("{:?}",Reference::DelegationScope(digest))];
                if !source.is_zero() { expected.push(format!("{:?}",Reference::Block(source))); }
                assert_eq!(actual,expected);
            }
            assert!(visit_references(NetworkId::Dev,genesis,&outgoing,BlockHash::ZERO.as_bytes(), |_| panic!("zero outgoing source")).is_err());
        }
    }
}
