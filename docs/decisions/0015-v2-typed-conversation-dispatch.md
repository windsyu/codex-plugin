# ADR 0015: V2 Typed Conversation Dispatch

## Context

V2 must control an exact existing App Server source without exposing generic JSON-RPC, replaying an ambiguous mutation, or allowing browser state to select a stale Thread or Turn. The first three slices established the actor, epoch registry, command ledger and audit trail, but deliberately did not write mutations upstream.

The App Server `0.149.1` experimental schema defines distinct contracts for `thread/start`, `thread/resume`, `thread/fork`, `turn/start`, `turn/steer` and `turn/interrupt`. In particular, steer requires `expectedTurnId`, and text input can carry `clientUserMessageId` for projection reconciliation.

## Decision

- Translate public capabilities into a closed Rust `ControllerOperation` enum. Only that enum may produce App Server method and params; browser-provided method names or arbitrary params are never forwarded.
- Dispatch through the `LiveSourceActor` found by exact `sourceId + sourceEpoch`.
- Validate the derived Thread key, loaded Thread state and active Turn precondition inside the actor immediately before writing.
- Move a command to `dispatching` only after actor-side preconditions pass.
- Persist every response, notification and server request as a V1 raw event before advancing the command from `dispatching`.
- Treat an upstream error response as `UPSTREAM_REJECTED` without retaining its private error body in the command or audit tables.
- Treat timeout, disconnect, malformed correlation or other loss of confirmation after the WebSocket write begins as `outcome_unknown`; never replay automatically.
- Keep `turn.start` in `running` until the matching `turn/completed` notification supplies its terminal status. Steer and interrupt complete when their RPC response is accepted because they do not own the Turn lifecycle.
- Discover loaded Threads on each new source epoch and rebuild active-Turn preconditions from `thread/read` responses.
- Make `POST /v2/threads` and `POST /v2/threads/{threadKey}/inputs` translate into the same command ledger path as `POST /v2/commands`.

## Alternatives

### Generic JSON-RPC passthrough

Rejected because it would bypass capability contracts, input validation, future CAS rules and method-level compatibility gates.

### Dispatch directly from HTTP handlers

Rejected because HTTP request cancellation would own mutation correlation, and multiple callers could write the same socket without one epoch-scoped authority.

### Retry timeout or disconnect automatically

Rejected because a completed socket write may already have created a Thread or Turn. Replaying would violate idempotency at the upstream boundary.

### Infer active Turn only from the V1 database

Rejected because the projection can be durable-only, stale after reconnect, or lag the actor's current notification sequence.

## Consequences

- The conversation slice is fail-closed when the source, epoch, Thread or Turn precondition cannot be proven.
- Source-wide actor serialization is stricter than the minimum same-Thread serialization requirement, but preserves correctness for the first control slice and can later be relaxed behind the same typed interface.
- Commands that reach `outcome_unknown` require reconciliation or explicit user action; no hidden retry occurs.
- This dispatch boundary also constrains the settings, Plan/Goal, pending-request, image, SSE and Web slices delivered later in V2.

## Status

Accepted

## Date

2026-08-29
