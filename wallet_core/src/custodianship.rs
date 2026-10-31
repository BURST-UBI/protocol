//! In-memory guardian relationships for wallet clients.
//! These records do not grant protocol signing authority or verify consent.

use burst_types::WalletAddress;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustodianshipStatus {
    Active,
    Terminated,
}

#[derive(Debug, Clone)]
pub struct Custodianship {
    pub guardian: WalletAddress,
    pub ward: WalletAddress,
    pub status: CustodianshipStatus,
    pub created_at: u64,
    pub terminated_at: Option<u64>,
}

pub struct CustodianshipRegistry {
    /// ward -> custodianship
    custodianships: HashMap<WalletAddress, Custodianship>,
}

impl CustodianshipRegistry {
    pub fn new() -> Self {
        Self {
            custodianships: HashMap::new(),
        }
    }

    /// Record a guardian relationship. The caller must establish consent.
    /// A prior record for the ward, even a terminated one, prevents replacement.
    pub fn establish(
        &mut self,
        guardian: WalletAddress,
        ward: WalletAddress,
        timestamp: u64,
    ) -> Result<(), CustodianshipError> {
        if self.custodianships.contains_key(&ward) {
            return Err(CustodianshipError::AlreadyHasCustodian);
        }
        if guardian == ward {
            return Err(CustodianshipError::SelfCustodianship);
        }
        self.custodianships.insert(
            ward.clone(),
            Custodianship {
                guardian,
                ward,
                status: CustodianshipStatus::Active,
                created_at: timestamp,
                terminated_at: None,
            },
        );
        Ok(())
    }

    /// Mark a recorded relationship terminated. Caller authorization is external.
    pub fn terminate(
        &mut self,
        ward: &WalletAddress,
        timestamp: u64,
    ) -> Result<(), CustodianshipError> {
        match self.custodianships.get_mut(ward) {
            Some(c) if c.status == CustodianshipStatus::Active => {
                c.status = CustodianshipStatus::Terminated;
                c.terminated_at = Some(timestamp);
                Ok(())
            }
            Some(_) => Err(CustodianshipError::AlreadyTerminated),
            None => Err(CustodianshipError::NoCustodianship),
        }
    }

    /// Check if a wallet is a guardian of another wallet.
    pub fn is_guardian(&self, guardian: &WalletAddress, ward: &WalletAddress) -> bool {
        self.custodianships
            .get(ward)
            .map(|c| c.guardian == *guardian && c.status == CustodianshipStatus::Active)
            .unwrap_or(false)
    }

    /// Check identity or an active relationship in this local registry.
    /// This does not validate a signature or authorize a protocol operation.
    pub fn is_self_or_guardian(&self, actor: &WalletAddress, account: &WalletAddress) -> bool {
        actor == account || self.is_guardian(actor, account)
    }

    /// Get a ward's record, including a terminated relationship.
    pub fn get(&self, ward: &WalletAddress) -> Option<&Custodianship> {
        self.custodianships.get(ward)
    }

    /// Get all wards managed by a guardian.
    pub fn wards_of(&self, guardian: &WalletAddress) -> Vec<&Custodianship> {
        self.custodianships
            .values()
            .filter(|c| c.guardian == *guardian && c.status == CustodianshipStatus::Active)
            .collect()
    }
}

impl Default for CustodianshipRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustodianshipError {
    AlreadyHasCustodian,
    SelfCustodianship,
    AlreadyTerminated,
    NoCustodianship,
}

impl std::fmt::Display for CustodianshipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyHasCustodian => write!(f, "ward already has a custodian"),
            Self::SelfCustodianship => write!(f, "cannot be own custodian"),
            Self::AlreadyTerminated => write!(f, "custodianship already terminated"),
            Self::NoCustodianship => write!(f, "no active custodianship found"),
        }
    }
}

impl std::error::Error for CustodianshipError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> WalletAddress {
        WalletAddress::new(s)
    }

    #[test]
    fn establish_and_check_relationship() {
        let mut reg = CustodianshipRegistry::new();
        let parent = addr("brst_parent000000000000000000000000000000000000000000000000000parent00");
        let child = addr("brst_child0000000000000000000000000000000000000000000000000child000");

        reg.establish(parent.clone(), child.clone(), 1000).unwrap();
        assert!(reg.is_self_or_guardian(&parent, &child));
        assert!(reg.is_self_or_guardian(&child, &child));
    }

    #[test]
    fn terminate_custodianship() {
        let mut reg = CustodianshipRegistry::new();
        let parent = addr("brst_parent000000000000000000000000000000000000000000000000000parent00");
        let child = addr("brst_child0000000000000000000000000000000000000000000000000child000");

        reg.establish(parent.clone(), child.clone(), 1000).unwrap();
        reg.terminate(&child, 2000).unwrap();
        assert!(!reg.is_guardian(&parent, &child));
    }

    #[test]
    fn self_custodianship_rejected() {
        let mut reg = CustodianshipRegistry::new();
        let addr1 = addr("brst_self00000000000000000000000000000000000000000000000000self0000");
        assert_eq!(
            reg.establish(addr1.clone(), addr1, 1000).unwrap_err(),
            CustodianshipError::SelfCustodianship
        );
    }

    #[test]
    fn duplicate_custodianship_rejected() {
        let mut reg = CustodianshipRegistry::new();
        let p1 = addr("brst_p1000000000000000000000000000000000000000000000000000000p10000");
        let p2 = addr("brst_p2000000000000000000000000000000000000000000000000000000p20000");
        let child = addr("brst_child0000000000000000000000000000000000000000000000000child000");

        reg.establish(p1, child.clone(), 1000).unwrap();
        assert_eq!(
            reg.establish(p2, child, 1000).unwrap_err(),
            CustodianshipError::AlreadyHasCustodian
        );
    }
}
