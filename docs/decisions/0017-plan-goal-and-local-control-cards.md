# ADR 0017: Plan, Goal, and Local Control Cards

## Context

Slice 6 adds protocol-coupled controls whose behavior cannot be represented as ordinary model text. Plan mode is experimental and depends on the source's `collaborationMode/list` catalog. Goal state is durable upstream state that must survive a Gateway reconnect. `/status`, `/mcp`, and `/usage` return local or account control data and must not appear as assistant messages.

An active-Turn `/plan <prompt>` also requires two ordered upstream effects: update the Thread collaboration mode, then steer the active Turn. A failure after the first response is not equivalent to a fully rejected command because Plan mode may already be active.

## Decision

- Resolve Plan entry and exit only from epoch-scoped `collaborationMode/list` entries whose `mode` is `plan` or `default`, whose model is visible, and whose reasoning effort is supported by that model.
- Emit the exact `CollaborationMode` object required by Codex, including `developer_instructions: null`; never simulate Plan with a prompt.
- For an idle Thread with a prompt, send one `turn/start` carrying `collaborationMode`. For an active Turn with a prompt, confirm `thread/settings/update` before sending `turn/steer`. Without a prompt, update Thread settings only.
- Map `/plan off` (and the equivalent `exit` or `default` argument) to `thread/settings/update` with the catalog's Default preset merged over the model/effort snapshot captured before entering Plan. A `null`/unspecified preset field preserves the prior Default value instead of clearing it. The Web control toggles between entering Plan and returning to Default.
- If the second active-Turn request fails or cannot be confirmed after Plan settings succeeded, transition the combined Gateway command to `outcome_unknown` and never replay it automatically.
- Maintain Goal state in a rebuildable `thread_goals` projection and the current source actor cache. On every Controller reconnect, call `thread/goal/get` for loaded Threads before publishing the actor as ready.
- Keep full Goal data only in the redacted V1 raw/projection path and actor cache. Command results and audit rows retain status/presence summaries, not objectives.
- Return `/status`, `/mcp`, and `/usage` as authenticated `gatewayStatusCard` responses. They are not Gateway mutations and are never projected as model messages.
- Hide catalog-backed commands when their catalog read fails. For typed methods without a safe probe, remember an upstream method-not-found response for the current epoch and remove that command from subsequent Slash catalogs.

## Alternatives

### Prompt-simulated Plan or Goal

Rejected because natural language does not change protocol state and could bypass capability and epoch checks.

### Always update settings and then start a Plan Turn

Rejected for idle Threads because `turn/start.collaborationMode` performs the required sticky update in one confirmed request and avoids an unnecessary partial-success boundary.

### Treat active Plan steering rejection as a normal failure

Rejected because the preceding settings mutation has already succeeded. A terminal failure would falsely imply that no effect occurred.

### Store Goal objectives in command and audit tables

Rejected because those tables are long-lived control metadata and must not retain full user content.

## Consequences

- Plan availability changes with each source epoch and current Thread model.
- Plan is advertised only when both Plan and Default presets are valid, so entering the mode never strands the Gateway UI without a catalog-backed exit.
- An active `/plan <prompt>` may end in `outcome_unknown` while the status card correctly shows Plan mode enabled.
- Goal state is restored before a reconnected actor becomes controllable, and projection rebuild reproduces Goal updates and clears.
- Local cards require a live exact-epoch source catalog but do not create command ledger rows.

## Status

Accepted

## Date

2026-08-29
