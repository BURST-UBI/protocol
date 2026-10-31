//! Vote signing and bounded response caches.
//!
//! The caller validates blocks, checks representative eligibility, and reserves
//! durable sequences and final choices before signing. This helper has no ledger
//! access and does not enforce those rules or check that its key matches its address.

use burst_crypto::sign_message;
use burst_types::{BlockHash, PrivateKey, Signature, WalletAddress};
use std::sync::Arc;

/// Domain tag separating consensus votes from other signed message formats.
pub const VOTE_DOMAIN: &[u8] = b"BURST_VOTE_V1";

/// The canonical byte payload that a consensus vote signature covers. Used by
/// the signer and the node's receive path:
///
/// `VOTE_DOMAIN ‖ voter ‖ is_final ‖ timestamp_le ‖ sequence_le ‖ block_hashes…`
///
/// The receiver derives the verification key from `voter`. Encoding the address
/// also prevents substituting another voter without changing the signed bytes.
pub fn vote_signing_payload(
    voter: &WalletAddress,
    is_final: bool,
    timestamp: u64,
    sequence: u64,
    block_hashes: &[BlockHash],
) -> Vec<u8> {
    let voter_bytes = voter.as_str().as_bytes();
    let mut payload = Vec::with_capacity(
        VOTE_DOMAIN.len() + voter_bytes.len() + 1 + 8 + 8 + block_hashes.len() * 32,
    );
    payload.extend_from_slice(VOTE_DOMAIN);
    payload.extend_from_slice(voter_bytes);
    payload.push(u8::from(is_final));
    payload.extend_from_slice(&timestamp.to_le_bytes());
    payload.extend_from_slice(&sequence.to_le_bytes());
    for hash in block_hashes {
        payload.extend_from_slice(hash.as_bytes());
    }
    payload
}

/// Signs caller-selected block hashes and caches responses.
pub struct VoteGenerator {
    /// This node's representative account.
    pub representative: WalletAddress,
    /// Caller-managed voting flag. The signing methods do not enforce it.
    pub is_representative: bool,
    /// Signing key for vote generation.
    signing_key: Arc<PrivateKey>,
    /// Monotonically increasing sequence counter for vote ordering.
    sequence: u64,
    final_responses: std::collections::BTreeMap<BlockHash, GeneratedVote>,
    final_response_order: std::collections::VecDeque<BlockHash>,
    soft_responses: std::collections::BTreeMap<(WalletAddress, BlockHash), GeneratedVote>,
    soft_response_order: std::collections::BTreeMap<u64, (WalletAddress, BlockHash)>,
}

impl VoteGenerator {
    pub fn new(representative: WalletAddress, signing_key: [u8; 32]) -> Self {
        Self::with_shared_key(representative, Arc::new(PrivateKey(signing_key)))
    }

    pub fn with_shared_key(representative: WalletAddress, signing_key: Arc<PrivateKey>) -> Self {
        Self {
            representative,
            is_representative: false,
            signing_key,
            sequence: 0,
            final_responses: Default::default(),
            final_response_order: Default::default(),
            soft_responses: Default::default(),
            soft_response_order: Default::default(),
        }
    }

    /// Set whether this node should act as a voting representative.
    pub fn set_representative(&mut self, is_rep: bool) {
        self.is_representative = is_rep;
    }

    /// Generate a non-final vote for a block.
    ///
    /// Non-final votes can be changed later (e.g. if the node sees a
    /// conflicting block with higher priority).
    pub fn generate_vote(&mut self, block_hash: BlockHash) -> GeneratedVote {
        self.sequence += 1;
        let timestamp = current_timestamp_ms();

        let signature = self.sign_vote(&block_hash, false, timestamp, self.sequence);

        GeneratedVote {
            voter: self.representative.clone(),
            block_hash,
            is_final: false,
            timestamp,
            sequence: self.sequence,
            signature,
        }
    }

    /// Generate a final vote (cannot be changed).
    ///
    /// The caller must persist the final choice before calling this method.
    /// This generator accepts hashes without roots and cannot prevent signing
    /// conflicting choices itself.
    pub fn generate_final_vote(&mut self, block_hash: BlockHash) -> GeneratedVote {
        self.sequence += 1;
        let timestamp = current_timestamp_ms();

        let signature = self.sign_vote(&block_hash, true, timestamp, self.sequence);

        GeneratedVote {
            voter: self.representative.clone(),
            block_hash,
            is_final: true,
            timestamp,
            sequence: self.sequence,
            signature,
        }
    }

    /// A cached signature is not authority to send: callers must revalidate its
    /// durable choice, epoch and current voting boundaries before reuse.
    pub fn cached_final_response(&self, hash: &BlockHash) -> Option<&GeneratedVote> {
        self.final_responses
            .get(hash)
            .filter(|vote| vote.voter == self.representative)
    }

    pub fn remember_final_response(&mut self, vote: GeneratedVote) {
        const CAPACITY: usize = 1024;
        if !vote.is_final || vote.voter != self.representative {
            return;
        }
        if !self.final_responses.contains_key(&vote.block_hash) {
            if self.final_responses.len() == CAPACITY {
                if let Some(oldest) = self.final_response_order.pop_front() {
                    self.final_responses.remove(&oldest);
                }
            }
            self.final_response_order.push_back(vote.block_hash);
        }
        self.final_responses.insert(vote.block_hash, vote);
    }

    pub fn cached_soft_response(
        &self,
        account: &WalletAddress,
        previous: BlockHash,
        hash: BlockHash,
    ) -> Option<&GeneratedVote> {
        self.soft_responses
            .get(&(account.clone(), previous))
            .filter(|vote| vote.voter == self.representative && vote.block_hash == hash)
    }

    /// Remember only the latest signed choice for an account position. A later
    /// conflicting choice or final vote must invalidate the earlier soft response.
    pub fn remember_response_choice(
        &mut self,
        account: WalletAddress,
        previous: BlockHash,
        vote: &GeneratedVote,
    ) {
        let root = (account, previous);
        if let Some(old) = self.soft_responses.remove(&root) {
            self.soft_response_order.remove(&old.sequence);
        }
        if vote.is_final || vote.voter != self.representative {
            return;
        }
        if self.soft_responses.len() == 1024 {
            if let Some((_, oldest)) = self.soft_response_order.pop_first() {
                self.soft_responses.remove(&oldest);
            }
        }
        self.soft_response_order.insert(vote.sequence, root.clone());
        self.soft_responses.insert(root, vote.clone());
    }

    /// Current sequence number.
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Restore a durable sequence reservation before signing. Never rewind a
    /// live generator, even when the caller supplies an older checkpoint.
    pub fn advance_sequence_to(&mut self, sequence: u64) {
        self.sequence = self.sequence.max(sequence);
    }

    /// Sign a vote over the canonical [`vote_signing_payload`] (domain-tagged and
    /// bound to this node's voter identity), matching the receiver's verification.
    fn sign_vote(
        &self,
        block_hash: &BlockHash,
        is_final: bool,
        timestamp: u64,
        sequence: u64,
    ) -> Signature {
        let payload = vote_signing_payload(
            &self.representative,
            is_final,
            timestamp,
            sequence,
            std::slice::from_ref(block_hash),
        );
        sign_message(&payload, &self.signing_key)
    }
}

/// A signed vote. The caller must establish that it is eligible for broadcast.
#[derive(Clone, Debug)]
pub struct GeneratedVote {
    /// The representative account that generated this vote.
    pub voter: WalletAddress,
    /// The block hash being voted for.
    pub block_hash: BlockHash,
    /// Whether this is a final (irrevocable) vote.
    pub is_final: bool,
    /// Timestamp in milliseconds when the vote was generated.
    pub timestamp: u64,
    /// Monotonic sequence number for ordering votes from the same voter.
    pub sequence: u64,
    /// Ed25519 signature over the vote payload.
    pub signature: Signature,
}

impl GeneratedVote {
    /// Encode this single-vote container. The node transports `WireVote` instead.
    ///
    /// Wire format: voter_len (2 LE) ‖ voter (variable) ‖ block_hash (32) ‖ is_final (1)
    ///              ‖ timestamp (8 LE) ‖ sequence (8 LE) ‖ signature (64)
    pub fn to_bytes(&self) -> Vec<u8> {
        let voter_bytes = self.voter.as_str().as_bytes();
        let voter_len = voter_bytes.len() as u16;

        let mut buf = Vec::with_capacity(2 + voter_bytes.len() + 32 + 1 + 8 + 8 + 64);
        buf.extend_from_slice(&voter_len.to_le_bytes());
        buf.extend_from_slice(voter_bytes);
        buf.extend_from_slice(self.block_hash.as_bytes());
        buf.push(u8::from(self.is_final));
        buf.extend_from_slice(&self.timestamp.to_le_bytes());
        buf.extend_from_slice(&self.sequence.to_le_bytes());
        buf.extend_from_slice(&self.signature.0);
        buf
    }
}

/// Helper: current time in milliseconds since epoch.
fn current_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_crypto::{generate_keypair, verify_signature};
    use burst_types::PublicKey;

    #[test]
    fn soft_response_reuse_tracks_latest_choice_and_bounded_positions() {
        let (mut generator, _) = make_generator();
        let account = make_rep();
        let previous = make_hash(8);
        let first = generator.generate_vote(make_hash(1));
        generator.remember_response_choice(account.clone(), previous, &first);
        assert_eq!(
            generator
                .cached_soft_response(&account, previous, first.block_hash)
                .unwrap()
                .signature,
            first.signature
        );
        let fork = generator.generate_vote(make_hash(2));
        generator.remember_response_choice(account.clone(), previous, &fork);
        assert!(generator
            .cached_soft_response(&account, previous, first.block_hash)
            .is_none());
        assert!(generator
            .cached_soft_response(&account, previous, fork.block_hash)
            .is_some());
        let final_vote = generator.generate_final_vote(fork.block_hash);
        generator.remember_response_choice(account.clone(), previous, &final_vote);
        assert!(generator.soft_responses.is_empty());
        assert!(generator.soft_response_order.is_empty());
        for n in 0u64..1025 {
            let mut vote = first.clone();
            vote.sequence = n + 10;
            let mut root = [0; 32];
            root[..8].copy_from_slice(&n.to_be_bytes());
            generator.remember_response_choice(account.clone(), BlockHash::new(root), &vote);
        }
        assert_eq!(generator.soft_responses.len(), 1024);
        assert_eq!(generator.soft_response_order.len(), 1024);
        assert!(generator
            .cached_soft_response(&account, BlockHash::ZERO, first.block_hash)
            .is_none());
    }

    #[test]
    fn final_response_cache_is_bounded_and_keeps_original_signature() {
        let (mut generator, _) = make_generator();
        let vote = generator.generate_final_vote(make_hash(1));
        generator.remember_final_response(vote.clone());
        let cached = generator.cached_final_response(&vote.block_hash).unwrap();
        assert_eq!(cached.signature, vote.signature);
        assert_eq!(cached.sequence, vote.sequence);
        let sequence = generator.sequence();
        // Synthetic keys exercise eviction without generating 1024 signatures.
        for i in 0u64..1024 {
            let mut entry = vote.clone();
            let mut key = [0; 32];
            key[..8].copy_from_slice(&i.to_be_bytes());
            entry.block_hash = BlockHash::new(key);
            generator.remember_final_response(entry);
        }
        assert_eq!(generator.final_responses.len(), 1024);
        assert_eq!(generator.final_response_order.len(), 1024);
        assert!(generator.cached_final_response(&vote.block_hash).is_none());
        assert_eq!(generator.sequence(), sequence);
        let hash = *generator.final_response_order.front().unwrap();
        let cached = generator.cached_final_response(&hash).unwrap().clone();
        generator.remember_final_response(cached);
        assert_eq!(generator.final_response_order.len(), 1024);
        generator.representative = WalletAddress::new("brst_changed");
        assert!(generator.cached_final_response(&hash).is_none());
    }

    fn make_hash(byte: u8) -> BlockHash {
        BlockHash::new([byte; 32])
    }

    fn make_rep() -> WalletAddress {
        WalletAddress::new("brst_testrep1111111111111111111111111111111111111111111111111111111")
    }

    fn make_generator() -> (VoteGenerator, PublicKey) {
        let kp = generate_keypair();
        let gen = VoteGenerator::new(make_rep(), kp.private.0);
        (gen, kp.public)
    }

    #[test]
    fn generate_vote_increments_sequence() {
        let (mut gen, _) = make_generator();
        assert_eq!(gen.sequence(), 0);

        let v1 = gen.generate_vote(make_hash(1));
        assert_eq!(v1.sequence, 1);
        assert_eq!(gen.sequence(), 1);

        let v2 = gen.generate_vote(make_hash(2));
        assert_eq!(v2.sequence, 2);
        assert_eq!(gen.sequence(), 2);
    }

    #[test]
    fn generate_vote_is_not_final() {
        let (mut gen, _) = make_generator();
        let vote = gen.generate_vote(make_hash(1));
        assert!(!vote.is_final);
        assert_eq!(vote.block_hash, make_hash(1));
        assert_eq!(vote.voter, make_rep());
    }

    #[test]
    fn generate_final_vote_is_final() {
        let (mut gen, _) = make_generator();
        let vote = gen.generate_final_vote(make_hash(1));
        assert!(vote.is_final);
        assert_eq!(vote.block_hash, make_hash(1));
    }

    #[test]
    fn vote_signature_is_valid() {
        let (mut gen, pubkey) = make_generator();
        let vote = gen.generate_vote(make_hash(42));

        // Reconstruct the canonical payload the signer used.
        let payload = vote_signing_payload(
            &vote.voter,
            vote.is_final,
            vote.timestamp,
            vote.sequence,
            std::slice::from_ref(&vote.block_hash),
        );
        assert!(verify_signature(&payload, &vote.signature, &pubkey));
    }

    #[test]
    fn final_vote_signature_is_valid() {
        let (mut gen, pubkey) = make_generator();
        let vote = gen.generate_final_vote(make_hash(99));

        let payload = vote_signing_payload(
            &vote.voter,
            vote.is_final,
            vote.timestamp,
            vote.sequence,
            std::slice::from_ref(&vote.block_hash),
        );
        assert!(verify_signature(&payload, &vote.signature, &pubkey));

        // The signature must not verify for a different voter identity.
        let wrong = vote_signing_payload(
            &burst_types::WalletAddress::new("brst_someoneelse"),
            vote.is_final,
            vote.timestamp,
            vote.sequence,
            std::slice::from_ref(&vote.block_hash),
        );
        assert!(!verify_signature(&wrong, &vote.signature, &pubkey));
    }

    #[test]
    fn to_bytes_produces_valid_encoding() {
        let (mut gen, _) = make_generator();
        let vote = gen.generate_vote(make_hash(1));
        let bytes = vote.to_bytes();

        // Minimum size: 2 (voter_len) + voter + 32 + 1 + 8 + 8 + 64
        assert!(bytes.len() > 2 + 32 + 1 + 8 + 8 + 64);

        // Parse voter length prefix
        let voter_len = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
        assert_eq!(voter_len, vote.voter.as_str().len());
    }

    #[test]
    fn different_blocks_produce_different_signatures() {
        let (mut gen, _) = make_generator();
        let v1 = gen.generate_vote(make_hash(1));
        let v2 = gen.generate_vote(make_hash(2));

        assert_ne!(v1.signature.0, v2.signature.0);
    }

    #[test]
    fn sequence_is_monotonic_across_final_and_non_final() {
        let (mut gen, _) = make_generator();
        let v1 = gen.generate_vote(make_hash(1));
        let v2 = gen.generate_final_vote(make_hash(2));
        let v3 = gen.generate_vote(make_hash(3));

        assert_eq!(v1.sequence, 1);
        assert_eq!(v2.sequence, 2);
        assert_eq!(v3.sequence, 3);
    }
}
