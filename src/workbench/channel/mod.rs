//! Channel-specific boundaries, independent of PTY forwarding and durable history.
//! Production consumers require the explicit experimental channel-ready mode.

pub(crate) mod binding;
pub(crate) mod interaction;
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
