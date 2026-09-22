//! VT reconstruction is an observation, not the lifetime owner of the CLI.
//! On a parser unwind discard the corrupt state, retain output sequencing and
//! keep forwarding sanitized bytes. Never call the damaged parser again.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::Duration;

use crate::workbench::terminal_screen::{OutputEvent, TerminalJournal, TerminalSnapshot};

pub(super) struct ScreenJournal {
    journal: Option<TerminalJournal>,
    last_seq: u64,
    rows: u16,
    cols: u16,
}

impl ScreenJournal {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            journal: Some(TerminalJournal::new(
                rows,
                cols,
                1024 * 1024,
                Duration::from_secs(60),
            )),
            last_seq: 0,
            rows,
            cols,
        }
    }

    fn with<T>(&mut self, action: impl FnOnce(&mut TerminalJournal) -> T) -> Option<T> {
        let journal = self.journal.as_mut()?;
        match catch_unwind(AssertUnwindSafe(|| action(journal))) {
            Ok(result) => Some(result),
            Err(_) => {
                self.journal = None;
                None
            }
        }
    }

    pub fn append(&mut self, bytes: Vec<u8>) -> OutputEvent {
        self.last_seq = self.last_seq.saturating_add(1);
        let data: Arc<[u8]> = bytes.into();
        // Keep the original copy even when reconstruction fails. The existing
        // xterm receives every sanitized output event in its original order.
        self.with(|journal| journal.append(data.to_vec()));
        OutputEvent {
            output_seq: self.last_seq,
            data,
        }
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.rows = rows;
        self.cols = cols;
        self.with(|journal| journal.resize(rows, cols));
    }

    pub fn snapshot(&mut self) -> TerminalSnapshot {
        self.with(|journal| journal.snapshot(None))
            .unwrap_or_else(|| TerminalSnapshot {
                checkpoint_seq: self.last_seq,
                from_seq: self.last_seq.saturating_add(1),
                to_seq: self.last_seq,
                rows: self.rows,
                cols: self.cols,
                screen: b"\x1bc".to_vec(),
                replay: Vec::new(),
                complete: false,
                truncated: true,
            })
    }

    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }
    pub fn failed(&self) -> bool {
        self.journal.is_none()
    }
    pub fn cursor_position(&mut self) -> Option<(u16, u16)> {
        self.with(|journal| journal.cursor_position())
    }
    pub fn retained_bytes(&self) -> usize {
        self.journal
            .as_ref()
            .map_or(0, TerminalJournal::retained_bytes)
    }
    pub fn checkpoint_bytes(&self) -> usize {
        self.journal
            .as_ref()
            .map_or(0, TerminalJournal::checkpoint_bytes)
    }

    #[cfg(test)]
    pub fn fail_for_test(&mut self) {
        self.with::<()>(|_| panic!("synthetic VT reconstruction failure"));
    }
}
