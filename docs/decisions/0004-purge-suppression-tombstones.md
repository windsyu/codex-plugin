# ADR 0004：本地 purge 使用持久化抑制墓碑

## Context

V1 的 rollout source 是只读且会周期 rescan。若 purge 只删除 Observer 的 projection/raw，下次源文件追加或 checkpoint 重建时，同一个 Thread 会再次导入，无法满足用户对本地副本删除的预期。同时，Observer 不得修改 Codex 原 store。

## Decision

显式 `purge --observer-copy-only --yes` 在同一事务中删除该 Thread 的 raw、projection、FTS、pending request、dedupe 与 blob reference，并写入 `purged_threads` 抑制墓碑及不含正文的 `maintenance_audit`。后续 store/live ingest 对命中墓碑的 Thread 只推进 source checkpoint，不保存事件。墓碑不提供 V1 HTTP mutation；未来恢复能力必须通过单独、显式且可审计的本机命令设计。

## Alternatives

- 仅删除当前 projection/raw：实现简单，但后续 rescan 会静默恢复数据。
- 修改或删除 Codex rollout：违反 V1 read-only 安全边界。
- 删除整个 source：范围过大，且会影响无关 Thread。

## Consequences

- purge 对 Observer 数据是持久的，对 Codex store 无副作用。
- checkpoint 仍可前进，避免同一已删除 Thread 反复解析入库。
- 墓碑保留 Thread 的本地派生 key、source ID、Codex Thread ID 与时间戳，但不保留正文。
- 若未来需要恢复观察，必须增加显式 unpurge 工作流并重新扫描来源。

## Status

Accepted

## Date

2026-08-14
