//! Optional lookup acceleration, never grant authentication.
use super::*;

/// Lookup index for one immutable metadata cut. The caller must supply every
/// record; this builder cannot detect an omitted row. Bytes stay borrowed and
/// each hit still requires authentication. Returns None on malformed records or
/// exhausted budget so the caller can use the original keyed lookup.
pub struct RecordIndex<'a> {
    owners: HashMap<WalletAddress, HashMap<KeyScope, &'a [u8]>>,
}

impl<'a> RecordIndex<'a> {
    pub const MAX_ROWS: usize = 16_384;
    const MAX_BYTES: usize = 8 << 20;

    pub fn build(
        network: NetworkId, genesis: BlockHash,
        records: impl IntoIterator<Item = (&'a str, &'a [u8])>,
    ) -> Option<Self> {
        Self::build_with_limit(network, genesis, records, Self::MAX_BYTES)
    }

    fn build_with_limit(
        network: NetworkId, genesis: BlockHash,
        records: impl IntoIterator<Item = (&'a str, &'a [u8])>, limit: usize,
    ) -> Option<Self> {
        let mut index = Self { owners: HashMap::new() };
        let mut used = 0usize;
        for (number, (stored_key, bytes)) in records.into_iter().enumerate() {
            if number >= Self::MAX_ROWS { return None }
            let Some(suffix) = stored_key.strip_prefix(PREFIX) else { continue };
            // The key encoder emits exactly 64 lowercase hex characters.
            // Discovery entries and other unreachable strings are not records.
            if suffix.len() != 64 || !suffix.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
                continue;
            }
            if bytes.len() > MAX_SCOPE_BYTES { return None }
            // Structural inspection only. A malformed signature remains a hit
            // and fails at the original lazy authentication point.
            let value = DelegationAuthority::restore_using(network, genesis, bytes, |_| Ok(())).ok()?;
            if value.entries.len() != 1 { return None }
            let ((owner, scope), _) = value.entries.into_iter().next()?;
            if key(network, genesis, &owner, &scope).ok()?.as_str() != stored_key { return None }
            // Conservative retained-work budget includes borrowed payload bytes
            // as well as an allowance for owned keys and hash-table storage.
            used = used.checked_add(bytes.len())?.checked_add(owner.as_str().len())?
                .checked_add(256 + std::mem::size_of::<KeyScope>())?;
            if used > limit { return None }
            if index.owners.entry(owner).or_default().insert(scope, bytes).is_some() { return None }
        }
        Some(index)
    }

    /// Only use after validating owner/chain and scope, preserving their errors.
    /// A returned record has not been authenticated; absence is only about this
    /// exact complete metadata cut, not historical authority or future state.
    pub fn get(&self, owner: &WalletAddress, scope: &KeyScope) -> Option<&'a [u8]> {
        self.owners.get(owner)?.get(scope).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_preserves_hits_tombstones_and_authentication_errors() {
        let owner_key = burst_crypto::keypair_from_seed(&[81; 32]);
        let recipient = burst_crypto::keypair_from_seed(&[82; 32]);
        let owner = burst_crypto::derive_address(&owner_key.public);
        let genesis = BlockHash::new([83; 32]);
        let network = NetworkId::Dev;
        let scope = KeyScope::Parameter(GovernableParam::BrnRate);
        let storage_key = key(network, genesis, &owner, &scope).unwrap();
        let grant = Grant::create(&owner_key, recipient.public, scope.clone(), 1, network, genesis).unwrap();
        let mut authority = DelegationAuthority::new(network, genesis).unwrap();
        authority.apply_grant(grant).unwrap();
        for state in 0..3 {
            if state == 1 {
                authority.entries.get_mut(&(owner.clone(), scope.clone())).unwrap()
                    .grant.as_mut().unwrap().generation += 1;
                // Keep structural fields consistent while invalidating its signature.
                authority.entries.get_mut(&(owner.clone(), scope.clone())).unwrap().generation += 1;
            } else if state == 2 {
                authority.entries.get_mut(&(owner.clone(), scope.clone())).unwrap().grant = None;
            }
            let bytes = authority.save().unwrap();
            let index = RecordIndex::build(network, genesis, [(storage_key.as_str(), bytes.as_slice())]).unwrap();
            assert_eq!(index.get(&owner, &scope), Some(bytes.as_slice()));
            assert!(index.get(&owner, &KeyScope::Proposal(TxHash::new([84; 32]))).is_none());
            let restored = restore_authenticated_grant(network, genesis, &owner, &scope, index.get(&owner, &scope).unwrap());
            match state {
                0 => assert!(restored.unwrap().is_some()),
                1 => assert!(restored.is_err()),
                _ => assert!(restored.unwrap().is_none()),
            }
            assert!(RecordIndex::build_with_limit(network, genesis,
                [(storage_key.as_str(), bytes.as_slice())], 0).is_none());
            let wrong_key = key(network, genesis, &owner, &KeyScope::Proposal(TxHash::new([85; 32]))).unwrap();
            assert!(RecordIndex::build(network, genesis, [(wrong_key.as_str(), bytes.as_slice())]).is_none());
            assert!(RecordIndex::build(NetworkId::Test, genesis, [(storage_key.as_str(), bytes.as_slice())]).is_none());
            let mut trailing = bytes.clone(); trailing.push(0);
            assert!(RecordIndex::build(network, genesis, [(storage_key.as_str(), trailing.as_slice())]).is_none());
        }
        assert!(RecordIndex::build(network, genesis, [(storage_key.as_str(), &[0xff][..])]).is_none());
        assert!(RecordIndex::build(network, genesis, std::iter::repeat(("unrelated", &[][..]))
            .take(RecordIndex::MAX_ROWS + 1)).is_none());
        let index = RecordIndex::build(network, genesis, [("key_delegation/v2/outgoing/untrusted", &[0xff][..])]).unwrap();
        assert!(index.get(&owner, &scope).is_none());
    }
}
