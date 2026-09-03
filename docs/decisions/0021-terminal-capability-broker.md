# ADR 0021: Terminal capability broker and native Codex rendering

## Context

The Session Kernel already forwards PTY output to xterm, so Codex SGR attributes were not being converted into React text. Native rendering was nevertheless incomplete because the Codex TUI starts before a Browser attachment and performs a 100 ms terminal probe for cursor position, foreground/background colors, keyboard enhancement and primary device attributes. The previous blanket OSC sanitizer discarded the color queries, while the Browser could not answer them in time. Snapshot restoration also called `terminal.reset()` outside xterm's asynchronous write queue, allowing an older write to complete after the reset.

Codex `0.146.1` renders reasoning summaries as dim and italic, uses semantic bold for tools and Markdown, and emits dim horizontal separators only after concrete work. The Browser must preserve those bytes, not infer transcript roles or add its own formatting.

## Decision

Adopt a server-side terminal capability broker and a shared versioned dark terminal profile.

- The PTY filter returns separate `display_bytes` and `terminal_replies`.
- Normal CSI/DEC/SGR output is byte-preserved. Exact `OSC 10/11` and `CSI 6n` probes are answered from server-owned VT/profile state; `CSI ?u` is consumed and the following primary-DA reply makes xterm's lack of enhanced-keyboard support explicit to Codex.
- Probe replies bypass InputLease only because they are generated from an allowlist match against PTY output. Browser input can never enter this path.
- OSC links, clipboard, titles, file transfer, oversized and malformed control strings remain stripped without reply.
- Rust and Browser read `codex-dark-v1`; TrueColor is not rewritten.
- Checkpoints use `vt100::Screen::state_formatted()`. Alternate-screen or unavailable scrollback recovery remains explicitly partial.
- Browser rendering uses xterm 6, Fit 0.11 and Unicode 11 with 400/700 font weights and native dim/bold-bright behavior.
- Live output and snapshot restore share one serialized write coordinator. Restore is an in-band `CAN + RIS + checkpoint + replay` write and supersedes queued stale output.
- The terminal shell contains only connection/InputLease state and the return-to-bottom action. It does not add semantic legends, separators or final-answer emphasis.

The `codex-terminal-v1` wire format and all `/v1` and `/v2` HTTP contracts remain unchanged.

## Alternatives

1. Let the Browser answer terminal probes. Rejected because Codex may complete its startup probe before any Browser attaches, and Browser-originated reply bytes would cross the InputLease/security boundary.
2. Keep stripping every OSC and hard-code equivalent CSS. Rejected because Codex itself uses the probed foreground/background to select terminal-native presentation.
3. Parse terminal text into reasoning/tool/final roles and render React components. Rejected because it duplicates Codex TUI semantics, breaks on protocol/UI evolution, and turns an untrusted transcript into a control boundary.
4. Call `terminal.reset()` before writing a snapshot. Rejected because xterm's private asynchronous write buffer can still apply older bytes afterward.

## Consequences

- Codex owns bold, dim, italic, color and conditional separators end to end.
- The Gateway gains a small terminal-emulation responsibility, covered by exact-query and malformed-input tests.
- A fixed dark profile is the V2 compatibility baseline; light/custom profiles require a later versioned extension.
- Snapshot completeness remains honest: current screen state can be restored, but alternate-screen history and discarded scrollback are not claimed as complete.
- No database migration or V3/IM capability is introduced.

## Status

Accepted

## Date

2026-09-03
