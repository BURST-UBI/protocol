use burst_types::NetworkId;
use burst_verification::{checkpoint_selection_enabled, CHECKPOINT_SELECTION_ENABLED};

// Integration tests link the normal library, not its cfg(test) implementation.
#[test]
fn experimental_selection_never_enables_live() {
    const { assert!(!CHECKPOINT_SELECTION_ENABLED) };
    assert!(!checkpoint_selection_enabled(NetworkId::Live));
}

#[test]
fn non_live_selection_requires_an_explicit_build_feature() {
    for network in [NetworkId::Dev, NetworkId::Test] {
        assert_eq!(
            checkpoint_selection_enabled(network),
            cfg!(feature = "experimental-testnet"),
        );
    }
}
