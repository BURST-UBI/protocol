//! Checkpoint agreement, replay, persistence and peer synchronization services.

pub mod activation;
pub mod agreement;
pub(crate) mod beacon;
pub mod deadline;
pub mod driver;
pub mod governance;
pub(crate) mod installation;
pub mod outcome;
pub(crate) mod recovery;
pub mod replay;
pub mod roster;
pub(crate) mod selection;
pub mod service;
pub mod store;
pub mod sync;
pub mod transport;
