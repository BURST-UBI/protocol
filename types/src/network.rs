//! Network identifier.

use serde::{Deserialize, Serialize};

/// Identifies which BURST network a node is connected to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NetworkId {
    /// The production network.
    Live,
    /// The public test network.
    Test,
    /// Local development network. Default: an unspecified network is treated as
    /// the isolated Dev network — never Live — so a missing/garbled network id
    /// fails closed rather than leaking onto production.
    #[default]
    Dev,
}

impl NetworkId {
    /// Default port for this network.
    pub fn default_port(&self) -> u16 {
        match self {
            Self::Live => 7076,
            Self::Test => 17076,
            Self::Dev => 27076,
        }
    }

    /// Human-readable name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Test => "test",
            Self::Dev => "dev",
        }
    }
}
