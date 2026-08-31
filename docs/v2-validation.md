# V2.0 Validation Report

> Date: 2026-08-30
> Version: `v0.2.0`
> Result: Passed with the documented compatibility limits below

## Scope

This report validates the first local V2 Control Plane release against [`codex-local-gateway-v2-development-constraints.md`](codex-local-gateway-v2-development-constraints.md). V1 remains the store-first, read-only baseline. V2 mutation remains default-off, uses the existing bearer/Cookie/verified Tailscale identity, binds every command to an exact live source epoch, and exposes only closed typed capabilities under `/v2`.

## Automated gates

| Gate | Command / evidence | Result |
| --- | --- | --- |
| Rust unit/integration | `cargo test --locked` | 134 passed; 1 explicit 2M-event capacity test ignored; 0 failed |
| Rust lint | `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| Release build | `cargo build --locked --release` | Passed; `codex-observerd 0.2.0` |
| Web unit | `npm test -- --run` | 42 passed; 0 failed |
| Web production build | `npm run build` | Passed |
| Browser E2E | `npm run test:e2e` | 12 Chromium tests passed at 390/820/1280/1440 px |
| Patch hygiene | `git diff --check` | Passed |

The ignored Rust test is `writer::tests::two_million_event_capacity_path`; it is intentionally opt-in through `OBSERVER_RUN_CAPACITY=1`. The ordinary bounded-writer, fan-out, scale-query, replay, and backpressure tests remain enabled and passed.

## Protocol and control validation

- Codex CLI compatibility baseline: installed `codex-cli 0.149.1`.
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

Web unit tests validate the Plan/Goal controls, Slash palette, pending-request card variants, offline disablement, optimistic send state, `clientUserMessageId` projection reconciliation, and `outcome_unknown` rendering. Playwright validates the V1 Viewer regression and the browser-level V2 controls together: project/recent navigation, search, safe raw inspection, responsive bounds, pairing, new exact-epoch Thread creation, catalog-gated model/reasoning/permission settings, Plan mode, Composer text/image send, optimistic display followed by SSE-driven projection reconciliation, interrupt, exact approval target/action, `outcome_unknown`, and SSE cursor reconnect.

A fresh temporary release run validated:

- synthetic fixture import: 3 files scanned, 21 events inserted, 0 decode errors, 0 degraded sources;
- `doctor`: healthy, SQLite schema 15, quick check OK, private database/fingerprint-key permissions;
- authenticated release `/v1/health`: healthy, projection lag 0, Controller disabled;
- disabled mutation boundary: `POST /v2/commands` returned `404`.

No real rollout, real uploaded image, bearer token, pairing code, user message, or local database is stored in the repository.

## Compatibility and known limits

- The configured official source checkout was unavailable in this environment, so the V2 protocol baseline is the generated schema and fixture set from installed `codex-cli 0.149.1`; the compatibility manifest records `sourceCommit: null` and the reason explicitly.
- Validation did not control a user's real App Server or real Codex session. The release contract permits real App Server **or protocol fixture** evidence; this report relies on deterministic protocol fixtures and WebSocket integration tests to avoid mutating private user state.
- Tailscale behavior is covered by proxy/header/principal tests, not a live external Tailnet session.
- The explicit 2M-event capacity test was not run in this gate. It remains available as an opt-in extended-capacity test.
- App Server control is experimental and capability-gated. A schema/method mismatch disables the affected V2 capability while V1 store-first browsing remains available.
- The macOS SDK lookup emitted a non-failing sandbox warning about `DARWIN_USER_TEMP_DIR`; all Rust test, lint, and release commands completed successfully.

## Migration and rollback

- Migration 0014 adds command, transition, audit, image metadata, and pending-request CAS fields.
- Migration 0015 adds the rebuildable Thread Goal projection.
- Both migrations have idempotency and late-failure rollback tests. They do not rewrite Codex rollout data or private Codex SQLite tables.
- Operational rollback is to set `controller.enabled=false` and restart. V1 routes and store-first import remain available; existing V2 ledger/audit rows are retained for diagnosis.
