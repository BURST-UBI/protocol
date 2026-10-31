//! Certificates for agreed ledger cuts. Signatures authenticate a precommit
//! round; they cannot be assembled from prevotes or mixed rounds.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use burst_crypto::{blake2b_256, decode_address, Blake2b256Hasher};
use burst_types::{BlockHash, NetworkId, Signature, WalletAddress};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const DOMAIN: &[u8] = b"BURST/checkpoint/v1\0";
const MAX_HUMANS: usize = 1_000_000;

mod signature_cache;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CheckpointError {
    #[error("invalid verified-human roster")]
    InvalidRoster,
    #[error("checkpoint quorum must be between 6667 and 10000 basis points")]
    InvalidQuorum,
    #[error("checkpoint does not extend the trusted parent")]
    WrongParent,
    #[error("invalid checkpoint commitments")]
    InvalidCommitment,
    #[error("duplicate, unknown or invalid checkpoint signer")]
    InvalidSigner,
    #[error("checkpoint has insufficient certified human weight")]
    InsufficientQuorum,
}

/// One entry per verified human. There is deliberately no token balance or
/// caller-supplied weight. A delegated human still contributes exactly one unit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HumanDelegation {
    pub human: WalletAddress,
    pub representative: WalletAddress,
}

/// A canonical roster whose authority must be established by the caller, through
/// configured genesis, certified replay, or a certified snapshot roster witness.
/// Construction validates addresses and uniqueness of entries, not humanity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HumanRoster {
    data: Arc<RosterData>,
}

#[derive(Debug, PartialEq, Eq)]
struct RosterData {
    humans: Vec<HumanDelegation>,
    weights: HashMap<WalletAddress, u64>,
    commitment: BlockHash,
}

impl HumanRoster {
    pub fn new(mut humans: Vec<HumanDelegation>) -> Result<Self, CheckpointError> {
        if humans.is_empty() || humans.len() > MAX_HUMANS {
            return Err(CheckpointError::InvalidRoster);
        }
        humans.sort_by(|a, b| a.human.as_str().cmp(b.human.as_str()));
        let mut weights = HashMap::new();
        let mut commitment = Blake2b256Hasher::default();
        commitment.update(b"BURST/human-roster/v1\0");
        commitment.update(&(humans.len() as u64).to_be_bytes());
        let mut previous = None;
        for entry in &humans {
            if previous == Some(&entry.human) {
                return Err(CheckpointError::InvalidRoster);
            }
            for address in [&entry.human, &entry.representative] {
                let key = decode_address(address.as_str()).ok_or(CheckpointError::InvalidRoster)?;
                commitment.update(&key);
            }
            *weights.entry(entry.representative.clone()).or_insert(0) += 1;
            previous = Some(&entry.human);
        }
        let commitment = BlockHash::new(commitment.finalize());
        Ok(Self {
            data: Arc::new(RosterData {
                humans,
                weights,
                commitment,
            }),
        })
    }

    pub fn commitment(&self) -> BlockHash {
        self.data.commitment
    }
    pub fn humans(&self) -> &[HumanDelegation] {
        &self.data.humans
    }
    pub fn total_humans(&self) -> u64 {
        self.data.humans.len() as u64
    }
    pub fn weight(&self, representative: &WalletAddress) -> u64 {
        self.data.weights.get(representative).copied().unwrap_or(0)
    }
    /// Distinct representatives in this roster, in unspecified order.
    pub fn representatives(&self) -> impl Iterator<Item = &WalletAddress> {
        self.data.weights.keys()
    }

    /// Reuse an identical immutable electorate without changing its commitment
    /// or one-unit-per-human weights.
    pub fn reuse_if_equal(self, previous: &Self) -> Self {
        if self == *previous {
            previous.clone()
        } else {
            self
        }
    }
}

/// All hashes are required even for an empty manifest (hash its canonical empty
/// encoding). `selection_root` commits request IDs, pools, parameters and rounds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub network: NetworkId,
    pub genesis: BlockHash,
    pub parent: BlockHash,
    pub height: u64,
    pub cutoff: u64,
    pub ledger_root: BlockHash,
    pub params_hash: BlockHash,
    pub next_roster: BlockHash,
    pub selection_root: BlockHash,
    pub next_quorum_bps: u16,
}

impl Checkpoint {
    /// Stable, fixed-width checkpoint encoding used for its hash. Certificate
    /// signatures cover the round's precommit ballot, which includes this hash.
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut bytes = DOMAIN.to_vec();
        bytes.push(match self.network {
            NetworkId::Live => 0,
            NetworkId::Test => 1,
            NetworkId::Dev => 2,
        });
        for hash in [self.genesis, self.parent] {
            bytes.extend_from_slice(hash.as_bytes());
        }
        bytes.extend_from_slice(&self.height.to_be_bytes());
        bytes.extend_from_slice(&self.cutoff.to_be_bytes());
        for hash in [
            self.ledger_root,
            self.params_hash,
            self.next_roster,
            self.selection_root,
        ] {
            bytes.extend_from_slice(hash.as_bytes());
        }
        bytes.extend_from_slice(&self.next_quorum_bps.to_be_bytes());
        bytes
    }

    pub fn hash(&self) -> BlockHash {
        BlockHash::new(blake2b_256(&self.signing_payload()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointSignature {
    pub representative: WalletAddress,
    pub signature: Signature,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointCertificate {
    pub checkpoint: Checkpoint,
    pub round: u64,
    pub signatures: Vec<CheckpointSignature>,
}

/// Never deserialize this from a peer. Initialize only from locally configured
/// genesis, then advance through certificates and either replayed rosters or
/// authenticated snapshot roster witnesses.
#[derive(Clone, Debug)]
pub struct CheckpointAnchor {
    network: NetworkId,
    genesis: BlockHash,
    hash: BlockHash,
    height: u64,
    cutoff: u64,
    roster: HumanRoster,
    quorum_bps: u16,
}

impl CheckpointAnchor {
    pub fn genesis(
        network: NetworkId,
        genesis: BlockHash,
        cutoff: u64,
        roster: HumanRoster,
        quorum_bps: u16,
    ) -> Result<Self, CheckpointError> {
        validate_quorum(quorum_bps)?;
        if genesis == BlockHash::ZERO {
            return Err(CheckpointError::InvalidCommitment);
        }
        Ok(Self {
            network,
            genesis,
            hash: genesis,
            height: 0,
            cutoff,
            roster,
            quorum_bps,
        })
    }

    pub fn hash(&self) -> BlockHash {
        self.hash
    }
    pub fn network(&self) -> NetworkId {
        self.network
    }
    pub fn genesis_hash(&self) -> BlockHash {
        self.genesis
    }
    pub fn height(&self) -> u64 {
        self.height
    }
    pub fn cutoff(&self) -> u64 {
        self.cutoff
    }
    pub fn roster(&self) -> &HumanRoster {
        &self.roster
    }
    pub fn quorum_bps(&self) -> u16 {
        self.quorum_bps
    }
    /// Rotate through the human-sorted roster at the next height plus round.
    /// A representative occupies one slot for each human delegated to it.
    pub fn proposer(&self, round: u64) -> &WalletAddress {
        let index =
            (u128::from(self.height) + 1 + u128::from(round)) % self.roster.humans().len() as u128;
        &self.roster.humans()[index as usize].representative
    }
    pub fn required_humans(&self) -> u64 {
        (self.roster.total_humans() * u64::from(self.quorum_bps)).div_ceil(10_000)
    }

    /// Check parent linkage, increasing cutoff, quorum range and nonzero hashes.
    /// Does not authenticate signatures or recompute committed ledger contents.
    pub fn validate_successor(&self, checkpoint: &Checkpoint) -> Result<(), CheckpointError> {
        if checkpoint.network != self.network
            || checkpoint.genesis != self.genesis
            || checkpoint.parent != self.hash
            || self.height.checked_add(1) != Some(checkpoint.height)
            || checkpoint.cutoff <= self.cutoff
        {
            return Err(CheckpointError::WrongParent);
        }
        validate_quorum(checkpoint.next_quorum_bps)?;
        if [
            checkpoint.ledger_root,
            checkpoint.params_hash,
            checkpoint.next_roster,
            checkpoint.selection_root,
        ]
        .contains(&BlockHash::ZERO)
        {
            return Err(CheckpointError::InvalidCommitment);
        }
        Ok(())
    }

    /// Checks signatures against the PREVIOUS roster and quorum. The proposed
    /// roster cannot certify itself or lower the threshold for its own approval.
    /// This verifies a certificate, not the ledger transition or beacon timing.
    pub fn verify_certificate(
        &self,
        certificate: &CheckpointCertificate,
    ) -> Result<(), CheckpointError> {
        self.validate_successor(&certificate.checkpoint)?;
        let payload =
            crate::checkpoint_round::Ballot::precommit(&certificate.checkpoint, certificate.round)
                .signing_payload();
        self.verify_signatures(&payload, &certificate.signatures)
    }

    pub(crate) fn verify_signatures(
        &self,
        payload: &[u8],
        signatures: &[CheckpointSignature],
    ) -> Result<(), CheckpointError> {
        if signatures.len() > self.roster.data.weights.len() {
            return Err(CheckpointError::InvalidSigner);
        }
        let mut seen = HashSet::new();
        let mut signed_weight = 0;
        for vote in signatures {
            let weight = self.roster.weight(&vote.representative);
            let key = decode_address(vote.representative.as_str())
                .ok_or(CheckpointError::InvalidSigner)?;
            if weight == 0
                || !seen.insert(&vote.representative)
                || !signature_cache::verify(payload, &vote.signature, &burst_types::PublicKey(key))
            {
                return Err(CheckpointError::InvalidSigner);
            }
            signed_weight += weight;
        }
        if signed_weight < self.required_humans() {
            return Err(CheckpointError::InsufficientQuorum);
        }
        Ok(())
    }

    /// `replayed_roster` must come from execution of the certified ledger cut,
    /// not a peer's unvalidated roster or the node's provisional account cache.
    pub fn advance(
        &self,
        certificate: &CheckpointCertificate,
        replayed_roster: HumanRoster,
    ) -> Result<Self, CheckpointError> {
        self.advance_certified_roster(certificate, replayed_roster)
    }

    /// Shared signature/commitment checks for semantic replay and the explicit
    /// quorum-trust snapshot route. Only the latter may use a roster witness
    /// without independently replaying its ledger history.
    pub(crate) fn advance_certified_roster(
        &self,
        certificate: &CheckpointCertificate,
        certified_roster: HumanRoster,
    ) -> Result<Self, CheckpointError> {
        self.verify_certificate(certificate)?;
        let cp = &certificate.checkpoint;
        if certified_roster.commitment() != cp.next_roster {
            return Err(CheckpointError::InvalidRoster);
        }
        Ok(Self {
            network: self.network,
            genesis: self.genesis,
            hash: cp.hash(),
            height: cp.height,
            cutoff: cp.cutoff,
            roster: certified_roster.reuse_if_equal(&self.roster),
            quorum_bps: cp.next_quorum_bps,
        })
    }
}

fn validate_quorum(bps: u16) -> Result<(), CheckpointError> {
    if !(6667..=10_000).contains(&bps) {
        return Err(CheckpointError::InvalidQuorum);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_crypto::{derive_address, keypair_from_seed, sign_message};

    fn address(seed: u8) -> WalletAddress {
        derive_address(&keypair_from_seed(&[seed; 32]).public)
    }
    fn roster(seeds: &[u8]) -> HumanRoster {
        HumanRoster::new(
            seeds
                .iter()
                .map(|&n| HumanDelegation {
                    human: address(n),
                    representative: address(n),
                })
                .collect(),
        )
        .unwrap()
    }
    fn parent(seeds: &[u8]) -> CheckpointAnchor {
        CheckpointAnchor::genesis(
            NetworkId::Dev,
            BlockHash::new([1; 32]),
            100,
            roster(seeds),
            6700,
        )
        .unwrap()
    }
    fn candidate(parent: &CheckpointAnchor) -> Checkpoint {
        Checkpoint {
            network: parent.network,
            genesis: parent.genesis,
            parent: parent.hash,
            height: parent.height + 1,
            cutoff: parent.cutoff + 1,
            ledger_root: BlockHash::new([2; 32]),
            params_hash: BlockHash::new([3; 32]),
            next_roster: parent.roster.commitment(),
            selection_root: BlockHash::new([4; 32]),
            next_quorum_bps: 6700,
        }
    }
    fn certify(checkpoint: Checkpoint, seeds: &[u8]) -> CheckpointCertificate {
        let signatures = seeds
            .iter()
            .map(|&n| CheckpointSignature {
                representative: address(n),
                signature: sign_message(
                    &crate::checkpoint_round::Ballot::precommit(&checkpoint, 0).signing_payload(),
                    &keypair_from_seed(&[n; 32]).private,
                ),
            })
            .collect();
        CheckpointCertificate {
            checkpoint,
            round: 0,
            signatures,
        }
    }

    #[test]
    fn cached_valid_signatures_do_not_cache_roster_quorum_or_checkpoint_authority() {
        let initial = parent(&[1, 2, 3]);
        let certificate = certify(candidate(&initial), &[1, 2, 3]);
        initial.verify_certificate(&certificate).unwrap();
        initial.verify_certificate(&certificate).unwrap();
        assert!(parent(&[4, 5, 6]).verify_certificate(&certificate).is_err());
        assert_eq!(
            parent(&[1, 2, 3, 4, 5]).verify_certificate(&certificate),
            Err(CheckpointError::InsufficientQuorum)
        );
        let mut duplicate = certificate.clone();
        duplicate.signatures[1] = duplicate.signatures[0].clone();
        assert_eq!(
            initial.verify_certificate(&duplicate),
            Err(CheckpointError::InvalidSigner)
        );
        let mut changed = certificate.clone();
        changed.round += 1;
        assert!(initial.verify_certificate(&changed).is_err());
        changed = certificate.clone();
        changed.checkpoint.next_quorum_bps = 6667;
        assert!(initial.verify_certificate(&changed).is_err());
        let next = initial.advance(&certificate, roster(&[1, 2, 3])).unwrap();
        assert_eq!(
            next.verify_certificate(&certificate),
            Err(CheckpointError::WrongParent)
        );
    }

    #[test]
    fn roster_reuse_preserves_changed_delegation_and_historical_weights() {
        let previous = roster(&[1, 2, 3]);
        let identical = roster(&[3, 1, 2]);
        assert!(!Arc::ptr_eq(&previous.data, &identical.data));
        let reused = identical.reuse_if_equal(&previous);
        assert!(Arc::ptr_eq(&previous.data, &reused.data));

        let mut humans = previous.humans().to_vec();
        humans
            .iter_mut()
            .find(|entry| entry.human == address(1))
            .unwrap()
            .representative = address(2);
        let changed = HumanRoster::new(humans).unwrap().reuse_if_equal(&previous);
        assert!(!Arc::ptr_eq(&previous.data, &changed.data));
        assert_ne!(previous.commitment(), changed.commitment());
        assert_eq!(previous.weight(&address(1)), 1);
        assert_eq!(previous.weight(&address(2)), 1);
        assert_eq!(changed.weight(&address(1)), 0);
        assert_eq!(changed.weight(&address(2)), 2);
        assert_eq!(changed.total_humans(), previous.total_humans());
    }

    #[test]
    fn authority_clones_share_immutable_rosters_and_transitions_preserve_old_weights() {
        let initial = parent(&[1, 2, 3]);
        let cloned = initial.clone();
        assert!(Arc::ptr_eq(&initial.roster.data, &cloned.roster.data));
        let unchanged = initial
            .advance(
                &certify(candidate(&initial), &[1, 2, 3]),
                roster(&[3, 2, 1]),
            )
            .unwrap();
        assert!(Arc::ptr_eq(&initial.roster.data, &unchanged.roster.data));

        let replacement = roster(&[9]);
        let mut changed = candidate(&unchanged);
        changed.next_roster = replacement.commitment();
        let next = unchanged
            .advance(&certify(changed, &[1, 2, 3]), replacement)
            .unwrap();
        assert!(!Arc::ptr_eq(&initial.roster.data, &next.roster.data));
        assert_eq!(initial.roster().total_humans(), 3);
        assert_eq!(initial.roster().weight(&address(9)), 0);
        assert_eq!(next.roster().total_humans(), 1);
        assert_eq!(next.roster().weight(&address(9)), 1);
        assert!(next
            .verify_certificate(&certify(candidate(&next), &[9]))
            .is_ok());
        assert!(next
            .verify_certificate(&certify(candidate(&next), &[1, 2, 3]))
            .is_err());
    }

    #[test]
    fn streamed_roster_commitment_matches_legacy_bytes_and_rejects_invalid_addresses() {
        for count in [1usize, 2, 128, 1025] {
            let mut humans: Vec<_> = (0..count)
                .rev()
                .map(|index| {
                    let mut seed = [0u8; 32];
                    seed[..8].copy_from_slice(&(index as u64).to_be_bytes());
                    HumanDelegation {
                        human: derive_address(&keypair_from_seed(&seed).public),
                        representative: address((index % 7 + 1) as u8),
                    }
                })
                .collect();
            let roster = HumanRoster::new(humans.clone()).unwrap();
            humans.sort_by(|a, b| a.human.as_str().cmp(b.human.as_str()));
            let mut legacy = b"BURST/human-roster/v1\0".to_vec();
            legacy.extend_from_slice(&(count as u64).to_be_bytes());
            for entry in &humans {
                legacy.extend_from_slice(&decode_address(entry.human.as_str()).unwrap());
                legacy.extend_from_slice(&decode_address(entry.representative.as_str()).unwrap());
            }
            assert_eq!(roster.commitment(), BlockHash::new(blake2b_256(&legacy)));
            assert_eq!(roster.humans(), humans);
            for representative in 1..=7 {
                let representative = address(representative);
                assert_eq!(roster.weight(&representative), humans.iter()
                    .filter(|entry| entry.representative == representative).count() as u64);
            }
            for invalid_human in [false, true] {
                let mut invalid = humans.clone();
                let last = invalid.last_mut().unwrap();
                if invalid_human { last.human = WalletAddress::new("brst_invalid"); }
                else { last.representative = WalletAddress::new("brst_invalid"); }
                assert_eq!(HumanRoster::new(invalid).unwrap_err(), CheckpointError::InvalidRoster);
            }
        }
    }

    #[test]
    fn roster_is_canonical_and_counts_each_human_once() {
        assert_eq!(
            roster(&[1, 2, 3]).commitment(),
            roster(&[3, 1, 2]).commitment()
        );
        let entry = HumanDelegation {
            human: address(1),
            representative: address(2),
        };
        assert_eq!(
            HumanRoster::new(vec![entry.clone(), entry]).unwrap_err(),
            CheckpointError::InvalidRoster
        );
        assert_eq!(
            HumanRoster::new(vec![]).unwrap_err(),
            CheckpointError::InvalidRoster
        );
        let delegated = HumanRoster::new(
            (1..=4)
                .map(|n| HumanDelegation {
                    human: address(n),
                    representative: address(9),
                })
                .collect(),
        )
        .unwrap();
        assert_eq!(delegated.weight(&address(9)), 4);
        assert_eq!(delegated.total_humans(), 4);
        assert_eq!(
            delegated.representatives().collect::<Vec<_>>(),
            vec![&address(9)]
        );
        assert_ne!(delegated.commitment(), roster(&[1, 2, 3, 4]).commitment());
    }

    #[test]
    fn offline_nodes_do_not_reduce_quorum() {
        let parent = parent(&[1, 2, 3, 4]);
        assert_eq!(parent.required_humans(), 3);
        for voters in [&[1][..], &[1, 2], &[]] {
            assert_eq!(
                parent.verify_certificate(&certify(candidate(&parent), voters)),
                Err(CheckpointError::InsufficientQuorum)
            );
        }
        assert!(parent
            .verify_certificate(&certify(candidate(&parent), &[1, 2, 3]))
            .is_ok());
    }

    #[test]
    fn delegation_counts_people_not_number_of_signatures() {
        let humans = (1..=4)
            .map(|n| HumanDelegation {
                human: address(n),
                representative: address(if n < 4 { 8 } else { 9 }),
            })
            .collect();
        let parent = CheckpointAnchor::genesis(
            NetworkId::Dev,
            BlockHash::new([1; 32]),
            100,
            HumanRoster::new(humans).unwrap(),
            6700,
        )
        .unwrap();
        assert!(parent
            .verify_certificate(&certify(candidate(&parent), &[8]))
            .is_ok());
        assert_eq!(
            parent.verify_certificate(&certify(candidate(&parent), &[9])),
            Err(CheckpointError::InsufficientQuorum)
        );
    }

    #[test]
    fn duplicate_unknown_and_forged_signers_fail() {
        let parent = parent(&[1, 2, 3, 4]);
        for voters in [&[1, 1, 1][..], &[1, 2, 9]] {
            assert_eq!(
                parent.verify_certificate(&certify(candidate(&parent), voters)),
                Err(CheckpointError::InvalidSigner)
            );
        }
        let mut cert = certify(candidate(&parent), &[1, 2, 3]);
        cert.signatures[0].signature.0[0] ^= 1;
        assert_eq!(
            parent.verify_certificate(&cert),
            Err(CheckpointError::InvalidSigner)
        );
    }

    #[test]
    fn all_commitments_and_network_identity_are_signed() {
        let parent = parent(&[1]);
        let cert = certify(candidate(&parent), &[1]);
        let mut alternatives = vec![];
        let mut cp = cert.checkpoint.clone();
        cp.network = NetworkId::Live;
        alternatives.push(cp);
        let mut cp = cert.checkpoint.clone();
        cp.genesis = BlockHash::new([9; 32]);
        alternatives.push(cp);
        let mut cp = cert.checkpoint.clone();
        cp.parent = BlockHash::new([9; 32]);
        alternatives.push(cp);
        let mut cp = cert.checkpoint.clone();
        cp.height += 1;
        alternatives.push(cp);
        let mut cp = cert.checkpoint.clone();
        cp.cutoff += 1;
        alternatives.push(cp);
        let mut cp = cert.checkpoint.clone();
        cp.ledger_root = BlockHash::new([9; 32]);
        alternatives.push(cp);
        let mut cp = cert.checkpoint.clone();
        cp.params_hash = BlockHash::new([9; 32]);
        alternatives.push(cp);
        let mut cp = cert.checkpoint.clone();
        cp.next_roster = BlockHash::new([9; 32]);
        alternatives.push(cp);
        let mut cp = cert.checkpoint.clone();
        cp.selection_root = BlockHash::new([9; 32]);
        alternatives.push(cp);
        let mut cp = cert.checkpoint.clone();
        cp.next_quorum_bps = 9000;
        alternatives.push(cp);
        for cp in alternatives {
            assert_ne!(cp.hash(), cert.checkpoint.hash());
            assert!(parent
                .verify_certificate(&CheckpointCertificate {
                    checkpoint: cp,
                    round: cert.round,
                    signatures: cert.signatures.clone()
                })
                .is_err());
        }
    }

    #[test]
    fn new_roster_cannot_approve_itself_or_weaken_its_approval() {
        let parent = parent(&[1, 2, 3, 4]);
        let mut cp = candidate(&parent);
        cp.next_roster = roster(&[9]).commitment();
        cp.next_quorum_bps = 6667;
        assert_eq!(
            parent.verify_certificate(&certify(cp.clone(), &[9])),
            Err(CheckpointError::InvalidSigner)
        );
        assert_eq!(
            parent.verify_certificate(&certify(cp.clone(), &[1, 2])),
            Err(CheckpointError::InsufficientQuorum)
        );
        let cert = certify(cp, &[1, 2, 3]);
        assert!(parent.advance(&cert, roster(&[8])).is_err());
        let next = parent.advance(&cert, roster(&[9])).unwrap();
        assert_eq!(next.hash(), cert.checkpoint.hash());
        assert!(
            next.verify_certificate(&cert).is_err(),
            "no rollback or replay"
        );
        assert!(next
            .verify_certificate(&certify(candidate(&next), &[1]))
            .is_err());
        assert!(next
            .verify_certificate(&certify(candidate(&next), &[9]))
            .is_ok());
    }

    #[test]
    fn quorum_never_rounds_down_to_two_thirds() {
        for n in 1..=100u8 {
            let humans = roster(&(1..=n).collect::<Vec<_>>());
            let parent =
                CheckpointAnchor::genesis(NetworkId::Dev, BlockHash::new([1; 32]), 0, humans, 6667)
                    .unwrap();
            assert!(3 * parent.required_humans() > 2 * u64::from(n));
        }
        for bad in [0, 5000, 6666, 10001, u16::MAX] {
            assert!(CheckpointAnchor::genesis(
                NetworkId::Dev,
                BlockHash::new([1; 32]),
                0,
                roster(&[1]),
                bad
            )
            .is_err());
        }
    }

    #[test]
    fn two_disjoint_partition_certificates_cannot_both_pass() {
        let parent = parent(&[1, 2, 3, 4]);
        for mask in 0..16u8 {
            let left: Vec<_> = (1..=4).filter(|n| mask & (1 << (n - 1)) != 0).collect();
            let right: Vec<_> = (1..=4).filter(|n| !left.contains(n)).collect();
            let a = certify(candidate(&parent), &left);
            let mut cp = candidate(&parent);
            cp.ledger_root = BlockHash::new([9; 32]);
            let b = certify(cp, &right);
            assert!(
                !(parent.verify_certificate(&a).is_ok() && parent.verify_certificate(&b).is_ok())
            );
        }
    }
}
