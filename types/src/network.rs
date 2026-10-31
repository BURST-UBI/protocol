//! Network identifier.

use serde::{Deserialize, Serialize};

/// Network selection used for configuration and protocol identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NetworkId {
    /// The production network.
    Live,
    /// The Test network identity.
    Test,
    /// Development network and default enum value. Parsers must still reject
    /// invalid input; this default does not validate or isolate a deployment.
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
