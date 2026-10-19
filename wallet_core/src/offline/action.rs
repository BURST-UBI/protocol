use super::OfflinePaymentContext;
use crate::{transaction_builder::*, WalletError};
use burst_governance::{key_delegation::view, ProposalContent};
use burst_ledger::{BlockType, StateBlock};
use burst_transactions::{governance::GovernanceVote, Transaction};
use burst_types::{appeal::AppealReview, BlockHash, ChallengeReason, KeyPair, Timestamp, TxHash, WalletAddress};

/// Public intent, not a mutable unsigned block. Serialize as an opaque string at
/// the JS bridge so raw amounts are never rounded through JavaScript numbers.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum OfflineAction {
    Transfer { to: WalletAddress, amount: u128, origin: TxHash },
    Merge { selected: Vec<crate::portfolio::MergeSelection> },
    Reject { payment: TxHash },
    Endorse { target: WalletAddress, burn: u128 },
    Challenge { target: WalletAddress, stake: u128, reason: ChallengeReason },
    Appeal { reviewed: AppealReview },
    VerificationVote { target: WalletAddress, context: BlockHash, vote: u8, stake: u128 },
    Proposal { content: ProposalContent },
    GovernanceVote { proposal: TxHash, vote: GovernanceVote },
    GovernanceEndorse { proposal: TxHash, burn: u128 },
    GovernanceWithdraw { proposal: TxHash },
    VerifierParticipation { enabled: bool },
    Delegation { scope: view::ScopeChoice, delegate: Option<WalletAddress>, generation: String, genesis: String },
    DelegatedVote { entry: view::Entry, proposal: TxHash, vote: GovernanceVote },
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct OfflineActionContext {
    pub(crate) signing: OfflinePaymentContext,
    pub(crate) action: OfflineAction,
    pub(crate) target_head: Option<BlockHash>,
    pub(crate) review: serde_json::Value,
}

pub struct SignedOfflineAction {
    block: StateBlock,
    ancestry: Vec<StateBlock>,
    content: Option<Vec<u8>>,
}

impl SignedOfflineAction {
    pub fn block(&self) -> &StateBlock { &self.block }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn save(&self, path: &std::path::Path) -> Result<(), WalletError> {
        super::save_signed_action(path, &self.block, &self.ancestry, self.content.as_deref())
    }
}

impl OfflineActionContext {
    pub fn action(&self) -> &OfflineAction { &self.action }
    pub fn review(&self) -> &serde_json::Value { &self.review }

    /// Pure construction: no clock, node client, work request or submission.
    pub fn sign(&self, keys: &KeyPair, now: Timestamp) -> Result<SignedOfflineAction, WalletError> {
        let signing = &self.signing;
        super::validate_ancestry(&signing.account, signing.state.head, &signing.ancestry)?;
        if burst_crypto::public_from_private(&keys.private) != keys.public
            || burst_crypto::derive_address(&keys.public) != signing.account
            || signing.state.head.is_zero() || signing.state.payment_epoch.is_zero()
            || signing.params_hash.is_zero() {
            return Err(WalletError::Signing("offline action account or signing context mismatch".into()));
        }
        if self.review.get("phase_deadline").and_then(serde_json::Value::as_u64)
            .is_some_and(|deadline| now.as_secs() >= deadline) {
            return Err(WalletError::TransactionBuild("the saved proposal phase has ended; prepare a fresh review".into()));
        }
        let account = &signing.account;
        let mut content = None;
        let mut block = match &self.action {
            OfflineAction::Proposal { content: proposal } => {
                let (block, encoded) = crate::governance_content::build_signed_proposal(
                    &signing.state, keys, proposal, signing.params_hash, now)?;
                content = Some(encoded.bytes().to_vec());
                block
            }
            OfflineAction::Delegation { scope, delegate, generation, genesis } => {
                let page: view::Page = serde_json::from_value(self.review.clone())
                    .map_err(|error| WalletError::TransactionBuild(error.to_string()))?;
                if page.genesis != *genesis {
                    return Err(WalletError::TransactionBuild("delegation network mismatch".into()));
                }
                let operation = crate::delegation::build_scoped_operation(keys, scope, delegate.as_ref(), generation, &page)?;
                crate::delegation::build_key_operation(&signing.state, keys, &operation, signing.params_hash, now)?
            }
            OfflineAction::DelegatedVote { entry, proposal, vote } => {
                let operation = crate::delegation::build_delegated_ballot(keys, entry, *proposal, *vote)?;
                crate::delegation::build_key_operation(&signing.state, keys, &operation, signing.params_hash, now)?
            }
            action => {
                let tx = match action {
                    OfflineAction::Transfer { to, amount, origin } => {
                        let batches = self.review["batches"].as_array().ok_or_else(|| WalletError::TransactionBuild("missing prepared TRST portfolio".into()))?;
                        crate::portfolio::select_transfer_origin(batches, *amount, Some(&origin.to_string())).map_err(WalletError::TransactionBuild)?;
                        Transaction::Send(build_send_tx(account, to, *amount, signing.state.head.into_tx_hash(), *origin, now)?)
                    }
                    OfflineAction::Merge { selected } => {
                        let batches = self.review["batches"].as_array().ok_or_else(|| WalletError::TransactionBuild("missing prepared TRST portfolio".into()))?;
                        let sources = crate::portfolio::reviewed_merge_sources(batches, selected).map_err(WalletError::TransactionBuild)?;
                        Transaction::Merge(build_merge_tx(account, sources, now)?)
                    }
                    OfflineAction::Reject { payment } => {
                        if payment.is_zero() { return Err(WalletError::TransactionBuild("missing payment".into())); }
                        Transaction::RejectReceive(build_reject_receive_tx(account, *payment, now)?)
                    }
                    OfflineAction::Endorse { target, burn } => {
                        if *burn == 0 { return Err(WalletError::TransactionBuild("endorsement burn must be positive".into())); }
                        Transaction::Endorse(build_endorse_tx(account, target, *burn, now)?)
                    }
                    OfflineAction::Challenge { target, stake, reason } =>
                        Transaction::Challenge(build_challenge_tx(account, target, *stake, *reason, now)?),
                    OfflineAction::Appeal { reviewed } => crate::appeals::prepare(reviewed, reviewed, account, now)?,
                    OfflineAction::VerificationVote { target, context, vote, stake } =>
                        Transaction::VerificationVote(build_verification_vote_tx(account, target, *context, *vote, *stake, now)?),
                    OfflineAction::GovernanceVote { proposal, vote } => {
                        if proposal.is_zero() { return Err(WalletError::TransactionBuild("missing proposal".into())); }
                        Transaction::GovernanceVote(build_governance_vote_tx(account, *proposal, *vote, now)?)
                    }
                    OfflineAction::GovernanceEndorse { .. } | OfflineAction::GovernanceWithdraw { .. }
                        | OfflineAction::VerifierParticipation { .. } =>
                        Transaction::ChangeRepresentative(build_change_rep_tx(account, &signing.state.representative, now)?),
                    _ => unreachable!(),
                };
                let origin = match action { OfflineAction::Transfer { origin, .. } => *origin, _ => TxHash::ZERO };
                let mut block = build_state_block(&signing.state, &tx, origin, signing.params_hash)?;
                match action {
                    OfflineAction::Endorse { .. } => {
                        block.target_head = self.target_head.ok_or_else(||
                            WalletError::TransactionBuild("missing prepared endorsement target head".into()))?;
                    }
                    OfflineAction::GovernanceEndorse { proposal, burn } => {
                        if proposal.is_zero() || *burn == 0 {
                            return Err(WalletError::TransactionBuild("invalid proposal endorsement".into()));
                        }
                        block.block_type = BlockType::GovernanceEndorse;
                        block.link = BlockHash::new(*proposal.as_bytes());
                        block.brn_balance = signing.state.brn_balance.checked_add(*burn)
                            .ok_or_else(|| WalletError::TransactionBuild("BRN spend overflow".into()))?;
                    }
                    OfflineAction::GovernanceWithdraw { proposal } => {
                        if proposal.is_zero() { return Err(WalletError::TransactionBuild("missing proposal".into())); }
                        block.block_type = BlockType::GovernanceWithdraw;
                        block.link = BlockHash::new(*proposal.as_bytes());
                    }
                    OfflineAction::VerifierParticipation { enabled } => {
                        block.block_type = if *enabled { BlockType::VerifierOptIn } else { BlockType::VerifierOptOut };
                    }
                    _ => {}
                }
                block
            }
        };
        block.hash = block.compute_hash();
        if !signing.work.apply_to(&mut block, signing.threshold) {
            return Err(WalletError::TransactionBuild("offline action has invalid prepared work".into()));
        }
        block.validate_format().map_err(|error| WalletError::TransactionBuild(error.into()))?;
        block = sign_state_block(block, &keys.private);
        super::validate_signed_action(&block)?;
        Ok(SignedOfflineAction { block, ancestry:signing.ancestry.clone(), content })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_trst_actions_preserve_origin_selection_balances_and_journal() {
        let keys = burst_crypto::keypair_from_seed(&[1;32]);
        let receiver = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[2;32]).public);
        let origin = TxHash::new([6;32]);
        let payment = TxHash::new([9;32]);
        let selected: Vec<_> = [7u8,8].into_iter().map(|id| crate::portfolio::MergeSelection {
            id:TxHash::new([id;32]).to_string(),amount:"7".into(),expiry_at:Some(100),status:"active".into(),
        }).collect();
        let batches = serde_json::to_value(&selected).unwrap();
        let actions = [
            (OfflineAction::Transfer {to:receiver,amount:3,origin},BlockType::Send,17,serde_json::json!({"batches":[{"origin":origin.to_string(),"amount":"20","status":"active"}]})),
            (OfflineAction::Merge {selected:selected.clone()},BlockType::Merge,20,serde_json::json!({"batches":batches})),
            (OfflineAction::Reject {payment},BlockType::RejectReceive,20,serde_json::json!({"payment":payment.to_string(),"amount":"3"})),
        ];
        for (action,kind,balance,review) in actions {
            let context = OfflineActionContext {signing:crate::offline::tests::context(&keys),action,target_head:None,review};
            let encoded=serde_json::to_string(&context).unwrap();
            let restored:OfflineActionContext=serde_json::from_str(&encoded).unwrap();
            let signed=restored.sign(&keys,Timestamp::new(42)).unwrap();
            assert_eq!(signed.block.block_type,kind);
            assert_eq!(signed.block.trst_balance,balance);
            assert_eq!(signed.block.brn_balance,11);
            assert_eq!(signed.block.previous,restored.signing.state.head);
            match kind {
                BlockType::Send=>assert_eq!(signed.block.origin,origin),
                BlockType::Merge=>assert_eq!(signed.block.merge_sources,vec![TxHash::new([7;32]),TxHash::new([8;32])]),
                BlockType::RejectReceive=>assert_eq!(signed.block.link,BlockHash::new(*payment.as_bytes())),
                _=>unreachable!(),
            }
            let directory=tempfile::tempdir().unwrap();let path=directory.path().join("action.json");
            signed.save(&path).unwrap();
            let (loaded,content)=super::super::load_action_with_content(&path).unwrap();
            assert_eq!(loaded.hash,signed.block.hash);assert!(content.is_none());
        }
        let mut context=OfflineActionContext {signing:crate::offline::tests::context(&keys),action:OfflineAction::Merge {selected},target_head:None,review:serde_json::json!({"batches":batches})};
        context.review["batches"][0]["amount"]=serde_json::json!("8");
        assert!(context.sign(&keys,Timestamp::new(42)).is_err());
    }

    #[test]
    fn offline_delegation_creates_grant_revocation_and_secondary_ballot() {
        use burst_governance::key_delegation::operation::DelegationOperation;
        let owner = burst_crypto::keypair_from_seed(&[1; 32]);
        let recipient = burst_crypto::keypair_from_seed(&[2; 32]);
        let delegate = burst_crypto::derive_address(&recipient.public);
        let scope = view::ScopeChoice::Parameter("brn_rate".into());
        let mut page = view::Page { enabled:true, network:burst_types::NetworkId::Dev,
            genesis:BlockHash::new([9; 32]).to_string(), entries:vec![], cursor:None, parameters:vec![] };
        let mut context = OfflineActionContext { signing:crate::offline::tests::context(&owner),
            action:OfflineAction::Delegation { scope:scope.clone(), delegate:Some(delegate.clone()),
                generation:"0".into(), genesis:page.genesis.clone() },
            target_head:None, review:serde_json::to_value(&page).unwrap() };
        let encoded = serde_json::to_string(&context).unwrap();
        context = serde_json::from_str(&encoded).unwrap();
        let signed = context.sign(&owner, Timestamp::new(42)).unwrap();
        assert_eq!(signed.block.block_type, BlockType::Delegate);
        let DelegationOperation::Grant(grant) = DelegationOperation::decode(&signed.block.delegation_payload).unwrap() else { panic!("expected grant") };
        grant.verify().unwrap();
        assert_eq!(grant.generation, 1);
        assert_eq!(grant.delegate, recipient.public);
        let entry = view::Entry { owner:burst_crypto::derive_address(&owner.public).to_string(),
            scope:scope.clone(), generation:"1".into(), delegate:Some(delegate.to_string()),
            grant_block:Some(signed.block.hash.to_string()), grant:Some(grant), confirmed:true };
        let proposal = TxHash::new([8; 32]);
        let ballot_context = OfflineActionContext { signing:crate::offline::tests::context(&recipient),
            action:OfflineAction::DelegatedVote { entry:entry.clone(), proposal, vote:GovernanceVote::Yea },
            target_head:None, review:serde_json::json!({"phase_deadline":100}) };
        let ballot = ballot_context.sign(&recipient, Timestamp::new(43)).unwrap();
        let DelegationOperation::Ballot { ballot, grant_block } = DelegationOperation::decode(&ballot.block.delegation_payload).unwrap() else { panic!("expected ballot") };
        assert_eq!(grant_block, signed.block.hash);
        assert_eq!(ballot.proposal, proposal);
        page.entries.push(entry);
        context.review = serde_json::to_value(&page).unwrap();
        assert!(context.sign(&owner, Timestamp::new(44)).is_err());
        context.action = OfflineAction::Delegation { scope, delegate:None, generation:"1".into(), genesis:page.genesis };
        let revoked = context.sign(&owner, Timestamp::new(44)).unwrap();
        assert_eq!(revoked.block.block_type, BlockType::RevokeDelegation);
        let DelegationOperation::Revoke(revocation) = DelegationOperation::decode(&revoked.block.delegation_payload).unwrap() else { panic!("expected revocation") };
        assert_eq!(revocation.generation, 2);
        DelegationOperation::Revoke(revocation).verify().unwrap();
    }

    #[test]
    fn offline_actions_roundtrip_sign_and_journal_the_reviewed_operation() {
        let keys = burst_crypto::keypair_from_seed(&[1; 32]);
        let target = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[2; 32]).public);
        let proposal = TxHash::new([3; 32]);
        let selection = BlockHash::new([4; 32]);
        let outcome = BlockHash::new([5; 32]);
        let target_head = BlockHash::new([6; 32]);
        let review = AppealReview { account:target.to_string(), enabled:true, challenge_active:false,
            required_stake:"3".into(), outcome:Some(burst_types::appeal::AppealOutcome {
                hash:outcome.to_string(), checkpoint:BlockHash::new([7; 32]).to_string(),
                reason:ChallengeReason::Fraud, legitimate:false, completed_at:1,
            }) };
        let content = ProposalContent::ParameterChange { param:burst_governance::GovernableParam::BrnRate,
            new_value:u64::MAX as u128 + 17 };
        let cases = [
            (OfflineAction::Endorse { target:target.clone(), burn:1 }, BlockType::Endorse, 12),
            (OfflineAction::Challenge { target:target.clone(), stake:2, reason:ChallengeReason::Fraud }, BlockType::Challenge, 13),
            (OfflineAction::Appeal { reviewed:review }, BlockType::Challenge, 14),
            (OfflineAction::VerificationVote { target:target.clone(), context:selection, vote:1, stake:3 }, BlockType::VerificationVote, 14),
            (OfflineAction::Proposal { content }, BlockType::GovernanceProposal, 11),
            (OfflineAction::GovernanceVote { proposal, vote:GovernanceVote::Abstain }, BlockType::GovernanceVote, 11),
            (OfflineAction::GovernanceEndorse { proposal, burn:2 }, BlockType::GovernanceEndorse, 13),
            (OfflineAction::GovernanceWithdraw { proposal }, BlockType::GovernanceWithdraw, 11),
            (OfflineAction::VerifierParticipation { enabled:true }, BlockType::VerifierOptIn, 11),
            (OfflineAction::VerifierParticipation { enabled:false }, BlockType::VerifierOptOut, 11),
        ];
        for (action, kind, spent) in cases {
            let context = OfflineActionContext { signing:crate::offline::tests::context(&keys),
                action, target_head:Some(target_head), review:serde_json::json!({"phase_deadline":100}) };
            let encoded = serde_json::to_string(&context).unwrap();
            let restored: OfflineActionContext = serde_json::from_str(&encoded).unwrap();
            let signed = restored.sign(&keys, Timestamp::new(42)).unwrap();
            let block = signed.block();
            assert_eq!(block.block_type, kind);
            assert_eq!(block.brn_balance, spent);
            assert_eq!(block.trst_balance, 20);
            assert_eq!(block.previous, restored.signing.state.head);
            assert_eq!(block.payment_epoch, restored.signing.state.payment_epoch);
            assert_eq!(block.timestamp, Timestamp::new(42));
            assert_eq!(block.signing_grant, restored.signing.state.signing_grant);
            super::super::validate_signed_action(block).unwrap();
            match restored.action() {
                OfflineAction::Endorse { .. } => assert_eq!(block.target_head, target_head),
                OfflineAction::Appeal { .. } => assert_eq!(block.target_head, outcome),
                OfflineAction::VerificationVote { .. } => {
                    assert_eq!(block.origin, selection.into_tx_hash());
                    assert_eq!(block.transaction.as_bytes()[0], 1);
                }
                OfflineAction::GovernanceVote { .. } => assert_eq!(block.transaction.as_bytes()[0], 2),
                OfflineAction::Proposal { content } => {
                    let manifest = burst_governance::content::ContentManifest::from_block_fields(
                        block.link, block.origin, block.transaction).unwrap().unwrap();
                    let actual = manifest.decode_verified(signed.content.as_deref().unwrap()).unwrap();
                    assert_eq!(&actual, content);
                }
                _ => {}
            }
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("action.json");
            signed.save(&path).unwrap();
            let (loaded, content) = super::super::load_action_with_content(&path).unwrap();
            assert_eq!(serde_json::to_string(&loaded).unwrap(), serde_json::to_string(block).unwrap());
            assert_eq!(content, signed.content);
            assert!(!super::super::payment_was_submitted(&path, &block.hash).unwrap());
            assert!(restored.sign(&keys, Timestamp::new(100)).is_err());
            assert!(restored.sign(&burst_crypto::keypair_from_seed(&[9; 32]), Timestamp::new(42)).is_err());
        }
    }

    #[test]
    fn offline_actions_reject_missing_target_context_and_invalid_work() {
        let keys = burst_crypto::keypair_from_seed(&[1; 32]);
        let mut context = OfflineActionContext { signing:crate::offline::tests::context(&keys),
            action:OfflineAction::Endorse { target:burst_crypto::derive_address(&keys.public), burn:1 },
            target_head:None, review:serde_json::Value::Null };
        assert!(context.sign(&keys, Timestamp::new(42)).is_err());
        context.target_head = Some(BlockHash::ZERO);
        context.signing.work.root = BlockHash::ZERO;
        assert!(context.sign(&keys, Timestamp::new(42)).is_err());
    }
}
