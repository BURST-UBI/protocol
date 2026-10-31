//! Removal of retained block bodies using a checked recovery catalogue.
//! Non-test deletion is gated in the installed-recovery commit path.

pub(crate) mod catalogue;
pub(crate) mod worker;
