//! Channel-specific boundaries, independent of PTY forwarding and durable history.
//! The isolated protocol example is the first consumer; production attachment is not enabled.

pub(crate) mod binding;
#[cfg(unix)]
pub mod native;
mod origin;
pub(crate) mod receipts;
pub(crate) mod service;
#[cfg(unix)]
pub(crate) mod session;
pub mod store;
#[cfg(unix)]
pub(crate) mod subscription;
pub mod telegram;

pub use origin::{Completion, Evidence, NativeStatus, OriginProof, Withheld};
