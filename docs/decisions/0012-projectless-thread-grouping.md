# ADR 0012: Preserve Codex projectless threads

## Context

ADR 0007 把每个规范化 `cwd` 都视为项目。该规则能聚合常规仓库会话，但会把 Codex Desktop 为独立对话创建的隔离工作目录（macOS 上形如 `~/Documents/Codex/YYYY-MM-DD/<name>`）显示为自动命名项目，与 Codex UI 不一致。

截至 2026-08-28，可验证证据如下：

- [OpenAI 官方 ChatGPT & Codex 变更记录](https://learn.chatgpt.com/docs/changelog)把 `projectless` 作为 Codex App 的独立任务类别，并在 2026-04-10、2026-04-20 条目中分别记录其 visibility、cwd 和 permission 修复；
- 本机 Codex 只读任务清单中，这类会话有 cwd，但没有 `projectId`，UI 将其放在“最近”而非“项目”；
- rollout `session_meta` 持久化 cwd、originator、source 等字段，但不持久化 App 的 `projectId`，因此仅凭 durable history 无法完整重建 Codex 项目注册表。

## Decision

- `ThreadProjection.project` 保持可选；缺少项目证据时返回 `null`，不创建“unknown”或 cwd basename 伪项目。
- 对已观察到的 Codex Desktop originator（`Codex Desktop`、`codex_work_desktop`）且 cwd 符合 `*/Documents/Codex/YYYY-MM-DD/<name>` 的会话，投影为 projectless。
- 其他 Thread 延续 ADR 0007 的规范化 cwd 分组，避免为 V1 引入 Codex App 私有数据库依赖或运行时 Git 探测。
- `/v1/projects` 排除 projectless Thread；`/v1/threads` 和 export 保留 cwd，但 `project` 为 `null`。
- Viewer 在项目树之后用“最近”展示 projectless Thread；“最近”只是 presentation 分区，不是项目实体，也不能用于 `project` 筛选。
- migration 13 将现有数据库中符合上述 Codex Desktop 规则的 `project_key` 清空；后续 import、replay 和 projection rebuild 使用同一分类函数。

## Alternatives

- 继续把所有 cwd 当项目：实现最简单，但持续制造 Codex UI 中不存在的项目。
- 读取 Codex App 私有项目数据库：可获得更完整映射，但违反 store-first 和不绑定私有表结构的 V1 边界。
- 运行时探测 Git 根：能识别仓库，但不能识别非 Git 项目，也会把项目身份绑定到易变文件系统状态。
- 把所有无法证明的 cwd 都视为 projectless：更保守，但会把非 Git 的真实 Codex 项目错误移入“最近”。

## Consequences

- 截图所示自动生成目录不再污染项目列表，Observer 与 Codex 的“项目 / 最近”信息架构对齐。
- cwd 仍作为执行上下文和诊断信息保留，不因分类变化丢失 provenance。
- 该规则是基于公开 projectless 语义和可观察持久化形态的 macOS 优先降级，不等价于完整 Codex 项目注册表。若上游未来在 durable schema 中提供稳定 `projectId`/`projectKind`，应优先使用显式字段并保留此规则只用于旧数据兼容。
- 位于同一日期目录形态下、由 Codex Desktop 启动的用户自建项目可能被保守归为“最近”；V1 不为这个罕见歧义读取私有 App 状态。

## Status

Accepted

## Date

2026-08-28
