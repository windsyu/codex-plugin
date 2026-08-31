# ADR 0019: Fail-closed Gateway restart reconciliation

## Context

The command ledger survives process crashes, but the WebSocket write boundary does not. After a restart, a command may remain in `received`, `authorized`, `dispatching`, `accepted_by_source`, or `running` while the in-memory actor and RPC correlation state are gone. Replaying any ambiguous mutation could create a duplicate Thread, Turn, answer, approval, or MCP response. Pending-request CAS and staged images must also leave a diagnosable, non-leaking state.

## Decision

- Before starting DbWriter or any LiveSourceActor, close every open `app_server` epoch with reason `gateway_restart` and mark its pending/resolving requests `source_disconnected`.
- Verify that each command's current state equals its last append-only transition. Abort startup on drift instead of guessing or rewriting history.
- Transition `received` and `authorized` commands to `failed` with `GATEWAY_RESTARTED`: these states precede the actor's upstream write boundary.
- Transition `dispatching`, `accepted_by_source`, and `running` commands to `outcome_unknown`: an upstream write may already have occurred.
- Append the recovery transition and audit row in the same SQLite transaction as the current-state update. Recovery never creates a new command and never invokes an App Server method.
- Delete command-attached images only for the proven pre-write states. Retain images for ambiguous outcomes until their normal expiry so later evidence can still be reconciled without fabricating completion.
- Make recovery idempotent; a second restart has no additional command transitions.

## Alternatives

- Replay every non-terminal command: rejected because the upstream boundary has no durable idempotency contract for all published methods.
- Mark every non-terminal command failed: rejected because it would present potentially completed upstream mutations as ordinary failures.
- Keep stale commands indefinitely: rejected because clients could mistake old `running` or `dispatching` states for live progress, and pending-request CAS would remain stuck.
- Infer outcomes solely from durable V1 projection: rejected because a crash may occur before durable rollout catches up, and ephemeral request responses may never be recoverable.

## Consequences

- Restart recovery is conservative: some operations that never reached the socket can become `outcome_unknown` once they entered `dispatching`.
- Users must reconcile ambiguous outcomes explicitly; the Gateway will not retry them.
- Open live epochs and their pending requests become visibly stale before a new exact epoch is registered.
- Command/audit consistency is checked on every writer startup, turning ledger corruption into an explicit startup failure.

## Status

Accepted

## Date

2026-08-29
