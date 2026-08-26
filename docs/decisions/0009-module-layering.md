# ADR 0009: Module layering (domain / store / ingest / live / http)

## Context

`src/` 目前为 15 个平铺模块，其中 4 个「神模块」占约 72% 代码（db.rs 3199 / api.rs 2501 / ingest.rs 1460 / live.rs 1094），并存在 `db ⇄ ingest` 环依赖（db 用 `ingest::thread_key`，ingest 用 `db::{classify_item, summary_text, now_ms}`），经 `writer` 扩展出 `ingest → writer → db → ingest` 三节点环。`db.rs` 同时承担 schema 迁移、连接池、blob 存储、投影/完整性业务逻辑、维护命令与查询入口；领域逻辑散落在 db/ingest/live 三处；`model.rs` 为贫血结构体。这与 AGENTS.md §7.2（IO/解析/标准化/投影/查询独立可测）与 §7.3（不在 adapter 内散落业务投影规则）相冲突。

## Decision

将 `src/` 重组为五个分层模块，并强制单向依赖：

- `domain/`：纯业务规则，零 IO。含 `model`/`project`/`redact` 以及 `classify`/`identity`/`normalize`/`live` 纯函数。不依赖任何其他 crate 模块。
- `store/`：仅持久化，不含业务规则。含 `schema`/`pool`/`blob`/`write`/`query`/`maintenance`/`projection`。可依赖 `domain`。
- `ingest/`：导入编排。含 `importer`/`io`/`keys`。依赖 `store` + `domain`。
- `live/`：live adapter 传输。含 `transport`。依赖 `store` + `domain` + `writer`。
- `http/`：接口层薄封装。含 `router`/`middleware`/`auth`/`cursor`/`query`/`handlers`/`stream`。依赖 `store` + `domain` + `writer`。
- 基础设施 `config`/`permissions`/`instance_lock`/`watcher`/`writer` 保持平铺。

依赖方向硬规则：`domain` 无 IO 且不依赖上层；`store` 不依赖 `ingest`/`live`/`http`；应用层与接口层单向依赖 `store` + `domain`。

消除环：把跨模块的纯领域符号（`classify_item`/`classify_live_item`/`live_summary`/`summary_text`/`now_ms`/`thread_key`/`stable_source_id`/`projection_reference_key` 等）统一下移到 `domain`，使 `db` 不再 import `ingest`、`ingest` 不再 import `db` 的纯函数，环自然消解。

## Alternatives

- 只拆分神模块、不分子目录：目录改动小，但无法用路径表达依赖方向，环可能重现。
- 最小提取纯函数：改动最小，但神模块仍偏大，分层边界不清晰。
- 引入 trait/DI 框架：可为测试抽象，但超出 V1 需要，增加复杂度（AGENTS.md 4.2）。

## Consequences

模块数从 15 增至约 30；测试随函数移动到各子模块，保持集成测试结构；无行为、schema、migration、API 契约变化。依赖方向可由目录结构与 code-review-graph 社区检测验证（当前 0 社区，重构后应 >0）。`db ⇄ ingest` 环消失。后续新增模块须遵循本分层；破坏单向依赖须另立 ADR。

## Status

Accepted

## Date

2026-08-17
