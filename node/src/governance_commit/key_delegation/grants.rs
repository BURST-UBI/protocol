use super::{BlockHash, GovernableParam, HashMap, WalletAddress};
use burst_governance::signing_delegation::{scoped::AuthenticatedGrant, Grant};
use std::{cell::OnceCell, collections::hash_map::Entry};

// None inherits legacy delegation; Some(None) is an explicit scoped revocation.
// Keep both negative results, as well as authenticated grants, within this cut.
pub(super) type ScopedGrant = Option<Option<CutGrant>>;

#[derive(Clone)]
pub(super) enum CutGrant {
    Authenticated(std::sync::Arc<AuthenticatedGrant>),
    // Deliberately malformed loader fixtures still exercise lazy ID errors.
    #[cfg(test)]
    Raw(Grant),
}
impl CutGrant {
    pub(super) fn id(&self) -> Result<BlockHash, String> {
        match self {
            Self::Authenticated(grant) => Ok(grant.id()),
            #[cfg(test)]
            Self::Raw(grant) => grant.id(),
        }
    }
}
impl std::ops::Deref for CutGrant {
    type Target = Grant;
    fn deref(&self) -> &Grant {
        match self {
            Self::Authenticated(grant) => grant.grant(),
            #[cfg(test)]
            Self::Raw(grant) => grant,
        }
    }
}


struct CachedGrant {
    grant: ScopedGrant,
    recipient: OnceCell<WalletAddress>,
    id: OnceCell<Result<BlockHash, String>>,
}

pub(super) struct CutGrants<F> {
    values: HashMap<WalletAddress, CachedGrant>,
    load: F,
}

impl<F: FnMut(&WalletAddress) -> Result<ScopedGrant, String>> CutGrants<F> {
    pub(super) fn new(load: F) -> Self {
        Self { values: HashMap::new(), load }
    }

    fn entry(&mut self, owner: &WalletAddress) -> Result<&CachedGrant, String> {
        Ok(match self.values.entry(owner.clone()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(CachedGrant {
                grant: (self.load)(owner)?, recipient: OnceCell::new(), id: OnceCell::new(),
            }),
        })
    }

    pub(super) fn get(&mut self, owner: &WalletAddress) -> Result<&ScopedGrant, String> {
        Ok(&self.entry(owner)?.grant)
    }

    /// Cache recipient and grant ID derivation. An ineligible recipient stops
    /// traversal before the grant ID is evaluated.
    pub(super) fn hop(
        &mut self, owner: &WalletAddress, eligible: impl FnOnce(&WalletAddress) -> bool,
    ) -> Result<Option<(&WalletAddress, BlockHash)>, String> {
        let entry = self.entry(owner)?;
        let Some(Some(grant)) = &entry.grant else { return Ok(None) };
        let recipient = entry.recipient.get_or_init(|| {
            #[cfg(test)]
            let _span = super::tests::projection_span(2);
            burst_crypto::derive_address(&grant.delegate)
        });
        if !eligible(recipient) { return Ok(None) }
        let id = entry.id.get_or_init(|| {
            #[cfg(test)]
            let _span = super::tests::projection_span(3);
            grant.id()
        }).as_ref().map_err(Clone::clone)?;
        Ok(Some((recipient, *id)))
    }

    #[cfg(test)]
    pub(super) fn derived_counts(&self) -> (usize, usize) {
        (self.values.values().filter(|entry| entry.recipient.get().is_some()).count(),
         self.values.values().filter(|entry| entry.id.get().is_some()).count())
    }
}

/// Cache absent parameters in a bitset per owner. Explicit revocations remain
/// records and suppress legacy delegation. Reuse only within one immutable
/// projection; inserting conflicting results does not invalidate prior entries.
#[derive(Default)]
pub(super) struct ParameterCache {
    absent: HashMap<WalletAddress, [u64; PARAMETER_WORDS]>,
    records: HashMap<(WalletAddress, GovernableParam), Option<CutGrant>>,
}
const PARAMETER_WORDS: usize = GovernableParam::ALL.len().div_ceil(64);

impl ParameterCache {
    fn bit(param: &GovernableParam) -> (usize, u64) {
        // Fieldless, append-only variants have contiguous discriminants. The
        // exhaustive parameter test guards this internal (non-wire) indexing.
        let index = param.clone() as usize;
        (index / 64, 1u64 << (index % 64))
    }

    pub(super) fn get(&self, owner: &WalletAddress, param: &GovernableParam) -> Option<ScopedGrant> {
        let (word, mask) = Self::bit(param);
        if self.absent.get(owner).is_some_and(|bits| bits[word] & mask != 0) {
            return Some(None);
        }
        self.records.get(&(owner.clone(), param.clone())).cloned().map(Some)
    }

    pub(super) fn insert(&mut self, owner: &WalletAddress, param: &GovernableParam, value: ScopedGrant) {
        match value {
            None => {
                let (word, mask) = Self::bit(param);
                self.absent.entry(owner.clone()).or_insert([0; PARAMETER_WORDS])[word] |= mask;
            }
            Some(record) => { self.records.insert((owner.clone(), param.clone()), record); }
        }
    }

    #[cfg(test)]
    pub(super) fn negative_count(&self) -> usize {
        self.absent.values().flat_map(|bits| bits.iter()).map(|word| word.count_ones() as usize).sum()
    }
    #[cfg(test)]
    pub(super) fn len(&self) -> usize { self.negative_count() + self.records.len() }
    #[cfg(test)]
    pub(super) fn storage_counts(&self) -> (usize, usize, usize, usize) {
        (self.absent.len(), self.absent.capacity(), self.records.len(), self.records.capacity())
    }
}
