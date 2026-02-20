//! Ledger snapshots — capture the state of all accounts at a point in time.
//!
//! Snapshots enable fast node bootstrapping: a new node can load a verified
//! snapshot instead of replaying every block from genesis. The snapshot hash
//! is computed deterministically from the account state so peers can verify
//! snapshot integrity.

use serde::{Deserialize, Serialize};

use burst_types::{BlockHash, Timestamp, WalletAddress, WalletState};

/// A ledger snapshot — captures the state of all accounts at a point in time.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedgerSnapshot {
    /// Hash of this snapshot (Blake2b of serialized accounts).
    pub hash: [u8; 32],
    /// Block height at which this snapshot was taken.
    pub block_height: u64,
    /// Timestamp when snapshot was created.
    pub created_at: Timestamp,
    /// Account state entries.
    pub accounts: Vec<AccountSnapshot>,
    /// Snapshot version for compatibility.
    pub version: u32,
}

/// The state of a single account captured in a snapshot.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountSnapshot {
    /// The account's wallet address.
    pub address: WalletAddress,
    /// Wallet state (Unverified, Pending, Verified).
    pub state: WalletState,
    /// Timestamp when this wallet was verified (required for BRN computation).
    pub verified_at: Option<Timestamp>,
    /// Hash of the head (latest) block in this account's chain.
    pub head: BlockHash,
    /// Total number of blocks in this account's chain.
    pub block_count: u64,
    /// Height below which all blocks are confirmed.
    pub confirmation_height: u64,
    /// Total BRN burned by this account (to create TRST).
    pub brn_burned: u128,
    /// Total BRN currently locked in active stakes.
    pub total_brn_staked: u128,
    /// Current TRST balance (sum of non-expired, non-revoked TRST).
    pub trst_balance: u128,
    /// The representative this account delegates voting weight to.
    pub representative: WalletAddress,
}

impl LedgerSnapshot {
    /// Create a snapshot from the current account store.
    pub fn create(accounts: Vec<AccountSnapshot>, block_height: u64) -> Self {
        let mut snap = Self {
            hash: [0u8; 32],
            block_height,
            created_at: Timestamp::now(),
            accounts,
            version: 1,
        };
        snap.hash = snap.compute_hash();
        snap
    }

    /// Compute the Blake2b-256 hash of this snapshot deterministically.
    fn compute_hash(&self) -> [u8; 32] {
        use blake2::digest::consts::U32;
        use blake2::{Blake2b, Digest};

        let mut hasher = Blake2b::<U32>::new();
        for account in &self.accounts {
            hasher.update(account.address.as_str().as_bytes());
            hasher.update(format!("{:?}", account.state).as_bytes());
            hasher.update(&account.verified_at.map_or(0u64, |t| t.as_secs()).to_le_bytes());
            hasher.update(account.head.as_bytes());
            hasher.update(&account.block_count.to_le_bytes());
            hasher.update(&account.confirmation_height.to_le_bytes());
            hasher.update(&account.brn_burned.to_le_bytes());
            hasher.update(&account.total_brn_staked.to_le_bytes());
            hasher.update(&account.trst_balance.to_le_bytes());
            hasher.update(account.representative.as_str().as_bytes());
        }
        hasher.update(&self.block_height.to_le_bytes());

        let result = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&result);
        out
    }

    /// Verify the snapshot hash matches the account data.
    pub fn verify(&self) -> bool {
        self.hash == self.compute_hash()
    }

    /// Serialize the snapshot to bytes (bincode).
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).expect("snapshot serialization should not fail")
    }

    /// Deserialize a snapshot from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        bincode::deserialize(bytes).map_err(|e| e.to_string())
    }

    /// Number of accounts in this snapshot.
    pub fn account_count(&self) -> usize {
        self.accounts.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::{BlockHash, WalletAddress};

    fn sample_account(suffix: &str) -> AccountSnapshot {
        AccountSnapshot {
            address: WalletAddress::new(format!("brst_{suffix}")),
            state: WalletState::Verified,
            verified_at: Some(Timestamp::new(1_000_000)),
            head: BlockHash::new([0xAA; 32]),
            block_count: 10,
            confirmation_height: 9,
            brn_burned: 1000,
            total_brn_staked: 0,
            trst_balance: 500,
            representative: WalletAddress::new(format!("brst_rep_{suffix}")),
        }
    }

    #[test]
    fn test_create_and_verify() {
        let accounts = vec![sample_account("alice"), sample_account("bob")];
        let snap = LedgerSnapshot::create(accounts, 100);

        assert!(snap.verify());
        assert_eq!(snap.block_height, 100);
        assert_eq!(snap.version, 1);
        assert_eq!(snap.account_count(), 2);
    }

    #[test]
    fn test_tampered_snapshot_fails_verify() {
        let accounts = vec![sample_account("alice")];
        let mut snap = LedgerSnapshot::create(accounts, 42);
        assert!(snap.verify());

        // Tamper with block_height
        snap.block_height = 999;
        assert!(!snap.verify());
    }

    #[test]
    fn test_serialize_roundtrip() {
        let accounts = vec![sample_account("alice"), sample_account("bob")];
        let snap = LedgerSnapshot::create(accounts, 50);

        let bytes = snap.to_bytes();
        let restored = LedgerSnapshot::from_bytes(&bytes).expect("deserialization failed");

        assert_eq!(snap.hash, restored.hash);
        assert_eq!(snap.block_height, restored.block_height);
        assert_eq!(snap.account_count(), restored.account_count());
        assert!(restored.verify());
    }

    #[test]
    fn test_empty_snapshot() {
        let snap = LedgerSnapshot::create(vec![], 0);
        assert!(snap.verify());
        assert_eq!(snap.account_count(), 0);
    }

    #[test]
    fn test_deterministic_hash() {
        let accounts = vec![sample_account("alice")];
        let snap1 = LedgerSnapshot {
            hash: [0u8; 32],
            block_height: 10,
            created_at: Timestamp::new(1000),
            accounts: accounts.clone(),
            version: 1,
        };
        let snap2 = LedgerSnapshot {
            hash: [0u8; 32],
            block_height: 10,
            created_at: Timestamp::new(2000), // different timestamp
            accounts,
            version: 1,
        };
        // Hash depends on accounts + block_height, not created_at
        let h1 = {
            let mut s = snap1;
            s.hash = s.compute_hash();
            s.hash
        };
        let h2 = {
            let mut s = snap2;
            s.hash = s.compute_hash();
            s.hash
        };
        assert_eq!(h1, h2);
    }
}
