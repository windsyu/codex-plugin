# ADR 0020: Use the real Codex TUI as the active session kernel

## Context

The validated V2 implementation controls an existing Codex App Server through one source-global `LiveSourceActor` and reconstructs a Codex-like conversation surface in React. This proved the command ledger, exact source epoch, typed protocol mapping, pending-request CAS, image staging, crash recovery, and read-only V1 compatibility. It also concentrated a growing amount of session behavior in `src/live/transport.rs`, `src/http/mod.rs`, and `web/src/App.tsx`.

The Web surface now has to reproduce input editing, IME behavior, Slash parsing, pickers, `/clear`, `/resume`, `/goal`, Plan mode, settings, approval/question cards, keyboard shortcuts, optimistic Turn state, and future Codex TUI changes. The reported `/clear` and `/goal` failures are symptoms of this architectural duplication rather than isolated rendering defects.

cc-viewer demonstrates a smaller interactive architecture: it runs the real Claude CLI in a PTY, connects xterm to the PTY, and gives IM bridges dedicated CLI workers with ACK, deduplication, conversation binding, single-flight queues, and output delivery. However, its IM mode uses `--dangerously-skip-permissions`, regex hard denies, prompt-level interaction restrictions, and transcript extraction. Those choices do not meet this project's requirement for full IM control, native approval/question handling, App Server provenance, or fail-closed audit.

Codex already provides a real TUI that can connect to a remote App Server. The official source implements its Slash and local interaction state machines, and `codex resume <SESSION_ID> --remote <endpoint>` is available in the verified CLI. App Server server requests are resolved by a subscribing connection; allowing a TUI and a separate Gateway actor to control the same Thread would create competing owners.

## Decision

- Make a real `codex` TUI process running in a PTY the interaction kernel for each active session.
- Create one `Thread Session Worker` per TUI session. Every primary, side, or child Thread controlled by that TUI has an exclusive `ThreadLease` pointing to the same worker; one Thread can never be leased by two workers. Multiple browsers attach to that worker's single PTY instead of creating another TUI or App Server owner.
- Place a private 1:1 App Server proxy between the TUI and the configured existing App Server. The proxy owns one upstream connection, transparently forwards protocol envelopes, maps request IDs, persists both protocol directions before observable forwarding, records the upstream write boundary for TUI-originated mutation, and exposes only a closed typed internal control port.
- Keep a `SourceSupervisor` for endpoint health, source epoch, catalogs, worker registry, and lease coordination. It must not resume or respond for a worker-owned Thread.
- Treat `sourceEpoch` as the Supervisor's shared source generation and give each proxy a separate worker connection epoch. One worker disconnect is isolated; only evidence that the configured source generation changed makes all workers stale.
- Use persistent `ThreadLease`, `InputLease`, and Turn owner state to prevent multiple workers, browsers, or channels from concurrently controlling one Thread/PTY.
- Send browser terminal input to the PTY without interpreting Slash or picker semantics. Keep only minimal host controls such as attach, lease, resize, interrupt, detach, and stop.
- Retain the V1 Observer, durable store-first history, projections, search, safe rendering, Raw Inspector, completeness states, and `/v1` read-only API.
- Retain the transactional command/audit ledger, exact source epoch, raw-event-first ingestion, pending-request CAS, typed closed actions, redaction, and `outcome_unknown` recovery semantics.
- For V3, put platform-neutral IM adapters above Session Worker. Bind an authenticated channel principal and conversation to a worker/Thread, serialize input through the same lease, and derive Turn completion/final replies from App Server events rather than ANSI or transcript tail inference.
- Give an enrolled IM principal the same Gateway control authority as the existing local login. Do not use skip-permissions, automatic approval, regex hard-deny policies, or prompts that disable native questions and elicitation. Codex managed requirements, sandbox, OS permissions, and native approvals still apply.
- Migrate behind a default-off feature flag. Keep the legacy Composer for at least one validation release; retire its write path only after PTY, proxy, lease, request-routing, recovery, security, and V1 regression gates pass.

This decision changes the ownership portion of ADR 0013 and supersedes the Web-emulated conversation portions of ADRs 0015, 0016, and 0017. Their closed typed mappings and capability evidence remain useful for worker control and non-terminal channels. ADR 0014 (ledger/audit), ADR 0018 (pending request CAS), and ADR 0019 (restart reconciliation) remain in force, with Session Worker/proxy taking the former source actor's ownership role.

## Alternatives

### Continue the source-global actor and repair the Web Composer

Rejected as the target architecture. It can fix individual bugs but continues to duplicate the official TUI state machine and requires each new Codex interaction to be rediscovered, reimplemented, rendered, and tested.

### Connect browser xterm directly to a local `codex` process

Rejected because a browser cannot safely own the local process, App Server endpoint, Unix socket, environment, lifecycle, or reconnect buffer. It also bypasses Gateway authentication, lease, audit, raw ingestion, and recovery.

### Let the TUI connect directly upstream while Gateway keeps a second observer/controller connection

Rejected because server requests and Thread mutations would have two possible owners. The App Server can accept multiple clients, but the project cannot prove that approval, question, or ephemeral state will be observed and resolved by the intended principal.

### Keep one source-global actor and virtualize several TUI protocol sessions inside it

Rejected because it preserves a central multiplexer that must understand and route every TUI protocol behavior. A per-session 1:1 proxy has more processes/connections but much less shared ownership and routing logic.

### Copy cc-viewer's per-platform CLI worker exactly

Rejected. Its worker, queue, binding, deduplication, and PTY patterns are valuable, but permission bypass, prompt-enforced restrictions, and transcript/ANSI inference conflict with this project's complete-control and audit requirements. V3 binds conversations to the same Session Worker abstraction rather than creating an unrelated, permission-reduced bot architecture.

### Use only typed App Server APIs for Web and IM

Rejected for the browser session experience because it retains the current reimplementation burden. Typed actions remain appropriate for audit-sensitive non-terminal operations, request resolution, attachments, and capabilities proven not to desynchronize the TUI.

## Consequences

- Active browser sessions inherit official Codex TUI behavior and future Slash/picker improvements with less project-specific rendering and state logic.
- The product intentionally has two complementary views: an ANSI terminal for active interaction and the existing structured Viewer for durable history/search. Terminal output is not promoted to a durable truth source.
- The Gateway must operate PTYs, child processes, private runtime directories, terminal WebSockets, output backpressure, and process cleanup. These are new reliability and security responsibilities.
- A private proxy and ID mapper become protocol-critical components and require compatibility fixtures, unknown-envelope preservation, and fault-injection tests.
- Per-Thread workers use more local processes and App Server connections than one source-global actor, but make ownership, isolation, failure scope, and IM binding explicit.
- Thread switching from TUI commands requires atomic lease transfer. A conflict freezes input rather than allowing two owners.
- Multi-agent TUI sessions require a worker-owned lease set rather than one mutable Thread ID. Side/child Thread subscribe and unsubscribe events become ownership transitions and must remain compatible with unknown lifecycle variants.
- Terminal reconnect requires bounded output plus a server-side VT screen checkpoint; replaying only a truncated ANSI suffix cannot claim to reconstruct the screen.
- Browser refresh becomes attach/detach instead of reconstructing the conversation composer. Gateway restart remains conservative and does not automatically adopt or replay an ambiguous worker in the first version.
- V3 can offer full control without turning every platform into a terminal emulator. Channel presentation and delivery remain adapter-specific, while authorization, queueing, Turn ownership, request CAS, and audit remain shared.
- Existing V2 code and tests are migrated incrementally rather than discarded. Some typed protocol code remains necessary even after the Web Composer is removed.
- Core constraints, detailed design, README, AGENTS guidance, compatibility evidence, and validation records must distinguish the validated legacy V2 baseline from the accepted target architecture until migration completes.

## Status

Accepted

## Date

2026-09-01
