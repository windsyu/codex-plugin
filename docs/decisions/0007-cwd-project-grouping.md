# ADR 0007: cwd-based project grouping

## Context

会话数量增加后，平铺的 Thread 列表无法快速按项目定位会话。Codex 自身没有持久化 `project_id`，会话元数据中最稳定、最贴近产品语义的项目标识是 `cwd`。

## Decision

Web Viewer 以规范化后的 `cwd` 绝对路径作为 `project_key` 聚合 Thread。规范化只做词法处理：去掉 `file://`、尾部分隔符和重复分隔符，并解析 `.`/`..`。项目显示名取 `cwd` 最后一段路径，根目录显示 `/`。不引入 Git 探测或独立 `project_id`。数据库新增 `threads.project_key` 并在 migration 12 中从已有 `cwd` 回填。

## Alternatives

- 探测 Git 根目录：更接近“项目”概念，但引入文件系统语义和降级规则。
- 使用 `cwd` 最后一段目录名：更短，但同名目录会错误合并。
- 新增独立 project 表：灵活但为 MVP 增加实体生命周期和迁移复杂度。

## Consequences

相同 `cwd` 的会话聚合为一个项目；不同 source 下相同路径也会合并。项目键只依赖持久化 `cwd`，不依赖 observer 运行时的文件系统状态。未来若需要 Git 根或用户自定义项目映射，可在现有 `project_key` 之外增加派生逻辑。

## Status

Superseded by [ADR 0012](0012-projectless-thread-grouping.md)

## Date

2026-08-16
