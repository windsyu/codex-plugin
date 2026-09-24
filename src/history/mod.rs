//! Read-only history adapters. No Observer startup, migration or writer imports.

mod files;
mod legacy_contract;
pub mod legacy_reader;
mod readonly_vfs;

// Pure JSON redaction only; no legacy runtime or persistence dependencies.
#[path = "../domain/redact.rs"]
mod redact;

#[cfg(test)]
mod tests;

#[path = "../domain/classify.rs"]
mod classify;
pub mod library;
