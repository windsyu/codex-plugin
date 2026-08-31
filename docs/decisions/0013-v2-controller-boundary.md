# ADR 0013: V2 Controller boundary and authentication

## Context

V1 has a default-off read-oriented App Server adapter and no Codex mutation routes. V2 must add local conversation control without weakening `/v1`, starting another App Server, or exposing raw JSON-RPC. Earlier exploratory work used a separate control token, but the approved V2 constraints explicitly require the existing bearer, pairing Cookie, and verified Tailscale identity to receive the same control authority.

## Decision

- Add a top-level `controller.enabled` switch with a default of `false`.
- Enabling the Controller requires strict Origin enforcement and at least one configured `app_server_socket`.
- Do not add a control-token file or a second credential system.
- Register Codex mutations only under `/v2`; `/v1` remains read-only compatible.
- Give each configured, controllable source one future `LiveSourceActor` that exclusively owns its WebSocket and creates a new epoch on reconnect.
- Route typed `GatewayCommand` values through a bounded actor channel. Browser/API code never sends arbitrary App Server methods or parameters.
- Initialize a Controller-owned connection with `experimentalApi: true`, but expose experimental capability only after runtime detection. A missing or rejected capability fails closed.

## Alternatives

- Separate read and control tokens: rejected because it conflicts with the approved single-user product decision and complicates Viewer/Tailscale behavior.
- Extend the existing read adapter with direct handler-to-WebSocket calls: rejected because it prevents exclusive request correlation, epoch invalidation, and ordered per-Thread mutation.
- Expose generic JSON-RPC passthrough: rejected because it bypasses typed capability, idempotency, CAS, confirmation, and audit policy.
- Start an App Server automatically: rejected because V2 only attaches to an explicitly configured existing process.

## Consequences

- Existing configurations remain read-only after upgrade.
- Any authenticated Tailscale user has local-user-equivalent V2 authority once the Controller is enabled; health and UI must make that risk visible.
- Actor, command storage, HTTP mutations, and Web controls can be delivered as separate fail-closed slices behind one stable configuration boundary.
- `experimentalApi: true` may add protocol variants, so unknown envelopes must still be retained and unverified capabilities must stay disabled.

## Status

Accepted

## Date

2026-08-29
