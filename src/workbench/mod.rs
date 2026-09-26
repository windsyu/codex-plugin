//! The new runtime does not depend on the Observer writer or control ledger.

#[cfg(unix)]
pub mod application;
pub mod capture;
#[cfg(unix)]
pub mod config;
pub mod config_sources;
pub mod control;
pub mod decode;
pub mod framing;
#[cfg(unix)]
pub mod launch;
pub mod live;
pub mod observe;
pub mod paths;
pub mod permission;
pub mod proxy;
#[cfg(unix)]
pub mod recording;
pub mod redaction;
#[cfg(unix)]
pub mod rollout;
#[cfg(unix)]
pub mod terminal;
// Shared VT utilities have no legacy control or storage dependency.
#[path = "../terminal/mod.rs"]
mod terminal_screen;
pub mod web;
#[cfg(unix)]
pub mod workspace;

#[cfg(test)]
pub(crate) mod probe_process;
#[cfg(test)]
mod test_browser;
#[cfg(test)]
pub(crate) mod test_native;
