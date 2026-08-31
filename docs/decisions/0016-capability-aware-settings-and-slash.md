# ADR 0016: Capability-Aware Settings and Slash Registry

## Context

The V2 UI needs `/model`, `/reasoning`, `/personality` and `/permissions`, but their legal values vary by source, installed Codex version, model and permission requirements. A static browser menu or prompt-based simulation could advertise settings that the source cannot execute. It could also bypass the exact Thread/epoch checks used by ordinary commands.

Codex `0.149.1` exposes model and permission profile catalogs and the typed `thread/settings/update` request. Model entries declare supported reasoning efforts and personality support; permission profile entries declare whether selection is allowed.

## Decision

- Keep the App Server catalog response in the epoch-scoped actor and expose it through authenticated `GET /v2/control/catalog`.
- Resolve an optional public `threadKey` through the V1 projection, then make the actor verify the derived Thread key before returning Thread-scoped availability.
- Build the Slash registry server-side from the actor's current loaded Thread, active Turn, model and catalog state.
- Map setting commands to a closed `ThreadSetting` enum. Emit `thread/settings/update` with exactly one allowed field: `model`, `effort`, `personality` or `permissions`.
- Validate model visibility, reasoning support, personality support and permission `allowed` state immediately before dispatch.
- Return `INTERACTION_REQUIRED` with a structured picker type when a selection command has no argument.
- Treat `//text` as literal `/text`; reject unknown `/command` before any model input is written.
- Project official `thread/settings/updated` notifications back into the V1 Thread projection.

## Alternatives

### Static Slash list in the Web bundle

Rejected because it becomes stale across source epochs and cannot express model- or Thread-specific capability changes.

### Let App Server reject arbitrary values

Rejected because the product contract requires the service to revalidate browser selections and hide unavailable capabilities.

### Encode settings in natural-language prompts

Rejected because model prompts are not protocol state and must not change permission or execution settings.

## Consequences

- The catalog endpoint is read-only but authenticated and returns the current source epoch for subsequent mutation binding.
- A source reconnect rebuilds the catalog and Slash availability; clients must discard the old epoch.
- Catalog absence or an unselectable value produces `CAPABILITY_UNAVAILABLE` without an upstream mutation.
- Additional Slash commands can be added only with their own typed capability and protocol fixture.

## Status

Accepted

## Date

2026-08-29
