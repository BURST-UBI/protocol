//! Arrival-ordered votes with a persistent lookup used by admission and row deltas.
use crate::state::VerifierVote;
use burst_types::WalletAddress;
use imbl::{OrdMap, Vector};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{
    ops::{Index, IndexMut},
    sync::OnceLock,
};

/// Votes in arrival order. The sequence permits duplicate verifiers, but
/// `indexed` rejects them; admission must enforce one vote per verifier.
/// Snapshot serializers may sort this sequence separately for canonical bytes.
#[derive(Clone, Debug)]
pub struct Votes {
    sequence: Vector<VerifierVote>,
    // None records duplicate identities, which normalized storage must reject.
    index: OnceLock<Option<OrdMap<WalletAddress, VerifierVote>>>,
}
impl Default for Votes {
    fn default() -> Self {
        Self {
            sequence: Vector::new(),
            index: OnceLock::from(Some(OrdMap::new())),
        }
    }
}
impl Votes {
    pub fn len(&self) -> usize {
        self.sequence.len()
    }
    pub fn is_empty(&self) -> bool {
        self.sequence.is_empty()
    }
    pub fn iter(&self) -> <&Vector<VerifierVote> as IntoIterator>::IntoIter {
        self.sequence.iter()
    }
    pub fn push(&mut self, vote: VerifierVote) {
        if let Some(index) = self.index.get_mut() {
            if let Some(values) = index {
                if values.contains_key(&vote.verifier) {
                    *index = None;
                } else {
                    values.insert(vote.verifier.clone(), vote.clone());
                }
            }
        }
        self.sequence.push_back(vote);
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn truncate(&mut self, len: usize) {
        if len < self.len() {
            self.index.take();
            self.sequence.truncate(len);
        }
    }
    pub fn reverse(&mut self) {
        self.sequence = self.sequence.iter().rev().cloned().collect();
    }
    pub fn last_mut(&mut self) -> Option<&mut VerifierVote> {
        self.index.take();
        self.sequence.back_mut()
    }
    pub fn contains_verifier(&self, verifier: &WalletAddress) -> bool {
        match self.indexed() {
            Ok(index) => index.contains_key(verifier),
            Err(_) => self.iter().any(|vote| &vote.verifier == verifier),
        }
    }
    pub(crate) fn indexed(&self) -> Result<&OrdMap<WalletAddress, VerifierVote>, String> {
        self.index
            .get_or_init(|| {
                let mut index = OrdMap::new();
                for vote in self.iter() {
                    if index.insert(vote.verifier.clone(), vote.clone()).is_some() {
                        return None;
                    }
                }
                Some(index)
            })
            .as_ref()
            .ok_or_else(|| "duplicate normalized verification vote identity".into())
    }
}
impl Index<usize> for Votes {
    type Output = VerifierVote;
    fn index(&self, index: usize) -> &Self::Output {
        &self.sequence[index]
    }
}
impl IndexMut<usize> for Votes {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        self.index.take();
        &mut self.sequence[index]
    }
}
impl FromIterator<VerifierVote> for Votes {
    fn from_iter<I: IntoIterator<Item = VerifierVote>>(iter: I) -> Self {
        Self {
            sequence: iter.into_iter().collect(),
            index: OnceLock::new(),
        }
    }
}
impl From<Vec<VerifierVote>> for Votes {
    fn from(values: Vec<VerifierVote>) -> Self {
        values.into_iter().collect()
    }
}
impl<'a> IntoIterator for &'a Votes {
    type Item = &'a VerifierVote;
    type IntoIter = <&'a Vector<VerifierVote> as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl IntoIterator for Votes {
    type Item = VerifierVote;
    type IntoIter = <Vector<VerifierVote> as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter {
        self.sequence.into_iter()
    }
}
impl Serialize for Votes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}
impl<'de> Deserialize<'de> for Votes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vector::<VerifierVote>::deserialize(deserializer).map(|sequence| Self {
            sequence,
            index: OnceLock::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vote;
    use burst_types::Timestamp;
    fn vote(n: usize) -> VerifierVote {
        VerifierVote {
            verifier: WalletAddress::new(format!("brst_vote_{n:06}")),
            vote: Vote::Legitimate,
            stake_amount: n as u128,
            timestamp: Timestamp::new(n as u64),
        }
    }
    #[test]
    fn persistent_votes_preserve_order_wire_bytes_and_mutation_indexes() {
        for count in [128, 1024, 10_000] {
            let original: Vec<_> = (0..count).rev().map(vote).collect();
            let before: Votes = original.clone().into();
            assert_eq!(
                bincode::serialize(&before).unwrap(),
                bincode::serialize(&original).unwrap()
            );
            let decoded: Votes =
                bincode::deserialize(&bincode::serialize(&original).unwrap()).unwrap();
            assert_eq!(decoded.iter().cloned().collect::<Vec<_>>(), original);
            let index = before.indexed().unwrap();
            let mut after = before.clone();
            assert!(index.ptr_eq(after.indexed().unwrap()));
            after.push(vote(count));
            assert_eq!(index.diff(after.indexed().unwrap()).count(), 1);
            assert!(!before.contains_verifier(&vote(count).verifier));
            assert!(after.contains_verifier(&vote(count).verifier));
            after[0].verifier = vote(count + 1).verifier;
            assert!(!after.contains_verifier(&vote(count - 1).verifier));
            assert!(after.contains_verifier(&vote(count + 1).verifier));
            after.last_mut().unwrap().stake_amount = 17;
            assert_eq!(
                after.indexed().unwrap()[&vote(count).verifier].stake_amount,
                17
            );
            after.reverse();
            assert_eq!(after[0].verifier, vote(count).verifier);
            after.truncate(1);
            assert_eq!(after.indexed().unwrap().len(), 1);
            after.push(after[0].clone());
            assert!(after.indexed().is_err());
            assert!(after.contains_verifier(&vote(count).verifier));
            after.truncate(1);
            assert_eq!(after.indexed().unwrap().len(), 1);
            after.clear();
            assert!(after.indexed().unwrap().is_empty());
            assert_eq!(before.iter().cloned().collect::<Vec<_>>(), original);
        }
    }
}
