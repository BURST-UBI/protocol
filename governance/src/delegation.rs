//! Vote delegation — entrust voting power to a representative.
//!
//! This is governance delegation (one-person-one-vote), distinct from
//! consensus delegation (balance-weighted for ORV).

use crate::error::GovernanceError;
use burst_types::WalletAddress;
use std::collections::HashMap;

/// Manages vote delegation for governance.
pub struct DelegationEngine {
    /// Maps delegator → delegate.
    delegations: HashMap<WalletAddress, WalletAddress>,
}

impl DelegationEngine {
    pub fn new() -> Self {
        Self {
            delegations: HashMap::new(),
        }
    }

    /// Delegate voting power to another wallet.
    pub fn delegate(
        &mut self,
        delegator: WalletAddress,
        delegate: WalletAddress,
    ) -> Result<(), GovernanceError> {
        self.delegations.insert(delegator, delegate);
        Ok(())
    }

    /// Revoke a delegation.
    pub fn revoke(&mut self, delegator: &WalletAddress) -> Result<(), GovernanceError> {
        self.delegations.remove(delegator);
        Ok(())
    }

    /// Get the delegate for a wallet (None if not delegated).
    pub fn get_delegate(&self, delegator: &WalletAddress) -> Option<&WalletAddress> {
        self.delegations.get(delegator)
    }

    /// Get all wallets that delegated to a given delegate.
    pub fn get_delegators(&self, delegate: &WalletAddress) -> Vec<&WalletAddress> {
        self.delegations
            .iter()
            .filter(|(_, d)| *d == delegate)
            .map(|(delegator, _)| delegator)
            .collect()
    }

    /// Total voting power of a delegate (1 + number of delegators).
    pub fn voting_power(&self, delegate: &WalletAddress) -> u32 {
        1 + self.get_delegators(delegate).len() as u32
    }
}

impl Default for DelegationEngine {
    fn default() -> Self {
        Self::new()
    }
}
