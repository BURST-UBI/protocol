//! Core wallet struct with optional node RPC connectivity.

use burst_types::{KeyPair, Timestamp, WalletAddress, WalletState};

use crate::error::WalletError;

#[cfg(not(target_arch = "wasm32"))]
use serde::Deserialize;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

// ── NodeClient ──────────────────────────────────────────────────────────

/// HTTP client for communicating with a BURST node via JSON-RPC.
///
/// Wraps `reqwest::Client` with the node's base URL and provides typed
/// methods for each RPC action the wallet needs.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
pub struct NodeClient {
    http: reqwest::Client,
    node_url: String,
}

#[cfg(not(target_arch = "wasm32"))]
impl NodeClient {
    /// Create a new NodeClient targeting the given base URL (e.g. `http://127.0.0.1:7076`).
    pub fn new(node_url: impl Into<String>) -> Result<Self, WalletError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| WalletError::Node(format!("failed to create HTTP client: {e}")))?;
        Ok(Self {
            http,
            node_url: node_url.into(),
        })
    }

    /// The configured node URL.
    pub fn node_url(&self) -> &str {
        &self.node_url
    }

    /// Send a JSON-RPC request and return the `result` field.
    async fn rpc_call(
        &self,
        action: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, WalletError> {
        let mut body = params;
        body.as_object_mut()
            .ok_or_else(|| WalletError::Node("params must be a JSON object".into()))?
            .insert("action".to_string(), serde_json::json!(action));

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

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| WalletError::Node(format!("invalid JSON response: {e}")))?;

        if let Some(err) = json.get("error").and_then(|e| e.as_str()) {
            return Err(WalletError::Node(format!("node error: {err}")));
        }

        json.get("result")
            .cloned()
            .unwrap_or(json)
            .pipe_ok()
    }

    /// Fetch account balance (BRN + TRST).
    pub async fn account_balance(
        &self,
        account: &str,
    ) -> Result<AccountBalanceResult, WalletError> {
        let result = self
            .rpc_call(
                "account_balance",
                serde_json::json!({ "account": account }),
            )
            .await?;

        let resp: AccountBalanceResult = serde_json::from_value(result)
            .map_err(|e| WalletError::Node(format!("invalid balance response: {e}")))?;
        Ok(resp)
    }

    /// Fetch full account info (state, block count, representative, etc.).
    pub async fn account_info(&self, account: &str) -> Result<AccountInfoResult, WalletError> {
        let result = self
            .rpc_call("account_info", serde_json::json!({ "account": account }))
            .await?;

        let resp: AccountInfoResult = serde_json::from_value(result)
            .map_err(|e| WalletError::Node(format!("invalid account_info response: {e}")))?;
        Ok(resp)
    }

    /// Submit a signed block to the node for processing.
    pub async fn process(&self, block_json: &str) -> Result<ProcessResult, WalletError> {
        let result = self
            .rpc_call("process", serde_json::json!({ "block": block_json }))
            .await?;

        serde_json::from_value(result)
            .map_err(|e| WalletError::Node(format!("invalid process response: {e}")))
    }

    /// Fetch pending (receivable) blocks for an account.
    pub async fn account_pending(
        &self,
        account: &str,
        count: u64,
    ) -> Result<AccountPendingResult, WalletError> {
        let result = self
            .rpc_call(
                "account_pending",
                serde_json::json!({ "account": account, "count": count }),
            )
            .await?;

        serde_json::from_value(result)
            .map_err(|e| WalletError::Node(format!("invalid pending response: {e}")))
    }

    /// Request proof-of-work for a block hash.
    pub async fn work_generate(&self, hash: &str) -> Result<WorkGenerateResult, WalletError> {
        let result = self
            .rpc_call("work_generate", serde_json::json!({ "hash": hash }))
            .await?;

        serde_json::from_value(result)
            .map_err(|e| WalletError::Node(format!("invalid work_generate response: {e}")))
    }
}

/// Balance response from the node.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Deserialize)]
pub struct AccountBalanceResult {
    pub brn_balance: String,
    pub trst_balance: String,
}

/// Account info response from the node.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Deserialize)]
pub struct AccountInfoResult {
    pub address: String,
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

/// Response from the `process` RPC (block submission).
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Deserialize)]
pub struct ProcessResult {
    pub hash: String,
    pub accepted: bool,
    #[serde(default)]
    pub detail: Option<String>,
}

/// Response from the `account_pending` RPC.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Deserialize)]
pub struct AccountPendingResult {
    #[serde(default)]
    pub blocks: Vec<PendingBlock>,
}

/// A single pending (receivable) block entry.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Deserialize)]
pub struct PendingBlock {
    pub source: String,
    pub amount: String,
    #[serde(default)]
    pub block_type: String,
}

/// Response from the `work_generate` RPC.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Deserialize)]
pub struct WorkGenerateResult {
    pub work: String,
    pub difficulty: String,
    #[serde(default)]
    pub hash: String,
}

/// Helper trait to wrap a value in `Ok`.
#[cfg(not(target_arch = "wasm32"))]
trait PipeOk: Sized {
    fn pipe_ok(self) -> Result<Self, WalletError> {
        Ok(self)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl PipeOk for serde_json::Value {}

/// WASM-compatible NodeClient stub.
///
/// On WASM targets, NodeClient uses fetch API instead of reqwest.
/// This is a minimal stub that allows the wallet_core crate to compile
/// to WASM. A full implementation would use `web-sys` or `gloo-net`.
#[cfg(target_arch = "wasm32")]
#[derive(Clone)]
pub struct NodeClient {
    node_url: String,
}

#[cfg(target_arch = "wasm32")]
impl NodeClient {
    pub fn new(node_url: impl Into<String>) -> Result<Self, WalletError> {
        Ok(Self {
            node_url: node_url.into(),
        })
    }

    pub fn node_url(&self) -> &str {
        &self.node_url
    }
}

// ── Wallet ──────────────────────────────────────────────────────────────

/// A BURST wallet.
pub struct Wallet {
    /// Primary key pair (identity and transaction signing).
    pub primary_keys: KeyPair,
    /// Wallet address (derived from primary public key).
    pub address: WalletAddress,
    /// Current verification state.
    pub state: WalletState,
    /// When this wallet was verified (None if not verified).
    pub verified_at: Option<Timestamp>,
    /// Cumulative BRN burned (subtracted from computed BRN counter).
    pub total_brn_burned: u128,
    /// Cumulative BRN staked (subtracted from computed BRN counter).
    pub total_brn_staked: u128,
    /// Optional node client for querying live balance data.
    node_client: Option<NodeClient>,
}

impl Wallet {
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

    /// Restore a wallet from a 32-byte seed (deterministic).
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

    /// Restore a wallet from a BIP39 mnemonic phrase (24 words).
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
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn from_private_key_with_node(
        private_key_bytes: &[u8],
        node_url: &str,
    ) -> Result<Self, WalletError> {
        let mut wallet = Self::from_private_key(private_key_bytes)?;
        wallet.connect_to_node(node_url)?;
        wallet.refresh_from_node().await?;
        Ok(wallet)
    }

    /// Configure the wallet to communicate with a BURST node.
    pub fn connect_to_node(&mut self, node_url: &str) -> Result<(), WalletError> {
        self.node_client = Some(NodeClient::new(node_url)?);
        Ok(())
    }

    /// Return a reference to the node client, if connected.
    pub fn node_client(&self) -> Option<&NodeClient> {
        self.node_client.as_ref()
    }

    /// Fetch and apply the current account state from the connected node.
    ///
    /// Updates `state` and `verified_at` from the node's `account_info` response.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn refresh_from_node(&mut self) -> Result<(), WalletError> {
        let client = self
            .node_client
            .as_ref()
            .ok_or(WalletError::NoNodeConnection)?;

        let info = client.account_info(self.address.as_str()).await?;

        self.state = match info.verification_state.as_str() {
            "verified" => WalletState::Verified,
            "endorsed" => WalletState::Endorsed,
            "voting" => WalletState::Voting,
            "challenged" => WalletState::Challenged,
            "revoked" => WalletState::Revoked,
            _ => WalletState::Unverified,
        };

        self.verified_at = info
            .verified_at
            .map(Timestamp::new);

        self.total_brn_burned = info
            .total_brn_burned
            .parse::<u128>()
            .unwrap_or(0);

        self.total_brn_staked = info
            .total_brn_staked
            .parse::<u128>()
            .unwrap_or(0);

        Ok(())
    }

    /// Get the current BRN balance (single-rate estimate).
    ///
    /// This is a simplified offline estimate assuming a constant rate.
    /// For correct results across governance rate changes, use
    /// `brn_balance_with_history` instead.
    /// Returns 0 if the wallet is not yet verified.
    pub fn brn_balance(&self, now: Timestamp, brn_rate: u128) -> u128 {
        match self.verified_at {
            Some(verified_at) => crate::balance::compute_display_balance(
                verified_at,
                now,
                brn_rate,
                self.total_brn_burned,
                self.total_brn_staked,
            ),
            None => 0,
        }
    }

    /// Get the current BRN balance using the full rate history.
    ///
    /// `BRN = Σ(rate_i × duration_i) − total_burned − total_staked`
    ///
    /// This correctly accounts for governance rate changes over the wallet's
    /// lifetime. Returns 0 if the wallet is not yet verified.
    pub fn brn_balance_with_history(
        &self,
        now: Timestamp,
        rate_history: &burst_brn::state::RateHistory,
    ) -> u128 {
        match self.verified_at {
            Some(verified_at) => crate::balance::compute_balance_with_history(
                verified_at,
                now,
                rate_history,
                self.total_brn_burned,
                self.total_brn_staked,
            ),
            None => 0,
        }
    }

    /// Get transferable TRST balance by querying the connected node.
    #[cfg(not(target_arch = "wasm32"))]
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

    /// Get expired TRST (virtue points / reputation) by querying the connected node.
    #[cfg(not(target_arch = "wasm32"))]
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
    /// Queries the node's `account_info` RPC and parses the `trst_revoked` field.
    /// The node currently returns "0" because cumulative revocation tracking
    /// requires the MergerGraph revocation subsystem to populate a dedicated
    /// counter on AccountInfo. The code path is complete so that once the node
    /// reports real data, the wallet will surface it without changes.
    #[cfg(not(target_arch = "wasm32"))]
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
    use burst_crypto::verify_signature;

    #[test]
    fn test_wallet_create() {
        let wallet = Wallet::create().unwrap();

        // Check wallet is in Unverified state
        assert_eq!(wallet.state, WalletState::Unverified);
        assert_eq!(wallet.verified_at, None);

        // Check address is valid
        assert!(wallet.address.as_str().starts_with("brst_"));
        assert!(wallet.address.is_valid());
    }

    #[test]
    fn test_wallet_from_private_key() {
        // Create a wallet first to get a private key
        let wallet1 = Wallet::create().unwrap();
        let private_key_bytes = wallet1.primary_keys.private.0;

        // Restore wallet from private key
        let wallet2 = Wallet::from_private_key(&private_key_bytes).unwrap();

        // Should have same address and keys
        assert_eq!(wallet1.address, wallet2.address);
        assert_eq!(
            wallet1.primary_keys.public.as_bytes(),
            wallet2.primary_keys.public.as_bytes()
        );

        // Should be in Unverified state
        assert_eq!(wallet2.state, WalletState::Unverified);
    }

    #[test]
    fn test_wallet_from_private_key_invalid_length() {
        // Test with wrong length
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

        // Verify signature is valid
        assert!(verify_signature(
            message,
            &signature,
            &wallet.primary_keys.public
        ));

        // Verify wrong message fails
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

        // Each wallet should have unique address and keys
        assert_ne!(wallet1.address, wallet2.address);
        assert_ne!(
            wallet1.primary_keys.public.as_bytes(),
            wallet2.primary_keys.public.as_bytes()
        );
    }

    #[test]
    fn test_wallet_brn_balance_unverified() {
        let wallet = Wallet::create().unwrap();
        assert_eq!(wallet.brn_balance(Timestamp::now(), 10), 0);
    }

    #[test]
    fn test_wallet_brn_balance_verified() {
        let mut wallet = Wallet::create().unwrap();
        wallet.state = WalletState::Verified;
        wallet.verified_at = Some(Timestamp::new(1000));

        // 500 seconds elapsed × rate 10 = 5000 BRN
        let balance = wallet.brn_balance(Timestamp::new(1500), 10);
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
        let balance = wallet.brn_balance(Timestamp::new(1500), 10);
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
