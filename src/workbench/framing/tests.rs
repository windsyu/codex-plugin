use super::*;

#[test]
fn sse_every_possible_split_preserves_utf8_crlf_multiline_bom_and_comments() {
    let input = "\u{feff}: heartbeat\r\nevent: delta\r\ndata: {\r\ndata:  \"text\":\"中文\"}\r\n\r\ndata: [DONE]\n\n";
    for split in 0..=input.len() {
        let mut framer = SseFramer::new(256);
        let mut messages = Vec::new();
        framer.feed(&input.as_bytes()[..split], |frame| {
            messages.push(frame.unwrap().bytes)
        });
        framer.feed(&input.as_bytes()[split..], |frame| {
            messages.push(frame.unwrap().bytes)
        });
        assert_eq!(
            messages,
            vec!["{\n \"text\":\"中文\"}".as_bytes(), b"[DONE]"]
        );
        assert_eq!(framer.finish(), None);
    }
}

#[test]
fn sse_bounds_both_lines_and_multiline_events_then_recovers_at_a_complete_boundary() {
    for oversized in [
        b"data: 12345678901234567890\n\n".as_slice(),
        b"data: 123456\ndata: 123456\ndata: 123456\n\n",
    ] {
        let mut framer = SseFramer::new(16);
        let mut issues = Vec::new();
        let mut messages = Vec::new();
        for byte in oversized.iter().chain(b"data: ok\n\n") {
            framer.feed(&[*byte], |frame| match frame {
                Ok(message) => messages.push(message.bytes),
                Err(issue) => issues.push(issue),
            });
            assert!(framer.line.len() <= 16);
            assert!(framer.data.len() <= 16);
        }
        assert_eq!(issues, vec![FramingIssue::TooLarge]);
        assert_eq!(messages, vec![b"ok".to_vec()]);
    }
}

#[test]
fn sse_gap_discards_partial_event_instead_of_joining_missing_json() {
    let mut framer = SseFramer::new(128);
    let mut messages = Vec::new();
    framer.feed(b"data: {\"text\":\"part", |_| panic!("premature event"));
    framer.gap();
    framer.feed(
        b"\r\ndata: {\"fake\":true}\r\n\r\ndata: {\"next\":true}\r\n\r\n",
        |frame| messages.push(frame.unwrap().bytes),
    );
    assert_eq!(messages, vec![br#"{"next":true}"#.to_vec()]);
    framer.feed(b"data: {\"unterminated\":true}", |_| {
        panic!("premature event")
    });
    assert_eq!(framer.finish(), Some(FramingIssue::Incomplete));
}

fn frame(opcode: u8, fin: bool, payload: &[u8], mask: bool) -> Vec<u8> {
    let mut bytes = vec![opcode | if fin { 0x80 } else { 0 }];
    let length = payload.len();
    let flag = if mask { 0x80 } else { 0 };
    if length < 126 {
        bytes.push(flag | length as u8);
    } else if length <= u16::MAX as usize {
        bytes.push(flag | 126);
        bytes.extend_from_slice(&(length as u16).to_be_bytes());
    } else {
        bytes.push(flag | 127);
        bytes.extend_from_slice(&(length as u64).to_be_bytes());
    }
    let key = [1, 2, 3, 4];
    if mask {
        bytes.extend_from_slice(&key);
    }
    bytes.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ if mask { key[index % 4] } else { 0 }),
    );
    bytes
}

#[test]
fn websocket_all_byte_boundaries_handle_mask_fragments_control_and_multiple_messages() {
    for masked in [false, true] {
        let text = "中文".as_bytes();
        let mut bytes = frame(1, false, &text[..1], masked);
        bytes.extend(frame(9, true, b"ping", masked));
        bytes.extend(frame(0, true, &text[1..], masked));
        bytes.extend(frame(2, true, &[0, 255], masked));
        bytes.extend(frame(1, true, b"", masked));
        bytes.extend(frame(8, true, &[0x03, 0xe8], masked));
        for split in 0..=bytes.len() {
            let mut framer = WebSocketFramer::new(1024, masked);
            let mut messages = Vec::new();
            framer.feed(&bytes[..split], |message| messages.push(message.unwrap()));
            framer.feed(&bytes[split..], |message| messages.push(message.unwrap()));
            assert_eq!(messages.len(), 3);
            assert_eq!(messages[0].bytes, text);
            assert_eq!(messages[0].kind, MessageKind::Text);
            assert_eq!(messages[1].bytes, vec![0, 255]);
            assert_eq!(messages[1].kind, MessageKind::Binary);
            assert!(messages[2].bytes.is_empty());
            assert_eq!(framer.finish(), None);
        }
    }
}

#[test]
fn websocket_extended_lengths_and_bounds_are_checked_before_payload_allocation() {
    for length in [126, 65536] {
        let payload = vec![b'x'; length];
        let bytes = frame(1, true, &payload, true);
        let mut framer = WebSocketFramer::new(length, true);
        let mut messages = Vec::new();
        for chunk in bytes.chunks(17) {
            framer.feed(chunk, |message| messages.push(message.unwrap().bytes));
        }
        assert_eq!(messages, vec![payload]);
        let mut bounded = WebSocketFramer::new(125, true);
        let mut issues = Vec::new();
        bounded.feed(&bytes, |message| issues.push(message.err().unwrap()));
        assert_eq!(issues, vec![FramingIssue::TooLarge]);
        assert_eq!(bounded.message.capacity(), 0);
    }
}

#[test]
fn websocket_gaps_extensions_and_invalid_frames_never_produce_plausible_text() {
    let mut framer = WebSocketFramer::new(128, false);
    framer.feed(&[0x81, 12, b'{'], |_| panic!("premature message"));
    framer.gap();
    framer.feed(&frame(1, true, b"looks valid", false), |_| {
        panic!("guessed frame alignment")
    });
    for (bytes, expected) in [
        (vec![0xc1, 0], FramingIssue::UnsupportedExtension),
        (vec![0x80, 0], FramingIssue::InvalidWebSocket),
        (vec![0x09, 0], FramingIssue::InvalidWebSocket),
        (vec![0x81, 126, 0, 1], FramingIssue::InvalidWebSocket),
        (vec![0x88, 1], FramingIssue::InvalidWebSocket),
    ] {
        let mut framer = WebSocketFramer::new(128, false);
        let mut issues = Vec::new();
        framer.feed(&bytes, |message| issues.push(message.err().unwrap()));
        assert_eq!(issues, vec![expected]);
    }
}
