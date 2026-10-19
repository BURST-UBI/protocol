//! Real wallet submission path. Keys remain on the client; the node only sees
//! signed blocks and a hash for work generation. No faucet/convenience RPCs.
use crate::{transaction_builder::*, wallet::ProcessResult, NodeClient, WalletError};
use burst_ledger::{BlockType, StateBlock};
use burst_transactions::Transaction;
use burst_types::{BlockHash, KeyPair, Timestamp, TxHash, WalletAddress};
use serde_json::{json, Value};
use std::time::Duration;
#[cfg(test)]
use std::time::Instant;

/// Work prepared for an account position before choosing the next transaction.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PreparedWork {
    pub root: BlockHash,
    pub nonce: u64,
}

impl PreparedWork {
    pub(crate) fn apply_to(&self, block: &mut StateBlock, threshold: u64) -> bool {
        if block.version != burst_ledger::CURRENT_BLOCK_VERSION
            || self.root != block.work_root()
            || !burst_work::validate_work(&self.root, self.nonce, threshold)
        {
            return false;
        }
        block.work = self.nonce;
        true
    }
}

fn work_parameters(params: &Value) -> Result<u64, WalletError> {
    match params["online_signing"].as_str() {
        Some("local" | "peer_matches") => {},
        Some("catching_up" | "peer_differs" | "unknown") => return Err(WalletError::Node(
            "node synchronization is not ready; wait for catch-up or connect to a synchronized node before signing".into(),
        )),
        _ => return Err(WalletError::Node(
            "node did not provide supported signing readiness; update the node and wallet together".into(),
        )),
    }
    if params["block_version"].as_u64() != Some(burst_ledger::CURRENT_BLOCK_VERSION as u64) {
        return Err(WalletError::Node(
            "node uses incompatible block rules; update the node and wallet together".into(),
        ));
    }
    payment_epoch(params)?;
    let value = params["min_work_difficulty"]
        .as_str()
        .ok_or_else(|| WalletError::Node("missing work difficulty".into()))?;
    u64::from_str_radix(value, 16).map_err(|_| WalletError::Node("invalid work difficulty".into()))
}

fn payment_epoch(value: &Value) -> Result<BlockHash, WalletError> {
    let epoch = parse_hash(
        value["payment_epoch"]
            .as_str()
            .ok_or_else(|| WalletError::Node("node did not provide a payment epoch".into()))?,
    )?;
    if epoch.is_zero() {
        return Err(WalletError::Node(
            "node returned an empty payment epoch".into(),
        ));
    }
    Ok(epoch)
}

fn require_payment_epoch(params: &Value, expected: BlockHash) -> Result<(), WalletError> {
    if payment_epoch(params)? != expected {
        return Err(WalletError::Node(
            "payment epoch changed while preparing this action; refresh and retry".into(),
        ));
    }
    Ok(())
}

pub fn parse_hash(value: &str) -> Result<BlockHash, WalletError> {
    if value.len() != 64 || !value.is_ascii() {
        return Err(WalletError::Node("expected a 32-byte hash".into()));
    }
    let mut bytes = [0; 32];
    for (i, out) in bytes.iter_mut().enumerate() {
        *out = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16)
            .map_err(|_| WalletError::Node("invalid hash".into()))?;
    }
    Ok(BlockHash::new(bytes))
}

pub fn parse_address(value: &str) -> Result<WalletAddress, WalletError> {
    let value = value.trim();
    burst_crypto::decode_address(value)
        .ok_or_else(|| WalletError::TransactionBuild("invalid BURST address or checksum".into()))?;
    Ok(WalletAddress::new(value))
}

fn raw(value: &Value, field: &str) -> Result<u128, WalletError> {
    value[field]
        .as_str()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| WalletError::Node(format!("missing or invalid {field}")))
}

impl NodeClient {
    /// A trusted node may prove exclusion from a permanently closed epoch while
    /// the account still ends at the signed predecessor, or remains unopened.
    /// Never infer this from an absent block, an advanced head, or an RPC error.
    pub async fn excluded_offline_payment(
        &self,
        block: &StateBlock,
    ) -> Result<Option<BlockHash>, WalletError> {
        crate::offline::validate_signed_action(block)?;
        let query = burst_types::payment_recovery::ExclusionQuery {
            account: block.account.to_string(),
            previous: block.previous.to_string(),
            payment_epoch: block.payment_epoch.to_string(),
        };
        let response = self
            .rpc_call(
                "payment_exclusion",
                serde_json::to_value(&query)
                    .map_err(|error| WalletError::Other(error.to_string()))?,
            )
            .await?;
        let review: burst_types::payment_recovery::ExclusionReview =
            serde_json::from_value(response)
                .map_err(|error| WalletError::Node(error.to_string()))?;
        if review.account != query.account
            || review.previous != query.previous
            || review.payment_epoch != query.payment_epoch
        {
            return Err(WalletError::Node(
                "payment exclusion response does not match the saved draft".into(),
            ));
        }
        let installed = parse_hash(&review.installed_epoch)?;
        if installed.is_zero() || (review.excluded && installed == block.payment_epoch) {
            return Err(WalletError::Node(
                "invalid payment exclusion authority".into(),
            ));
        }
        Ok(review.excluded.then_some(installed))
    }

    pub async fn review_offline_payment(
        &self,
        block: &StateBlock,
    ) -> Result<crate::offline::OfflinePaymentReview, WalletError> {
        crate::offline::validate_signed_payment(block)?;
        if matches!(block.block_type, BlockType::Receive | BlockType::Open) {
            let (receipt, confirmed) = self
                .pending_receipt(&block.account, &block.link.to_string())
                .await?;
            if !confirmed || receipt.origin != block.origin {
                return Err(WalletError::Node(
                    "cannot verify the saved receipt's source".into(),
                ));
            }
            let transaction = build_receive_tx(
                &block.account,
                receipt.source,
                receipt.amount,
                block.timestamp,
            )?;
            if transaction.hash.as_bytes() != block.transaction.as_bytes() {
                return Err(WalletError::Node(
                    "saved receipt amount differs from the pending payment".into(),
                ));
            }
            return Ok(crate::offline::OfflinePaymentReview {
                kind: "receive",
                hash: block.hash.to_string(),
                provider: receipt.sender.to_string(),
                amount: receipt.amount.to_string(),
                payment: Some(receipt.source.to_string()),
            });
        }
        if block.block_type != BlockType::Burn || block.previous.is_zero() {
            return Err(WalletError::TransactionBuild(
                "this payment needs a different review flow".into(),
            ));
        }
        let previous = self
            .rpc_call("block_info", json!({"hash":block.previous.to_string()}))
            .await?;
        if previous["account"].as_str() != Some(block.account.as_str())
            || previous["confirmed"] != true
        {
            return Err(WalletError::Node(
                "cannot verify the saved payment's previous balance".into(),
            ));
        }
        let amount = block
            .brn_balance
            .checked_sub(raw(&previous, "brn_balance")?)
            .filter(|amount| *amount > 0)
            .ok_or_else(|| WalletError::TransactionBuild("invalid saved payment amount".into()))?;
        Ok(crate::offline::OfflinePaymentReview {
            kind: "burn",
            payment: None,
            hash: block.hash.to_string(),
            provider: burst_crypto::derive_address(&burst_types::PublicKey(*block.link.as_bytes()))
                .to_string(),
            amount: amount.to_string(),
        })
    }

    /// Find a confirmed transaction occupying this draft's exact account slot.
    /// Missing history or old nodes without successor lookup leave it unresolved.
    pub async fn confirmed_offline_successor(
        &self,
        block: &StateBlock,
    ) -> Result<Option<BlockHash>, WalletError> {
        crate::offline::validate_signed_action(block)?;
        self.confirmed_account_slot(block).await
    }

    /// Resolve the draft or a confirmed conflict in its saved predecessor chain.
    /// An RPC error or missing ancestor is never evidence of exclusion.
    pub async fn confirmed_offline_resolution(
        &self,
        block: &StateBlock,
        ancestry: &[StateBlock],
    ) -> Result<Option<BlockHash>, WalletError> {
        crate::offline::validate_signed_action(block)?;
        crate::offline::validate_ancestry(&block.account, block.previous, ancestry)?;
        let direct = self.confirmed_indexed_slot(block).await;
        if matches!(direct, Ok(Some(_))) {
            return direct;
        }
        // Start at the retained chain's oldest slot. A missing or unconfirmed
        // anchor cannot justify walking further, and a disconnected node must
        // not incur one full RPC timeout for every saved predecessor.
        for ancestor in ancestry.iter().rev() {
            match self.confirmed_indexed_slot(ancestor).await? {
                Some(winner) if winner != ancestor.hash => return Ok(Some(winner)),
                Some(_) => {}
                None => return Ok(None),
            }
        }
        direct
    }

    async fn confirmed_indexed_slot(
        &self,
        block: &StateBlock,
    ) -> Result<Option<BlockHash>, WalletError> {
        let query = burst_types::payment_recovery::SlotQuery {
            account: block.account.to_string(),
            previous: block.previous.to_string(),
        };
        let response = match self.rpc_call("confirmed_account_slot",serde_json::to_value(&query)
            .map_err(|e|WalletError::Other(e.to_string()))?).await {
            Ok(response) => response,
            Err(WalletError::Node(message)) if message == "node error: invalid request: unknown action: confirmed_account_slot"
                || message == "node error: server error: confirmed account slot recovery is unavailable on this node" => {
                return self.confirmed_account_slot(block).await;
            }
            Err(error) => return Err(error),
        };
        let review: burst_types::payment_recovery::SlotReview =
            serde_json::from_value(response).map_err(|e| WalletError::Node(e.to_string()))?;
        if review.account != query.account || review.previous != query.previous {
            return Err(WalletError::Node(
                "confirmed account slot response does not match the saved draft".into(),
            ));
        }
        review
            .successor
            .map(|value| {
                let hash = parse_hash(&value)?;
                if hash.is_zero() {
                    return Err(WalletError::Node(
                        "invalid confirmed account slot successor".into(),
                    ));
                }
                Ok(hash)
            })
            .transpose()
    }

    async fn confirmed_account_slot(
        &self,
        block: &StateBlock,
    ) -> Result<Option<BlockHash>, WalletError> {
        let anchor = if block.previous.is_zero() {
            let info = self
                .rpc_call("account_info", json!({"account":block.account.as_str()}))
                .await?;
            if info["address"].as_str() != Some(block.account.as_str()) {
                return Err(WalletError::Node(
                    "node returned a different opening account".into(),
                ));
            }
            if info["confirmation_height"].as_u64().unwrap_or(0) == 0 {
                return Ok(None);
            }
            info
        } else {
            let anchor = self
                .rpc_call("block_info", json!({"hash":block.previous.to_string()}))
                .await?;
            if anchor["confirmed"] != true
                || anchor["account"].as_str() != Some(block.account.as_str())
            {
                return Err(WalletError::Node(
                    "node cannot confirm the saved payment's account position".into(),
                ));
            }
            anchor
        };
        let field = if block.previous.is_zero() {
            "confirmed_open"
        } else {
            "confirmed_successor"
        };
        let Some(successor) = anchor[field].as_str() else {
            return Ok(None);
        };
        let hash = parse_hash(successor)?;
        if hash.is_zero() {
            return Err(WalletError::Node("invalid confirmed successor".into()));
        }
        let candidate = self
            .rpc_call("block_info", json!({"hash":hash.to_string()}))
            .await?;
        if candidate["confirmed"] != true
            || candidate["account"].as_str() != Some(block.account.as_str())
            || candidate["previous"].as_str() != Some(block.previous.to_string().as_str())
        {
            return Err(WalletError::Node(
                "node returned an unconfirmed or unrelated successor; keep the saved payment"
                    .into(),
            ));
        }
        Ok(Some(hash))
    }

    /// Resume a saved payment without accessing signing keys. Keep its file even
    /// on failure: an unavailable response is not proof that it was rejected.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn submit_saved_offline_payment(
        &self,
        path: &std::path::Path,
        expected_account: &WalletAddress,
    ) -> Result<ProcessResult, WalletError> {
        let block = crate::offline::load_payment(path)?;
        if &block.account != expected_account {
            return Err(WalletError::Signing(
                "saved payment belongs to another wallet".into(),
            ));
        }
        let hash = block.hash;
        // This draft already owns its reservation and recovery ancestry. Keep
        // the device binding, but do not reserve/archive it as a new action.
        let mut resumed = self.clone();
        resumed.payment_journal = None;
        resumed
            .submit_offline_payment_before(block, || crate::offline::record_submission(path, &hash))
            .await
    }

    /// Submit a journaled action and its committed content without signing again.
    /// The caller retains the journal until confirmation or certified exclusion.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn submit_saved_offline_action(
        &self,
        path: &std::path::Path,
        expected_account: &WalletAddress,
    ) -> Result<ProcessResult, WalletError> {
        let (block, content) = crate::offline::load_action_with_content(path)?;
        if &block.account != expected_account {
            return Err(WalletError::Signing(
                "saved action belongs to another wallet".into(),
            ));
        }
        let hash = block.hash;
        let mut resumed = self.clone();
        resumed.payment_journal = None;
        resumed
            .submit_offline_action_before(block, content.as_deref(), || {
                crate::offline::record_submission(path, &hash)
            })
            .await
    }

    /// Capture public signing inputs without releasing stakes or signing anything.
    /// The snapshot is node-provided data, not an authenticated ledger proof.
    pub async fn prepare_offline_payment(
        &self,
        account: &WalletAddress,
    ) -> Result<crate::offline::OfflinePaymentContext, WalletError> {
        parse_address(account.as_str())?;
        let (state, more) = self.signing_state_with_releases(account).await?;
        if more {
            return Err(WalletError::TransactionBuild(
                "finish pending stake-release batches online before preparing offline payments"
                    .into(),
            ));
        }
        let params = self.rpc_call("params_hash", json!({})).await?;
        let threshold = work_parameters(&params)?;
        require_payment_epoch(&params, state.payment_epoch)?;
        let params_hash = parse_hash(
            params["params_hash"]
                .as_str()
                .ok_or_else(|| WalletError::Node("missing params_hash".into()))?,
        )?;
        let root = StateBlock::work_root_for(account, state.head);
        let work = self.work_generate(&root.to_string()).await?;
        let nonce = u64::from_str_radix(work.work.trim_start_matches("0x"), 16)
            .map_err(|_| WalletError::Node("invalid prepared work nonce".into()))?;
        if work.hash != root.to_string() || !burst_work::validate_work(&root, nonce, threshold) {
            return Err(WalletError::Node(
                "node returned invalid prepared work".into(),
            ));
        }
        let ancestry = self.recovery_ancestry(account, state.head).await?;
        Ok(crate::offline::OfflinePaymentContext {
            ancestry,
            account: account.clone(),
            state,
            params_hash,
            threshold,
            work: PreparedWork { root, nonce },
        })
    }

    async fn recovery_ancestry(
        &self,
        account: &WalletAddress,
        mut previous: BlockHash,
    ) -> Result<Vec<StateBlock>, WalletError> {
        let mut ancestry = Vec::new();
        let mut ancestry_bytes = 2;
        while !previous.is_zero() {
            let info = self
                .rpc_call(
                    "block_info",
                    json!({"hash":previous.to_string(), "include_signed_block":true}),
                )
                .await?;
            if info["account"].as_str() != Some(account.as_str()) {
                return Err(WalletError::Node(
                    "offline recovery node returned a different account".into(),
                ));
            }
            if info["confirmed"] == true {
                break;
            }
            let encoded = info["signed_block"].as_str().ok_or_else(|| {
                WalletError::Node(
                    "node cannot supply lossless signed recovery ancestry; update the node".into(),
                )
            })?;
            let ancestor: StateBlock = serde_json::from_str(encoded)
                .map_err(|_| WalletError::Node("node cannot supply signed offline recovery ancestry; wait for confirmation or update the node".into()))?;
            crate::offline::validate_signed_block(&ancestor)?;
            if ancestor.hash != previous || &ancestor.account != account {
                return Err(WalletError::Node(
                    "offline recovery node returned a different predecessor".into(),
                ));
            }
            ancestry_bytes += serde_json::to_vec(&ancestor)
                .map_err(|e| WalletError::Other(e.to_string()))?
                .len()
                + 1;
            if ancestry.len() >= crate::offline::MAX_ANCESTRY_BLOCKS
                || ancestry_bytes > crate::offline::MAX_ANCESTRY_BYTES
            {
                return Err(WalletError::Node("unconfirmed ancestry is too large to save offline; wait for frontier confirmation".into()));
            }
            previous = ancestor.previous;
            ancestry.push(ancestor);
        }
        Ok(ancestry)
    }

    /// Capture public action review and signing inputs without signing or submitting.
    pub async fn prepare_offline_action(
        &self,
        account: &WalletAddress,
        action: crate::offline::OfflineAction,
    ) -> Result<crate::offline::OfflineActionContext, WalletError> {
        use crate::offline::OfflineAction;
        parse_address(account.as_str())?;
        let capabilities = self.rpc_call("params_hash", json!({})).await?;
        work_parameters(&capabilities)?;
        let mut target_head = None;
        let review = match &action {
            OfflineAction::Transfer { to, amount, origin } => {
                parse_address(to.as_str())?;
                let portfolio = self
                    .rpc_call("trst_portfolio", json!({"account":account.as_str()}))
                    .await?;
                let batches = portfolio["batches"]
                    .as_array()
                    .ok_or_else(|| WalletError::Node("missing TRST portfolio".into()))?;
                crate::portfolio::select_transfer_origin(
                    batches,
                    *amount,
                    Some(&origin.to_string()),
                )
                .map_err(WalletError::TransactionBuild)?;
                json!({"batches":batches.iter().filter(|batch|batch["origin"].as_str()==Some(origin.to_string().as_str()) && batch["status"]=="active").collect::<Vec<_>>()})
            }
            OfflineAction::Merge { selected } => {
                let portfolio = self
                    .rpc_call("trst_portfolio", json!({"account":account.as_str()}))
                    .await?;
                let batches = portfolio["batches"]
                    .as_array()
                    .ok_or_else(|| WalletError::Node("missing TRST portfolio".into()))?;
                crate::portfolio::reviewed_merge_sources(batches, selected)
                    .map_err(WalletError::TransactionBuild)?;
                json!({"batches":batches.iter().filter(|batch|selected.iter().any(|selection|batch["id"].as_str()==Some(selection.id.as_str()))).collect::<Vec<_>>()})
            }
            OfflineAction::Reject { payment } => {
                let entry = self
                    .account_pending_source(account.as_str(), &payment.to_string())
                    .await?
                    .ok_or_else(|| {
                        WalletError::Node(
                            "payment is no longer pending; refresh before trying again".into(),
                        )
                    })?;
                if entry.return_credit {
                    return Err(WalletError::TransactionBuild(
                        "returned funds are already yours; receive the credit instead".into(),
                    ));
                }
                let source = self
                    .rpc_call("block_info", json!({"hash":payment.to_string()}))
                    .await?;
                if source["confirmed"] != true {
                    return Err(WalletError::Node("wait for the incoming payment to confirm before preparing its return offline".into()));
                }
                json!({"payment":payment.to_string(),"sender":entry.source,"amount":entry.amount})
            }
            OfflineAction::Endorse { target, burn } => {
                parse_address(target.as_str())?;
                if *burn == 0 {
                    return Err(WalletError::TransactionBuild(
                        "endorsement burn must be positive".into(),
                    ));
                }
                target_head = Some(self.inspect_signing_state(target).await?.head);
                self.rpc_call("verification_status", json!({"account":target.as_str()}))
                    .await?
            }
            OfflineAction::Challenge { target, stake, .. } => {
                parse_address(target.as_str())?;
                if *stake == 0 {
                    return Err(WalletError::TransactionBuild(
                        "challenge stake must be positive".into(),
                    ));
                }
                self.rpc_call("verification_status", json!({"account":target.as_str()}))
                    .await?
            }
            OfflineAction::Appeal { reviewed } => {
                let current = self
                    .appeal_review(&parse_address(&reviewed.account)?)
                    .await?;
                crate::appeals::prepare(&current, reviewed, account, Timestamp::now())?;
                serde_json::to_value(current)
                    .map_err(|error| WalletError::Other(error.to_string()))?
            }
            OfflineAction::VerificationVote {
                target,
                context,
                vote,
                stake,
            } => {
                parse_address(target.as_str())?;
                let status = self
                    .rpc_call("verification_status", json!({"account":target.as_str()}))
                    .await?;
                let rules = self.rpc_call("protocol_params", json!({})).await?;
                let selected = status["selected_verifiers"]
                    .as_array()
                    .ok_or_else(|| WalletError::Node("missing selected verifier panel".into()))?;
                let votes = status["verifier_votes"]
                    .as_array()
                    .ok_or_else(|| WalletError::Node("missing panel votes".into()))?;
                if context.is_zero()
                    || !(1..=3).contains(vote)
                    || status["selection_available"] != true
                    || status["selection_context"].as_str() != Some(context.to_string().as_str())
                    || !matches!(status["phase"].as_str(), Some("voting" | "challenged"))
                    || !selected
                        .iter()
                        .any(|value| value.as_str() == Some(account.as_str()))
                    || votes
                        .iter()
                        .any(|value| value["verifier"].as_str() == Some(account.as_str()))
                    || if *vote == 3 {
                        *stake != 0
                    } else {
                        rules["verifier_stake_amount"].as_str() != Some(stake.to_string().as_str())
                    }
                {
                    return Err(WalletError::TransactionBuild(
                        "verification assignment or stake changed; review it again".into(),
                    ));
                }
                json!({"assignment":status,"stake":stake.to_string()})
            }
            OfflineAction::Proposal { content } => {
                if capabilities["full_governance_content"] != true {
                    return Err(WalletError::Node(
                        "full governance content is unavailable".into(),
                    ));
                }
                content
                    .validate_content()
                    .map_err(|error| WalletError::TransactionBuild(error.to_string()))?;
                self.rpc_call("consti", json!({})).await?
            }
            OfflineAction::GovernanceVote { proposal, .. }
            | OfflineAction::GovernanceEndorse { proposal, .. }
            | OfflineAction::GovernanceWithdraw { proposal } => {
                if proposal.is_zero() {
                    return Err(WalletError::TransactionBuild("missing proposal".into()));
                }
                let info = self
                    .rpc_call(
                        "governance_proposal_info",
                        json!({"hash":proposal.to_string()}),
                    )
                    .await?;
                let phase = if matches!(&action, OfflineAction::GovernanceVote { .. }) {
                    "Exploration"
                } else {
                    "Proposal"
                };
                if info["hash"].as_str() != Some(proposal.to_string().as_str())
                    || !info["phase_deadline"].as_u64().is_some_and(|deadline| Timestamp::now().as_secs() < deadline)
                    || info["phase"].as_str() != Some(phase) || info["phase_projected"].as_bool().is_none()
                    // Opening Exploration is a read-only projection; ballots are
                    // admitted in that window before its result is certified.
                    || !matches!(&action, OfflineAction::GovernanceVote { .. }) && info["phase_projected"] != false
                    || matches!(&action, OfflineAction::GovernanceWithdraw { .. })
                        && info["proposer"].as_str() != Some(account.as_str())
                {
                    return Err(WalletError::TransactionBuild(
                        "proposal is not available for this action".into(),
                    ));
                }
                info
            }
            OfflineAction::VerifierParticipation { .. } => {
                self.rpc_call("verification_status", json!({"account":account.as_str()}))
                    .await?
            }
            OfflineAction::Delegation {
                scope,
                generation,
                genesis,
                ..
            } => {
                if capabilities["delegation_key_journal"] != true {
                    return Err(WalletError::Node(
                        "delegation key registration is unavailable".into(),
                    ));
                }
                let page = self
                    .delegation_keys(&crate::delegation::view::Query {
                        account: account.to_string(),
                        direction: crate::delegation::view::Direction::Outgoing,
                        scope: Some(scope.clone()),
                        cursor: None,
                    })
                    .await?;
                crate::delegation::require_review(&page, generation)?;
                if parse_hash(&page.genesis)? != parse_hash(genesis)? {
                    return Err(WalletError::TransactionBuild(
                        "delegation network changed; review again".into(),
                    ));
                }
                serde_json::to_value(page).map_err(|error| WalletError::Other(error.to_string()))?
            }
            OfflineAction::DelegatedVote {
                entry, proposal, ..
            } => {
                if capabilities["delegation_key_journal"] != true {
                    return Err(WalletError::Node(
                        "delegation key registration is unavailable".into(),
                    ));
                }
                let page = self
                    .delegation_keys(&crate::delegation::view::Query {
                        account: entry.owner.clone(),
                        direction: crate::delegation::view::Direction::Outgoing,
                        scope: Some(entry.scope.clone()),
                        cursor: None,
                    })
                    .await?;
                crate::delegation::require_review(&page, &entry.generation)?;
                let current = page.entries.first().ok_or_else(|| {
                    WalletError::TransactionBuild("delegation was removed".into())
                })?;
                if serde_json::to_value(current).ok() != serde_json::to_value(entry).ok()
                    || current.delegate.as_deref() != Some(account.as_str())
                {
                    return Err(WalletError::TransactionBuild(
                        "delegation changed; review again".into(),
                    ));
                }
                let info = self
                    .rpc_call(
                        "governance_proposal_info",
                        json!({"hash":proposal.to_string()}),
                    )
                    .await?;
                if info["hash"].as_str() != Some(proposal.to_string().as_str())
                    || !info["phase_deadline"]
                        .as_u64()
                        .is_some_and(|deadline| Timestamp::now().as_secs() < deadline)
                    || info["phase"].as_str() != Some("Exploration")
                    || info["phase_projected"].as_bool().is_none()
                {
                    return Err(WalletError::TransactionBuild(
                        "proposal is not available for voting".into(),
                    ));
                }
                info
            }
        };
        let signing = self.prepare_offline_payment(account).await?;
        require_payment_epoch(&capabilities, signing.state.payment_epoch)?;
        if capabilities["params_hash"].as_str() != Some(signing.params_hash.to_string().as_str()) {
            return Err(WalletError::TransactionBuild(
                "network rules changed while preparing the action".into(),
            ));
        }
        if signing.state.head.is_zero() {
            return Err(WalletError::TransactionBuild(
                "open the account before preparing this action".into(),
            ));
        }
        Ok(crate::offline::OfflineActionContext {
            signing,
            action,
            target_head,
            review,
        })
    }

    /// Capture one confirmed pending receipt and its offline account context.
    pub async fn prepare_offline_receive(
        &self,
        account: &WalletAddress,
        payment: &str,
    ) -> Result<crate::offline::OfflineReceiveContext, WalletError> {
        let (receipt, confirmed) = self.pending_receipt(account, payment).await?;
        if !confirmed {
            return Err(WalletError::Node(
                "wait for the incoming payment to confirm before preparing it offline".into(),
            ));
        }
        let signing = self.prepare_offline_payment(account).await?;
        Ok(crate::offline::OfflineReceiveContext { signing, receipt })
    }

    /// Submit exact offline-signed bytes. Never rebase, sign or generate work.
    pub async fn submit_offline_payment(
        &self,
        block: StateBlock,
    ) -> Result<ProcessResult, WalletError> {
        self.submit_offline_payment_before(block, || Ok(())).await
    }

    async fn submit_offline_payment_before(
        &self,
        block: StateBlock,
        before_submit: impl FnOnce() -> Result<(), WalletError>,
    ) -> Result<ProcessResult, WalletError> {
        crate::offline::validate_signed_payment(&block)?;
        self.submit_offline_action_before(block, None, before_submit)
            .await
    }

    async fn submit_offline_action_before(
        &self,
        block: StateBlock,
        content: Option<&[u8]>,
        before_submit: impl FnOnce() -> Result<(), WalletError>,
    ) -> Result<ProcessResult, WalletError> {
        crate::offline::validate_signed_action(&block)?;
        // A previous response may have been lost. Check the exact hash before
        // rejecting an advanced frontier, including one advanced by this block.
        if let Ok(info) = self
            .rpc_call("block_info", json!({"hash":block.hash.to_string()}))
            .await
        {
            if info["confirmed"] == true {
                return Ok(ProcessResult {
                    hash: block.hash.to_string(),
                    accepted: true,
                    detail: None,
                });
            }
        }
        let state = self.signing_state(&block.account).await?;
        let params = self.rpc_call("params_hash", json!({})).await?;
        let threshold = work_parameters(&params)?;
        require_payment_epoch(&params, block.payment_epoch)?;
        if state.payment_epoch != block.payment_epoch
            || state.head != block.previous
            || state.signing_grant != block.signing_grant
        {
            return Err(WalletError::TransactionBuild(
                format!("offline payment {} is stale; check this hash in history before preparing a replacement", block.hash),
            ));
        }
        if params["params_hash"].as_str() != Some(block.params_hash.to_string().as_str()) {
            return Err(WalletError::TransactionBuild(
                format!("network rules changed for offline payment {}; check this hash in history before preparing a replacement", block.hash),
            ));
        }
        if !burst_work::validate_work(&block.work_root(), block.work, threshold) {
            return Err(WalletError::TransactionBuild(
                "offline payment work is no longer valid".into(),
            ));
        }
        before_submit()?;
        self.submit_ready_block_with_content(block, content).await
    }

    /// Submit account operations without sending private keys to the node.
    async fn account_operation_signed(
        &self,
        keys: &KeyPair,
        kind: BlockType,
        link: BlockHash,
        burn: u128,
    ) -> Result<ProcessResult, WalletError> {
        self.account_operation_reviewed(keys, kind, link, burn, None)
            .await
    }

    async fn account_operation_reviewed(
        &self,
        keys: &KeyPair,
        kind: BlockType,
        link: BlockHash,
        burn: u128,
        expected_params: Option<&str>,
    ) -> Result<ProcessResult, WalletError> {
        let address = burst_crypto::derive_address(&keys.public);
        let timestamp = Timestamp::now();
        let state = self
            .prepare_release_batches(keys, timestamp)
            .await
            .map_err(|error| WalletError::Preparation(Box::new(error)))?;
        if state.head.is_zero() {
            return Err(WalletError::TransactionBuild(
                "open the account before submitting this operation".into(),
            ));
        }
        let params = self.rpc_call("params_hash", json!({})).await?;
        work_parameters(&params)?;
        let params_hash = parse_hash(
            params["params_hash"]
                .as_str()
                .ok_or_else(|| WalletError::Node("missing params_hash".into()))?,
        )?;
        if expected_params.is_some_and(|expected| expected != params_hash.to_string()) {
            return Err(WalletError::TransactionBuild(
                "network rules changed; review this action again".into(),
            ));
        }
        let mut identifier = b"BURST/account-operation/v1\0".to_vec();
        require_payment_epoch(&params, state.payment_epoch)?;
        identifier.extend_from_slice(state.head.as_bytes());
        identifier.extend_from_slice(link.as_bytes());
        identifier.extend_from_slice(&timestamp.as_secs().to_be_bytes());
        let mut block = StateBlock {
            signing_grant: state.signing_grant,
            delegation_payload: Vec::new(),
            version: burst_ledger::CURRENT_BLOCK_VERSION,
            payment_epoch: state.payment_epoch,
            block_type: kind,
            account: address,
            previous: state.head,
            representative: state.representative,
            brn_balance: state
                .brn_balance
                .checked_add(burn)
                .ok_or_else(|| WalletError::TransactionBuild("BRN spend overflow".into()))?,
            trst_balance: state.trst_balance,
            link,
            origin: TxHash::ZERO,
            transaction: burst_crypto::hash_transaction(&identifier),
            timestamp,
            params_hash,
            target_head: burst_types::BlockHash::ZERO,
            merge_sources: vec![],
            release_sources: state.release_sources,
            work: 0,
            signature: burst_types::Signature([0; 64]),
            hash: BlockHash::ZERO,
        };
        block.hash = block.compute_hash();
        self.submit_with_work(sign_state_block(block, &keys.private))
            .await
    }

    pub async fn propose_parameter_signed(
        &self,
        keys: &KeyPair,
        param: burst_governance::GovernableParam,
        new_value: u128,
    ) -> Result<ProcessResult, WalletError> {
        let link = burst_governance::ProposalContent::ParameterChange { param, new_value }
            .encode_compact()
            .map_err(|e| WalletError::TransactionBuild(e.to_string()))?;
        self.account_operation_signed(keys, BlockType::GovernanceProposal, BlockHash::new(link), 0)
            .await
    }

    pub async fn endorse_proposal_signed(
        &self,
        keys: &KeyPair,
        proposal: TxHash,
        burn: u128,
    ) -> Result<ProcessResult, WalletError> {
        if burn == 0 {
            return Err(WalletError::TransactionBuild(
                "endorsement burn must be positive".into(),
            ));
        }
        self.account_operation_signed(
            keys,
            BlockType::GovernanceEndorse,
            BlockHash::new(*proposal.as_bytes()),
            burn,
        )
        .await
    }

    pub async fn propose_parameter_reviewed_signed(
        &self,
        keys: &KeyPair,
        param: burst_governance::GovernableParam,
        new_value: u128,
        params_hash: &str,
    ) -> Result<ProcessResult, WalletError> {
        let link = burst_governance::ProposalContent::ParameterChange { param, new_value }
            .encode_compact()
            .map_err(|e| WalletError::TransactionBuild(e.to_string()))?;
        self.account_operation_reviewed(
            keys,
            BlockType::GovernanceProposal,
            BlockHash::new(link),
            0,
            Some(params_hash),
        )
        .await
    }

    pub async fn propose_content_signed(
        &self,
        keys: &KeyPair,
        content: &burst_governance::ProposalContent,
    ) -> Result<ProcessResult, WalletError> {
        self.propose_content_checked(keys, content, None).await
    }

    pub async fn propose_content_reviewed_signed(
        &self,
        keys: &KeyPair,
        content: &burst_governance::ProposalContent,
        params_hash: &str,
    ) -> Result<ProcessResult, WalletError> {
        self.propose_content_checked(keys, content, Some(params_hash))
            .await
    }

    async fn propose_content_checked(
        &self,
        keys: &KeyPair,
        content: &burst_governance::ProposalContent,
        expected_params: Option<&str>,
    ) -> Result<ProcessResult, WalletError> {
        content
            .validate_content()
            .map_err(|error| WalletError::TransactionBuild(error.to_string()))?;
        let capability = self.rpc_call("params_hash", json!({})).await?;
        if capability["full_governance_content"] != true {
            return Err(WalletError::TransactionBuild(
                "this node has not enabled full governance content".into(),
            ));
        }
        let now = Timestamp::now();
        let state = self.prepare_release_batches(keys, now).await?;
        let params = self.rpc_call("params_hash", json!({})).await?;
        let threshold = work_parameters(&params)?;
        require_payment_epoch(&params, state.payment_epoch)?;
        let params_hash = parse_hash(
            params["params_hash"]
                .as_str()
                .ok_or_else(|| WalletError::Node("missing params_hash".into()))?,
        )?;
        if expected_params.is_some_and(|expected| expected != params_hash.to_string()) {
            return Err(WalletError::TransactionBuild(
                "network rules changed; review the proposal again".into(),
            ));
        }
        let (mut block, encoded) = crate::governance_content::build_signed_proposal(
            &state,
            keys,
            content,
            params_hash,
            now,
        )?;
        let work = self.work_generate(&block.work_root().to_string()).await?;
        block.work = u64::from_str_radix(work.work.trim_start_matches("0x"), 16)
            .map_err(|_| WalletError::Node("invalid proposal work nonce".into()))?;
        if work.hash != block.work_root().to_string() || !block.verify_work(threshold) {
            return Err(WalletError::Node(
                "node returned invalid proposal work".into(),
            ));
        }
        self.submit_ready_block_with_content(block, Some(encoded.bytes()))
            .await
    }

    pub async fn submit_delegation_key_operation_signed(
        &self,
        keys: &KeyPair,
        operation: &burst_governance::key_delegation::operation::DelegationOperation,
    ) -> Result<ProcessResult, WalletError> {
        let capability = self.rpc_call("params_hash", json!({})).await?;
        if capability["delegation_key_journal"] != true {
            return Err(WalletError::TransactionBuild(
                "this node has not enabled delegation key registration".into(),
            ));
        }
        let now = Timestamp::now();
        let state = self.prepare_release_batches(keys, now).await?;
        let params = self.rpc_call("params_hash", json!({})).await?;
        let threshold = work_parameters(&params)?;
        require_payment_epoch(&params, state.payment_epoch)?;
        let params_hash = parse_hash(
            params["params_hash"]
                .as_str()
                .ok_or_else(|| WalletError::Node("missing params_hash".into()))?,
        )?;
        let mut block =
            crate::delegation::build_key_operation(&state, keys, operation, params_hash, now)?;
        let work = self.work_generate(&block.work_root().to_string()).await?;
        block.work = u64::from_str_radix(work.work.trim_start_matches("0x"), 16)
            .map_err(|_| WalletError::Node("invalid delegation work nonce".into()))?;
        if work.hash != block.work_root().to_string() || !block.verify_work(threshold) {
            return Err(WalletError::Node(
                "node returned invalid delegation work".into(),
            ));
        }
        self.submit_ready_block(block).await
    }

    pub async fn withdraw_proposal_signed(
        &self,
        keys: &KeyPair,
        proposal: TxHash,
    ) -> Result<ProcessResult, WalletError> {
        self.account_operation_signed(
            keys,
            BlockType::GovernanceWithdraw,
            BlockHash::new(*proposal.as_bytes()),
            0,
        )
        .await
    }

    /// Opt in to or out of the verifier pool with a locally signed block.
    /// Eligibility and panel selection are separate protocol checks.
    pub async fn set_verifier_opt_in_signed(
        &self,
        keys: &KeyPair,
        opted_in: bool,
    ) -> Result<ProcessResult, WalletError> {
        let kind = if opted_in {
            BlockType::VerifierOptIn
        } else {
            BlockType::VerifierOptOut
        };
        self.account_operation_signed(keys, kind, BlockHash::ZERO, 0)
            .await
    }

    pub async fn signing_state(
        &self,
        account: &WalletAddress,
    ) -> Result<AccountState, WalletError> {
        self.signing_state_with_releases(account)
            .await
            .map(|(state, _)| state)
    }

    async fn signing_state_with_releases(
        &self,
        account: &WalletAddress,
    ) -> Result<(AccountState, bool), WalletError> {
        let result = self.query_signing_state_with_releases(account).await?;
        if result.0.signing_grant != self.expected_signing_grant {
            return Err(WalletError::Signing(
                "Another device controls signing. Review an explicit device takeover before signing.".into(),
            ));
        }
        Ok(result)
    }

    /// Read-only state for explicit takeover review; does not adopt its grant.
    pub async fn inspect_signing_state(
        &self,
        account: &WalletAddress,
    ) -> Result<AccountState, WalletError> {
        self.query_signing_state_with_releases(account)
            .await
            .map(|result| result.0)
    }

    /// Submit exactly the reviewed authority transition. Confirmation and local
    /// role activation are separate: an RPC acceptance is not a completed takeover.
    pub async fn handoff_signing_device(
        &self,
        keys: &KeyPair,
        reviewed_head: BlockHash,
        reviewed_grant: BlockHash,
        reviewed_params: BlockHash,
        nonce: BlockHash,
    ) -> Result<ProcessResult, WalletError> {
        let account = burst_crypto::derive_address(&keys.public);
        let mut state = self.inspect_signing_state(&account).await?;
        if state.head != reviewed_head || state.signing_grant != reviewed_grant {
            return Err(WalletError::Signing(
                "Account changed; review the device takeover again.".into(),
            ));
        }
        let params = self.rpc_call("params_hash", json!({})).await?;
        work_parameters(&params)?;
        require_payment_epoch(&params, state.payment_epoch)?;
        if params["params_hash"].as_str() != Some(reviewed_params.to_string().as_str()) {
            return Err(WalletError::Signing(
                "Network rules changed; review the device takeover again.".into(),
            ));
        }
        if state.head.is_zero() {
            self.clone()
                .with_signing_grant(BlockHash::ZERO)
                .open_empty_account(keys)
                .await
                .map_err(|error| WalletError::Preparation(Box::new(error)))?;
            state = self.inspect_signing_state(&account).await?;
            if state.head.is_zero()
                || state.block_count != 1
                || !state.signing_grant.is_zero()
                || state.brn_balance != 0
                || state.trst_balance != 0
            {
                return Err(WalletError::Signing(
                    "Account changed during enrollment; review the device again.".into(),
                ));
            }
            require_payment_epoch(&params, state.payment_epoch)?;
        }
        let block = crate::transaction_builder::build_signing_handoff(
            &state,
            &account,
            nonce,
            reviewed_params,
            Timestamp::now(),
        )?;
        self.submit_with_work(sign_state_block(block, &keys.private))
            .await
    }

    async fn query_signing_state_with_releases(
        &self,
        account: &WalletAddress,
    ) -> Result<(AccountState, bool), WalletError> {
        let info = match self
            .rpc_call("account_info", json!({"account":account.as_str()}))
            .await
        {
            Ok(info) => info,
            Err(WalletError::Node(message))
                if message.starts_with("node error: account not found:") =>
            {
                let params = self.rpc_call("params_hash", json!({})).await?;
                work_parameters(&params)?;
                return Ok((
                    AccountState {
                        signing_grant: burst_types::BlockHash::ZERO,
                        payment_epoch: payment_epoch(&params)?,
                        head: BlockHash::ZERO,
                        block_count: 0,
                        representative: account.clone(),
                        brn_balance: 0,
                        trst_balance: 0,
                        release_sources: Vec::new(),
                    },
                    false,
                ));
            }
            Err(error) => return Err(error),
        };
        let head = parse_hash(
            info["head"]
                .as_str()
                .ok_or_else(|| WalletError::Node("missing head".into()))?,
        )?;
        // Account RPC balance is spendable BRN, NOT the signed block odometer.
        let (spent, signing_grant) =
            if head.is_zero() {
                (0, BlockHash::ZERO)
            } else {
                let block = self
                    .rpc_call("block_info", json!({"hash":head.to_string()}))
                    .await?;
                (
                    raw(&block, "brn_balance")?,
                    parse_hash(block["active_signing_grant"].as_str().ok_or_else(|| {
                        WalletError::Node("missing signing-device grant".into())
                    })?)?,
                )
            };
        let release_sources = info["release_sources"]
            .as_array()
            .filter(|sources| sources.len() <= burst_ledger::state_block::MAX_RELEASE_SOURCES)
            .ok_or_else(|| {
                WalletError::Node("missing or oversized returned-stake evidence".into())
            })?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| WalletError::Node("invalid returned-stake source".into()))
                    .and_then(parse_hash)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if release_sources.iter().any(BlockHash::is_zero)
            || release_sources.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(WalletError::Node(
                "noncanonical returned-stake evidence".into(),
            ));
        }
        let has_more_releases = info["has_more_releases"]
            .as_bool()
            .ok_or_else(|| WalletError::Node("missing stake-release pagination state".into()))?;
        Ok((
            AccountState {
                signing_grant,
                payment_epoch: payment_epoch(&info)?,
                head,
                block_count: info["block_count"].as_u64().unwrap_or(0),
                release_sources,
                representative: WalletAddress::new(
                    info["representative"].as_str().unwrap_or(account.as_str()),
                ),
                brn_balance: spent,
                trst_balance: raw(&info, "trst_balance")?,
            },
            has_more_releases,
        ))
    }

    async fn prepare_release_batches(
        &self,
        keys: &KeyPair,
        at: Timestamp,
    ) -> Result<AccountState, WalletError> {
        let account = burst_crypto::derive_address(&keys.public);
        let mut previous_count = None;
        loop {
            let (state, more) = self.signing_state_with_releases(&account).await?;
            if previous_count.is_some_and(|count| state.block_count <= count) {
                return Err(WalletError::Node(
                    "stake-release acknowledgement is confirmed but account state is stale".into(),
                ));
            }
            if !more {
                return Ok(state);
            }
            if state.head.is_zero() || state.release_sources.is_empty() {
                return Err(WalletError::Node(
                    "node cannot provide a complete stake-release batch".into(),
                ));
            }
            let params = self.rpc_call("params_hash", json!({})).await?;
            work_parameters(&params)?;
            let params_hash = parse_hash(
                params["params_hash"]
                    .as_str()
                    .ok_or_else(|| WalletError::Node("missing params_hash".into()))?,
            )?;
            // A no-op representative block acknowledges a full proof batch
            // without changing funds or delegation before the requested action.
            let tx = Transaction::ChangeRepresentative(build_change_rep_tx(
                &account,
                &state.representative,
                at,
            )?);
            require_payment_epoch(&params, state.payment_epoch)?;
            let block = build_state_block(&state, &tx, TxHash::ZERO, params_hash)?;
            self.submit_with_work(sign_state_block(block, &keys.private))
                .await?;
            previous_count = Some(state.block_count);
        }
    }

    pub async fn submit_signed(
        &self,
        keys: &KeyPair,
        transaction: &Transaction,
        origin: TxHash,
    ) -> Result<ProcessResult, WalletError> {
        self.submit_signed_with_prepared_work(keys, transaction, origin, None)
            .await
    }

    pub async fn prepare_work(&self, account: &WalletAddress) -> Result<PreparedWork, WalletError> {
        parse_address(account.as_str())?;
        let state = self.signing_state(account).await?;
        let params = self.rpc_call("params_hash", json!({})).await?;
        let threshold = work_parameters(&params)?;
        let root = StateBlock::work_root_for(account, state.head);
        require_payment_epoch(&params, state.payment_epoch)?;
        let work = self.work_generate(&root.to_string()).await?;
        let nonce = u64::from_str_radix(work.work.trim_start_matches("0x"), 16)
            .map_err(|_| WalletError::Node("invalid prepared work nonce".into()))?;
        if work.hash != root.to_string() || !burst_work::validate_work(&root, nonce, threshold) {
            return Err(WalletError::Node(
                "node returned invalid prepared work".into(),
            ));
        }
        Ok(PreparedWork { root, nonce })
    }

    pub async fn submit_signed_with_prepared_work(
        &self,
        keys: &KeyPair,
        transaction: &Transaction,
        origin: TxHash,
        prepared: Option<&PreparedWork>,
    ) -> Result<ProcessResult, WalletError> {
        let address = burst_crypto::derive_address(&keys.public);
        if transaction.sender() != &address {
            return Err(WalletError::Signing(
                "transaction account differs from signing key".into(),
            ));
        }
        let state = self
            .prepare_release_batches(keys, transaction.timestamp())
            .await
            .map_err(|error| WalletError::Preparation(Box::new(error)))?;
        let params = self.rpc_call("params_hash", json!({})).await?;
        let threshold = work_parameters(&params)?;
        let params_hash = parse_hash(
            params["params_hash"]
                .as_str()
                .ok_or_else(|| WalletError::Node("missing params_hash".into()))?,
        )?;
        let mut block = build_state_block(&state, transaction, origin, params_hash)?;
        require_payment_epoch(&params, state.payment_epoch)?;
        if let Transaction::Endorse(endorsement) = transaction {
            block.target_head = self.inspect_signing_state(&endorsement.target).await?.head;
            block.hash = block.compute_hash();
        }
        if state.head.is_zero() && matches!(transaction, Transaction::Receive(_)) {
            block.block_type = BlockType::Open;
            block.hash = block.compute_hash();
        }
        block = sign_state_block(block, &keys.private);
        if let Some(work) = prepared {
            if work.apply_to(&mut block, threshold) {
                return self.submit_ready_block(block).await;
            }
        }
        self.submit_with_work(block).await
    }

    async fn submit_with_work(&self, mut block: StateBlock) -> Result<ProcessResult, WalletError> {
        let work = self
            .rpc_call(
                "work_generate",
                json!({"hash":block.work_root().to_string(), "block_type":block.block_type}),
            )
            .await?;
        let nonce = work["work"]
            .as_str()
            .ok_or_else(|| WalletError::Node("missing work nonce".into()))?;
        block.work = u64::from_str_radix(nonce.trim_start_matches("0x"), 16)
            .map_err(|_| WalletError::Node("invalid work nonce".into()))?;
        self.submit_ready_block(block).await
    }

    async fn submit_ready_block(&self, block: StateBlock) -> Result<ProcessResult, WalletError> {
        self.submit_ready_block_with_content(block, None).await
    }

    async fn submit_ready_block_with_content(
        &self,
        block: StateBlock,
        content: Option<&[u8]>,
    ) -> Result<ProcessResult, WalletError> {
        let payload =
            serde_json::to_string(&block).map_err(|e| WalletError::Other(e.to_string()))?;
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(path) = &self.payment_journal {
            let ancestry = self
                .recovery_ancestry(&block.account, block.previous)
                .await?;
            crate::offline::save_signed_action(path, &block, &ancestry, content)?;
            crate::offline::record_submission(path, &block.hash)?;
        }
        // A lost/malformed process response cannot prove rejection. Preserve the
        // locally signed hash so callers can reconcile instead of sending twice.
        let submitted = if let Some(content) = content {
            self.rpc_call(
                "process",
                json!({"block":payload, "proposal_content":hex::encode(content)}),
            )
            .await
            .and_then(|value| {
                serde_json::from_value::<ProcessResult>(value)
                    .map_err(|error| WalletError::Node(error.to_string()))
            })
        } else {
            self.process(&payload).await
        };
        let result = submitted.map_err(|error| {
            WalletError::Node(format!("confirmation pending for {}; submission response unavailable: {error}; check history before retrying", block.hash))
        })?;
        if !result.accepted {
            return Err(WalletError::Node(
                result.detail.unwrap_or("block rejected".into()),
            ));
        }
        if result.hash != block.hash.to_string() {
            return Err(WalletError::Node(format!("confirmation pending for {}; node returned a different block hash; check history before retrying", block.hash)));
        }
        // Accepted means queued, not confirmed. Do not tell the UI payment
        // succeeded until the real ledger confirms this exact signed block.
        self.wait_confirmed(&result.hash, Duration::from_secs(30))
            .await?;
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(path) = &self.payment_journal {
            if block.block_type != BlockType::ChangeSigner {
                crate::offline::archive_payment(path, "confirmed")?;
            }
        }
        Ok(result)
    }

    pub async fn wait_confirmed(&self, hash: &str, timeout: Duration) -> Result<(), WalletError> {
        let confirmed = crate::runtime::timeout(timeout, async {
            loop {
                if let Ok(info) = self.rpc_call("block_info", json!({"hash":hash})).await {
                    if info["confirmed"] == true {
                        return;
                    }
                }
                crate::runtime::sleep(Duration::from_millis(200)).await;
            }
        })
        .await;
        confirmed.map_err(|_| {
            WalletError::Node(format!(
                "confirmation pending for {hash}; check history before retrying"
            ))
        })
    }

    pub async fn pay_signed(
        &self,
        keys: &KeyPair,
        provider: &WalletAddress,
        amount: u128,
    ) -> Result<ProcessResult, WalletError> {
        if amount == 0 {
            return Err(WalletError::TransactionBuild(
                "amount must be positive".into(),
            ));
        }
        let sender = burst_crypto::derive_address(&keys.public);
        let tx = build_burn_tx(&sender, provider, amount, Timestamp::now())?;
        self.submit_signed(keys, &Transaction::Burn(tx), TxHash::ZERO)
            .await
    }

    pub async fn receive_signed(
        &self,
        keys: &KeyPair,
        hash: &str,
    ) -> Result<ProcessResult, WalletError> {
        let address = burst_crypto::derive_address(&keys.public);
        let (receipt, _) = self.pending_receipt(&address, hash).await?;
        self.submit_signed(
            keys,
            &receipt.transaction(&address, Timestamp::now())?,
            receipt.origin,
        )
        .await
    }

    async fn pending_receipt(
        &self,
        address: &WalletAddress,
        hash: &str,
    ) -> Result<(crate::offline::Receipt, bool), WalletError> {
        let source = parse_hash(hash)?.into_tx_hash();
        let entry = self
            .account_pending_source(address.as_str(), hash)
            .await?
            .ok_or_else(|| WalletError::Node("payment is not pending for this wallet".into()))?;
        let amount = entry
            .amount
            .parse()
            .map_err(|_| WalletError::Node("invalid pending amount".into()))?;
        let send = self.rpc_call("block_info", json!({"hash":hash})).await?;
        let origin = if send["block_type"] == "Burn" {
            parse_hash(hash)?.into_tx_hash()
        } else {
            parse_hash(
                send["origin"]
                    .as_str()
                    .ok_or_else(|| WalletError::Node("missing origin".into()))?,
            )?
            .into_tx_hash()
        };
        Ok((
            crate::offline::Receipt {
                source,
                origin,
                amount,
                sender: parse_address(&entry.source)?,
            },
            send["confirmed"] == true,
        ))
    }

    /// Decline without receiving first, including a wallet with no account chain.
    pub async fn reject_signed(
        &self,
        keys: &KeyPair,
        hash: &str,
    ) -> Result<ProcessResult, WalletError> {
        let source = parse_hash(hash)?;
        let address = burst_crypto::derive_address(&keys.public);
        let entry = self
            .account_pending_source(address.as_str(), &source.to_string())
            .await?
            .ok_or_else(|| {
                WalletError::Node(
                    "payment is no longer pending; refresh before trying again".into(),
                )
            })?;
        if entry.return_credit {
            return Err(WalletError::TransactionBuild(
                "returned funds are already yours; receive the credit instead".into(),
            ));
        }
        self.open_empty_account(keys)
            .await
            .map_err(|error| WalletError::Preparation(Box::new(error)))?;
        let tx = build_reject_receive_tx(&address, source.into_tx_hash(), Timestamp::now())?;
        self.submit_signed(keys, &Transaction::RejectReceive(tx), TxHash::ZERO)
            .await
    }

    async fn open_empty_account(&self, keys: &KeyPair) -> Result<(), WalletError> {
        let account = burst_crypto::derive_address(&keys.public);
        let state = self.signing_state(&account).await?;
        if !state.head.is_zero() {
            return Ok(());
        }
        if state.block_count != 0
            || state.brn_balance != 0
            || state.trst_balance != 0
            || !state.release_sources.is_empty()
        {
            return Err(WalletError::Node(
                "unopened account has inconsistent state".into(),
            ));
        }
        let params = self.rpc_call("params_hash", json!({})).await?;
        work_parameters(&params)?;
        let params_hash = parse_hash(
            params["params_hash"]
                .as_str()
                .ok_or_else(|| WalletError::Node("missing params_hash".into()))?,
        )?;
        let mut block = StateBlock {
            signing_grant: burst_types::BlockHash::ZERO,
            delegation_payload: Vec::new(),
            version: burst_ledger::CURRENT_BLOCK_VERSION,
            payment_epoch: state.payment_epoch,
            block_type: BlockType::Open,
            account,
            previous: BlockHash::ZERO,
            representative: state.representative,
            brn_balance: 0,
            trst_balance: 0,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::now(),
            params_hash,
            target_head: BlockHash::ZERO,
            merge_sources: Vec::new(),
            release_sources: Vec::new(),
            work: 0,
            signature: burst_types::Signature([0; 64]),
            hash: BlockHash::ZERO,
        };
        require_payment_epoch(&params, state.payment_epoch)?;
        block.hash = block.compute_hash();
        self.submit_with_work(sign_state_block(block, &keys.private))
            .await?;
        Ok(())
    }

    pub async fn transfer_signed(
        &self,
        keys: &KeyPair,
        receiver: &WalletAddress,
        amount: u128,
        origin: TxHash,
    ) -> Result<ProcessResult, WalletError> {
        if amount == 0 {
            return Err(WalletError::TransactionBuild(
                "amount must be positive".into(),
            ));
        }
        let sender = burst_crypto::derive_address(&keys.public);
        let state = self.signing_state(&sender).await?;
        let tx = build_send_tx(
            &sender,
            receiver,
            amount,
            state.head.into_tx_hash(),
            origin,
            Timestamp::now(),
        )?;
        self.submit_signed(keys, &Transaction::Send(tx), origin)
            .await
    }
}

#[cfg(test)]
mod submission_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[tokio::test]
    async fn online_action_journal_precedes_submission_and_survives_uncertainty() {
        let keys = burst_crypto::keypair_from_seed(&[68; 32]);
        let account = burst_crypto::derive_address(&keys.public);
        let state = AccountState {
            signing_grant: burst_types::BlockHash::ZERO,
            payment_epoch: BlockHash::new([1; 32]),
            head: BlockHash::new([2; 32]),
            block_count: 1,
            representative: account.clone(),
            brn_balance: 0,
            trst_balance: 0,
            release_sources: vec![],
        };
        let tx = Transaction::Burn(build_burn_tx(&account, &account, 1, Timestamp::now()).unwrap());
        let block = build_and_sign_state_block(
            &state,
            &tx,
            &keys.private,
            TxHash::ZERO,
            BlockHash::new([3; 32]),
        )
        .unwrap();
        let vote = Transaction::GovernanceVote(
            build_governance_vote_tx(
                &account,
                TxHash::new([4; 32]),
                burst_transactions::governance::GovernanceVote::Yea,
                Timestamp::now(),
            )
            .unwrap(),
        );
        let vote = build_and_sign_state_block(
            &state,
            &vote,
            &keys.private,
            TxHash::ZERO,
            BlockHash::new([3; 32]),
        )
        .unwrap();
        let content = burst_governance::ProposalContent::ParameterChange {
            param: burst_governance::GovernableParam::BrnRate,
            new_value: 1,
        };
        let (proposal, _) = crate::governance_content::build_signed_proposal(
            &state,
            &keys,
            &content,
            BlockHash::new([3; 32]),
            Timestamp::now(),
        )
        .unwrap();
        let handoff = sign_state_block(
            crate::transaction_builder::build_signing_handoff(
                &state,
                &account,
                BlockHash::new([5; 32]),
                BlockHash::new([3; 32]),
                Timestamp::now(),
            )
            .unwrap(),
            &keys.private,
        );
        for (block, content) in [
            (block, None),
            (vote, None),
            (proposal, Some(content.encode_content().unwrap())),
            (handoff, None),
        ] {
            for confirmed in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("payment.json");
                let inspect_path = path.clone();
                let expected = block.clone();
                let expected_content = content.as_ref().map(hex::encode);
                let mut script = vec![
                    (
                        "block_info",
                        json!({"account":account.as_str(),"confirmed":true}),
                    ),
                    (
                        "process",
                        if confirmed {
                            json!({"hash":block.hash.to_string(),"accepted":true})
                        } else {
                            json!({"malformed":true})
                        },
                    ),
                ];
                if confirmed {
                    script.push(("block_info", json!({"confirmed":true})));
                }
                let (client, server) = offline_rpc_script_check(script, move |request| {
                    if request["action"] == "process" {
                        assert_eq!(
                            request["proposal_content"].as_str(),
                            expected_content.as_deref()
                        );
                        let saved = crate::offline::load_payment(&inspect_path).unwrap();
                        assert_eq!(
                            serde_json::to_string(&saved).unwrap(),
                            serde_json::to_string(&expected).unwrap()
                        );
                        assert!(
                            crate::offline::payment_was_submitted(&inspect_path, &saved.hash)
                                .unwrap()
                        );
                    }
                });
                let result = client
                    .with_payment_journal(path.clone())
                    .submit_ready_block_with_content(block.clone(), content.as_deref())
                    .await;
                server.join().unwrap();
                if confirmed {
                    assert!(result.is_ok());
                    if block.block_type == BlockType::ChangeSigner {
                        assert_eq!(
                            crate::offline::load_payment(&path).unwrap().hash,
                            block.hash
                        );
                        let binding = directory.path().join("device.json");
                        assert_eq!(
                            crate::signing_device::load(&binding, &account).unwrap(),
                            BlockHash::ZERO
                        );
                        crate::signing_device::activate(&binding, &account, &block).unwrap();
                        crate::offline::archive_payment(&path, "confirmed").unwrap();
                        assert_eq!(
                            crate::signing_device::load(&binding, &account).unwrap(),
                            block.hash
                        );
                    }
                    assert!(!path.exists());
                    let archive = path
                        .parent()
                        .unwrap()
                        .join("completed")
                        .join(block.account.as_str())
                        .join(format!("confirmed-{}.json", block.hash));
                    assert_eq!(
                        crate::offline::load_payment(&archive).unwrap().hash,
                        block.hash
                    );
                } else {
                    assert!(result
                        .unwrap_err()
                        .to_string()
                        .contains("confirmation pending"));
                    assert_eq!(
                        crate::offline::load_payment(&path).unwrap().hash,
                        block.hash
                    );
                    assert!(crate::offline::payment_was_submitted(&path, &block.hash).unwrap());
                }
            }
        }
    }

    #[tokio::test]
    async fn online_payment_journal_failure_sends_no_process_request() {
        let keys = burst_crypto::keypair_from_seed(&[69; 32]);
        let account = burst_crypto::derive_address(&keys.public);
        let state = AccountState {
            signing_grant: burst_types::BlockHash::ZERO,
            payment_epoch: BlockHash::new([1; 32]),
            head: BlockHash::ZERO,
            block_count: 0,
            representative: account.clone(),
            brn_balance: 0,
            trst_balance: 0,
            release_sources: vec![],
        };
        let tx = Transaction::Burn(build_burn_tx(&account, &account, 1, Timestamp::now()).unwrap());
        let block = build_and_sign_state_block(
            &state,
            &tx,
            &keys.private,
            TxHash::ZERO,
            BlockHash::new([3; 32]),
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("payment.json");
        std::fs::write(&path, b"existing reservation").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let client = NodeClient::new(format!("http://{}", listener.local_addr().unwrap()))
            .unwrap()
            .with_payment_journal(path.clone());
        assert!(client.submit_ready_block(block).await.is_err());
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
        assert_eq!(std::fs::read(path).unwrap(), b"existing reservation");
    }

    fn offline_rpc_script(
        script: Vec<(&'static str, Value)>,
    ) -> (NodeClient, std::thread::JoinHandle<Vec<Value>>) {
        offline_rpc_script_check(script, |_| {})
    }

    #[tokio::test]
    async fn offline_action_preparation_keeps_neither_unstaked_and_binds_proposal_review() {
        use crate::offline::OfflineAction;
        let keys = burst_crypto::keypair_from_seed(&[81; 32]);
        let account = burst_crypto::derive_address(&keys.public);
        let head = BlockHash::new([1; 32]);
        let epoch = BlockHash::new([2; 32]);
        let params = json!({"online_signing":"peer_matches","block_version":burst_ledger::CURRENT_BLOCK_VERSION,
            "payment_epoch":epoch.to_string(),"params_hash":BlockHash::new([3;32]).to_string(),"min_work_difficulty":"0"});
        let proposal = TxHash::new([4; 32]);
        let panel = BlockHash::new([5; 32]);
        let deadline = Timestamp::now().as_secs() + 300;
        let review = json!({"hash":proposal.to_string(),"phase":"Exploration","phase_projected":false,"phase_deadline":deadline,
            "param":"BrnRate","proposed_value":"1000000000000000001"});
        for projected in [Some(false), Some(true), None] {
            let neither = projected.is_none();
            let mut review = review.clone();
            review["phase_projected"] = json!(projected.unwrap_or(false));
            let action = if neither {
                OfflineAction::VerificationVote {
                    target: account.clone(),
                    context: panel,
                    vote: 3,
                    stake: 0,
                }
            } else {
                OfflineAction::GovernanceVote {
                    proposal,
                    vote: burst_transactions::governance::GovernanceVote::Yea,
                }
            };
            let mut script = vec![("params_hash", params.clone())];
            if neither {
                script.extend([("verification_status",json!({"selected_verifiers":[account.as_str()],"verifier_votes":[],
                    "selection_available":true,"selection_context":panel.to_string(),"phase":"voting"})),
                    ("protocol_params",json!({"verifier_stake_amount":"1000000000000000000"}))]);
            } else {
                script.push(("governance_proposal_info", review.clone()));
            }
            script.extend([
                ("account_info",json!({"head":head.to_string(),"payment_epoch":epoch.to_string(),"block_count":1,
                    "representative":account.as_str(),"trst_balance":"0","release_sources":[],"has_more_releases":false})),
                ("block_info",json!({"active_signing_grant":BlockHash::ZERO.to_string(),"brn_balance":"7"})),
                ("params_hash",params.clone()),
                ("work_generate",json!({"hash":head.to_string(),"work":"0","difficulty":"0"})),
                ("block_info",json!({"account":account.as_str(),"confirmed":true})),
            ]);
            let (client, server) = offline_rpc_script(script);
            let context = client
                .prepare_offline_action(&account, action)
                .await
                .unwrap();
            let requests = server.join().unwrap();
            assert!(requests
                .iter()
                .all(|request| request["action"] != "process"));
            drop(client);
            let signed = context.sign(&keys, Timestamp::now()).unwrap();
            assert_eq!(signed.block().brn_balance, 7);
            assert_eq!(
                signed.block().transaction.as_bytes()[0],
                if neither { 3 } else { 0 }
            );
            if !neither {
                assert_eq!(context.review(), &review);
            }
        }
        for field in ["hash", "phase_deadline", "phase_projected"] {
            let mut invalid = review.clone();
            invalid[field] = Value::Null;
            let (client, server) = offline_rpc_script(vec![
                ("params_hash", params.clone()),
                ("governance_proposal_info", invalid),
            ]);
            assert!(client
                .prepare_offline_action(
                    &account,
                    OfflineAction::GovernanceVote {
                        proposal,
                        vote: burst_transactions::governance::GovernanceVote::Yea
                    }
                )
                .await
                .is_err());
            assert_eq!(server.join().unwrap().len(), 2);
        }
    }

    #[tokio::test]
    async fn saved_offline_actions_preserve_content_and_recover_without_resigning() {
        let keys = burst_crypto::keypair_from_seed(&[71; 32]);
        let account = burst_crypto::derive_address(&keys.public);
        let state = AccountState {
            signing_grant: BlockHash::ZERO,
            payment_epoch: BlockHash::new([1; 32]),
            head: BlockHash::new([2; 32]),
            block_count: 1,
            representative: account.clone(),
            brn_balance: 11,
            trst_balance: 20,
            release_sources: vec![],
        };
        let params_hash = BlockHash::new([3; 32]);
        let now = Timestamp::new(42);
        let vote = Transaction::GovernanceVote(
            build_governance_vote_tx(
                &account,
                TxHash::new([4; 32]),
                burst_transactions::governance::GovernanceVote::Yea,
                now,
            )
            .unwrap(),
        );
        let endorsement =
            Transaction::Endorse(build_endorse_tx(&account, &account, 1, now).unwrap());
        let content = burst_governance::ProposalContent::ParameterChange {
            param: burst_governance::GovernableParam::BrnRate,
            new_value: 1,
        };
        let (proposal, encoded) = crate::governance_content::build_signed_proposal(
            &state,
            &keys,
            &content,
            params_hash,
            now,
        )
        .unwrap();
        let actions = [
            (
                build_and_sign_state_block(&state, &vote, &keys.private, TxHash::ZERO, params_hash)
                    .unwrap(),
                None,
            ),
            (
                build_and_sign_state_block(
                    &state,
                    &endorsement,
                    &keys.private,
                    TxHash::ZERO,
                    params_hash,
                )
                .unwrap(),
                None,
            ),
            (proposal, Some(encoded.bytes().to_vec())),
        ];
        let info = json!({"head":state.head.to_string(), "payment_epoch":state.payment_epoch.to_string(),
            "block_count":1, "representative":account.as_str(), "trst_balance":"20",
            "release_sources":[], "has_more_releases":false});
        let predecessor =
            json!({"active_signing_grant":BlockHash::ZERO.to_string(), "brn_balance":"11"});
        let params = json!({"online_signing":"peer_matches", "block_version":burst_ledger::CURRENT_BLOCK_VERSION,
            "payment_epoch":state.payment_epoch.to_string(), "params_hash":params_hash.to_string(),
            "min_work_difficulty":"0"});
        for (block, content) in actions {
            for lost_response in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("action.json");
                crate::offline::save_signed_action(&path, &block, &[], content.as_deref()).unwrap();
                let original = std::fs::read(&path).unwrap();
                let expected_block = serde_json::to_string(&block).unwrap();
                let expected_content = content.as_ref().map(hex::encode);
                let inspect_path = path.clone();
                let mut script = vec![
                    ("block_info", json!({"confirmed":false})),
                    ("account_info", info.clone()),
                    ("block_info", predecessor.clone()),
                    ("params_hash", params.clone()),
                    (
                        "process",
                        if lost_response {
                            json!({"malformed":true})
                        } else {
                            json!({"accepted":true,"hash":block.hash.to_string()})
                        },
                    ),
                ];
                if !lost_response {
                    script.push(("block_info", json!({"confirmed":true})));
                }
                let saved_hash = block.hash;
                let (client, server) = offline_rpc_script_check(script, move |request| {
                    if request["action"] == "process" {
                        assert_eq!(request["block"].as_str(), Some(expected_block.as_str()));
                        assert_eq!(
                            request["proposal_content"].as_str(),
                            expected_content.as_deref()
                        );
                        assert!(
                            crate::offline::payment_was_submitted(&inspect_path, &saved_hash)
                                .unwrap()
                        );
                    }
                });
                let result = client.submit_saved_offline_action(&path, &account).await;
                assert_eq!(result.is_ok(), !lost_response);
                let requests = server.join().unwrap();
                assert_eq!(
                    requests.iter().filter(|r| r["action"] == "process").count(),
                    1
                );
                assert_eq!(std::fs::read(&path).unwrap(), original);
                // A new client resolves an accepted-but-unanswered action without
                // fetching a fresh signing context or issuing another process call.
                let (client, server) =
                    offline_rpc_script(vec![("block_info", json!({"confirmed":true}))]);
                let result = client
                    .submit_saved_offline_action(&path, &account)
                    .await
                    .unwrap();
                assert_eq!(result.hash, block.hash.to_string());
                assert_eq!(server.join().unwrap().len(), 1);
                assert_eq!(std::fs::read(&path).unwrap(), original);
            }
            for stale in ["head", "params", "grant"] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("action.json");
                crate::offline::save_signed_action(&path, &block, &[], content.as_deref()).unwrap();
                let mut changed_info = info.clone();
                let mut changed_params = params.clone();
                let grant = if stale == "grant" {
                    BlockHash::new([10; 32])
                } else {
                    BlockHash::ZERO
                };
                if stale == "head" {
                    changed_info["head"] = json!(BlockHash::new([9; 32]).to_string());
                }
                if stale == "params" {
                    changed_params["params_hash"] = json!(BlockHash::new([9; 32]).to_string());
                }
                let mut changed_predecessor = predecessor.clone();
                changed_predecessor["active_signing_grant"] = json!(grant.to_string());
                let (client, server) = offline_rpc_script(vec![
                    ("block_info", json!({"confirmed":false})),
                    ("account_info", changed_info),
                    ("block_info", changed_predecessor),
                    ("params_hash", changed_params),
                ]);
                assert!(client
                    .with_signing_grant(grant)
                    .submit_saved_offline_action(&path, &account)
                    .await
                    .is_err());
                assert_eq!(server.join().unwrap().len(), 4);
                assert!(!crate::offline::payment_was_submitted(&path, &block.hash).unwrap());
            }
        }
    }

    fn offline_rpc_script_check(
        script: Vec<(&'static str, Value)>,
        inspect: impl Fn(&Value) + Send + 'static,
    ) -> (NodeClient, std::thread::JoinHandle<Vec<Value>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let client = NodeClient::new(format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let handle = std::thread::spawn(move || {
            let mut requests = vec![];
            for (action, response) in script {
                let started = Instant::now();
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                started.elapsed() < Duration::from_secs(5),
                                "missing RPC {action}"
                            );
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("{error}"),
                    }
                };
                let request = read_request(&mut stream);
                assert_eq!(request["action"], action);
                inspect(&request);
                requests.push(request);
                let body = response.to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            }
            requests
        });
        (client, handle)
    }

    #[tokio::test]
    async fn offline_ancestry_survives_restart_and_resolves_only_a_confirmed_conflict() {
        let keys = burst_crypto::keypair_from_seed(&[67; 32]);
        let account = burst_crypto::derive_address(&keys.public);
        let anchor = BlockHash::new([7; 32]);
        let epoch = BlockHash::new([8; 32]);
        let params_hash = BlockHash::new([9; 32]);
        let state = AccountState {
            signing_grant: burst_types::BlockHash::ZERO,
            payment_epoch: epoch,
            release_sources: vec![],
            head: anchor,
            block_count: 1,
            representative: account.clone(),
            brn_balance: u64::MAX as u128 + 1,
            trst_balance: 0,
        };
        let tx =
            Transaction::Burn(build_burn_tx(&account, &account, 1, Timestamp::new(42)).unwrap());
        let ancestor =
            build_and_sign_state_block(&state, &tx, &keys.private, TxHash::ZERO, params_hash)
                .unwrap();
        let tip_state = AccountState {
            head: ancestor.hash,
            block_count: 2,
            brn_balance: ancestor.brn_balance,
            ..state.clone()
        };
        let tip =
            build_and_sign_state_block(&tip_state, &tx, &keys.private, TxHash::ZERO, params_hash)
                .unwrap();
        let info = json!({"head":tip.hash.to_string(),"payment_epoch":epoch.to_string(),
            "block_count":3,"representative":account.as_str(),"trst_balance":"0",
            "release_sources":[],"has_more_releases":false});
        let params = json!({"online_signing":"peer_matches", "block_version":burst_ledger::CURRENT_BLOCK_VERSION,
            "payment_epoch":epoch.to_string(),"params_hash":params_hash.to_string(),"min_work_difficulty":"0"});
        let (client, server) = offline_rpc_script(vec![
            ("account_info", info),
            (
                "block_info",
                json!({"active_signing_grant":BlockHash::ZERO.to_string(), "brn_balance":tip.brn_balance.to_string()}),
            ),
            ("params_hash", params),
            (
                "work_generate",
                json!({"hash":tip.hash.to_string(),"work":"0","difficulty":"0"}),
            ),
            (
                "block_info",
                json!({"account":account.as_str(),"confirmed":false,"signed_block":serde_json::to_string(&tip).unwrap()}),
            ),
            (
                "block_info",
                json!({"account":account.as_str(),"confirmed":false,"signed_block":serde_json::to_string(&ancestor).unwrap()}),
            ),
            (
                "block_info",
                json!({"account":account.as_str(),"confirmed":true}),
            ),
        ]);
        let context = client.prepare_offline_payment(&account).await.unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests[4]["include_signed_block"], true);
        assert_eq!(requests[5]["hash"], ancestor.hash.to_string());
        assert_eq!(requests[6]["hash"], anchor.to_string());
        let draft = context.sign(&keys, &tx, TxHash::ZERO).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("draft.json");
        context.save_payment(&path, &draft).unwrap();
        drop(context);
        let (draft, ancestry) = crate::offline::load_payment_with_ancestry(&path).unwrap();
        assert_eq!(ancestry.len(), 2);
        assert_eq!(ancestry[0].hash, tip.hash);
        assert_eq!(ancestry[1].hash, ancestor.hash);
        assert_eq!(ancestry[0].brn_balance, tip.brn_balance);
        assert_eq!(ancestry[1].brn_balance, ancestor.brn_balance);
        assert!(ancestry[1].brn_balance > u64::MAX as u128);
        let original = std::fs::read(&path).unwrap();
        let winner = BlockHash::new([10; 32]);
        let slot = |previous: BlockHash, successor: Option<BlockHash>| {
            json!({"account":account.as_str(),
            "previous":previous.to_string(),"successor":successor.map(|hash|hash.to_string())})
        };
        let missing = slot(draft.previous, None);
        let confirmed_anchor = slot(anchor, Some(winner));
        let (client, server) = offline_rpc_script(vec![
            ("confirmed_account_slot", missing.clone()),
            ("confirmed_account_slot", confirmed_anchor.clone()),
        ]);
        assert_eq!(
            client
                .confirmed_offline_resolution(&draft, &ancestry)
                .await
                .unwrap(),
            Some(winner)
        );
        server.join().unwrap();
        let (client, server) = offline_rpc_script(vec![
            ("confirmed_account_slot", missing.clone()),
            ("confirmed_account_slot", slot(anchor, Some(ancestor.hash))),
            ("confirmed_account_slot", slot(ancestor.hash, None)),
        ]);
        assert_eq!(
            client
                .confirmed_offline_resolution(&draft, &ancestry)
                .await
                .unwrap(),
            None
        );
        server.join().unwrap();
        for (field, value) in [
            ("successor", json!(BlockHash::ZERO.to_string())),
            ("successor", json!("invalid")),
            ("previous", json!(draft.hash.to_string())),
            ("account", json!("unrelated")),
        ] {
            let mut invalid = confirmed_anchor.clone();
            invalid[field] = value;
            let (client, server) = offline_rpc_script(vec![
                ("confirmed_account_slot", missing.clone()),
                ("confirmed_account_slot", invalid),
            ]);
            assert!(client
                .confirmed_offline_resolution(&draft, &ancestry)
                .await
                .is_err());
            server.join().unwrap();
        }
        let (client, server) = offline_rpc_script(vec![
            ("confirmed_account_slot", missing.clone()),
            (
                "confirmed_account_slot",
                json!({"error":"recovery unavailable"}),
            ),
        ]);
        assert!(client
            .confirmed_offline_resolution(&draft, &ancestry)
            .await
            .is_err());
        server.join().unwrap();
        let (client, server) = offline_rpc_script(vec![
            (
                "confirmed_account_slot",
                json!({"error":"invalid request: unknown action: confirmed_account_slot"}),
            ),
            (
                "block_info",
                json!({"account":account.as_str(),"confirmed":true,"confirmed_successor":winner.to_string()}),
            ),
            (
                "block_info",
                json!({"account":account.as_str(),"previous":draft.previous.to_string(),"confirmed":true}),
            ),
        ]);
        assert_eq!(
            client
                .confirmed_offline_resolution(&draft, &ancestry)
                .await
                .unwrap(),
            Some(winner)
        );
        server.join().unwrap();
        let (client, server) = offline_rpc_script(vec![]);
        let mut invalid = ancestry.clone();
        invalid[0].previous = BlockHash::ZERO;
        assert!(client
            .confirmed_offline_resolution(&draft, &invalid)
            .await
            .is_err());
        server.join().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[tokio::test]
    async fn offline_review_preserves_exact_amount_and_requires_confirmed_account_balance() {
        let keys = burst_crypto::keypair_from_seed(&[62; 32]);
        let account = burst_crypto::derive_address(&keys.public);
        let provider =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[64; 32]).public);
        let previous = BlockHash::new([7; 32]);
        let burned = 1_000_000_000_000_000_003_u128;
        let amount = 1_000_000_000_000_000_001_u128;
        let state = AccountState {
            signing_grant: burst_types::BlockHash::ZERO,
            payment_epoch: BlockHash::new([8; 32]),
            release_sources: vec![],
            head: previous,
            block_count: 1,
            representative: account.clone(),
            brn_balance: burned,
            trst_balance: 0,
        };
        let tx = Transaction::Burn(
            build_burn_tx(&account, &provider, amount, Timestamp::new(42)).unwrap(),
        );
        let block = build_and_sign_state_block(
            &state,
            &tx,
            &keys.private,
            TxHash::ZERO,
            BlockHash::new([9; 32]),
        )
        .unwrap();
        let anchor = json!({"account": account.as_str(), "confirmed": true, "brn_balance": burned.to_string()});
        let (client, server) = offline_rpc_script(vec![("block_info", anchor.clone())]);
        let review = client.review_offline_payment(&block).await.unwrap();
        assert_eq!(review.hash, block.hash.to_string());
        assert_eq!(review.provider, provider.as_str());
        assert_eq!(review.amount, amount.to_string());
        assert_eq!(server.join().unwrap()[0]["hash"], previous.to_string());
        let exclusion = json!({"account": account.as_str(), "previous": previous.to_string(),
            "payment_epoch": block.payment_epoch.to_string(), "installed_epoch": BlockHash::new([10; 32]).to_string(), "excluded": true});
        let (client, server) = offline_rpc_script(vec![("payment_exclusion", exclusion.clone())]);
        assert_eq!(
            client.excluded_offline_payment(&block).await.unwrap(),
            Some(BlockHash::new([10; 32]))
        );
        let request = &server.join().unwrap()[0];
        assert_eq!(request["account"], account.as_str());
        assert_eq!(request["previous"], previous.to_string());
        assert_eq!(request["payment_epoch"], block.payment_epoch.to_string());
        let mut unresolved = exclusion.clone();
        unresolved["excluded"] = json!(false);
        let (client, server) = offline_rpc_script(vec![("payment_exclusion", unresolved)]);
        assert_eq!(client.excluded_offline_payment(&block).await.unwrap(), None);
        server.join().unwrap();
        for (field, value) in [
            ("account", json!(provider.as_str())),
            ("previous", json!(BlockHash::ZERO.to_string())),
            ("payment_epoch", json!(BlockHash::ZERO.to_string())),
            ("installed_epoch", json!(BlockHash::ZERO.to_string())),
            ("installed_epoch", json!(block.payment_epoch.to_string())),
        ] {
            let mut invalid = exclusion.clone();
            invalid[field] = value;
            let (client, server) = offline_rpc_script(vec![("payment_exclusion", invalid)]);
            assert!(client.excluded_offline_payment(&block).await.is_err());
            server.join().unwrap();
        }
        for (field, value) in [
            ("account", json!(provider.as_str())),
            ("confirmed", json!(false)),
            ("brn_balance", json!(block.brn_balance.to_string())),
            ("brn_balance", json!(u128::MAX.to_string())),
            ("brn_balance", json!(burned)),
            ("brn_balance", Value::Null),
        ] {
            let mut invalid = anchor.clone();
            invalid[field] = value;
            let (client, server) = offline_rpc_script(vec![("block_info", invalid)]);
            assert!(client.review_offline_payment(&block).await.is_err());
            server.join().unwrap();
        }
    }

    #[tokio::test]
    async fn offline_opening_recovers_confirmed_slot_or_closed_epoch_only() {
        let keys = burst_crypto::keypair_from_seed(&[65; 32]);
        let account = burst_crypto::derive_address(&keys.public);
        let epoch = BlockHash::new([8; 32]);
        let context = crate::offline::OfflinePaymentContext {
            ancestry: Vec::new(),
            account: account.clone(),
            state: AccountState {
                signing_grant: burst_types::BlockHash::ZERO,
                payment_epoch: epoch,
                release_sources: vec![],
                head: BlockHash::ZERO,
                block_count: 0,
                representative: account.clone(),
                brn_balance: 0,
                trst_balance: 0,
            },
            params_hash: BlockHash::new([9; 32]),
            threshold: 0,
            work: PreparedWork {
                root: StateBlock::work_root_for(&account, BlockHash::ZERO),
                nonce: 0,
            },
        };
        let tx = Transaction::Receive(
            build_receive_tx(&account, TxHash::new([5; 32]), 7, Timestamp::new(42)).unwrap(),
        );
        let block = context.sign(&keys, &tx, TxHash::new([6; 32])).unwrap();
        let sender =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[66; 32]).public);
        let pending = json!({"pending":[{"hash":block.link.to_string(),"source":sender.as_str(),"amount":"7"}]});
        let source =
            json!({"block_type":"Send", "origin":block.origin.to_string(), "confirmed":true});
        let (client, server) = offline_rpc_script(vec![
            ("account_pending", pending.clone()),
            ("block_info", source.clone()),
        ]);
        let review = client.review_offline_payment(&block).await.unwrap();
        assert_eq!(review.kind, "receive");
        assert_eq!(review.amount, "7");
        assert_eq!(review.provider, sender.as_str());
        assert_eq!(review.payment, Some(block.link.to_string()));
        server.join().unwrap();
        let mut wrong_amount = pending.clone();
        wrong_amount["pending"][0]["amount"] = json!("8");
        let mut unconfirmed = source.clone();
        unconfirmed["confirmed"] = json!(false);
        for (pending, source) in [(wrong_amount, source.clone()), (pending, unconfirmed)] {
            let (client, server) =
                offline_rpc_script(vec![("account_pending", pending), ("block_info", source)]);
            assert!(client.review_offline_payment(&block).await.is_err());
            server.join().unwrap();
        }
        for successor in [block.hash, BlockHash::new([10; 32])] {
            let info = json!({"address":account.as_str(), "confirmation_height":1, "confirmed_open":successor.to_string()});
            let candidate = json!({"account":account.as_str(), "confirmed":true, "previous":BlockHash::ZERO.to_string()});
            let (client, server) = offline_rpc_script(vec![
                ("account_info", info.clone()),
                ("block_info", candidate.clone()),
            ]);
            assert_eq!(
                client.confirmed_offline_successor(&block).await.unwrap(),
                Some(successor)
            );
            let requests = server.join().unwrap();
            assert_eq!(requests[0]["account"], account.as_str());
            assert_eq!(requests[1]["hash"], successor.to_string());
            for (field, value) in [
                ("confirmed", json!(false)),
                ("account", json!("unrelated")),
                ("previous", json!(epoch.to_string())),
            ] {
                let mut invalid = candidate.clone();
                invalid[field] = value;
                let (client, server) = offline_rpc_script(vec![
                    ("account_info", info.clone()),
                    ("block_info", invalid),
                ]);
                assert!(client.confirmed_offline_successor(&block).await.is_err());
                server.join().unwrap();
            }
            let mut unconfirmed = info.clone();
            unconfirmed["confirmation_height"] = json!(0);
            let (client, server) = offline_rpc_script(vec![("account_info", unconfirmed)]);
            assert_eq!(
                client.confirmed_offline_successor(&block).await.unwrap(),
                None
            );
            server.join().unwrap();
            let mut old_node = info;
            old_node.as_object_mut().unwrap().remove("confirmed_open");
            let (client, server) = offline_rpc_script(vec![("account_info", old_node)]);
            assert_eq!(
                client.confirmed_offline_successor(&block).await.unwrap(),
                None
            );
            server.join().unwrap();
        }
        let review = json!({"account":account.as_str(), "previous":BlockHash::ZERO.to_string(),
            "payment_epoch":epoch.to_string(), "installed_epoch":BlockHash::new([11;32]).to_string(), "excluded":true});
        let (client, server) = offline_rpc_script(vec![("payment_exclusion", review.clone())]);
        assert_eq!(
            client.excluded_offline_payment(&block).await.unwrap(),
            Some(BlockHash::new([11; 32]))
        );
        server.join().unwrap();
        let mut current = review;
        current["installed_epoch"] = json!(epoch.to_string());
        let (client, server) = offline_rpc_script(vec![("payment_exclusion", current)]);
        assert!(client.excluded_offline_payment(&block).await.is_err());
        server.join().unwrap();
    }

    #[tokio::test]
    async fn offline_recovery_requires_confirmed_successor_of_exact_anchor() {
        let keys = burst_crypto::keypair_from_seed(&[63; 32]);
        let account = burst_crypto::derive_address(&keys.public);
        let previous = BlockHash::new([7; 32]);
        let state = AccountState {
            signing_grant: burst_types::BlockHash::ZERO,
            payment_epoch: BlockHash::new([8; 32]),
            release_sources: vec![],
            head: previous,
            block_count: 1,
            representative: account.clone(),
            brn_balance: 0,
            trst_balance: 0,
        };
        let tx =
            Transaction::Burn(build_burn_tx(&account, &account, 1, Timestamp::new(42)).unwrap());
        let block = build_and_sign_state_block(
            &state,
            &tx,
            &keys.private,
            TxHash::ZERO,
            BlockHash::new([9; 32]),
        )
        .unwrap();
        for successor in [block.hash, BlockHash::new([10; 32])] {
            let anchor = json!({"account":account.as_str(),"confirmed":true,"confirmed_successor":successor.to_string()});
            let candidate = json!({"account":account.as_str(),"confirmed":true,"previous":previous.to_string()});
            let (client, server) = offline_rpc_script(vec![
                ("block_info", anchor.clone()),
                ("block_info", candidate.clone()),
            ]);
            assert_eq!(
                client.confirmed_offline_successor(&block).await.unwrap(),
                Some(successor)
            );
            let requests = server.join().unwrap();
            assert_eq!(requests[0]["hash"], previous.to_string());
            assert_eq!(requests[1]["hash"], successor.to_string());
            for field in ["confirmed", "account", "previous"] {
                let mut invalid = candidate.clone();
                invalid[field] = if field == "confirmed" {
                    json!(false)
                } else {
                    json!("unrelated")
                };
                let (client, server) = offline_rpc_script(vec![
                    ("block_info", anchor.clone()),
                    ("block_info", invalid),
                ]);
                assert!(client.confirmed_offline_successor(&block).await.is_err());
                server.join().unwrap();
            }
        }
        for anchor in [
            json!({"account":account.as_str(),"confirmed":true}),
            json!({"account":account.as_str(),"confirmed":true,"confirmed_successor":null}),
        ] {
            let (client, server) = offline_rpc_script(vec![("block_info", anchor)]);
            assert_eq!(
                client.confirmed_offline_successor(&block).await.unwrap(),
                None
            );
            server.join().unwrap();
        }
        for anchor in [
            json!({"account":account.as_str(),"confirmed":false}),
            json!({"account":"unrelated","confirmed":true}),
        ] {
            let (client, server) = offline_rpc_script(vec![("block_info", anchor)]);
            assert!(client.confirmed_offline_successor(&block).await.is_err());
            server.join().unwrap();
        }
    }

    #[tokio::test]
    async fn signing_device_does_not_adopt_another_devices_grant() {
        let account =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[60; 32]).public);
        let grant = BlockHash::new([10; 32]);
        let info = json!({"head":BlockHash::new([7;32]).to_string(),
            "payment_epoch":BlockHash::new([8;32]).to_string(), "block_count":1,
            "representative":account.as_str(),"trst_balance":"20",
            "release_sources":[],"has_more_releases":false});
        for local in [BlockHash::ZERO, BlockHash::new([9; 32]), grant] {
            let (client, server) = offline_rpc_script(vec![
                ("account_info", info.clone()),
                (
                    "block_info",
                    json!({"brn_balance":"11", "active_signing_grant":grant.to_string()}),
                ),
            ]);
            let result = client
                .with_signing_grant(local)
                .signing_state(&account)
                .await;
            assert_eq!(result.is_ok(), local == grant);
            if let Ok(state) = result {
                assert_eq!(state.signing_grant, grant);
            }
            server.join().unwrap();
        }
    }

    #[tokio::test]
    async fn offline_payment_prepares_disconnects_signs_and_submits_exact_bytes() {
        let keys = burst_crypto::keypair_from_seed(&[61; 32]);
        let account = burst_crypto::derive_address(&keys.public);
        let head = BlockHash::new([7; 32]);
        let epoch = BlockHash::new([8; 32]);
        let info = json!({"head":head.to_string(),"payment_epoch":epoch.to_string(),
            "block_count":1,"representative":account.as_str(),"trst_balance":"20",
            "release_sources":[],"has_more_releases":false});
        let params = json!({"online_signing":"peer_matches", "block_version":burst_ledger::CURRENT_BLOCK_VERSION,
            "payment_epoch":epoch.to_string(),"params_hash":BlockHash::new([9;32]).to_string(),
            "min_work_difficulty":"0"});
        let (online, prepare_server) = offline_rpc_script(vec![
            ("account_info", info.clone()),
            (
                "block_info",
                json!({"active_signing_grant":BlockHash::ZERO.to_string(), "brn_balance":"11"}),
            ),
            ("params_hash", params.clone()),
            (
                "work_generate",
                json!({"hash":head.to_string(),"work":"0","difficulty":"0"}),
            ),
            (
                "block_info",
                json!({"account":account.as_str(),"confirmed":true}),
            ),
        ]);
        let context = online.prepare_offline_payment(&account).await.unwrap();
        prepare_server.join().unwrap(); // Listener is gone before choosing the payment.
        let tx =
            Transaction::Burn(build_burn_tx(&account, &account, 7, Timestamp::new(42)).unwrap());
        let block = context.sign(&keys, &tx, TxHash::ZERO).unwrap();
        let drafts = tempfile::tempdir().unwrap();
        let draft_path = drafts.path().join("payment.json");
        context.save_payment(&draft_path, &block).unwrap();
        assert!(!crate::offline::payment_was_submitted(&draft_path, &block.hash).unwrap());
        drop(context);
        drop(keys);
        let expected = serde_json::to_string(&block).unwrap();
        let inspected_path = draft_path.clone();
        let inspected_hash = block.hash;
        let (reconnected, submit_server) = offline_rpc_script_check(
            vec![
                ("block_info", json!({"confirmed":false})),
                ("account_info", info.clone()),
                (
                    "block_info",
                    json!({"active_signing_grant":BlockHash::ZERO.to_string(), "brn_balance":"11"}),
                ),
                ("params_hash", params.clone()),
                (
                    "process",
                    json!({"accepted":true,"hash":block.hash.to_string()}),
                ),
                ("block_info", json!({"confirmed":true})),
            ],
            move |request| {
                if request["action"] == "process" {
                    assert!(crate::offline::payment_was_submitted(
                        &inspected_path,
                        &inspected_hash
                    )
                    .unwrap());
                }
            },
        );
        let other_account =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[62; 32]).public);
        let reconnected = reconnected.with_payment_journal(draft_path.clone());
        let original_draft = std::fs::read(&draft_path).unwrap();
        assert!(reconnected
            .submit_saved_offline_payment(&draft_path, &other_account)
            .await
            .unwrap_err()
            .to_string()
            .contains("another wallet"));
        assert_eq!(
            reconnected
                .submit_saved_offline_payment(&draft_path, &account)
                .await
                .unwrap()
                .hash,
            block.hash.to_string()
        );
        let requests = submit_server.join().unwrap();
        assert_eq!(requests[4]["block"].as_str(), Some(expected.as_str()));
        assert_eq!(std::fs::read(&draft_path).unwrap(), original_draft);

        // Retrying an already confirmed payment does not generate a replacement.
        let (retry, retry_server) =
            offline_rpc_script(vec![("block_info", json!({"confirmed":true}))]);
        retry
            .submit_saved_offline_payment(&draft_path, &account)
            .await
            .unwrap();
        assert_eq!(retry_server.join().unwrap().len(), 1);

        let mut changed = info.clone();
        changed["head"] = json!(BlockHash::new([10; 32]).to_string());
        let (stale, stale_server) = offline_rpc_script(vec![
            ("block_info", json!({"confirmed":false})),
            ("account_info", changed),
            (
                "block_info",
                json!({"active_signing_grant":BlockHash::ZERO.to_string(), "brn_balance":"12"}),
            ),
            ("params_hash", params.clone()),
        ]);
        let stale_directory = tempfile::tempdir().unwrap();
        let stale_path = stale_directory.path().join("payment.json");
        crate::offline::save_payment(&stale_path, &block).unwrap();
        assert!(stale
            .submit_saved_offline_payment(&stale_path, &account)
            .await
            .unwrap_err()
            .to_string()
            .contains("stale"));
        assert!(!crate::offline::payment_was_submitted(&stale_path, &block.hash).unwrap());
        assert!(stale_server
            .join()
            .unwrap()
            .iter()
            .all(|request| request["action"] != "process"));

        for field in ["params_hash", "payment_epoch", "min_work_difficulty"] {
            let mut changed = params.clone();
            changed[field] = if field == "min_work_difficulty" {
                json!("ffffffffffffffff")
            } else {
                json!(BlockHash::new([10; 32]).to_string())
            };
            let (stale, server) = offline_rpc_script(vec![
                ("block_info", json!({"confirmed":false})),
                ("account_info", info.clone()),
                (
                    "block_info",
                    json!({"active_signing_grant":BlockHash::ZERO.to_string(), "brn_balance":"11"}),
                ),
                ("params_hash", changed),
            ]);
            assert!(
                stale.submit_offline_payment(block.clone()).await.is_err(),
                "{field}"
            );
            assert_eq!(server.join().unwrap().len(), 4);
        }

        // The node may accept a block but lose its receipt. Preserve its exact
        // hash for reconciliation; no replacement signature or work request.
        let (uncertain, server) = offline_rpc_script(vec![
            ("block_info", json!({"confirmed":false})),
            ("account_info", info),
            (
                "block_info",
                json!({"active_signing_grant":BlockHash::ZERO.to_string(), "brn_balance":"11"}),
            ),
            ("params_hash", params),
            ("process", json!({"malformed_receipt":true})),
        ]);
        let error = uncertain
            .with_payment_journal(draft_path.clone())
            .submit_saved_offline_payment(&draft_path, &account)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("confirmation pending"));
        assert!(error.contains(&block.hash.to_string()));
        assert_eq!(
            server.join().unwrap()[4]["block"].as_str(),
            Some(expected.as_str())
        );
        assert_eq!(
            serde_json::to_string(&crate::offline::load_payment(&draft_path).unwrap()).unwrap(),
            expected
        );
    }

    fn read_request(stream: &mut std::net::TcpStream) -> Value {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        loop {
            let mut chunk = [0u8; 4096];
            let length = stream.read(&mut chunk).unwrap();
            assert_ne!(length, 0);
            request.extend_from_slice(&chunk[..length]);
            assert!(request.len() <= 256 * 1024);
            if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]);
                let size: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse().unwrap())
                    })
                    .unwrap();
                if request.len() >= end + 4 + size {
                    return serde_json::from_slice(&request[end + 4..end + 4 + size]).unwrap();
                }
            }
        }
    }

    async fn release_batch_attempt(
        drop_preparation_response: bool,
    ) -> (Result<ProcessResult, WalletError>, Vec<StateBlock>) {
        let keys = burst_crypto::keypair_from_seed(&[55; 32]);
        let address = burst_crypto::derive_address(&keys.public);
        let public = keys.public.clone();
        let signer = address.clone();
        let representative =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[56; 32]).public);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let sources: Vec<_> = (1..=1200u64)
                .map(|index| {
                    let mut bytes = [0; 32];
                    bytes[..8].copy_from_slice(&index.to_be_bytes());
                    BlockHash::new(bytes)
                })
                .collect();
            let mut blocks = Vec::<StateBlock>::new();
            let mut head = BlockHash::new([88; 32]);
            let started = Instant::now();
            loop {
                assert!(
                    started.elapsed() < Duration::from_secs(10),
                    "wallet batching RPC timed out"
                );
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("wallet batching listener failed: {error}"),
                };
                let request = read_request(&mut stream);
                let action = request["action"].as_str().unwrap();
                let response = match action {
                    "account_info" => {
                        let selected = if blocks.is_empty() {
                            &sources[..600]
                        } else {
                            &sources[600..]
                        };
                        json!({"payment_epoch":BlockHash::new([1;32]).to_string(), "head":head.to_string(), "block_count":1 + blocks.len(), "representative":representative.as_str(),
                            "trst_balance":"5", "release_sources":selected.iter().map(ToString::to_string).collect::<Vec<_>>(),
                            "has_more_releases":blocks.is_empty()})
                    }
                    "block_info" => {
                        json!({"active_signing_grant":BlockHash::ZERO.to_string(), "brn_balance":blocks.last().map_or(0, |block| block.brn_balance).to_string(), "confirmed":true})
                    }
                    "params_hash" => {
                        json!({"online_signing":"peer_matches", "payment_epoch":BlockHash::new([1;32]).to_string(), "block_version":burst_ledger::CURRENT_BLOCK_VERSION,
                        "params_hash":BlockHash::ZERO.to_string(), "min_work_difficulty":"0"})
                    }
                    "work_generate" => json!({"work":"0"}),
                    "process" => {
                        let block: StateBlock =
                            serde_json::from_str(request["block"].as_str().unwrap()).unwrap();
                        assert_eq!(block.account, signer);
                        assert_eq!(block.previous, head);
                        assert_eq!(block.representative, representative);
                        assert_eq!(block.trst_balance, 5);
                        assert_eq!(block.hash, block.compute_hash());
                        assert!(block.validate_format().is_ok());
                        assert!(burst_crypto::verify_signature(
                            block.hash.as_bytes(),
                            &block.signature,
                            &public
                        ));
                        if blocks.is_empty() {
                            assert_eq!(block.block_type, BlockType::ChangeRepresentative);
                            assert_eq!(block.brn_balance, 0);
                            assert_eq!(block.release_sources, sources[..600]);
                        } else {
                            assert_eq!(blocks.len(), 1);
                            assert_eq!(block.block_type, BlockType::Burn);
                            assert_eq!(block.brn_balance, 7);
                            assert_eq!(block.release_sources, sources[600..]);
                        }
                        head = block.hash;
                        blocks.push(block);
                        if drop_preparation_response {
                            return blocks;
                        }
                        json!({"accepted":true,"hash":head.to_string()})
                    }
                    _ => panic!("unexpected wallet RPC {action}"),
                };
                let body = response.to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                if blocks.len() == 2 && action == "block_info" {
                    return blocks;
                }
            }
        });
        let result = NodeClient::new(url)
            .unwrap()
            .pay_signed(&keys, &address, 7)
            .await;
        let blocks = server.join().unwrap();
        (result, blocks)
    }

    #[tokio::test]
    async fn oversized_release_backlog_uses_signed_batches_without_changing_delegation_or_spending_twice(
    ) {
        let (result, blocks) = release_batch_attempt(false).await;
        assert_eq!(result.unwrap().hash, blocks[1].hash.to_string());
    }

    #[tokio::test]
    async fn lost_preparation_response_does_not_report_the_unsubmitted_payment_as_pending() {
        let (result, blocks) = release_batch_attempt(true).await;
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].block_type, BlockType::ChangeRepresentative);
        let error = result.unwrap_err();
        assert!(matches!(&error, WalletError::Preparation(_)));
        let message = error.to_string();
        assert!(
            message
                .starts_with("account preparation incomplete; requested action was not submitted:"),
            "{message}"
        );
        assert!(message.contains(&blocks[0].hash.to_string()));
    }

    #[test]
    fn prepared_work_rechecks_root_version_and_current_threshold() {
        let keys = burst_crypto::keypair_from_seed(&[21; 32]);
        let account = burst_crypto::derive_address(&keys.public);
        let state = AccountState {
            signing_grant: burst_types::BlockHash::ZERO,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            head: BlockHash::new([8; 32]),
            block_count: 1,
            representative: account.clone(),
            brn_balance: 0,
            trst_balance: 0,
            release_sources: Vec::new(),
        };
        let tx = build_burn_tx(&account, &account, 1, Timestamp::new(100)).unwrap();
        let mut block = build_state_block(
            &state,
            &Transaction::Burn(tx),
            TxHash::ZERO,
            BlockHash::ZERO,
        )
        .unwrap();
        let work = PreparedWork {
            root: block.work_root(),
            nonce: 42,
        };
        assert!(work.apply_to(&mut block, 0));
        assert_eq!(block.work, 42);
        block.work = 7;
        assert!(!work.apply_to(&mut block, u64::MAX));
        assert_eq!(block.work, 7);
        block.version = 1;
        assert!(!work.apply_to(&mut block, 0));
        block.version = burst_ledger::CURRENT_BLOCK_VERSION;
        block.previous = BlockHash::new([9; 32]);
        assert!(!work.apply_to(&mut block, 0));
    }

    async fn reject_attempt(
        opened: bool,
        pending: bool,
        lose_open_response: bool,
    ) -> (Result<ProcessResult, WalletError>, Vec<StateBlock>) {
        account_control_attempt(opened, pending, lose_open_response, false).await
    }

    async fn account_control_attempt(
        opened: bool,
        pending: bool,
        lose_open_response: bool,
        handoff: bool,
    ) -> (Result<ProcessResult, WalletError>, Vec<StateBlock>) {
        let keys = burst_crypto::keypair_from_seed(&[57; 32]);
        let account = burst_crypto::derive_address(&keys.public);
        let public = keys.public.clone();
        let source = BlockHash::new([77; 32]);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let mut blocks = Vec::<StateBlock>::new();
            let mut head = if opened {
                BlockHash::new([88; 32])
            } else {
                BlockHash::ZERO
            };
            let started = Instant::now();
            loop {
                assert!(
                    started.elapsed() < Duration::from_secs(10),
                    "return RPC timed out"
                );
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("return RPC listener failed: {error}"),
                };
                let request = read_request(&mut stream);
                let action = request["action"].as_str().unwrap();
                let response = match action {
                    "account_pending" => {
                        assert_eq!(request["account"], account.as_str());
                        if pending {
                            json!({"pending":[{"hash":source.to_string(),"source":account.as_str(),"amount":"17"}]})
                        } else {
                            json!({"pending":[]})
                        }
                    }
                    "account_info" if head.is_zero() => {
                        json!({"error":format!("account not found: {account}")})
                    }
                    "account_info" => {
                        json!({"payment_epoch":BlockHash::new([1;32]).to_string(), "head":head.to_string(),"block_count":usize::from(opened)+blocks.len(),
                        "representative":account.as_str(),"trst_balance":if opened {"5"} else {"0"},
                        "release_sources":[],"has_more_releases":false})
                    }
                    "block_info" => {
                        json!({"account":account.as_str(), "active_signing_grant":blocks.last().map_or(BlockHash::ZERO, StateBlock::active_signing_grant).to_string(), "confirmed":true,"brn_balance":"0"})
                    }
                    "params_hash" => {
                        json!({"online_signing":"peer_matches", "payment_epoch":BlockHash::new([1;32]).to_string(), "block_version":burst_ledger::CURRENT_BLOCK_VERSION,
                        "params_hash":BlockHash::new([99;32]).to_string(),"min_work_difficulty":"0"})
                    }
                    "work_generate" => json!({"work":"0"}),
                    "process" => {
                        let block: StateBlock =
                            serde_json::from_str(request["block"].as_str().unwrap()).unwrap();
                        assert_eq!(block.account, account);
                        assert_eq!(block.representative, account);
                        assert_eq!(block.previous, head);
                        assert_eq!(block.brn_balance, 0);
                        assert_eq!(block.trst_balance, if opened { 5 } else { 0 });
                        assert_eq!(block.hash, block.compute_hash());
                        assert!(block.validate_format().is_ok());
                        assert!(burst_crypto::verify_signature(
                            block.hash.as_bytes(),
                            &block.signature,
                            &public
                        ));
                        if head.is_zero() {
                            assert_eq!(block.block_type, BlockType::Open);
                            assert!(block.link.is_zero());
                            assert_eq!(block.origin, TxHash::ZERO);
                            assert_eq!(block.transaction, TxHash::ZERO);
                        } else {
                            assert_eq!(
                                block.block_type,
                                if handoff {
                                    BlockType::ChangeSigner
                                } else {
                                    BlockType::RejectReceive
                                }
                            );
                            assert_eq!(block.link, source);
                        }
                        block
                            .validate_signing_grant(blocks.last())
                            .unwrap_or_else(|error| {
                                // An already-open fixture starts with an external head.
                                assert!(
                                    opened && blocks.is_empty() && block.signing_grant.is_zero(),
                                    "{error}"
                                );
                            });
                        head = block.hash;
                        let opening = block.block_type == BlockType::Open;
                        blocks.push(block);
                        if opening && lose_open_response {
                            return blocks;
                        }
                        json!({"accepted":true,"hash":head.to_string()})
                    }
                    _ => panic!("unexpected return RPC {action}"),
                };
                let body = response.to_string();
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
                if (!handoff && !pending)
                    || (action == "block_info"
                        && blocks.last().is_some_and(|block| {
                            block.block_type
                                == if handoff {
                                    BlockType::ChangeSigner
                                } else {
                                    BlockType::RejectReceive
                                }
                        }))
                {
                    return blocks;
                }
            }
        });
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pending.json");
        let client = NodeClient::new(url)
            .unwrap()
            .with_payment_journal(path.clone());
        let result = if handoff {
            client
                .handoff_signing_device(
                    &keys,
                    if opened {
                        BlockHash::new([88; 32])
                    } else {
                        BlockHash::ZERO
                    },
                    BlockHash::ZERO,
                    BlockHash::new([99; 32]),
                    source,
                )
                .await
        } else {
            client.reject_signed(&keys, &source.to_string()).await
        };
        if handoff {
            let saved = crate::offline::load_payment(&path).unwrap();
            assert_eq!(
                saved.block_type,
                if lose_open_response {
                    BlockType::Open
                } else {
                    BlockType::ChangeSigner
                }
            );
            assert!(crate::offline::payment_was_submitted(&path, &saved.hash).unwrap());
        }
        (result, server.join().unwrap())
    }

    #[tokio::test]
    async fn signing_device_enrollment_opens_once_and_keeps_uncertain_actions_reserved() {
        for (opened, lose_response) in [(false, false), (true, false), (false, true)] {
            let (result, blocks) = account_control_attempt(opened, true, lose_response, true).await;
            if lose_response {
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains(&blocks[0].hash.to_string()));
                assert_eq!(blocks.len(), 1);
            } else {
                assert_eq!(
                    result.unwrap().hash,
                    blocks.last().unwrap().hash.to_string()
                );
                assert_eq!(blocks.len(), if opened { 1 } else { 2 });
                assert_eq!(blocks.last().unwrap().block_type, BlockType::ChangeSigner);
            }
        }
    }

    #[tokio::test]
    async fn first_return_opens_without_accepting_any_trst() {
        let (result, blocks) = reject_attempt(false, true, false).await;
        assert_eq!(blocks.len(), 2);
        assert_eq!(result.unwrap().hash, blocks[1].hash.to_string());
    }

    #[tokio::test]
    async fn existing_account_return_preserves_balances_without_an_extra_open() {
        let (result, blocks) = reject_attempt(true, true, false).await;
        assert_eq!(blocks.len(), 1);
        assert_eq!(result.unwrap().hash, blocks[0].hash.to_string());
    }

    #[tokio::test]
    async fn lost_open_response_does_not_claim_the_payment_was_returned() {
        let (result, blocks) = reject_attempt(false, true, true).await;
        assert_eq!(blocks.len(), 1);
        let error = result.unwrap_err();
        assert!(matches!(&error, WalletError::Preparation(_)));
        assert!(error.to_string().contains(&blocks[0].hash.to_string()));
    }

    #[tokio::test]
    async fn no_pending_payment_cannot_even_open_an_account() {
        let (result, blocks) = reject_attempt(false, false, false).await;
        assert!(blocks.is_empty());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("no longer pending"));
    }

    #[test]
    fn old_or_missing_node_work_rules_fail_explicitly() {
        assert!(work_parameters(&json!({})).is_err());
        assert!(work_parameters(
            &json!({"online_signing":"peer_matches", "block_version":1,"min_work_difficulty":"0"})
        )
        .is_err());
        assert_eq!(work_parameters(&json!({"online_signing":"peer_matches", "payment_epoch":BlockHash::new([1;32]).to_string(), "block_version":burst_ledger::CURRENT_BLOCK_VERSION,"min_work_difficulty":"ff"})).unwrap(), 255);
    }

    #[tokio::test]
    async fn changed_epoch_or_unready_node_never_requests_work_or_submits_a_payment() {
        let keys = burst_crypto::keypair_from_seed(&[55; 32]);
        let address = burst_crypto::derive_address(&keys.public);
        for (epoch, readiness, expected) in [
            (Value::Null, json!("peer_matches"), "payment epoch"),
            (
                json!(BlockHash::ZERO.to_string()),
                json!("peer_matches"),
                "payment epoch",
            ),
            (
                json!(BlockHash::new([2; 32]).to_string()),
                json!("peer_matches"),
                "payment epoch",
            ),
            (json!("malformed"), json!("peer_matches"), "32-byte hash"),
            (
                json!(BlockHash::new([1; 32]).to_string()),
                json!("catching_up"),
                "synchronization is not ready",
            ),
            (
                json!(BlockHash::new([1; 32]).to_string()),
                json!("peer_differs"),
                "synchronization is not ready",
            ),
            (
                json!(BlockHash::new([1; 32]).to_string()),
                json!("unknown"),
                "synchronization is not ready",
            ),
            (
                json!(BlockHash::new([1; 32]).to_string()),
                Value::Null,
                "supported signing readiness",
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let replies = [
                (
                    "account_info",
                    json!({"payment_epoch":BlockHash::new([1; 32]).to_string(),
                    "head":BlockHash::new([8; 32]).to_string(), "block_count":1,
                    "representative":address.as_str(), "trst_balance":"0",
                    "release_sources":[], "has_more_releases":false}),
                ),
                (
                    "block_info",
                    json!({"active_signing_grant":BlockHash::ZERO.to_string(), "brn_balance":"0", "confirmed":true}),
                ),
                (
                    "params_hash",
                    json!({"payment_epoch":epoch,
                    "online_signing":readiness, "block_version":burst_ledger::CURRENT_BLOCK_VERSION,
                    "params_hash":BlockHash::new([9; 32]).to_string(), "min_work_difficulty":"0"}),
                ),
            ];
            let server = std::thread::spawn(move || {
                for (action, reply) in replies {
                    let (mut stream, _) = listener.accept().unwrap();
                    assert_eq!(read_request(&mut stream)["action"], action);
                    let body = reply.to_string();
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                }
                listener.set_nonblocking(true).unwrap();
                listener
            });
            let tx =
                Transaction::Burn(build_burn_tx(&address, &address, 1, Timestamp::now()).unwrap());
            let error = NodeClient::new(url)
                .unwrap()
                .submit_signed(&keys, &tx, TxHash::ZERO)
                .await
                .unwrap_err();
            let message = error.to_string();
            assert!(message.contains(expected), "{message}");
            let listener = server.join().unwrap();
            assert!(
                matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
            );
        }
    }

    async fn submission_error(process_body: &'static str) -> (String, String) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for body in [r#"{"work":"0"}"#, process_body] {
                let (mut stream, _) = listener.accept().unwrap();
                read_request(&mut stream);
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            }
        });
        let keys = burst_crypto::generate_keypair();
        let sender = burst_crypto::derive_address(&keys.public);
        let receiver = burst_crypto::derive_address(&burst_crypto::generate_keypair().public);
        let state = AccountState {
            signing_grant: burst_types::BlockHash::ZERO,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            head: BlockHash::ZERO,
            block_count: 0,
            representative: sender.clone(),
            brn_balance: 0,
            trst_balance: 0,
            release_sources: Vec::new(),
        };
        let tx = build_burn_tx(&sender, &receiver, 1, Timestamp::now()).unwrap();
        let block = build_state_block(
            &state,
            &Transaction::Burn(tx),
            TxHash::ZERO,
            BlockHash::ZERO,
        )
        .unwrap();
        let hash = block.hash.to_string();
        let error = NodeClient::new(url)
            .unwrap()
            .submit_with_work(block)
            .await
            .unwrap_err()
            .to_string();
        server.join().unwrap();
        (hash, error)
    }

    #[tokio::test]
    async fn malformed_submission_response_preserves_hash_for_reconciliation() {
        let (hash, error) = submission_error("not-json").await;
        assert!(error.contains(&format!("confirmation pending for {hash}")));
    }

    #[tokio::test]
    async fn explicit_rejection_is_not_reported_as_pending() {
        let (_, error) =
            submission_error(r#"{"hash":"","accepted":false,"detail":"invalid block"}"#).await;
        assert!(error.contains("invalid block"));
        assert!(!error.contains("confirmation pending"));
    }

    #[tokio::test]
    async fn unrelated_response_hash_cannot_confirm_this_submission() {
        let (hash, error) = submission_error(r#"{"hash":"unrelated","accepted":true}"#).await;
        assert!(error.contains(&format!("confirmation pending for {hash}")));
    }

    #[tokio::test]
    async fn pending_source_lookup_requires_an_exact_unpaginated_result() {
        let account =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[57; 32]).public);
        let hash = TxHash::new([58; 32]).to_string();
        let entry = json!({"hash":hash,"source":account.as_str(),"amount":"17"});
        let other =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[60; 32]).public);
        let cases = [
            (json!({"pending":[entry.clone()]}), Some(true)),
            (
                json!({"pending":[{"hash":hash,"source":account.as_str(),"amount":"17","return_credit":true}]}),
                Some(true),
            ),
            (
                json!({"pending":[{"hash":hash,"source":other.as_str(),"amount":"17","return_credit":true}]}),
                None,
            ),
            (json!({"pending":[]}), Some(false)),
            (json!({"pending":[entry.clone()],"cursor":"next"}), None),
            (json!({"pending":[entry.clone(),entry.clone()]}), None),
            (
                json!({"pending":[{"hash":TxHash::new([59;32]).to_string(),"source":account.as_str(),"amount":"17"}]}),
                None,
            ),
        ];
        for (response, expected) in cases {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let expected_account = account.to_string();
            let expected_hash = hash.clone();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                assert_eq!(request["action"], "account_pending");
                assert_eq!(request["account"], expected_account);
                assert_eq!(request["hash"], expected_hash);
                assert_eq!(request["count"], 1);
                let body = response.to_string();
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
            });
            let result = NodeClient::new(url)
                .unwrap()
                .account_pending_source(account.as_str(), &hash)
                .await;
            server.join().unwrap();
            match expected {
                Some(present) => assert_eq!(result.unwrap().is_some(), present),
                None => assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains("exact pending source")),
            }
        }
    }
}
