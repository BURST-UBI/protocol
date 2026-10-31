//! Core wallet struct with optional node RPC connectivity.

use burst_types::{KeyPair, Timestamp, WalletAddress, WalletState};

use crate::error::WalletError;

use serde::Deserialize;
use std::time::Duration;

/// HTTP client for BURST's action-based JSON RPC.
///
/// Uses native HTTP or browser Fetch with the same typed RPC and signing API.
#[derive(Clone)]
pub struct NodeClient {
    #[cfg(not(target_arch = "wasm32"))]
    http: reqwest::Client,
    node_url: String,
    pub(crate) expected_signing_grant: burst_types::BlockHash,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) payment_journal: Option<std::path::PathBuf>,
}

impl NodeClient {
    /// Configure a node URL without contacting it. Native requests use a
    /// 10-second connection timeout and a 30-second total timeout. The client
    /// starts without an action journal and with a zero expected signing grant.
    pub fn new(node_url: impl Into<String>) -> Result<Self, WalletError> {
        #[cfg(not(target_arch = "wasm32"))]
        let http = Self::native_http(None)?;
        Ok(Self {
            #[cfg(not(target_arch = "wasm32"))]
            http,
            node_url: node_url.into(),
            expected_signing_grant: burst_types::BlockHash::ZERO,
            #[cfg(not(target_arch = "wasm32"))]
            payment_journal: None,
        })
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn native_http(
        local_address: Option<std::net::IpAddr>,
    ) -> Result<reqwest::Client, WalletError> {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .local_address(local_address)
            .build()
            .map_err(|e| WalletError::Node(format!("failed to create HTTP client: {e}")))
    }

    /// Bind native connections to a local interface address. Replaces the
    /// connection pool while retaining the URL, signing grant and action journal.
    /// Connection and request deadlines remain the same as `new`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn with_local_address(mut self, address: std::net::IpAddr) -> Result<Self, WalletError> {
        self.http = Self::native_http(Some(address))?;
        Ok(self)
    }

    /// Retain each signed action and proposal content before transmission. The caller must hold its
    /// account/profile signing lease and resolve existing drafts before signing.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn with_payment_journal(mut self, path: std::path::PathBuf) -> Self {
        self.payment_journal = Some(path);
        self
    }

    /// The configured node URL.
    pub fn node_url(&self) -> &str {
        &self.node_url
    }

    /// Set the expected grant for signing-state checks. Supply this device's
    /// saved binding; this setter does not load or validate a binding file.
    pub fn with_signing_grant(mut self, grant: burst_types::BlockHash) -> Self {
        self.expected_signing_grant = grant;
        self
    }

    /// POST a JSON object with `action`, replacing any action supplied in `params`.
    /// Returns the `result` field when present, otherwise the whole response.
    /// A string-valued `error` becomes `WalletError::Node`.
    pub async fn rpc_call(
        &self,
        action: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, WalletError> {
        let mut body = params;
        body.as_object_mut()
            .ok_or_else(|| WalletError::Node("params must be a JSON object".into()))?
            .insert("action".to_string(), serde_json::json!(action));

        #[cfg(not(target_arch = "wasm32"))]
        let json = {
            let response = self
                .http
                .post(&self.node_url)
                .json(&body)
                .send()
                .await
                .map_err(|e| WalletError::Node(format!("request failed: {e}")))?;

            if !response.status().is_success() {
                return Err(WalletError::Node(format!(
                    "node returned HTTP {}",
                    response.status()
                )));
            }

            let json = burst_http_client::read_json(response)
                .await
                .map_err(WalletError::Node)?;
            json
        };
        #[cfg(target_arch = "wasm32")]
        let json: serde_json::Value = {
            let payload = serde_json::to_string(&body)
                .map_err(|error| WalletError::Node(error.to_string()))?;
            let bytes = burst_web_runtime::fetch_bytes(
                &self.node_url,
                Some(&payload),
                Duration::from_secs(30),
                16 * 1024 * 1024,
            )
            .await
            .map_err(|error| WalletError::Node(error.to_string()))?;
            serde_json::from_slice(&bytes)
                .map_err(|error| WalletError::Node(format!("invalid JSON response: {error}")))?
        };

        if let Some(err) = json.get("error").and_then(|e| e.as_str()) {
            return Err(WalletError::Node(format!("node error: {err}")));
        }

        Ok(json.get("result").cloned().unwrap_or(json))
    }

    /// Fetch account balance (BRN + TRST).
    pub async fn account_balance(
        &self,
        account: &str,
    ) -> Result<AccountBalanceResponse, WalletError> {
        let result = self
            .rpc_call(
                "account_balance",
                serde_json::to_value(burst_rpc_messages::accounts::AccountBalanceRequest {
                    account: account.into(),
                })
                .map_err(|e| WalletError::Node(e.to_string()))?,
            )
            .await?;

        let resp: AccountBalanceResponse = serde_json::from_value(result)
            .map_err(|e| WalletError::Node(format!("invalid balance response: {e}")))?;
        Ok(resp)
    }

    /// Fetch the typed account-info fields used by this wallet. Additional
    /// response fields are ignored; parsing is not a proof of ledger state.
    pub async fn account_info(&self, account: &str) -> Result<AccountInfoResponse, WalletError> {
        let result = self
            .rpc_call("account_info", serde_json::json!({ "account": account }))
            .await?;

        let resp: AccountInfoResponse = serde_json::from_value(result)
            .map_err(|e| WalletError::Node(format!("invalid account_info response: {e}")))?;
        Ok(resp)
    }

    /// Submit block JSON and parse the node's processing response.
    /// Does not sign, journal, validate the returned hash or wait for confirmation.
    pub async fn process(&self, block_json: &str) -> Result<ProcessResponse, WalletError> {
        let result = self
            .rpc_call("process", serde_json::json!({ "block": block_json }))
            .await?;

        serde_json::from_value(result)
            .map_err(|e| WalletError::Node(format!("invalid process response: {e}")))
    }

    /// Request a pending-receipt page. Inspect the returned cursor for more
    /// entries; this method does not paginate automatically.
    pub async fn account_pending(
        &self,
        account: &str,
        count: u64,
    ) -> Result<AccountPendingResponse, WalletError> {
        let result = self
            .rpc_call(
                "account_pending",
                serde_json::json!({ "account": account, "count": count }),
            )
            .await?;

        serde_json::from_value(result)
            .map_err(|e| WalletError::Node(format!("invalid pending response: {e}")))
    }

    /// Inspect a single receivable without relying on a truncated list page.
    pub async fn account_pending_source(
        &self,
        account: &str,
        hash: &str,
    ) -> Result<Option<PendingBlock>, WalletError> {
        let hash = crate::signed_client::parse_hash(hash)?.to_string();
        let value = self
            .rpc_call(
                "account_pending",
                serde_json::json!({"account":account,"hash":hash,"count":1}),
            )
            .await?;
        let result: AccountPendingResponse = serde_json::from_value(value)
            .map_err(|error| WalletError::Node(format!("invalid pending source: {error}")))?;
        if result.cursor.is_some()
            || result.blocks.len() > 1
            || result
                .blocks
                .iter()
                .any(|entry| entry.hash != hash || (entry.return_credit && entry.source != account))
        {
            return Err(WalletError::Node(
                "node did not return an exact pending source; update the node before receiving"
                    .into(),
            ));
        }
        Ok(result.blocks.into_iter().next())
    }

    /// Request work for a predecessor or account-opening root. This method only
    /// parses the reply; the signing path must validate the root and work value.
    pub async fn work_generate(&self, hash: &str) -> Result<WorkGenerateResponse, WalletError> {
        let result = self
            .rpc_call("work_generate", serde_json::json!({ "hash": hash }))
            .await?;

        serde_json::from_value(result)
            .map_err(|e| WalletError::Node(format!("invalid work_generate response: {e}")))
    }
}

/// Balance response from the node.
pub use burst_rpc_messages::accounts::AccountBalanceResponse;

/// Account info response from the node.
#[derive(Debug, Clone, Deserialize)]
pub struct AccountInfoResponse {
    pub address: String,
    pub head: String,
    pub brn_balance: String,
    pub trst_balance: String,
    #[serde(default)]
    pub trst_expired: String,
    #[serde(default)]
    pub trst_revoked: String,
    #[serde(default)]
    pub total_brn_burned: String,
    #[serde(default)]
    pub total_brn_staked: String,
    pub verification_state: String,
    #[serde(default)]
    pub verified_at: Option<u64>,
    pub block_count: u64,
    #[serde(default)]
    pub confirmation_height: u64,
    pub representative: String,
}

/// Node-reported processing result, not a confirmation certificate.
#[derive(Debug, Clone, Deserialize)]
pub struct ProcessResponse {
    pub hash: String,
    pub accepted: bool,
    #[serde(default)]
    pub detail: Option<String>,
}

/// Response from the `account_pending` RPC.
#[derive(Debug, Clone, Deserialize)]
pub struct AccountPendingResponse {
    #[serde(default, rename = "pending", alias = "blocks")]
    pub blocks: Vec<PendingBlock>,
    #[serde(default)]
    pub cursor: Option<String>,
}

/// A single pending (receivable) block entry.
#[derive(Debug, Clone, Deserialize)]
pub struct PendingBlock {
    pub hash: String,
    pub source: String,
    pub amount: String,
    #[serde(default)]
    pub block_type: String,
    #[serde(default)]
    pub return_credit: bool,
}

/// Response from the `work_generate` RPC.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkGenerateResponse {
    pub work: String,
    pub difficulty: String,
    #[serde(default)]
    pub hash: String,
}

/// Local keys and cached account fields. Offline constructors initialize the
/// cache as unverified with zero deductions; this is not a network-state claim.
pub struct Wallet {
    /// Primary key pair (identity and transaction signing).
    pub primary_keys: KeyPair,
    /// Wallet address (derived from primary public key).
    pub address: WalletAddress,
    /// Cached verification state, populated by refresh or the caller.
    pub state: WalletState,
    /// Cached verification timestamp; it can remain set after deactivation.
    pub verified_at: Option<Timestamp>,
    /// Cumulative BRN burned (subtracted from computed BRN counter).
    pub total_brn_burned: u128,
    /// Cached stake deduction used by local BRN estimates.
    pub total_brn_staked: u128,
    /// Optional node client for querying live balance data.
    node_client: Option<NodeClient>,
}

impl Wallet {
    /// Create a wallet recoverable through the existing BURST mnemonic
    /// derivation. Return the phrase once so the caller can require backup.
    pub fn create_with_mnemonic() -> Result<(Self, String), WalletError> {
        let phrase =
            burst_crypto::generate_mnemonic().map_err(|e| WalletError::Key(e.to_string()))?;
        let wallet = Self::from_mnemonic(&phrase)?;
        Ok((wallet, phrase))
    }

    /// Create a new wallet with fresh key pair.
    pub fn create() -> Result<Self, WalletError> {
        use burst_crypto::{derive_address, generate_keypair};

        let primary_keys = generate_keypair();
        let address = derive_address(&primary_keys.public);

        Ok(Self {
            primary_keys,
            address,
            state: WalletState::Unverified,
            verified_at: None,
            total_brn_burned: 0,
            total_brn_staked: 0,
            node_client: None,
        })
    }

    /// Restore directly from an Ed25519 signing seed, without account derivation.
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        use burst_crypto::{derive_address, keypair_from_seed};

        let primary_keys = keypair_from_seed(seed);
        let address = derive_address(&primary_keys.public);

        Self {
            primary_keys,
            address,
            state: WalletState::Unverified,
            verified_at: None,
            total_brn_burned: 0,
            total_brn_staked: 0,
            node_client: None,
        }
    }

    /// Restore account zero from a 24-word wallet-seed phrase; state remains unsynced.
    pub fn from_mnemonic(phrase: &str) -> Result<Self, WalletError> {
        use burst_crypto::{derive_address, keypair_from_mnemonic};

        let primary_keys = keypair_from_mnemonic(phrase)
            .map_err(|e| WalletError::Key(format!("mnemonic error: {e}")))?;
        let address = derive_address(&primary_keys.public);

        Ok(Self {
            primary_keys,
            address,
            state: WalletState::Unverified,
            verified_at: None,
            total_brn_burned: 0,
            total_brn_staked: 0,
            node_client: None,
        })
    }

    /// Restore a wallet from an existing private key (offline — no node query).
    pub fn from_private_key(private_key_bytes: &[u8]) -> Result<Self, WalletError> {
        use burst_crypto::{derive_address, keypair_from_private};
        use burst_types::PrivateKey;

        if private_key_bytes.len() != 32 {
            return Err(WalletError::Key(format!(
                "private key must be 32 bytes, got {}",
                private_key_bytes.len()
            )));
        }

        let mut key_bytes = [0u8; 32];
        key_bytes.copy_from_slice(private_key_bytes);
        let private_key = PrivateKey(key_bytes);

        let primary_keys = keypair_from_private(private_key);
        let address = derive_address(&primary_keys.public);

        Ok(Self {
            primary_keys,
            address,
            state: WalletState::Unverified,
            verified_at: None,
            total_brn_burned: 0,
            total_brn_staked: 0,
            node_client: None,
        })
    }

    /// Restore a wallet from a private key and immediately sync state from a node.
    pub async fn from_private_key_with_node(
        private_key_bytes: &[u8],
        node_url: &str,
    ) -> Result<Self, WalletError> {
        let mut wallet = Self::from_private_key(private_key_bytes)?;
        wallet.connect_to_node(node_url)?;
        wallet.refresh_from_node().await?;
        Ok(wallet)
    }

    /// Replace the configured node client without querying or refreshing state.
    /// The replacement starts without a journal or saved signing grant.
    pub fn connect_to_node(&mut self, node_url: &str) -> Result<(), WalletError> {
        self.node_client = Some(NodeClient::new(node_url)?);
        Ok(())
    }

    /// Return the configured client, if any; configuration does not prove connectivity.
    pub fn node_client(&self) -> Option<&NodeClient> {
        self.node_client.as_ref()
    }

    /// Fetch and apply the current account state from the connected node.
    ///
    /// Validates the address, state name and deduction amounts before updating
    /// `state`, `verified_at`, `total_brn_burned` and `total_brn_staked` together.
    pub async fn refresh_from_node(&mut self) -> Result<(), WalletError> {
        let client = self
            .node_client
            .as_ref()
            .ok_or(WalletError::NoNodeConnection)?;

        let info = client.account_info(self.address.as_str()).await?;

        self.apply_account_info(info)
    }

    fn apply_account_info(&mut self, info: AccountInfoResponse) -> Result<(), WalletError> {
        if info.address != self.address.as_str() {
            return Err(WalletError::Node("account_info address mismatch".into()));
        }
        let state = match info.verification_state.as_str() {
            "unverified" => WalletState::Unverified,
            "verified" => WalletState::Verified,
            "endorsed" => WalletState::Endorsed,
            "voting" => WalletState::Voting,
            "challenged" => WalletState::Challenged,
            "revoked" => WalletState::Revoked,
            "deactivated" => WalletState::Deactivated,
            _ => return Err(WalletError::Node("unknown verification state".into())),
        };
        let burned = info
            .total_brn_burned
            .parse::<u128>()
            .map_err(|_| WalletError::Node("invalid total_brn_burned".into()))?;
        let staked = info
            .total_brn_staked
            .parse::<u128>()
            .map_err(|_| WalletError::Node("invalid total_brn_staked".into()))?;
        self.state = state;
        self.verified_at = info.verified_at.map(Timestamp::new);
        self.total_brn_burned = burned;
        self.total_brn_staked = staked;

        Ok(())
    }

    /// Estimate BRN from cached fields at one constant rate.
    ///
    /// This is a simplified offline estimate assuming a constant rate.
    /// Assumes uninterrupted accrual; use `balance::compute_balance_from_state`
    /// to include pauses and stopped accrual. That helper returns available BRN,
    /// before deducting returned stakes awaiting acknowledgment.
    /// Returns zero when `verified_at` is absent. Does not check `state`; a cached
    /// timestamp on a deactivated account still produces an estimate.
    pub fn estimate_brn_balance(
        &self,
        now: Timestamp,
        brn_rate: u128,
    ) -> Result<u128, WalletError> {
        match self.verified_at {
            Some(verified_at) => crate::balance::estimate_balance(
                verified_at,
                now,
                brn_rate,
                self.total_brn_burned,
                self.total_brn_staked,
            ),
            None => Ok(0),
        }
    }

    /// Estimate BRN from cached fields and the supplied rate history.
    ///
    /// `BRN = Σ(rate_i × duration_i) − total_burned − total_staked`
    ///
    /// Assumes uninterrupted accrual; excludes pause/deactivation history.
    /// Returns zero without `verified_at`, or an error on overflow. Does not
    /// inspect the cached verification state.
    pub fn estimate_brn_balance_with_history(
        &self,
        now: Timestamp,
        rate_history: &burst_brn::state::RateHistory,
    ) -> Result<u128, WalletError> {
        match self.verified_at {
            Some(verified_at) => crate::balance::estimate_balance_with_history(
                verified_at,
                now,
                rate_history,
                self.total_brn_burned,
                self.total_brn_staked,
            ),
            None => Ok(0),
        }
    }

    /// Get transferable TRST balance by querying the connected node.
    pub async fn trst_balance(&self) -> Result<u128, WalletError> {
        let client = self
            .node_client
            .as_ref()
            .ok_or(WalletError::NoNodeConnection)?;

        let balance = client.account_balance(self.address.as_str()).await?;
        balance
            .trst_balance
            .parse::<u128>()
            .map_err(|e| WalletError::Node(format!("invalid trst_balance value: {e}")))
    }

    /// Query the node-reported expired TRST amount. Expiry is not evidence of
    /// useful work, reputation or delivery of goods.
    pub async fn trst_expired(&self) -> Result<u128, WalletError> {
        let client = self
            .node_client
            .as_ref()
            .ok_or(WalletError::NoNodeConnection)?;

        let info = client.account_info(self.address.as_str()).await?;
        info.trst_expired
            .parse::<u128>()
            .map_err(|e| WalletError::Node(format!("invalid trst_expired value: {e}")))
    }

    /// Get revoked TRST by querying the connected node.
    ///
    /// Parses `trst_revoked` from `account_info`; an empty field returns zero.
    pub async fn trst_revoked(&self) -> Result<u128, WalletError> {
        let client = self
            .node_client
            .as_ref()
            .ok_or(WalletError::NoNodeConnection)?;

        let info = client.account_info(self.address.as_str()).await?;
        if info.trst_revoked.is_empty() {
            return Ok(0);
        }
        info.trst_revoked
            .parse::<u128>()
            .map_err(|e| WalletError::Node(format!("invalid trst_revoked value: {e}")))
    }

    /// Sign a message with the primary private key.
    pub fn sign(&self, message: &[u8]) -> burst_types::Signature {
        burst_crypto::sign_message(message, &self.primary_keys.private)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mnemonic_creation_recovers_the_same_account() {
        let (created, phrase) = Wallet::create_with_mnemonic().unwrap();
        let recovered = Wallet::from_mnemonic(&phrase).unwrap();
        assert_eq!(phrase.split_whitespace().count(), 24);
        assert_eq!(created.address, recovered.address);
        assert_eq!(
            created.primary_keys.private.0,
            recovered.primary_keys.private.0
        );
    }

    #[test]
    fn mnemonic_account_zero_matches_known_signing_seed() {
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art";
        let recovered = Wallet::from_mnemonic(phrase).unwrap();
        let signing_seed = [
            159, 14, 68, 76, 105, 247, 122, 73, 189, 11, 232, 157, 185, 44, 56, 254, 113, 62, 9,
            99, 22, 92, 202, 18, 250, 245, 113, 45, 118, 87, 18, 15,
        ];
        let direct = Wallet::from_seed(&signing_seed);
        assert_eq!(recovered.address, direct.address);
        assert_eq!(recovered.primary_keys.private.0, signing_seed);
        assert_ne!(recovered.address, Wallet::from_seed(&[0; 32]).address);
        assert!(Wallet::from_mnemonic("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about").is_err());
    }

    use burst_crypto::verify_signature;

    #[test]
    fn account_refresh_validates_before_mutating_and_preserves_deactivation() {
        let mut wallet = Wallet::create().unwrap();
        let info: AccountInfoResponse = serde_json::from_value(serde_json::json!({
            "address": wallet.address.as_str(), "head": "", "brn_balance": "0",
            "trst_balance": "0", "verification_state": "deactivated",
            "total_brn_burned": "12", "total_brn_staked": "34",
            "verified_at": 10, "block_count": 0, "representative": ""
        }))
        .unwrap();
        wallet.apply_account_info(info.clone()).unwrap();
        assert_eq!(wallet.state, WalletState::Deactivated);
        for field in [
            "address",
            "verification_state",
            "total_brn_burned",
            "total_brn_staked",
        ] {
            let mut invalid = info.clone();
            match field {
                "address" => invalid.address = "different".into(),
                "verification_state" => invalid.verification_state = "unknown".into(),
                "total_brn_burned" => invalid.total_brn_burned = "not an amount".into(),
                _ => invalid.total_brn_staked = "340282366920938463463374607431768211456".into(),
            }
            invalid.verified_at = Some(99);
            assert!(wallet.apply_account_info(invalid).is_err());
            assert_eq!(wallet.state, WalletState::Deactivated);
            assert_eq!(wallet.verified_at, Some(Timestamp::new(10)));
            assert_eq!((wallet.total_brn_burned, wallet.total_brn_staked), (12, 34));
        }
    }

    #[test]
    fn test_wallet_create() {
        let wallet = Wallet::create().unwrap();

        assert_eq!(wallet.state, WalletState::Unverified);
        assert_eq!(wallet.verified_at, None);

        assert!(wallet.address.as_str().starts_with("brst_"));
        assert!(wallet.address.has_prefix_and_payload());
    }

    #[test]
    fn test_wallet_from_private_key() {
        let wallet1 = Wallet::create().unwrap();
        let private_key_bytes = wallet1.primary_keys.private.0;

        let wallet2 = Wallet::from_private_key(&private_key_bytes).unwrap();

        assert_eq!(wallet1.address, wallet2.address);
        assert_eq!(
            wallet1.primary_keys.public.as_bytes(),
            wallet2.primary_keys.public.as_bytes()
        );

        assert_eq!(wallet2.state, WalletState::Unverified);
    }

    #[test]
    fn test_wallet_from_private_key_invalid_length() {
        let short_key = vec![0u8; 16];
        assert!(Wallet::from_private_key(&short_key).is_err());

        let long_key = vec![0u8; 64];
        assert!(Wallet::from_private_key(&long_key).is_err());
    }

    #[test]
    fn test_wallet_sign() {
        let wallet = Wallet::create().unwrap();
        let message = b"test message for signing";

        let signature = wallet.sign(message);

        assert!(verify_signature(
            message,
            &signature,
            &wallet.primary_keys.public
        ));

        assert!(!verify_signature(
            b"wrong message",
            &signature,
            &wallet.primary_keys.public
        ));
    }

    #[test]
    fn test_wallet_create_unique() {
        let wallet1 = Wallet::create().unwrap();
        let wallet2 = Wallet::create().unwrap();

        assert_ne!(wallet1.address, wallet2.address);
        assert_ne!(
            wallet1.primary_keys.public.as_bytes(),
            wallet2.primary_keys.public.as_bytes()
        );
    }

    #[test]
    fn test_wallet_brn_balance_unverified() {
        let wallet = Wallet::create().unwrap();
        assert_eq!(
            wallet.estimate_brn_balance(Timestamp::now(), 10).unwrap(),
            0
        );
    }

    #[test]
    fn test_wallet_brn_balance_verified() {
        let mut wallet = Wallet::create().unwrap();
        wallet.state = WalletState::Verified;
        wallet.verified_at = Some(Timestamp::new(1000));

        // 500 seconds at 10 raw units per second = 5000 raw units.
        let balance = wallet
            .estimate_brn_balance(Timestamp::new(1500), 10)
            .unwrap();
        assert_eq!(balance, 5000);
    }

    #[test]
    fn test_wallet_brn_balance_with_deductions() {
        let mut wallet = Wallet::create().unwrap();
        wallet.state = WalletState::Verified;
        wallet.verified_at = Some(Timestamp::new(1000));
        wallet.total_brn_burned = 2000;
        wallet.total_brn_staked = 1000;

        // 500s × rate 10 = 5000 - 2000 burned - 1000 staked = 2000
        let balance = wallet
            .estimate_brn_balance(Timestamp::new(1500), 10)
            .unwrap();
        assert_eq!(balance, 2000);
    }

    #[test]
    fn test_wallet_from_seed_deterministic() {
        let seed = [42u8; 32];
        let w1 = Wallet::from_seed(&seed);
        let w2 = Wallet::from_seed(&seed);
        assert_eq!(w1.address, w2.address);
    }

    #[test]
    fn test_node_client_creation() {
        let mut wallet = Wallet::create().unwrap();
        assert!(wallet.node_client().is_none());

        wallet.connect_to_node("http://127.0.0.1:7076").unwrap();
        assert!(wallet.node_client().is_some());
        assert_eq!(
            wallet.node_client().unwrap().node_url(),
            "http://127.0.0.1:7076"
        );
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod rpc_contract_tests {
    use super::*;
    use burst_rpc_messages::accounts::{AccountBalanceRequest, AccountBalanceResponse};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn balance_client_consumes_server_contract_without_numeric_loss() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let body_start = loop {
                let mut bytes = [0; 1024];
                let count = socket.read(&mut bytes).await.unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&bytes[..count]);
                assert!(request.len() < 8192);
                if let Some(index) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..body_start]);
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap();
            assert!(length < 4096);
            while request.len() < body_start + length {
                let mut bytes = [0; 1024];
                let count = socket.read(&mut bytes).await.unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&bytes[..count]);
            }
            let value: serde_json::Value =
                serde_json::from_slice(&request[body_start..body_start + length]).unwrap();
            assert_eq!(value["action"], "account_balance");
            let decoded: AccountBalanceRequest = serde_json::from_value(value).unwrap();
            assert_eq!(decoded.account, "contract-test-account");
            let response = AccountBalanceResponse {
                brn_balance: u128::MAX.to_string(),
                trst_balance: "9007199254740993".into(),
            };
            let body = serde_json::to_string(&serde_json::json!({"result": response})).unwrap();
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        let client = NodeClient::new(format!("http://{address}")).unwrap();
        let balance = client
            .account_balance("contract-test-account")
            .await
            .unwrap();
        assert_eq!(balance.brn_balance, u128::MAX.to_string());
        assert_eq!(balance.trst_balance, "9007199254740993");
        server.await.unwrap();
    }
}
