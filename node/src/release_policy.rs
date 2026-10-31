//! Feature availability: a protocol-wide enable flag or experimental-testnet
//! on Dev/Test. The experimental feature alone never enables Live.

use burst_types::NetworkId;

const fn experimental(network: NetworkId) -> bool {
    cfg!(feature = "experimental-testnet") && matches!(network, NetworkId::Test | NetworkId::Dev)
}

pub(crate) const fn governance_content(network: NetworkId) -> bool {
    burst_governance::content::FULL_CONTENT_ENABLED || experimental(network)
}

pub(crate) const fn delegation(network: NetworkId) -> bool {
    burst_governance::signing_delegation::operation::LEDGER_DELEGATION_ENABLED || experimental(network)
}

pub(crate) const fn appeals(network: NetworkId) -> bool {
    burst_verification::META_CHALLENGES_ENABLED || experimental(network)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn governance_policy_requires_opt_in_and_never_enables_live() {
        for policy in [governance_content, delegation, appeals] {
            assert!(!policy(NetworkId::Live));
            for network in [NetworkId::Dev, NetworkId::Test] {
                assert_eq!(policy(network), cfg!(feature = "experimental-testnet"));
            }
        }
    }
}
