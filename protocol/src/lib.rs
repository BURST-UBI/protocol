//! Wire protocol — message framing, encoding/decoding, handshake, versioning.

pub mod codec;
pub mod error;
pub mod handshake;
pub mod version;

pub use error::ProtocolError;
pub use version::PROTOCOL_VERSION;
