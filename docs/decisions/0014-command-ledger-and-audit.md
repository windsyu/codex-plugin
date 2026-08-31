# ADR 0014: Transactional command ledger and append-only audit

## Context

V2 mutation must be idempotent, bound to an exact source epoch, diagnosable after a crash, and auditable without retaining full user messages or secrets. V1 already enforces one SQLite writer and publishes only committed events. A memory-only dispatcher or a separate database would break those guarantees.

## Decision

- Add `gateway_commands`, `command_transitions`, `control_audit`, and `image_uploads` in additive migration 0014.
- Extend `pending_requests` with a monotonic `request_version` and resolving command fields for later compare-and-set actions.
- Uniquely bind idempotency to `principal_id + capability + idempotency_key`; the row also stores a keyed/canonical payload hash.
- Keep the current command state as a query projection while recording every transition in `command_transitions`.
- Make transitions and audit append-only with SQLite triggers that reject update and delete.
- Write command state, transition, and audit in one DbWriter transaction.
- Store only bounded `input_summary_json` and `result_summary_json`; never store full text, image bytes, unredacted upstream payload, or secrets in command/audit tables.
- Keep `control_audit` outside ordinary raw-event retention.

## Alternatives

- Memory-only commands: rejected because restart loses idempotency, outcome, and audit evidence.
- Reuse `raw_events` as the command ledger: rejected because raw retention and projection replay semantics differ from durable audit requirements.
- Separate command database/writer: rejected because cross-database state advancement cannot preserve the existing single-transaction invariant.
- Mutable audit rows: rejected because later mutation could erase the authorization and outcome trail.

## Consequences

- Schema version advances from 13 to 14 through an in-place additive migration.
- Current state can be rebuilt or checked against ordered transitions.
- Future approval/question actions can use request-version CAS without another incompatible schema change.
- Image metadata has a durable lifecycle table, while staged bytes remain outside SQLite and are deleted separately.
- Audit growth needs a dedicated future archival policy; normal raw retention must not delete it.

## Status

Accepted

## Date

2026-08-29
