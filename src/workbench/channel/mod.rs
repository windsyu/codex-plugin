//! Channel-specific boundaries, independent of PTY forwarding and durable history.
//! The isolated protocol example is the first consumer; production attachment is not enabled.

#[cfg(unix)]
pub mod native;
mod origin;
pub mod store;
pub mod telegram;

pub use origin::{Completion, Evidence, NativeStatus, OriginProof, Withheld};
