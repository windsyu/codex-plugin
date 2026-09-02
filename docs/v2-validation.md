# V2.0 Validation Report

> Date: 2026-09-02
> Version: `v0.2.0`
> Result: Passed with the documented compatibility limits below

## Scope

This report validates the first local V2 Control Plane release against [`codex-local-gateway-v2-development-constraints.md`](codex-local-gateway-v2-development-constraints.md). V1 remains the store-first, read-only baseline. V2 mutation remains default-off, uses the existing bearer/Cookie/verified Tailscale identity, binds every command to an exact live source epoch, and exposes only closed typed capabilities under `/v2`.

## Automated gates

| Gate | Command / evidence | Result |
| --- | --- | --- |
| Rust unit/integration | `cargo test --all-targets --all-features` | 211 passed; installed-Codex manual smoke and explicit 2M-event capacity test ignored; 0 failed |
| Rust lint | `cargo clippy --all-targets --all-features -- -D warnings` | Passed |
| Release build | `cargo build --release --all-features` | Passed; `codex-observerd 0.2.0` |
| Web type check | `npm exec tsc -- --noEmit` | Passed |
| Web unit | `npm test` | 65 passed; 0 failed |
| Web production build | `npm run build` | Passed |
| Browser E2E | `PLAYWRIGHT_USE_SYSTEM_CHROME=1 npm run test:e2e` | 18 system-Chrome tests passed |
| Patch hygiene | `git diff --check` | Passed |

The ignored Rust tests are `writer::tests::two_million_event_capacity_path`, which is intentionally opt-in through `OBSERVER_RUN_CAPACITY=1`, and the installed-Codex smoke, which is run separately with an explicit executable. The ordinary bounded-writer, fan-out, scale-query, replay, backpressure and synthetic App Server tests remain enabled and passed.

## Protocol and control validation

- Codex CLI compatibility evidence covers the generated `0.149.1` Controller schema baseline and the installed `0.146.1` Session Kernel/App Server acceptance baseline.
- Schema evidence: experimental and experimental-v2 bundles generated from that binary; hashes are recorded in [`codex-0.149.1-v2.json`](../compatibility/codex-0.149.1-v2.json).
- Synthetic JSONL fixtures cover initialize/catalog, new/resume/fork, start/steer/interrupt, settings, Plan, Goal, rename/archive/compact/review, approvals, permissions, user questions, and MCP elicitation.
- Transport tests use actual WebSocket framing over in-memory duplex I/O and the production command writer. They verify correlation with interleaved notifications, raw/projection commit before command progress, exact epoch, terminal Turn reconciliation, upstream rejection, timeout, disconnect, and no replay after `outcome_unknown`.
- Unknown or rejected protocol methods are retained as redacted Observer events and disable only the unverified capability; there is no generic JSON-RPC passthrough.

## Security and failure validation

- Controller configuration defaults to `false`; invalid Controller configurations without strict Origin or a configured socket fail validation.
- With Controller disabled, the release server returned `404` for `POST /v2/commands`; `/v1/health` remained healthy and read-only.
- Bearer, pairing Cookie, and verified Tailscale principals share one authentication boundary. Unauthenticated, forged Tailscale, invalid Host/forwarding, invalid Origin, stale epoch, stale Turn, and unavailable capability paths fail closed. New-Thread cwd tests cover canonicalization plus relative, missing, and non-directory rejection.
- Health/config/UI explicitly report that enabling both Tailscale Serve and Controller gives verified Tailnet users the same V2 mutation authority as local sessions.
- Command, transition, and append-only audit updates are transactional. Audits retain principal/source/epoch/command/outcome but exclude full messages, Goal objectives, answers, image bytes, MCP content, secrets, and private upstream errors.
- Restart recovery closes stale App Server epochs, verifies current state against the append-only transition ledger, marks pre-write commands failed, marks possibly written commands `outcome_unknown`, and never dispatches or replays them. A second recovery is idempotent.
- Pending-request action tests cover signed request keys, request-version CAS, two-client competition, resolved/version-drift/stale-epoch failures, original JSON-RPC ID types, and timeout after response write.
- Image tests cover PNG/JPEG/WebP/GIF signatures, forged MIME, SVG, 20 MiB per image, four images/50 MiB per message, idempotency conflict, principal isolation, duplicate IDs, expiry, tampering, symlink, path traversal, `0600`/`0700`, typed `LocalImage`, pre-dispatch cleanup, Turn-terminal cleanup, and startup orphan sweep.
- SSE tests cover Authorization headers, no bearer in URLs, retention floor, signed composite cursor replay, fragmented parsing, reconnect, and expired-cursor recovery.

## UI and release E2E

Web unit tests validate the Plan/Goal controls, persistent Goal state line, keyboard-first Slash palette, `/clear` current-session setting reuse, IME-safe Enter handling, Shift/Alt+Enter newlines, Esc popup/interrupt precedence, in-memory prompt history, textarea autosizing, clipboard image type filtering and four-image cap, failed-draft retention, individual image removal, latest-message follow threshold and return action, current-session presence states, pending-request card variants, offline disablement, optimistic send state, `clientUserMessageId` projection reconciliation, and `outcome_unknown` rendering. Playwright validates the V1 Viewer regression and the browser-level V2 controls together: project/recent navigation, search, safe raw inspection, responsive bounds, pairing, new exact-epoch Thread creation, `/clear` new-Thread dispatch and automatic switch, catalog-gated model/reasoning/permission settings, Plan mode, active-Turn stop/immediate-steer actions without a local queue, prompt recall, Esc interrupt with draft retention, cross-Thread draft isolation, Composer text/image send, optimistic display followed by SSE-driven projection reconciliation, exact approval target/action, `outcome_unknown`, and SSE cursor reconnect. A browser-level visual pass additionally checked the sticky Composer, current-session status strip, return-to-latest affordance, and 390 px no-overflow layout against local `cc-viewer` commit `5352005402fd`.

The Source Actor regression suite additionally verifies that a Default reasoning effort is snapshotted before entering Plan, the Plan preset can temporarily override it, and `/plan off` sends the original Default effort instead of serializing an unspecified preset field as `null`.

A fresh temporary release run validated:

- synthetic fixture import: 3 files scanned, 21 events inserted, 0 decode errors, 0 degraded sources;
- `doctor`: healthy, SQLite schema 15, quick check OK, private database/fingerprint-key permissions;
- authenticated release `/v1/health`: healthy, projection lag 0, Controller disabled;
- disabled mutation boundary: `POST /v2/commands` returned `404`.

### Real local App Server E2E (2026-08-31 and 2026-09-01)

An additional manual browser E2E used an already-running official local App Server over its Unix socket. The Gateway did not start, stop, or modify the App Server process. The run used an isolated temporary Gateway database and a dedicated test Thread; no real identifiers, pairing material, message bodies, rollout files, or uploaded image bytes are committed to this repository.

The run verified:

- exact-source connection, new Thread creation with an absolute cwd, resume behavior, and multi-turn text replies;
- model/reasoning/permission catalog projection, a reversible reasoning update, Plan enter/exit, Goal read/set/clear with visible persistent status, `/clear` new-Thread creation and automatic switch, and local `/status` and `/mcp` status cards;
- viewport-level control feedback placement: `/status` refresh feedback remained visible directly above the sticky Composer instead of appearing at the Thread top, with the Composer unobscured and the close action available;
- missing terminal-notification reconciliation through stable `thread/read`, including command completion and Composer recovery;
- interrupt from an active Turn, cancellation of the originating command, and durable interrupted-Turn completeness;
- PNG staging, typed `LocalImage` dispatch, successful model image reading, terminal cleanup, safe attachment rendering, and local-path redaction;
- Gateway restart with source epoch rotation, rejection and audit of a stale-epoch mutation before upstream dispatch, Thread reload, and a successful reply on the new epoch;
- browser projection reconciliation without optimistic, App Server, `event_msg`, or `response_item` duplicate dialogue messages.

No real rollout, real uploaded image, bearer token, pairing code, user message, or local database is stored in the repository.

## Session Kernel Slice 1–3 preview validation (2026-09-01)

This is a default-off preview validation layered on the completed `v0.2.0` Controller baseline above. It proves the Session Kernel boundary, fixed fake CLI PTY Worker, and Browser xterm transport; it does **not** claim Slice 4 private App Server proxy, persistent ThreadLease, real Codex TUI start/resume, or Slice 5+ ownership/recovery is complete.

| Gate | Command / evidence | Result |
| --- | --- | --- |
| Rust unit/integration | `cargo test` | 163 passed; 1 explicit 2M-event capacity test ignored; 0 failed |
| Session-focused Rust | `cargo test session` | 21 passed; 0 failed |
| Rust format | `cargo fmt --all -- --check` | Passed |
| Rust lint | `cargo clippy --all-targets -- -D warnings` | Passed |
| Rust release build | `cargo build --release` | Passed |
| Web type check | `npm exec tsc -- --noEmit` | Passed |
| Web unit | `npm test -- --run` | 64 passed; 0 failed |
| Web production build | `npm run build` | Passed |
| Browser E2E | `PLAYWRIGHT_USE_SYSTEM_CHROME=1 npm run test:e2e` | 15 Chrome tests passed; 2 cover xterm input, conflict-read-only, refresh reattach, control-token resume, and 390 px layout |
| Patch hygiene | `git diff --check` | Passed |

Slice 1 validation covers default `off`, invalid mode/config combinations, the explicit fixed-fixture-only preview exception, capability output without executable paths, `/v1` read-only compatibility, and module dependency direction. The fake-only fixture does not configure an App Server socket, so it starts no Legacy source reconnect loop.

Slice 2 validation covers canonical executable/cwd, no-shell argv, environment allowlist, private runtime directory and marker permissions, readiness/early-exit classification, PTY resize, EOF/exit merge, process-group `SIGTERM`→`SIGKILL`, priority stop, bounded command/output queues, 100 create/stop cycles without process/runtime leaks, and a 10 MiB output stress path that remains bounded without blocking the async HTTP runtime.

Slice 3 validation covers the frozen `0x01 | outputSeq:u64 big-endian | PTY bytes` output frame, deny-unknown client JSON, Base64 snapshot frames, 2 MiB/5-minute journal, `vt100` checkpoint recovery, 64-attachment cap and expiry cleanup, 30-second one-use WebSocket descriptors, five-second detach grace, versioned single-winner InputLease, active takeover rejection, slow-consumer isolation, and streaming stripping of 7-bit/8-bit OSC clipboard/link/title/file sequences. Attachment resume/acquire/release requires a principal-bound high-entropy control token; the Actor retains only its BLAKE3 hash. Tests reject missing/forged tokens even when the owner attachment ID is visible, and public spawn errors exclude local paths and underlying error detail.

Final live Browser validation found and repaired two terminal wire regressions before Slice 4 work began: enum struct-variant fields now deserialize the frozen Browser camelCase names (`leaseId`, `outputSeq`, and `afterSeq`), and standard WebSocket Pong frames are treated as transport control rather than invalid typed client data. Exact contract tests reject the accidental snake_case shape and retain the binary-frame fail-closed boundary. The final release stayed connected beyond a heartbeat interval, accepted input before and after the heartbeat, restored the same Worker and terminal screen after refresh, and continued to accept input.

The same validation found that xterm could overlap or push the stop control outside its hit target at 390 px. Session Worker content now has an explicit bounded flex container with zero intrinsic minimum width, and the narrow-screen Playwright path asserts that the stop button center resolves to the button before clicking it. The final release reported a 390 px document width, hit the actual `BUTTON`, entered `STOPPING`, reached the API `exited` state, and removed the fake process and private Worker runtime directory.

A final release-binary fixture smoke used the fixed synthetic CLI and an isolated repository-local runtime. Health reported `preview` and fake CLI availability without `cliPath`; the Worker reached `ready`; attach returned a 64-character control token with `Cache-Control: no-store` and an HttpOnly pairing Cookie; a forged lease returned `401 ATTACHMENT_TOKEN_INVALID`; the valid token reached the distinct `ATTACHMENT_NOT_CONNECTED` guard before WebSocket attach; stop entered `stopping`. The fixture process and Worker were stopped after the run, and no private Codex data was read or written.

## Session Kernel Slice 4–9 validation (2026-09-02)

This validation supersedes the Slice 1–3 implementation-status limits above without rewriting that historical evidence. `controller.session_kernel="tui"` is the real Session Kernel path; `off` remains the default and `preview` remains the fixed fake-fixture rollback path. Slice 10–12 are V3 and were not implemented.

| Gate | Command / evidence | Result |
| --- | --- | --- |
| Rust unit/integration | `cargo test --all-targets --all-features` | 211 passed; installed-Codex manual smoke and explicit 2M-event capacity test ignored; 0 failed |
| Installed Codex TUI | `SESSION_REAL_CODEX_CLI=/opt/homebrew/bin/codex cargo test installed_codex_tui_new_and_resume_connect_through_private_session_proxy -- --ignored --nocapture` | Passed with `codex-cli 0.146.1`: new, native `/clear`, primary ThreadLease switch, resume |
| Rust format | `cargo fmt --all -- --check` | Passed |
| Rust lint | `cargo clippy --all-targets --all-features -- -D warnings` | Passed |
| Rust release build | `cargo build --release --all-features` | Passed; `codex-observerd 0.2.0` |
| Web type check | `npm exec tsc -- --noEmit` | Passed |
| Web unit | `npm test` | 65 passed; 0 failed |
| Web production build | `npm run build` | Passed |
| Browser E2E | `PLAYWRIGHT_USE_SYSTEM_CHROME=1 npm run test:e2e` | 18 system-Chrome tests passed |

Slice 4 verifies a private `0600` one-downstream/one-upstream Unix WebSocket proxy, and authorizes the downstream against the exact PTY child PID in addition to same-user socket credentials. Tests reject symlink parents, a same-user process with the wrong PID, and a second downstream. They also cover preserved TUI initialize identity, distinct TUI/Gateway request-ID namespaces, numeric/string/null callback IDs, fragmented messages, back-to-back burst backpressure, reverse-order responses, one monotonic sequence across both directions, worker connection epochs, recoverable unknown envelopes, raw-first forwarding and write-after-audit ordering. Unattributed potential mutations fail closed; owner-attributed unknown requests and notifications are audited; a TUI or upstream disconnect after write becomes `outcome_unknown`; one worker disconnect does not rotate or stop a peer worker connection.

Slice 5 and 7 add schema 16–18 and persist Worker, WorkerTransition, ThreadLease, InputLease, TurnOwner and connection-epoch state. Each migration has direct idempotency and late-failure rollback coverage. Tests cover new reservation upgrade, unique primary/side/child ownership, double create/resume races, `/clear`/fork/side/child lease-set changes, exact Turn ownership, safe stop/interrupt, source/worker epoch separation, restart orphan reconciliation, PID-reuse non-adoption, symlink/malformed-marker orphan safety and no automatic mutation replay. A dedicated reopen matrix covers crashes before raw commit, after raw commit, after audit dispatch, after socket write-complete and after response; only confirmed responses complete, while ambiguous dispatch/write states recover as `outcome_unknown`.

The installed `codex-cli 0.146.1` smoke uses a temporary `CODEX_HOME`, temporary cwd, private proxy and target-schema synthetic App Server. It does not read or modify the user's Codex data. The real TUI completed `new`, selected and submitted `/clear` through the native Slash palette, caused a second UUIDv7 Thread to become the active primary lease, then completed `resume`. Synthetic catalog responses cover the startup methods observed from that installed version (`account/read`, `hooks/list`, `skills/list`, `plugin/list`, `model/list`, `configRequirements/read`, `thread/read/list/start/resume`).

Slice 6 and 9 browser tests verify that `tui` mode makes SessionShell/xterm the default mutation route, hides Composer writes for session-owned Threads, preserves History/Raw Inspector as structured V1 reads, restores only an exact source/epoch/active-lease worker, keeps a second attachment read-only, and sends Slash/picker keys and interrupt only to the owned session. Browser-level coverage also sends IME-style Unicode and multiline paste through xterm, observes debounced resize, injects long CJK/emoji plus hostile OSC text without creating executable DOM, and recovers the same attachment control after a network disconnect. The legacy typed actor/Composer implementation remains during the documented rollback window, but a session-owned Thread returns `THREAD_OWNED_BY_SESSION` and cannot be double-written.

Slice 8 tests both terminal and simulated channel routing end to end for command/file/permission approval, user question and MCP elicitation; request-version CAS permits one upstream response. Unsupported channel requests preserve the complete callback and can be explicitly handed to terminal without creating a duplicate. Temporarily unattributed requests likewise remain withheld and unanswerable until an explicit terminal handoff, rather than losing their callback or guessing an owner. No V3 platform adapter or public arbitrary-protocol endpoint was added.

The freshly built release binary was also started on an isolated loopback port with a temporary database, `CODEX_HOME` and fixed fake PTY. Runtime health was `healthy` at schema 18; create replay was idempotent; Session SSE emitted `session_state`; attachment was `no-store`; a connected WebSocket acquired the versioned InputLease, sent typed input and observed `ECHO:release-smoke`; stop reached `exited`. The process was then shut down cleanly.

### Real existing App Server acceptance (2026-09-02)

The final acceptance used an operator-started official `codex-cli 0.146.1` App Server with `codex app-server --listen unix://`, the public Unix-listener form documented in [OpenAI Developer commands](https://learn.chatgpt.com/docs/developer-commands?surface=cli). This was an out-of-process test prerequisite: the Gateway connected to the existing endpoint but did not start, supervise, restart or stop it. The installed `codex remote-control start` path was unavailable because this CLI was not an installer-managed standalone installation. The Desktop app's private stdio App Server and a stale configured Unix socket therefore could not serve as the V2 source.

The Browser/xterm acceptance then verified all of the following against that real endpoint:

- a native TUI Worker reached `ready`, held one private 1:1 App Server connection and acquired exactly one primary ThreadLease;
- a real Turn returned the exact requested answer, while `session.create`, `tui.protocol.thread/start` and `tui.protocol.turn/start` each traversed `received → authorized → dispatching → accepted_by_source → completed` with a persisted pre-write audit boundary;
- the first 48 bidirectional envelopes had a gap-free unique proxy sequence (`1..48`) and zero decode failures;
- native `/clear` retained the Worker, released the old primary lease, acquired the new primary lease, and advanced the same connection sequence without a gap; duplicate `thread/unsubscribe` remained idempotent and created no synthetic side lease;
- returning to History expired/released the detached attachment and InputLease, while reopening xterm reused the same Worker with one new connected attachment and one active InputLease;
- stopping released all ThreadLeases, attachments and the InputLease. A live-found regression was repaired so the persisted Worker now ends as `exited` with no error code and the connection epoch ends as `closed` with `close_reason=worker_stopping` and its final non-zero proxy sequence, rather than misclassifying operator SIGTERM as `SESSION_WORKER_EXITED`/`connection_failed`;
- resume of the same Thread displayed its prior real TUI transcript, completed a second Turn, and recalled the code word supplied before the earlier Worker was stopped, proving durable Thread context continuity rather than UI-only replay;
- the History view showed both completed Turns and the exact final answer after the Worker stopped.

This live run also exposed that transient TUI→App Server JSON-RPC requests (`thread/read`, `thread/resume`, `thread/goal/get`, `turn/start`) were being projected as actionable pending server requests. Projection now admits only App Server→owner live requests. Migration 0020 removes already projected client requests by joining their raw event provenance and direction; the real database upgraded to schema 20, removed all four invalid rows, retained the raw envelopes, and the Viewer stopped showing a false pending-request count. Direction and cleanup regression tests are included in the 211-test Rust gate.

Migration 0019 was also exercised against the existing approximately 72,000-event database. Its stable FTS rowid rebuild removes the previous quadratic per-item delete path. A subsequent no-migration startup of the final debug binary opened the Viewer between 16 and 21 seconds after process launch (five-second observation resolution) while rescanning two local stores and 284 Threads. No release requirement currently defines a lower startup latency threshold.

The App Server emitted non-fatal local-environment warnings for remote plugin catalog authentication and an older models-cache shape (`base_instructions` missing). They did not interrupt initialize, new/resume, Turn, `/clear`, reconnect or stop. No real pairing material, raw private payload, local database or rollout file is committed to the repository.

## Compatibility and known limits

- The 2026-08-31 UI follow-up observed the configured official source checkout at `41ece455b7fa7166f4fc38522952afdaa2604e18`. The release protocol baseline remains the previously generated schema and fixture set from installed `codex-cli 0.149.1`; this UI-only follow-up did not retroactively rewrite the compatibility manifest's `sourceCommit: null` evidence.
- Automated release validation remains deterministic and fixture-based. The additional manual E2E above exercised a real local App Server only through a dedicated test Thread and retained no private test payloads in the repository.
- Tailscale behavior is covered by proxy/header/principal tests, not a live external Tailnet session.
- The explicit 2M-event capacity test was not run in this gate. It remains available as an opt-in extended-capacity test.
- App Server control is experimental and capability-gated. A schema/method mismatch disables the affected V2 capability while V1 store-first browsing remains available.
- Session Kernel Slice 1–9 state is persisted where correctness requires it. Gateway restart intentionally orphans rather than adopts an old TUI/PTY, freezes its leases, and requires explicit user start/resume; terminal journal and live attachments are not treated as durable history.
- Automated Session Kernel protocol validation uses a synthetic App Server plus the real installed TUI. Full live acceptance additionally used the operator-started compatible endpoint documented above; the Gateway still never starts or guards that process. The pre-existing configured `~/.codex/app-server-control/app-server-control.sock` had no owning process and rejected a Unix connect probe, so it was treated as stale and left untouched rather than being deleted or replaced.
- The macOS SDK lookup emitted a non-failing sandbox warning about `DARWIN_USER_TEMP_DIR`; all Rust test, lint, and release commands completed successfully.

## Migration and rollback

- Session Kernel Slice 1–3 adds no database migration. Migrations 0016–0018 add proxy provenance/connection epochs, persistent Worker/ThreadLease/InputLease state, and TurnOwner state. Migration 0019 rebuilds FTS with stable `items.rowid` ownership so updates and purges avoid quadratic lookup. Migration 0020 repairs the rebuildable pending-request projection by removing TUI→App Server client requests while retaining their append-only raw events. The current Observer schema is 20; migrations are idempotent and covered by rollback/reopen or data-repair tests as applicable.
- Session Kernel rollback is to stop/detach active test workers, set `controller.session_kernel = "off"` (or `preview` for the fixed fake fixture), and restart. No database downgrade, Codex Thread deletion, or App Server restart is required.
- Migration 0014 adds command, transition, audit, image metadata, and pending-request CAS fields.
- Migration 0015 adds the rebuildable Thread Goal projection.
- Both migrations have idempotency and late-failure rollback tests. They do not rewrite Codex rollout data or private Codex SQLite tables.
- Operational rollback is to set `controller.enabled=false` and restart. V1 routes and store-first import remain available; existing V2 ledger/audit rows are retained for diagnosis.
