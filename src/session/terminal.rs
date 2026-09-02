use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct OutputEvent {
    pub output_seq: u64,
    pub data: Arc<[u8]>,
}

#[derive(Debug, Clone)]
pub struct TerminalSnapshot {
    pub checkpoint_seq: u64,
    pub from_seq: u64,
    pub to_seq: u64,
    pub rows: u16,
    pub cols: u16,
    pub screen: Vec<u8>,
    pub replay: Vec<u8>,
    pub complete: bool,
    pub truncated: bool,
}

#[derive(Debug)]
struct OutputChunk {
    seq: u64,
    observed_at: Instant,
    data: Arc<[u8]>,
}

pub struct TerminalJournal {
    parser: vt100::Parser,
    rows: u16,
    cols: u16,
    max_bytes: usize,
    max_age: Duration,
    journal_bytes: usize,
    chunks: VecDeque<OutputChunk>,
    next_seq: u64,
    checkpoint_seq: u64,
    checkpoint_screen: Vec<u8>,
    checkpoint_complete: bool,
}

#[derive(Debug, Default)]
pub struct TerminalSanitizer {
    state: SanitizeState,
}

#[derive(Debug, Default)]
enum SanitizeState {
    #[default]
    Ground,
    Escape,
    Osc,
    OscEscape,
}

impl TerminalSanitizer {
    pub fn process(&mut self, input: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(input.len());
        for byte in input.iter().copied() {
            match self.state {
                SanitizeState::Ground if byte == 0x1b => self.state = SanitizeState::Escape,
                SanitizeState::Ground if byte == 0x9d => self.state = SanitizeState::Osc,
                SanitizeState::Ground => output.push(byte),
                SanitizeState::Escape if byte == b']' => self.state = SanitizeState::Osc,
                SanitizeState::Escape if byte == 0x1b => {}
                SanitizeState::Escape => {
                    output.push(0x1b);
                    output.push(byte);
                    self.state = SanitizeState::Ground;
                }
                SanitizeState::Osc if byte == 0x07 => self.state = SanitizeState::Ground,
                SanitizeState::Osc if byte == 0x9c => self.state = SanitizeState::Ground,
                SanitizeState::Osc if byte == 0x1b => self.state = SanitizeState::OscEscape,
                SanitizeState::Osc => {}
                SanitizeState::OscEscape if byte == b'\\' => self.state = SanitizeState::Ground,
                SanitizeState::OscEscape if byte == 0x9c => self.state = SanitizeState::Ground,
                SanitizeState::OscEscape if byte == 0x1b => {}
                SanitizeState::OscEscape => self.state = SanitizeState::Osc,
            }
        }
        output
    }
}

impl TerminalJournal {
    pub fn new(rows: u16, cols: u16, max_bytes: usize, max_age: Duration) -> Self {
        let parser = vt100::Parser::new(rows, cols, 0);
        let checkpoint_screen = parser.screen().contents_formatted();
        Self {
            parser,
            rows,
            cols,
            max_bytes,
            max_age,
            journal_bytes: 0,
            chunks: VecDeque::new(),
            next_seq: 1,
            checkpoint_seq: 0,
            checkpoint_screen,
            checkpoint_complete: true,
        }
    }

    pub fn append(&mut self, data: Vec<u8>) -> OutputEvent {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        self.parser.process(&data);
        let data: Arc<[u8]> = data.into();
        self.journal_bytes = self.journal_bytes.saturating_add(data.len());
        self.chunks.push_back(OutputChunk {
            seq,
            observed_at: Instant::now(),
            data: data.clone(),
        });
        self.compact_if_needed();
        OutputEvent {
            output_seq: seq,
            data,
        }
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.rows = rows;
        self.cols = cols;
        self.parser.screen_mut().set_size(rows, cols);
        self.checkpoint();
    }

    pub fn last_seq(&self) -> u64 {
        self.next_seq.saturating_sub(1)
    }

    pub fn retained_bytes(&self) -> usize {
        self.journal_bytes
    }

    pub fn checkpoint_bytes(&self) -> usize {
        self.checkpoint_screen.len()
    }

    pub fn snapshot(&mut self, _after_seq: Option<u64>) -> TerminalSnapshot {
        self.compact_if_needed();
        let replay = self
            .chunks
            .iter()
            .filter(|chunk| chunk.seq > self.checkpoint_seq)
            .flat_map(|chunk| chunk.data.iter().copied())
            .collect::<Vec<_>>();
        let from_seq = self
            .chunks
            .iter()
            .find(|chunk| chunk.seq > self.checkpoint_seq)
            .map(|chunk| chunk.seq)
            .unwrap_or_else(|| self.checkpoint_seq.saturating_add(1));
        TerminalSnapshot {
            checkpoint_seq: self.checkpoint_seq,
            from_seq,
            to_seq: self.last_seq(),
            rows: self.rows,
            cols: self.cols,
            screen: self.checkpoint_screen.clone(),
            replay,
            complete: self.checkpoint_complete,
            truncated: self.checkpoint_seq > 0,
        }
    }

    fn compact_if_needed(&mut self) {
        let age_exceeded = self
            .chunks
            .front()
            .is_some_and(|chunk| chunk.observed_at.elapsed() > self.max_age);
        if self.journal_bytes > self.max_bytes || age_exceeded {
            self.checkpoint();
        }
    }

    fn checkpoint(&mut self) {
        self.checkpoint_seq = self.last_seq();
        self.checkpoint_screen = self.parser.screen().contents_formatted();
        self.checkpoint_complete = true;
        self.chunks.clear();
        self.journal_bytes = 0;
    }

    #[cfg(test)]
    pub fn invalidate_checkpoint_for_test(&mut self) {
        self.checkpoint_screen.clear();
        self.checkpoint_complete = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_journal_compacts_to_a_complete_vt_checkpoint() {
        let mut journal = TerminalJournal::new(24, 80, 8, Duration::from_secs(60));
        let first = journal.append(b"\x1b[31mred".to_vec());
        let second = journal.append(b" and green".to_vec());
        assert_eq!(first.output_seq, 1);
        assert_eq!(second.output_seq, 2);
        let snapshot = journal.snapshot(None);
        assert!(snapshot.complete);
        assert!(snapshot.truncated);
        assert_eq!(snapshot.checkpoint_seq, 2);
        assert_eq!(snapshot.to_seq, 2);
        assert!(snapshot.replay.is_empty());
        assert!(!snapshot.screen.is_empty());
    }

    #[test]
    fn snapshot_never_claims_a_corrupt_checkpoint_is_complete() {
        let mut journal = TerminalJournal::new(10, 20, 1024, Duration::from_secs(60));
        journal.append(b"visible".to_vec());
        journal.invalidate_checkpoint_for_test();
        let snapshot = journal.snapshot(None);
        assert!(!snapshot.complete);
    }

    #[test]
    fn resize_updates_checkpoint_dimensions() {
        let mut journal = TerminalJournal::new(24, 80, 1024, Duration::from_secs(60));
        journal.append(b"before resize".to_vec());
        journal.resize(40, 120);
        let snapshot = journal.snapshot(None);
        assert_eq!((snapshot.rows, snapshot.cols), (40, 120));
        assert_eq!(snapshot.checkpoint_seq, 1);
    }

    #[test]
    fn strips_osc_clipboard_links_titles_and_file_sequences_across_chunks() {
        let mut sanitizer = TerminalSanitizer::default();
        let first = sanitizer.process(b"safe\x1b]52;c;secret");
        let second = sanitizer.process(b"\x07after\x1b]8;;https://evil");
        let third = sanitizer.process(b"\x1b\\link\x1b]1337;File=name=x:data\x07done");
        let fourth = sanitizer.process(b"\x9d52;c;c1-secret\x9cc1-safe");
        let fifth = sanitizer.process(b"\x1b\x1b]0;repeated-escape\x07tail");
        assert_eq!(
            [first, second, third, fourth, fifth].concat(),
            b"safeafterlinkdonec1-safetail"
        );
    }

    #[test]
    fn unicode_wide_cells_and_long_lines_remain_valid_after_checkpoint_compaction() {
        let mut journal = TerminalJournal::new(4, 20, 32, Duration::from_secs(60));
        journal.append("中文🙂".repeat(64).into_bytes());
        journal.append(b"\r\nfinal".to_vec());
        let snapshot = journal.snapshot(None);
        assert!(snapshot.complete);
        assert!(snapshot.truncated);
        assert!(std::str::from_utf8(&snapshot.screen).is_ok());
        assert!(std::str::from_utf8(&snapshot.replay).is_ok());
        assert!(String::from_utf8_lossy(&snapshot.replay).contains("final"));
        assert_eq!(snapshot.from_seq, snapshot.checkpoint_seq + 1);
    }
}
