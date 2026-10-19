//! State block — the unified block format for BURST's block-lattice.
//!
//! Inspired by Nano's state blocks: every block contains the full account state,
//! enabling efficient pruning without losing security.

use burst_crypto::blake2b_256;
use burst_types::{BlockHash, Signature, Timestamp, TxHash, WalletAddress};
use burst_work::validate_work;
use serde::{Deserialize, Serialize};

/// The type of operation this block represents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockType {
    /// Account opening block (first block in the chain).
    Open,
    /// BRN burn → TRST mint.
    Burn,
    /// TRST send to another account.
    Send,
    /// Receive TRST from another account.
    Receive,
    /// Merge multiple TRST tokens.
    Merge,
    /// Endorse a new wallet.
    Endorse,
    /// Challenge another wallet.
    Challenge,
    /// Governance proposal submission.
    GovernanceProposal,
    /// Governance vote.
    GovernanceVote,
    /// Delegate voting power.
    Delegate,
    /// Revoke delegation.
    RevokeDelegation,
    /// Change consensus representative.
    ChangeRepresentative,
    /// Epoch block (protocol upgrade marker).
    Epoch,
    /// Reject a pending TRST receive (returns to sender).
    RejectReceive,
    /// Verification vote — verifier casts a vote on a wallet's humanity.
    VerificationVote,
    /// Governance activation block — records an on-chain parameter change
    /// (Tezos-style self-amendment). Placed on the genesis account's chain.
    GovernanceActivation,
    /// Opt in to the verifier pool — a verified wallet volunteers to be
    /// randomly selected (via VRF) to verify others. On-chain so every node
    /// derives the same eligible pool (keeps VRF selection deterministic).
    VerifierOptIn,
    /// Opt out of the verifier pool (revokes a prior VerifierOptIn).
    VerifierOptOut,
    /// Permanently burn BRN to support a governance proposal.
    GovernanceEndorse,
    /// Withdraw the signer's proposal before voting begins.
    GovernanceWithdraw,
    /// Replace the consumer wallet's active signing-device grant.
    ChangeSigner,
}

/// Current state block version.
pub const CURRENT_BLOCK_VERSION: u8 = 7;

pub const MAX_MERGE_INPUTS: usize = 256;
pub const MAX_RELEASE_SOURCES: usize = 1024;
pub const MAX_BASE_BLOCK_BYTES: u64 = 1024;
pub const MAX_MERGE_BLOCK_BYTES: u64 = MAX_BASE_BLOCK_BYTES + MAX_MERGE_INPUTS as u64 * 32;

/// A state block in BURST's block-lattice.
///
/// Each block contains the full account state after the operation,
/// enabling database pruning without losing balance information.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateBlock {
    /// Consensus format version. Version 7 binds actions to signing-device grants.
    pub version: u8,

    /// Active grant before this operation. Zero denotes an unenrolled account.
    pub signing_grant: BlockHash,

    /// Certified economic boundary under which this operation was authored.
    /// Initially the configured genesis hash. Zero is reserved for genesis
    /// itself, whose hash cannot recursively include its own identifier.
    pub payment_epoch: BlockHash,

    /// The block type / operation.
    pub block_type: BlockType,

    /// The account this block belongs to.
    pub account: WalletAddress,

    /// Hash of the previous block in this account's chain (zero for Open).
    pub previous: BlockHash,

    /// The account's consensus representative.
    pub representative: WalletAddress,

    /// Cumulative BRN this account has committed on-chain (burned + staked),
    /// monotonically increasing. Must be 0 on Open blocks.
    ///
    /// BRN itself never lives on the ledger — it is a computed function of
    /// time (whitepaper: "it doesn't exist on the ledger until it's spent").
    /// This field only records spending: the delta between consecutive blocks
    /// is the burn/stake amount, and nodes validate every delta against the
    /// independently computed BRN(w) at the block's timestamp.
    pub brn_balance: u128,

    /// TRST balance after this block (transferable only).
    pub trst_balance: u128,

    /// Link field — context-dependent:
    /// - For Burn: unused (zero)
    /// - For Send: destination account's block hash (pairing)
    /// - For Receive: the send block hash being received
    /// - For Endorse: the target wallet's pending verification
    /// - For GovernanceVote: the proposal hash
    pub link: BlockHash,

    /// TRST origin; for Challenge, the reason; for VerificationVote, the
    /// certified checkpoint/selection context. All are covered by the signature.
    pub origin: TxHash,

    /// The transaction contained in this block.
    pub transaction: TxHash,

    /// For Endorse: the target's account head observed by the signer. This
    /// makes earlier receive/delegation blocks causal prerequisites of identity
    /// admission. Zero allows endorsement before the target opens its account.
    /// For Challenge: nonzero names the recorded outcome being appealed; zero
    /// is an ordinary challenge. Appeal admission is separately release-gated.
    /// Must be zero for other operations.
    pub target_head: BlockHash,

    /// For Merge blocks: the token ids consumed by this merge, chosen by the
    /// wallet (IMPLEMENTATION_DECISIONS 6.17b: "the merge tx itself lists the
    /// inputs"). Lets wallets group tokens with similar expiries (whitepaper
    /// §Merging) and makes the merger graph reconstructible from the chain.
    /// Empty on non-Merge blocks; empty on a Merge block requests the node's
    /// deterministic expiry-grouped auto-selection. At most 256 entries (6.12b).
    #[serde(default)]
    pub merge_sources: Vec<TxHash>,

    /// Complete signed vote sets releasing this account's stakes. Sorted,
    /// distinct hashes make releases explicit dependencies of this block.
    pub release_sources: Vec<BlockHash>,

    /// Block timestamp.
    pub timestamp: Timestamp,

    /// Deterministic hash of the ProtocolParams this block was validated under.
    /// Equivalent to Tezos's protocol hash in block headers.
    pub params_hash: BlockHash,

    /// Proof-of-work nonce (anti-spam).
    pub work: u64,

    /// Signature by the account holder.
    pub signature: Signature,

    /// The computed hash of this block.
    pub hash: BlockHash,

    /// Scoped key grant, revocation, or secondary-signed ballot. Never replaces the
    /// primary account signature or authorizes spending with a secondary key.
    #[serde(default)]
    pub delegation_payload: Vec<u8>,
}

impl StateBlock {
    /// Versioned structural rules, shared by admission and fork validation.
    pub fn validate_format(&self) -> Result<(), &'static str> {
        if self.version != CURRENT_BLOCK_VERSION {
            return Err("unsupported block version");
        }
        if self.block_type == BlockType::Epoch
            && (!self.link.is_zero()
                || !self.origin.is_zero()
                || !self.transaction.is_zero()
                || !self.release_sources.is_empty()
                || !self.delegation_payload.is_empty())
        {
            return Err("epoch marker cannot contain an account action");
        }
        if self.block_type == BlockType::ChangeSigner
            && (self.previous.is_zero()
                || self.link.is_zero()
                || !self.origin.is_zero()
                || !self.transaction.is_zero()
                || !self.release_sources.is_empty()
                || !self.delegation_payload.is_empty())
        {
            return Err("invalid signing-device handoff");
        }
        if !self.delegation_payload.is_empty() {
            use burst_governance::key_delegation::operation::DelegationOperation;
            let operation = DelegationOperation::decode(&self.delegation_payload)
                .map_err(|_| "invalid delegation payload")?;
            if operation
                .identity()
                .is_some_and(|identity| identity.2 != &self.account)
                || !self.origin.is_zero()
            {
                return Err("delegation owner or origin mismatch");
            }
            match (&self.block_type, operation) {
                (BlockType::Delegate, DelegationOperation::Grant(grant))
                    if self.link.as_bytes() == &grant.delegate.0 => {}
                (BlockType::RevokeDelegation, DelegationOperation::Revoke(_))
                    if self.link.is_zero() => {}
                (BlockType::GovernanceVote, DelegationOperation::Ballot { ballot, .. })
                    if self.link.into_tx_hash() == ballot.proposal
                        && self.transaction.as_bytes()[0]
                            == match ballot.vote {
                                burst_transactions::governance::GovernanceVote::Yea => 0,
                                burst_transactions::governance::GovernanceVote::Nay => 1,
                                burst_transactions::governance::GovernanceVote::Abstain => 2,
                            } => {}
                _ => return Err("delegation payload does not match its operation"),
            }
        }
        if !matches!(self.block_type, BlockType::Endorse | BlockType::Challenge)
            && !self.target_head.is_zero()
        {
            return Err("target reference is only allowed on endorsement or challenge blocks");
        }
        if self.merge_sources.len() > MAX_MERGE_INPUTS {
            return Err("too many merge inputs");
        }
        if self.block_type != BlockType::Merge && !self.merge_sources.is_empty() {
            return Err("merge inputs are only allowed on merge blocks");
        }
        if self.release_sources.len() > MAX_RELEASE_SOURCES
            || self.release_sources.iter().any(BlockHash::is_zero)
            || self
                .release_sources
                .windows(2)
                .any(|pair| pair[0].as_bytes() >= pair[1].as_bytes())
        {
            return Err("invalid returned-stake sources");
        }
        let limit = if self.block_type == BlockType::Merge {
            MAX_MERGE_BLOCK_BYTES
        } else {
            MAX_BASE_BLOCK_BYTES
        };
        let limit =
            limit + self.release_sources.len() as u64 * 32 + self.delegation_payload.len() as u64;
        let size = bincode::serialized_size(self).map_err(|_| "invalid block encoding")?;
        if size > limit {
            return Err("block exceeds its encoded size limit");
        }
        Ok(())
    }

    /// The grant carried by successors, derived from authenticated chain state.
    pub fn active_signing_grant(&self) -> BlockHash {
        if self.block_type == BlockType::ChangeSigner {
            self.hash
        } else {
            self.signing_grant
        }
    }

    /// Shared by live admission, branch replacement and confirmed replay.
    /// The caller must authenticate the predecessor, not trust an RPC afterimage.
    pub fn validate_signing_grant(
        &self,
        previous: Option<&StateBlock>,
    ) -> Result<(), &'static str> {
        let expected = match previous {
            Some(previous)
                if previous.account == self.account && previous.hash == self.previous =>
            {
                previous.active_signing_grant()
            }
            Some(_) => return Err("signing grant predecessor mismatch"),
            None if self.previous.is_zero() => BlockHash::ZERO,
            None => return Err("signing grant predecessor missing"),
        };
        if self.signing_grant != expected {
            return Err("signing-device grant is no longer active");
        }
        if self.block_type == BlockType::Epoch {
            let previous = previous.ok_or("epoch marker requires an open account")?;
            if self.representative != previous.representative
                || self.brn_balance != previous.brn_balance
                || self.trst_balance != previous.trst_balance
            {
                return Err("epoch marker cannot change balances or representative");
            }
        }
        if self.block_type == BlockType::ChangeSigner {
            let previous = previous.ok_or("signing handoff requires an open account")?;
            if self.representative != previous.representative
                || self.brn_balance != previous.brn_balance
                || self.trst_balance != previous.trst_balance
            {
                return Err("signing handoff cannot change balances or representative");
            }
        }
        Ok(())
    }

    /// Known before constructing the next transaction (decisions 14.4/31.9).
    /// The opening root is account-bound, never a shared all-zero root.
    pub fn work_root(&self) -> BlockHash {
        Self::work_root_for(&self.account, self.previous)
    }

    pub fn work_root_for(account: &WalletAddress, previous: BlockHash) -> BlockHash {
        if !previous.is_zero() {
            return previous;
        }
        let mut bytes = b"BURST/open-work/v2\0".to_vec();
        bytes.extend_from_slice(account.as_str().as_bytes());
        BlockHash::new(blake2b_256(&bytes))
    }

    /// Reserve 40 bytes for the largest block-carrying envelope: bootstrap
    /// response tags, request ID, outer vector and per-block vector lengths.
    /// Do not accept a local block that peers cannot receive (decision 23.15).
    /// Structural size limits are enforced separately by `validate_format`.
    pub fn fits_p2p_message(&self) -> bool {
        bincode::serialized_size(self)
            .is_ok_and(|size| size <= (burst_types::MAX_P2P_MESSAGE_BYTES - 40) as u64)
    }

    /// Explicit ledger dependencies. Address-valued links are not source
    /// blocks; an endorsement or appeal's target_head is. Lifecycle certificates/rewards
    /// still need their own commitments before those paths can be enabled.
    pub fn dependencies(&self) -> impl Iterator<Item = BlockHash> + '_ {
        let proposal = if self.delegation_payload.is_empty() {
            None
        } else {
            burst_governance::key_delegation::operation::DelegationOperation::decode(
                &self.delegation_payload,
            )
            .ok()
            .and_then(|operation| match operation {
                burst_governance::key_delegation::operation::DelegationOperation::Ballot {
                    grant_block,
                    ..
                } => Some(grant_block),
                _ => operation
                    .proposal()
                    .map(|hash| BlockHash::new(*hash.as_bytes())),
            })
        };
        let source = matches!(
            self.block_type,
            BlockType::Open
                | BlockType::Receive
                | BlockType::RejectReceive
                | BlockType::GovernanceEndorse
                | BlockType::GovernanceWithdraw
                | BlockType::GovernanceVote
                | BlockType::GovernanceActivation
        ) && !self.link.is_zero();
        [
            (!self.previous.is_zero()).then_some(self.previous),
            source.then_some(self.link),
            (!self.target_head.is_zero()).then_some(self.target_head),
        ]
        .into_iter()
        .flatten()
        .chain(proposal)
        .chain(self.release_sources.iter().copied())
    }

    /// Compute the hash of this block from its contents.
    ///
    /// Serializes all fields except `signature` and `work` in canonical order,
    /// then hashes with Blake2b-256.
    pub fn compute_hash(&self) -> BlockHash {
        // Serialize fields in canonical order:
        // 0. version (1 byte)
        // 0b. payment_epoch (32 bytes)
        // 1. block_type (u8 enum discriminant)
        // 2. account (string bytes)
        // 3. previous (32 bytes)
        // 4. representative (string bytes)
        // 5. brn_balance (16 bytes, big-endian u128)
        // 6. trst_balance (16 bytes, big-endian u128)
        // 7. link (32 bytes)
        // 7b. origin (32 bytes)
        // 8. transaction (32 bytes)
        // 9. timestamp (8 bytes, big-endian u64)

        let mut buffer = Vec::with_capacity(256);

        // 0. version
        buffer.push(self.version);
        buffer.extend_from_slice(self.signing_grant.as_bytes());
        buffer.extend_from_slice(self.payment_epoch.as_bytes());
        buffer.extend_from_slice(&(self.delegation_payload.len() as u64).to_be_bytes());
        buffer.extend_from_slice(&self.delegation_payload);

        // 1. block_type as u8 discriminant
        let block_type_byte = match self.block_type {
            BlockType::Open => 0,
            BlockType::Burn => 1,
            BlockType::Send => 2,
            BlockType::Receive => 3,
            BlockType::Merge => 5,
            BlockType::Endorse => 6,
            BlockType::Challenge => 7,
            BlockType::GovernanceProposal => 8,
            BlockType::GovernanceVote => 9,
            BlockType::Delegate => 10,
            BlockType::RevokeDelegation => 11,
            BlockType::ChangeRepresentative => 12,
            BlockType::Epoch => 13,
            BlockType::RejectReceive => 14,
            BlockType::VerificationVote => 15,
            BlockType::GovernanceActivation => 16,
            BlockType::VerifierOptIn => 17,
            BlockType::VerifierOptOut => 18,
            BlockType::GovernanceEndorse => 19,
            BlockType::GovernanceWithdraw => 20,
            BlockType::ChangeSigner => 21,
        };
        buffer.push(block_type_byte);

        // 2. account (string bytes)
        buffer.extend_from_slice(self.account.as_str().as_bytes());

        // 3. previous (32 bytes)
        buffer.extend_from_slice(self.previous.as_bytes());

        // 4. representative (string bytes)
        buffer.extend_from_slice(self.representative.as_str().as_bytes());

        // 5. brn_balance (16 bytes, big-endian u128)
        buffer.extend_from_slice(&self.brn_balance.to_be_bytes());

        // 6. trst_balance (16 bytes, big-endian u128)
        buffer.extend_from_slice(&self.trst_balance.to_be_bytes());

        // 7. link (32 bytes)
        buffer.extend_from_slice(self.link.as_bytes());

        // 7b. origin (32 bytes)
        buffer.extend_from_slice(self.origin.as_bytes());

        // 8. transaction (32 bytes)
        buffer.extend_from_slice(self.transaction.as_bytes());
        buffer.extend_from_slice(self.target_head.as_bytes());

        // 9. timestamp (8 bytes, big-endian u64)
        buffer.extend_from_slice(&self.timestamp.as_secs().to_be_bytes());

        // 10. params_hash (32 bytes)
        buffer.extend_from_slice(self.params_hash.as_bytes());

        // 11. merge_sources (u16 BE count + 32 bytes each) — committed into
        // the hash so the merge input set is signed and consensus-final.
        buffer.extend_from_slice(&(self.merge_sources.len() as u16).to_be_bytes());
        for source in &self.merge_sources {
            buffer.extend_from_slice(source.as_bytes());
        }
        buffer.extend_from_slice(&(self.release_sources.len() as u16).to_be_bytes());
        for source in &self.release_sources {
            buffer.extend_from_slice(source.as_bytes());
        }

        // Hash the concatenated bytes
        let hash_bytes = blake2b_256(&buffer);
        BlockHash::new(hash_bytes)
    }

    /// Verify this block's proof-of-work meets the minimum difficulty.
    pub fn verify_work(&self, min_difficulty: u64) -> bool {
        self.version == CURRENT_BLOCK_VERSION
            && validate_work(&self.work_root(), self.work, min_difficulty)
    }

    /// Whether this is the first block in an account chain.
    pub fn is_open(&self) -> bool {
        self.block_type == BlockType::Open
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::{BlockHash, Signature, Timestamp, TxHash, WalletAddress};
    use burst_work::WorkGenerator;

    #[test]
    fn signing_handoff_preserves_account_and_invalidates_stale_grant() {
        let mut head = create_test_block();
        head.hash = head.compute_hash();
        let mut handoff = head.clone();
        handoff.block_type = BlockType::ChangeSigner;
        handoff.previous = head.hash;
        handoff.link = BlockHash::new([42; 32]);
        handoff.hash = handoff.compute_hash();
        handoff.validate_format().unwrap();
        handoff.validate_signing_grant(Some(&head)).unwrap();
        assert_eq!(handoff.active_signing_grant(), handoff.hash);

        let mut next = handoff.clone();
        next.block_type = BlockType::ChangeRepresentative;
        next.previous = handoff.hash;
        next.link = BlockHash::ZERO;
        assert!(next.validate_signing_grant(Some(&handoff)).is_err());
        next.signing_grant = handoff.hash;
        next.hash = next.compute_hash();
        next.validate_signing_grant(Some(&handoff)).unwrap();
        assert_eq!(next.active_signing_grant(), handoff.hash);
        let encoded = bincode::serialize(&next).unwrap();
        let restored: StateBlock = bincode::deserialize(&encoded).unwrap();
        assert_eq!(restored.active_signing_grant(), handoff.hash);

        let mut second = handoff.clone();
        second.previous = next.hash;
        second.signing_grant = handoff.hash;
        second.link = BlockHash::new([43; 32]);
        second.hash = second.compute_hash();
        second.validate_signing_grant(Some(&next)).unwrap();
        assert_ne!(second.active_signing_grant(), handoff.hash);
        second.trst_balance += 1;
        assert!(second.validate_signing_grant(Some(&next)).is_err());
        assert!(handoff.validate_signing_grant(None).is_err());
    }

    #[test]
    fn signing_grant_and_handoff_nonce_are_hash_bound() {
        let block = create_test_block();
        let mut changed = block.clone();
        changed.signing_grant = BlockHash::new([9; 32]);
        assert_ne!(block.compute_hash(), changed.compute_hash());
        assert!(changed.validate_signing_grant(None).is_err());
        changed.block_type = BlockType::ChangeSigner;
        let first = changed.compute_hash();
        changed.link = BlockHash::new([8; 32]);
        assert_ne!(first, changed.compute_hash());
    }

    fn create_test_block() -> StateBlock {
        StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: CURRENT_BLOCK_VERSION,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            block_type: BlockType::Open,
            account: WalletAddress::new(
                "brst_1111111111111111111111111111111111111111111111111111111111111111111",
            ),
            previous: BlockHash::ZERO,
            representative: WalletAddress::new(
                "brst_2222222222222222222222222222222222222222222222222222222222222222222",
            ),
            brn_balance: 1000,
            trst_balance: 500,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1234567890),
            params_hash: BlockHash::ZERO,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: Signature([0u8; 64]),
            hash: BlockHash::ZERO,
        }
    }

    #[test]
    fn payment_epoch_is_signed_and_roundtrips() {
        let mut block = create_test_block();
        let original = block.compute_hash();
        let keys = burst_crypto::keypair_from_seed(&[42; 32]);
        let signature = burst_crypto::sign_message(original.as_bytes(), &keys.private);
        block.payment_epoch = BlockHash::new([77; 32]);
        assert_ne!(block.compute_hash(), original);
        assert!(!burst_crypto::verify_signature(
            block.compute_hash().as_bytes(),
            &signature,
            &keys.public
        ));
        let decoded: StateBlock =
            bincode::deserialize(&bincode::serialize(&block).unwrap()).unwrap();
        assert_eq!(decoded.payment_epoch, block.payment_epoch);
        assert_eq!(decoded.compute_hash(), block.compute_hash());
    }

    #[test]
    fn endorsement_target_head_is_signed_and_a_causal_dependency() {
        let mut block = create_test_block();
        block.block_type = BlockType::Endorse;
        block.previous = BlockHash::new([1; 32]);
        block.link = BlockHash::new([2; 32]);
        let without_anchor = block.compute_hash();
        block.target_head = BlockHash::new([3; 32]);
        assert_ne!(block.compute_hash(), without_anchor);
        assert_eq!(
            block.dependencies().collect::<Vec<_>>(),
            vec![block.previous, block.target_head]
        );
        assert!(block.validate_format().is_ok());
        let decoded: StateBlock =
            bincode::deserialize(&bincode::serialize(&block).unwrap()).unwrap();
        assert_eq!(decoded.target_head, block.target_head);
        for kind in [
            BlockType::Open,
            BlockType::Send,
            BlockType::Receive,
            BlockType::Merge,
        ] {
            block.block_type = kind;
            assert_eq!(
                block.validate_format(),
                Err("target reference is only allowed on endorsement or challenge blocks")
            );
        }
        block.block_type = BlockType::Endorse;
        block.version = 2;
        assert_eq!(block.validate_format(), Err("unsupported block version"));
    }

    #[test]
    fn appeal_outcome_reference_is_signed_and_causal() {
        let mut block = create_test_block();
        block.block_type = BlockType::Challenge;
        block.previous = BlockHash::new([1; 32]);
        let ordinary = block.compute_hash();
        block.target_head = BlockHash::new([2; 32]);
        assert_ne!(block.compute_hash(), ordinary);
        assert!(block.validate_format().is_ok());
        assert_eq!(
            block.dependencies().collect::<Vec<_>>(),
            vec![block.previous, block.target_head]
        );
        let decoded: StateBlock =
            bincode::deserialize(&bincode::serialize(&block).unwrap()).unwrap();
        assert_eq!(decoded.target_head, block.target_head);
        assert_eq!(decoded.compute_hash(), block.compute_hash());
    }

    #[test]
    fn returned_stake_sources_are_signed_canonical_dependencies() {
        let mut block = create_test_block();
        let empty = block.compute_hash();
        block.release_sources = vec![BlockHash::new([1; 32]), BlockHash::new([2; 32])];
        let signed = block.compute_hash();
        assert_ne!(signed, empty);
        assert_eq!(
            block.dependencies().collect::<Vec<_>>(),
            block.release_sources
        );
        assert!(block.validate_format().is_ok());
        let decoded: StateBlock =
            bincode::deserialize(&bincode::serialize(&block).unwrap()).unwrap();
        assert_eq!(decoded.release_sources, block.release_sources);
        let key = burst_crypto::keypair_from_seed(&[42; 32]);
        let signature = burst_crypto::sign_message(signed.as_bytes(), &key.private);
        block.release_sources[1] = BlockHash::new([3; 32]);
        assert!(!burst_crypto::verify_signature(
            block.compute_hash().as_bytes(),
            &signature,
            &key.public
        ));
        for sources in [
            vec![BlockHash::ZERO],
            vec![BlockHash::new([1; 32]); 2],
            vec![BlockHash::new([2; 32]), BlockHash::new([1; 32])],
        ] {
            block.release_sources = sources;
            assert_eq!(
                block.validate_format(),
                Err("invalid returned-stake sources")
            );
        }
        block.release_sources.clear();
        block.version = 3;
        assert_eq!(block.validate_format(), Err("unsupported block version"));
    }

    #[test]
    fn full_release_proof_and_merge_fit_the_wire_budget() {
        let mut block = create_test_block();
        block.block_type = BlockType::Merge;
        block.merge_sources = vec![TxHash::ZERO; MAX_MERGE_INPUTS];
        block.release_sources = (1..=MAX_RELEASE_SOURCES)
            .map(|index| {
                let mut bytes = [0; 32];
                bytes[..8].copy_from_slice(&(index as u64).to_be_bytes());
                BlockHash::new(bytes)
            })
            .collect();
        assert!(block.validate_format().is_ok());
        assert!(block.fits_p2p_message());
        block.release_sources.push(BlockHash::new([255; 32]));
        assert_eq!(
            block.validate_format(),
            Err("invalid returned-stake sources")
        );
    }

    #[test]
    fn format_preserves_256_input_merges_but_not_arbitrary_payloads() {
        let mut block = create_test_block();
        assert!(block.validate_format().is_ok());
        block.block_type = BlockType::Merge;
        block.merge_sources = (0..MAX_MERGE_INPUTS)
            .map(|i| {
                let mut bytes = [0; 32];
                bytes[..8].copy_from_slice(&(i as u64).to_be_bytes());
                TxHash::new(bytes)
            })
            .collect();
        assert!(bincode::serialized_size(&block).unwrap() > MAX_BASE_BLOCK_BYTES);
        assert!(block.validate_format().is_ok());
        assert!(block.fits_p2p_message());
        block.merge_sources.push(TxHash::ZERO);
        assert_eq!(block.validate_format(), Err("too many merge inputs"));
        block.merge_sources.pop();
        block.block_type = BlockType::Send;
        assert_eq!(
            block.validate_format(),
            Err("merge inputs are only allowed on merge blocks")
        );
        block.merge_sources.clear();
        block.account = WalletAddress::new(format!(
            "brst_{}",
            "x".repeat(MAX_BASE_BLOCK_BYTES as usize)
        ));
        assert_eq!(
            block.validate_format(),
            Err("block exceeds its encoded size limit")
        );
    }

    #[test]
    fn opening_work_is_account_bound_and_transaction_independent() {
        let mut a = create_test_block();
        let root = a.work_root();
        a.link = BlockHash::new([1; 32]);
        a.timestamp = Timestamp::new(999);
        a.trst_balance = 700;
        assert_eq!(a.work_root(), root);
        a.account = WalletAddress::new("brst_another-account");
        assert_ne!(a.work_root(), root);
        a.previous = BlockHash::new([2; 32]);
        assert_eq!(a.work_root(), a.previous);
    }

    #[test]
    fn work_generated_before_transaction_construction_remains_valid() {
        let mut block = create_test_block();
        block.previous = BlockHash::new([8; 32]);
        let threshold = 0xf000_0000_0000_0000;
        block.work = WorkGenerator
            .generate(&block.work_root(), threshold)
            .unwrap()
            .0;
        block.timestamp = Timestamp::new(54321);
        block.trst_balance = 999;
        block.hash = block.compute_hash();
        assert!(block.verify_work(threshold));
    }

    #[test]
    fn new_block_hash_work_is_not_a_fallback() {
        let mut block = create_test_block();
        block.previous = BlockHash::new([8; 32]);
        block.hash = block.compute_hash();
        let root = block.work_root();
        let threshold = 0xf000_0000_0000_0000;
        let wrong_nonce = (0..100_000)
            .find(|nonce| {
                validate_work(&block.hash, *nonce, threshold)
                    && !validate_work(&root, *nonce, threshold)
            })
            .unwrap();
        block.work = wrong_nonce;
        assert!(!block.verify_work(threshold));
        block.work = (0..100_000)
            .find(|nonce| {
                validate_work(&root, *nonce, threshold)
                    && !validate_work(&block.hash, *nonce, threshold)
            })
            .unwrap();
        assert!(block.verify_work(threshold));
        block.version = 1;
        assert!(!block.verify_work(0));
        assert_eq!(block.validate_format(), Err("unsupported block version"));
    }

    #[test]
    fn scoped_key_payload_is_signed_bounded_and_never_authorizes_spending() {
        use burst_governance::key_delegation::{operation::DelegationOperation, Grant, KeyScope};
        let owner = burst_crypto::keypair_from_seed(&[1; 32]);
        let recipient = burst_crypto::keypair_from_seed(&[2; 32]);
        let proposal = TxHash::new([3; 32]);
        let grant = Grant::create(
            &owner,
            recipient.public.clone(),
            KeyScope::Proposal(proposal),
            1,
            burst_types::NetworkId::Dev,
            BlockHash::new([4; 32]),
        )
        .unwrap();
        let mut block = create_test_block();
        block.account = burst_crypto::derive_address(&owner.public);
        block.block_type = BlockType::Delegate;
        block.link = BlockHash::new(recipient.public.0);
        block.origin = TxHash::ZERO;
        let old_hash = block.compute_hash();
        block.delegation_payload = DelegationOperation::Grant(grant).encode().unwrap();
        assert!(block.validate_format().is_ok());
        assert!(block
            .dependencies()
            .any(|hash| hash.into_tx_hash() == proposal));
        assert_ne!(block.compute_hash(), old_hash);
        block.hash = block.compute_hash();
        block.signature = burst_crypto::sign_message(block.hash.as_bytes(), &owner.private);
        let decoded: StateBlock =
            bincode::deserialize(&bincode::serialize(&block).unwrap()).unwrap();
        assert_eq!(decoded.delegation_payload, block.delegation_payload);
        for kind in [
            BlockType::Send,
            BlockType::Burn,
            BlockType::GovernanceVote,
            BlockType::RevokeDelegation,
        ] {
            block.block_type = kind;
            assert!(block.validate_format().is_err());
        }
        block.block_type = BlockType::Delegate;
        block.delegation_payload.push(0);
        assert!(block.validate_format().is_err());
        block.delegation_payload = vec![0; 1025];
        assert!(block.validate_format().is_err());
    }

    #[test]
    fn test_compute_hash_deterministic() {
        let block = create_test_block();
        let hash1 = block.compute_hash();
        let hash2 = block.compute_hash();

        // Same block should produce same hash
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_compute_hash_different_blocks() {
        let block1 = create_test_block();
        let mut block2 = create_test_block();

        // Change one field
        block2.brn_balance = 2000;

        let hash1 = block1.compute_hash();
        let hash2 = block2.compute_hash();

        // Different blocks should produce different hashes
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_compute_hash_different_block_types() {
        let mut block1 = create_test_block();
        let mut block2 = create_test_block();

        block1.block_type = BlockType::Open;
        block2.block_type = BlockType::Burn;

        let hash1 = block1.compute_hash();
        let hash2 = block2.compute_hash();

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_compute_hash_different_accounts() {
        let mut block1 = create_test_block();
        let mut block2 = create_test_block();

        block1.account = WalletAddress::new(
            "brst_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        block2.account = WalletAddress::new(
            "brst_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        );

        let hash1 = block1.compute_hash();
        let hash2 = block2.compute_hash();

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_compute_hash_excludes_signature_and_work() {
        let block1 = create_test_block();
        let mut block2 = create_test_block();

        // Change signature and work (should not affect hash)
        block2.signature = Signature([0xFFu8; 64]);
        block2.work = 999999;

        let hash1 = block1.compute_hash();
        let hash2 = block2.compute_hash();

        // Hash should be the same since signature and work are excluded
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_verify_work_with_valid_nonce() {
        let mut block = create_test_block();

        // Compute hash first
        block.hash = block.compute_hash();

        // Generate valid work for a low difficulty
        let min_difficulty = 1000;
        let generator = WorkGenerator;
        let work_nonce = generator
            .generate(&block.work_root(), min_difficulty)
            .unwrap();

        // Set the work nonce
        block.work = work_nonce.0;

        // Verify work should pass
        assert!(block.verify_work(min_difficulty));
    }

    #[test]
    fn test_verify_work_with_invalid_nonce() {
        let mut block = create_test_block();
        block.hash = block.compute_hash();

        // Use a random nonce that likely won't meet high difficulty
        block.work = 12345;

        // With very high difficulty, this should fail
        assert!(!block.verify_work(u64::MAX));
    }

    #[test]
    fn test_verify_work_zero_difficulty() {
        let mut block = create_test_block();
        block.hash = block.compute_hash();
        block.work = 0;

        // Zero difficulty should always pass
        assert!(block.verify_work(0));
    }
}
