//! Deterministic test doubles. Production code is injectable only where it
//! explicitly accepts one of these implementations; this is not a universal
//! clock/network abstraction. In-memory delegation storage lives in burst-store.

pub mod clock;
pub mod network;
pub mod random;
pub mod store;

pub use clock::NullClock;
pub use network::NullNetwork;
pub use random::NullRandom;
pub use store::NullStore;
