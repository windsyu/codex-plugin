# ADR 0018: Pending Request CAS and Closed Typed Responses

## Context

Codex App Server sends approval, permission, user-input, and MCP elicitation as JSON-RPC server requests. Multiple authenticated browser clients may display the same request, while reconnect creates a new source epoch and the App Server may reuse a request ID or issue a newer version. A browser response is itself a mutation: once bytes are written, retrying an uncertain answer could approve or reject twice.

The request payload may contain exact commands, paths, workspace permissions, question text, or MCP form schema. The UI needs the redacted projection for an informed decision, but long-lived command and audit tables must not retain answers or elicitation content.

## Decision

- Encode `sourceId + sourceEpoch + requestId` in a bearer-token-keyed, signed `requestKey`. Require the action body to repeat the exact `sourceEpoch` and `expectedRequestVersion`.
- Expose only `approval`, `permissions`, `userInput`, and `mcpElicitation` action variants. Translate each variant to the matching Codex response schema in the source actor; do not expose a JSON-RPC response passthrough.
- Validate advertised approval decisions, exact user-question IDs and options, requested permission overlays, and MCP form schema before claiming the request.
- In one DbWriter transaction, compare request state/version, change `pending → resolving`, bind `resolving_command_id`, and change the command `authorized → dispatching` with transition and audit rows.
- Preserve the original JSON-RPC request ID value and type in the response envelope.
- After the response write succeeds, wait for the matching `serverRequest/resolved`. Ingest and project that notification through the V1 raw-event transaction before completing the Gateway command.
- Treat write failure, disconnect, or timeout after claim as `outcome_unknown`. Never replay the response automatically. A disconnected epoch invalidates both pending and resolving requests.
- Persist only keyed payload hashes, byte-count summaries, request type, and sanitized result/error summaries in command and audit tables. Answers and MCP content remain transient in the typed dispatcher.

## Alternatives

### Generic JSON-RPC response endpoint

Rejected because it would bypass type validation, capability boundaries, payload redaction, and protocol compatibility controls.

### Claim after writing the response

Rejected because two clients could both write before either persistent compare-and-set wins.

### Complete on successful socket write

Rejected because a successful local write does not prove that App Server resolved the request, and would violate raw-event-first reconciliation.

### Retry on timeout or reconnect

Rejected because the first response may already have taken effect. Replaying could duplicate approval or submit a stale answer to a new epoch.

## Consequences

- Exactly one command can claim a given request version, even under concurrent clients.
- A claimed request can remain `resolving` while its command is `outcome_unknown`; reconnect marks the old epoch stale instead of making it actionable again.
- Browser cards can show exact redacted request targets without copying user answers or MCP content into the audit ledger.
- Protocol drift disables only the mismatched action type and keeps the unknown raw envelope available to V1 diagnostics.

## Status

Accepted

## Date

2026-08-29
