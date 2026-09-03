use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::Deserialize;

const TERMINAL_PROFILE: &str = include_str!("../../web/src/session/terminalProfile.json");
const MAX_CSI_BYTES: usize = 256;
const MAX_OSC_BYTES: usize = 4 * 1024;

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

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TerminalReplyRequest {
    CursorPosition,
    DefaultForeground,
    DefaultBackground,
    PrimaryDeviceAttributes,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub struct TerminalFilterResult {
    pub display_bytes: Vec<u8>,
    pub terminal_replies: Vec<TerminalReplyRequest>,
}

#[derive(Debug, Default)]
pub struct TerminalOutputFilter {
    state: FilterState,
    utf8_continuations: u8,
}

#[derive(Debug, Default)]
enum FilterState {
    #[default]
    Ground,
    Escape,
    Csi(Vec<u8>),
    CsiDiscard,
    Osc {
        payload: Vec<u8>,
        overflowed: bool,
    },
    OscEscape {
        payload: Vec<u8>,
        overflowed: bool,
    },
}

#[derive(Debug, Deserialize)]
struct TerminalHostProfile {
    foreground: String,
    background: String,
}

#[derive(Debug, Clone, Copy)]
struct TerminalProbeColors {
    foreground: (u8, u8, u8),
    background: (u8, u8, u8),
}

impl TerminalOutputFilter {
    pub fn process(&mut self, input: &[u8]) -> TerminalFilterResult {
        let mut result = TerminalFilterResult {
            display_bytes: Vec::with_capacity(input.len()),
            terminal_replies: Vec::new(),
        };
        for byte in input.iter().copied() {
            if matches!(&self.state, FilterState::Ground) && self.utf8_continuations > 0 {
                if (0x80..=0xbf).contains(&byte) {
                    result.display_bytes.push(byte);
                    self.utf8_continuations -= 1;
                    continue;
                }
                self.utf8_continuations = 0;
            }
            let state = std::mem::take(&mut self.state);
            self.state = match state {
                FilterState::Ground if byte == 0x1b => FilterState::Escape,
                FilterState::Ground if byte == 0x9d => FilterState::Osc {
                    payload: Vec::new(),
                    overflowed: false,
                },
                FilterState::Ground if (0xc2..=0xdf).contains(&byte) => {
                    result.display_bytes.push(byte);
                    self.utf8_continuations = 1;
                    FilterState::Ground
                }
                FilterState::Ground if (0xe0..=0xef).contains(&byte) => {
                    result.display_bytes.push(byte);
                    self.utf8_continuations = 2;
                    FilterState::Ground
                }
                FilterState::Ground if (0xf0..=0xf4).contains(&byte) => {
                    result.display_bytes.push(byte);
                    self.utf8_continuations = 3;
                    FilterState::Ground
                }
                FilterState::Ground => {
                    result.display_bytes.push(byte);
                    FilterState::Ground
                }
                FilterState::Escape if byte == b']' => FilterState::Osc {
                    payload: Vec::new(),
                    overflowed: false,
                },
                FilterState::Escape if byte == b'[' => FilterState::Csi(vec![0x1b, b'[']),
                FilterState::Escape if byte == 0x9d => FilterState::Osc {
                    payload: Vec::new(),
                    overflowed: false,
                },
                FilterState::Escape if byte == 0x1b => FilterState::Escape,
                FilterState::Escape => {
                    result.display_bytes.extend_from_slice(&[0x1b, byte]);
                    FilterState::Ground
                }
                FilterState::Csi(_) if byte == 0x1b => FilterState::Escape,
                FilterState::Csi(_) if byte == 0x9d => FilterState::Osc {
                    payload: Vec::new(),
                    overflowed: false,
                },
                FilterState::Csi(mut sequence) => {
                    sequence.push(byte);
                    if is_csi_final(byte) {
                        handle_csi(sequence, &mut result);
                        FilterState::Ground
                    } else if sequence.len() >= MAX_CSI_BYTES {
                        FilterState::CsiDiscard
                    } else {
                        FilterState::Csi(sequence)
                    }
                }
                FilterState::CsiDiscard if byte == 0x1b => FilterState::Escape,
                FilterState::CsiDiscard if byte == 0x9d => FilterState::Osc {
                    payload: Vec::new(),
                    overflowed: false,
                },
                FilterState::CsiDiscard if is_csi_final(byte) => FilterState::Ground,
                FilterState::CsiDiscard => FilterState::CsiDiscard,
                FilterState::Osc {
                    mut payload,
                    mut overflowed,
                } => match byte {
                    0x07 | 0x9c => {
                        handle_osc(&payload, overflowed, &mut result);
                        FilterState::Ground
                    }
                    0x1b => FilterState::OscEscape {
                        payload,
                        overflowed,
                    },
                    _ => {
                        push_bounded_osc(&mut payload, &mut overflowed, byte);
                        FilterState::Osc {
                            payload,
                            overflowed,
                        }
                    }
                },
                FilterState::OscEscape {
                    mut payload,
                    mut overflowed,
                } => match byte {
                    b'\\' | 0x9c => {
                        handle_osc(&payload, overflowed, &mut result);
                        FilterState::Ground
                    }
                    0x1b => FilterState::OscEscape {
                        payload,
                        overflowed,
                    },
                    _ => {
                        push_bounded_osc(&mut payload, &mut overflowed, 0x1b);
                        push_bounded_osc(&mut payload, &mut overflowed, byte);
                        FilterState::Osc {
                            payload,
                            overflowed,
                        }
                    }
                },
            };
        }
        result
    }
}

fn is_csi_final(byte: u8) -> bool {
    (0x40..=0x7e).contains(&byte)
}

fn handle_csi(sequence: Vec<u8>, result: &mut TerminalFilterResult) {
    match sequence.as_slice() {
        b"\x1b[6n" => result
            .terminal_replies
            .push(TerminalReplyRequest::CursorPosition),
        b"\x1b[c" => result
            .terminal_replies
            .push(TerminalReplyRequest::PrimaryDeviceAttributes),
        // xterm does not implement the Kitty keyboard enhancement protocol. Codex batches this
        // query with a primary-device-attributes fallback, which we answer above.
        b"\x1b[?u" => {}
        _ => result.display_bytes.extend_from_slice(&sequence),
    }
}

fn push_bounded_osc(payload: &mut Vec<u8>, overflowed: &mut bool, byte: u8) {
    if payload.len() < MAX_OSC_BYTES {
        payload.push(byte);
    } else {
        *overflowed = true;
    }
}

fn handle_osc(payload: &[u8], overflowed: bool, result: &mut TerminalFilterResult) {
    if overflowed {
        return;
    }
    match payload {
        b"10;?" => result
            .terminal_replies
            .push(TerminalReplyRequest::DefaultForeground),
        b"11;?" => result
            .terminal_replies
            .push(TerminalReplyRequest::DefaultBackground),
        _ => {}
    }
}

impl TerminalReplyRequest {
    pub fn encode(self, cursor_position: (u16, u16)) -> Vec<u8> {
        match self {
            Self::CursorPosition => format!(
                "\x1b[{};{}R",
                cursor_position.0.saturating_add(1),
                cursor_position.1.saturating_add(1)
            )
            .into_bytes(),
            Self::DefaultForeground => osc_color_reply(10, terminal_probe_colors().foreground),
            Self::DefaultBackground => osc_color_reply(11, terminal_probe_colors().background),
            Self::PrimaryDeviceAttributes => b"\x1b[?1;2c".to_vec(),
        }
    }
}

fn terminal_probe_colors() -> TerminalProbeColors {
    static COLORS: OnceLock<TerminalProbeColors> = OnceLock::new();
    *COLORS.get_or_init(|| {
        let profile: TerminalHostProfile =
            serde_json::from_str(TERMINAL_PROFILE).expect("valid embedded terminal host profile");
        TerminalProbeColors {
            foreground: parse_hex_color(&profile.foreground)
                .expect("terminal foreground is a six-digit hex color"),
            background: parse_hex_color(&profile.background)
                .expect("terminal background is a six-digit hex color"),
        }
    })
}

fn parse_hex_color(value: &str) -> Option<(u8, u8, u8)> {
    let value = value.strip_prefix('#')?;
    if value.len() != 6 {
        return None;
    }
    Some((
        u8::from_str_radix(&value[0..2], 16).ok()?,
        u8::from_str_radix(&value[2..4], 16).ok()?,
        u8::from_str_radix(&value[4..6], 16).ok()?,
    ))
}

fn osc_color_reply(slot: u8, (red, green, blue): (u8, u8, u8)) -> Vec<u8> {
    format!("\x1b]{slot};rgb:{red:02x}{red:02x}/{green:02x}{green:02x}/{blue:02x}{blue:02x}\x1b\\")
        .into_bytes()
}

impl TerminalJournal {
    pub fn new(rows: u16, cols: u16, max_bytes: usize, max_age: Duration) -> Self {
        let parser = vt100::Parser::new(rows, cols, 0);
        let checkpoint_screen = parser.screen().state_formatted();
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

    pub fn cursor_position(&self) -> (u16, u16) {
        self.parser.screen().cursor_position()
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
            // state_formatted restores the current VT screen and modes, but a checkpoint cannot
            // recreate scrollback represented by compacted output chunks. Keep that distinction
            // visible to the Browser instead of presenting the reconstructed screen as complete.
            complete: self.checkpoint_complete && self.checkpoint_seq == 0,
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
        self.checkpoint_screen = self.parser.screen().state_formatted();
        self.checkpoint_complete = !self.parser.screen().alternate_screen();
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
    fn bounded_journal_compacts_to_a_formatted_but_partial_vt_checkpoint() {
        let mut journal = TerminalJournal::new(24, 80, 8, Duration::from_secs(60));
        let first = journal.append(b"\x1b[31mred".to_vec());
        let second = journal.append(b" and green".to_vec());
        assert_eq!(first.output_seq, 1);
        assert_eq!(second.output_seq, 2);
        let snapshot = journal.snapshot(None);
        assert!(!snapshot.complete);
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
        let mut filter = TerminalOutputFilter::default();
        let first = filter.process(b"safe\x1b]52;c;secret").display_bytes;
        let second = filter
            .process(b"\x07after\x1b]8;;https://evil")
            .display_bytes;
        let third = filter
            .process(b"\x1b\\link\x1b]1337;File=name=x:data\x07done")
            .display_bytes;
        let fourth = filter
            .process(b"\x9d52;c;c1-secret\x9cc1-safe")
            .display_bytes;
        let fifth = filter
            .process(b"\x1b\x1b]0;repeated-escape\x07tail")
            .display_bytes;
        assert_eq!(
            [first, second, third, fourth, fifth].concat(),
            b"safeafterlinkdonec1-safetail"
        );
    }

    #[test]
    fn preserves_codex_sgr_and_dec_sequences_byte_for_byte() {
        let mut filter = TerminalOutputFilter::default();
        let input = b"\x1b[1mbold\x1b[0m \x1b[2;3mdim italic\x1b[0m \x1b[36mcyan\x1b[0m \x1b[38;2;1;2;3mtruecolor\x1b[0m \x1b[41mansi-bg\x1b[0m \x1b[48;2;4;5;6mrgb-bg\x1b[0m \x1b[?2026hframe\x1b[?2026l";
        let result = filter.process(input);

        assert_eq!(result.display_bytes, input);
        assert!(result.terminal_replies.is_empty());
    }

    #[test]
    fn answers_split_codex_terminal_probes_without_exposing_queries() {
        let mut filter = TerminalOutputFilter::default();
        let first = filter.process(b"before\x1b[6");
        let second = filter.process(b"n\x1b]10;?\x1b\\\x1b]11");
        let third = filter.process(b";?\x07\x1b[?u\x1b[cafter");

        assert_eq!(
            [
                first.display_bytes,
                second.display_bytes,
                third.display_bytes
            ]
            .concat(),
            b"beforeafter"
        );
        assert_eq!(
            [
                first.terminal_replies,
                second.terminal_replies,
                third.terminal_replies
            ]
            .concat(),
            [
                TerminalReplyRequest::CursorPosition,
                TerminalReplyRequest::DefaultForeground,
                TerminalReplyRequest::DefaultBackground,
                TerminalReplyRequest::PrimaryDeviceAttributes,
            ]
        );
        assert_eq!(
            TerminalReplyRequest::CursorPosition.encode((4, 9)),
            b"\x1b[5;10R"
        );
        assert_eq!(
            TerminalReplyRequest::DefaultForeground.encode((0, 0)),
            b"\x1b]10;rgb:e8e8/eaea/eded\x1b\\"
        );
        assert_eq!(
            TerminalReplyRequest::DefaultBackground.encode((0, 0)),
            b"\x1b]11;rgb:0d0d/0f0f/1010\x1b\\"
        );
        assert_eq!(
            TerminalReplyRequest::PrimaryDeviceAttributes.encode((0, 0)),
            b"\x1b[?1;2c"
        );
    }

    #[test]
    fn oversized_or_malformed_osc_never_generates_a_terminal_reply() {
        let mut filter = TerminalOutputFilter::default();
        let mut input = b"\x1b]10;?".to_vec();
        input.extend(std::iter::repeat_n(b'x', MAX_OSC_BYTES));
        input.push(0x07);
        input.extend_from_slice(b"safe\x1b]10;not-a-query\x1b\\tail");
        let result = filter.process(&input);

        assert_eq!(result.display_bytes, b"safetail");
        assert!(result.terminal_replies.is_empty());
    }

    #[test]
    fn preserves_utf8_continuations_that_overlap_c1_osc_across_chunks() {
        let mut filter = TerminalOutputFilter::default();
        let first = filter.process(&[0xe4, 0xb9]);
        let second = filter.process(&[0x9d, b'-', b'o', b'k']);

        assert_eq!(
            [first.display_bytes, second.display_bytes].concat(),
            "九-ok".as_bytes()
        );
        assert!(first.terminal_replies.is_empty());
        assert!(second.terminal_replies.is_empty());
    }

    #[test]
    fn oversized_csi_is_discarded_and_cannot_smuggle_an_osc_sequence() {
        let mut filter = TerminalOutputFilter::default();
        let mut input = b"before\x1b[".to_vec();
        input.extend(std::iter::repeat_n(b'1', MAX_CSI_BYTES));
        input.extend_from_slice(b"\x1b]52;c;secret\x07after");
        let result = filter.process(&input);

        assert_eq!(result.display_bytes, b"beforeafter");
        assert!(result.terminal_replies.is_empty());
    }

    #[test]
    fn checkpoint_preserves_codex_visual_attributes_and_input_modes() {
        let mut journal = TerminalJournal::new(4, 20, 1, Duration::from_secs(60));
        journal.append(
            b"\x1b[1mB\x1b[0m\x1b[2;3mD\x1b[0m\x1b[36mC\x1b[0m\x1b[38;2;1;2;3mT\x1b[0m\x1b[41mA\x1b[0m\x1b[48;2;4;5;6mR\x1b[0m\x1b[2m\xe2\x94\x80\x1b[0m\x1b[?1h\x1b[?2004h"
                .to_vec(),
        );
        let snapshot = journal.snapshot(None);
        let mut restored = vt100::Parser::new(4, 20, 0);
        restored.process(&snapshot.screen);
        let screen = restored.screen();

        assert!(screen.cell(0, 0).is_some_and(vt100::Cell::bold));
        assert!(screen.cell(0, 1).is_some_and(vt100::Cell::dim));
        assert!(screen.cell(0, 1).is_some_and(vt100::Cell::italic));
        assert_eq!(
            screen.cell(0, 2).map(vt100::Cell::fgcolor),
            Some(vt100::Color::Idx(6))
        );
        assert_eq!(
            screen.cell(0, 3).map(vt100::Cell::fgcolor),
            Some(vt100::Color::Rgb(1, 2, 3))
        );
        assert_eq!(
            screen.cell(0, 4).map(vt100::Cell::bgcolor),
            Some(vt100::Color::Idx(1))
        );
        assert_eq!(
            screen.cell(0, 5).map(vt100::Cell::bgcolor),
            Some(vt100::Color::Rgb(4, 5, 6))
        );
        assert!(screen.cell(0, 6).is_some_and(vt100::Cell::dim));
        assert!(screen.application_cursor());
        assert!(screen.bracketed_paste());
    }

    #[test]
    fn alternate_screen_checkpoint_is_explicitly_incomplete() {
        let mut journal = TerminalJournal::new(4, 20, 1, Duration::from_secs(60));
        journal.append(b"\x1b[?1049halt screen".to_vec());

        assert!(!journal.snapshot(None).complete);
    }

    #[test]
    fn unicode_wide_cells_and_long_lines_remain_valid_after_checkpoint_compaction() {
        let mut journal = TerminalJournal::new(4, 20, 32, Duration::from_secs(60));
        journal.append("中文🙂".repeat(64).into_bytes());
        journal.append(b"\r\nfinal".to_vec());
        let snapshot = journal.snapshot(None);
        assert!(!snapshot.complete);
        assert!(snapshot.truncated);
        assert!(std::str::from_utf8(&snapshot.screen).is_ok());
        assert!(std::str::from_utf8(&snapshot.replay).is_ok());
        assert!(String::from_utf8_lossy(&snapshot.replay).contains("final"));
        assert_eq!(snapshot.from_seq, snapshot.checkpoint_seq + 1);
    }
}
