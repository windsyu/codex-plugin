//! Components of the native CLI workbench, independent of the legacy controller.

pub mod workbench;

#[cfg(unix)]
pub mod history;

// Let opt-in library tests reuse the existing example measurement code.
#[cfg(test)]
extern crate self as codex_local_observer;
