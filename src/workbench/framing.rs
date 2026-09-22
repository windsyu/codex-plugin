//! Incremental framing for transient observation copies, never for forwarding.
//! Payloads remain untrusted and unredacted. Only the decoder may consume them.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramingIssue {
    TooLarge,
    Incomplete,
    InvalidWebSocket,
    UnsupportedExtension,
    ObservationGap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Text,
    Binary,
}

// Do not add Debug/Serialize: this data has not passed through redaction.
pub struct WireMessage {
    pub kind: MessageKind,
    pub bytes: Vec<u8>,
}

pub type FrameResult = Result<WireMessage, FramingIssue>;

pub struct SseFramer {
    limit: usize,
    line: Vec<u8>,
    data: Vec<u8>,
    has_data: bool,
    discard_event: bool,
    discard_line: bool,
    after_cr: bool,
    first_line: bool,
}

impl SseFramer {
    pub fn buffered_bytes(&self) -> usize {
        self.line.capacity() + self.data.capacity()
    }

    pub fn new(limit: usize) -> Self {
        assert!(limit > 0);
        Self {
            limit,
            line: Vec::new(),
            data: Vec::new(),
            has_data: false,
            discard_event: false,
            discard_line: false,
            after_cr: false,
            first_line: true,
        }
    }

    pub fn feed(&mut self, bytes: &[u8], mut emit: impl FnMut(FrameResult)) {
        for &byte in bytes {
            if self.after_cr {
                self.after_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            if matches!(byte, b'\r' | b'\n') {
                self.end_line(&mut emit);
                self.after_cr = byte == b'\r';
            } else if !self.discard_line {
                if self.line.len() >= self.limit {
                    self.line.clear();
                    self.data.clear();
                    self.discard_line = true;
                    if !self.discard_event {
                        emit(Err(FramingIssue::TooLarge));
                    }
                    self.discard_event = true;
                } else {
                    self.line.push(byte);
                }
            }
        }
    }

    fn end_line(&mut self, emit: &mut impl FnMut(FrameResult)) {
        if self.discard_line {
            self.discard_line = false;
            self.line.clear();
            return;
        }
        let line = std::mem::take(&mut self.line);
        let line = if std::mem::replace(&mut self.first_line, false) {
            line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&line)
        } else {
            &line
        };
        if line.is_empty() {
            if !self.discard_event && self.has_data {
                emit(Ok(WireMessage {
                    kind: MessageKind::Text,
                    bytes: std::mem::take(&mut self.data),
                }));
            }
            self.data.clear();
            self.has_data = false;
            self.discard_event = false;
        } else if !self.discard_event {
            let value = if line == b"data" {
                Some(&b""[..])
            } else {
                line.strip_prefix(b"data:")
                    .map(|value| value.strip_prefix(b" ").unwrap_or(value))
            };
            if let Some(value) = value {
                let separator = usize::from(self.has_data);
                if self.data.len() + value.len() + separator > self.limit {
                    self.data.clear();
                    self.discard_event = true;
                    emit(Err(FramingIssue::TooLarge));
                } else {
                    if self.has_data {
                        self.data.push(b'\n');
                    }
                    self.data.extend_from_slice(value);
                    self.has_data = true;
                }
            }
        }
    }

    pub fn gap(&mut self) {
        self.line.clear();
        self.data.clear();
        self.has_data = false;
        self.discard_event = true;
        // The first newline may end a lost nonempty line. Skip it before
        // looking for the next fully observed event boundary.
        self.discard_line = true;
        self.after_cr = false;
        self.first_line = false;
    }

    pub fn finish(&mut self) -> Option<FramingIssue> {
        let incomplete = self.has_data || !self.line.is_empty() || self.discard_event;
        self.line.clear();
        self.data.clear();
        self.has_data = false;
        incomplete.then_some(FramingIssue::Incomplete)
    }
}

struct WsFrame {
    fin: bool,
    opcode: u8,
    mask: Option<[u8; 4]>,
    length: usize,
    read: usize,
}

pub struct WebSocketFramer {
    limit: usize,
    expect_mask: bool,
    header: Vec<u8>,
    frame: Option<WsFrame>,
    message: Vec<u8>,
    fragment_kind: Option<MessageKind>,
    unavailable: bool,
    closed: bool,
}

impl WebSocketFramer {
    pub fn buffered_bytes(&self) -> usize {
        self.header.capacity() + self.message.capacity()
    }

    pub fn new(limit: usize, expect_mask: bool) -> Self {
        assert!(limit > 0);
        Self {
            limit,
            expect_mask,
            header: Vec::with_capacity(14),
            frame: None,
            message: Vec::new(),
            fragment_kind: None,
            unavailable: false,
            closed: false,
        }
    }

    pub fn gap(&mut self) {
        // Arbitrary TCP byte loss destroys frame alignment; never guess a new
        // boundary from payload bytes that happen to resemble a frame header.
        self.unavailable = true;
        self.header.clear();
        self.message.clear();
        self.frame = None;
        self.fragment_kind = None;
    }

    pub fn feed(&mut self, bytes: &[u8], mut emit: impl FnMut(FrameResult)) {
        if self.unavailable {
            return;
        }
        for &byte in bytes {
            if self.closed {
                self.gap();
                emit(Err(FramingIssue::InvalidWebSocket));
                return;
            }
            if let Some(frame) = self.frame.as_mut() {
                if frame.opcode < 8 {
                    self.message
                        .push(byte ^ frame.mask.map_or(0, |mask| mask[frame.read % 4]));
                }
                frame.read += 1;
                if frame.read == frame.length {
                    self.end_frame(&mut emit);
                }
            } else {
                self.header.push(byte);
                match self.try_header() {
                    Ok(Some(frame)) => {
                        let empty = frame.length == 0;
                        self.frame = Some(frame);
                        self.header.clear();
                        if empty {
                            self.end_frame(&mut emit);
                        }
                    }
                    Ok(None) => {}
                    Err(issue) => {
                        self.gap();
                        emit(Err(issue));
                        return;
                    }
                }
            }
        }
    }

    fn try_header(&mut self) -> Result<Option<WsFrame>, FramingIssue> {
        if self.header.len() < 2 {
            return Ok(None);
        }
        let fin = self.header[0] & 0x80 != 0;
        let opcode = self.header[0] & 0x0f;
        let masked = self.header[1] & 0x80 != 0;
        let short_length = self.header[1] & 0x7f;
        let extended = match short_length {
            126 => 2,
            127 => 8,
            _ => 0,
        };
        let needed = 2 + extended + if masked { 4 } else { 0 };
        if self.header.len() < needed {
            return Ok(None);
        }
        if self.header[0] & 0x70 != 0 {
            return Err(FramingIssue::UnsupportedExtension);
        }
        if masked != self.expect_mask || !matches!(opcode, 0 | 1 | 2 | 8 | 9 | 10) {
            return Err(FramingIssue::InvalidWebSocket);
        }
        let length = match extended {
            2 => u16::from_be_bytes(self.header[2..4].try_into().unwrap()) as u64,
            8 => u64::from_be_bytes(self.header[2..10].try_into().unwrap()),
            _ => short_length as u64,
        };
        if (extended == 2 && length < 126)
            || (extended == 8 && (length < 65536 || length & (1 << 63) != 0))
            || (opcode >= 8 && (!fin || length > 125))
            || (opcode == 8 && length == 1)
            || (opcode == 0 && self.fragment_kind.is_none())
            || (matches!(opcode, 1 | 2) && self.fragment_kind.is_some())
        {
            return Err(FramingIssue::InvalidWebSocket);
        }
        if opcode < 8 && length > self.limit.saturating_sub(self.message.len()) as u64 {
            return Err(FramingIssue::TooLarge);
        }
        if matches!(opcode, 1 | 2) {
            self.fragment_kind = Some(if opcode == 1 {
                MessageKind::Text
            } else {
                MessageKind::Binary
            });
        }
        Ok(Some(WsFrame {
            fin,
            opcode,
            mask: masked.then(|| self.header[2 + extended..needed].try_into().unwrap()),
            length: length as usize,
            read: 0,
        }))
    }

    fn end_frame(&mut self, emit: &mut impl FnMut(FrameResult)) {
        let frame = self.frame.take().unwrap();
        if frame.opcode < 8 && frame.fin {
            emit(Ok(WireMessage {
                kind: self.fragment_kind.take().unwrap(),
                bytes: std::mem::take(&mut self.message),
            }));
        } else if frame.opcode == 8 {
            self.closed = true;
            if self.fragment_kind.take().is_some() {
                self.message.clear();
                emit(Err(FramingIssue::Incomplete));
            }
        }
    }

    pub fn finish(&mut self) -> Option<FramingIssue> {
        let incomplete = !self.unavailable
            && (self.frame.is_some() || !self.header.is_empty() || self.fragment_kind.is_some());
        self.gap();
        incomplete.then_some(FramingIssue::Incomplete)
    }
}

#[cfg(test)]
mod tests;
