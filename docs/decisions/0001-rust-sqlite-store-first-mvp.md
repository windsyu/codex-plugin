# ADR 0001：V1 使用 Rust、SQLite 与 store-first 纵向切片

- Status：Accepted
- Date：2026-08-14

## Context

V1 需要在 macOS 本机以 read-only sidecar 方式导入 Codex rollout，形成可重建的 raw event log 和查询投影，并提供本地 API 与 Web Viewer。详细设计已经选定 Tokio、Axum、rusqlite 和 SQLite；首次实现需要冻结可运行的最小边界。

## Decision

交付单一 Rust daemon `codex-observerd`。durable rollout 是当前正确性主链路，所有输入先脱敏并写入 Observer SQLite raw event，再在同一事务内更新 Thread/Turn/Item、FTS 和 checkpoint。API 使用 Axum，只注册 GET 路由；Viewer 以嵌入式静态资源发布。`live_mode` 在当前切片强制为 `off`。

实现不依赖 Codex 内部 crate 或其本机源码路径。兼容性通过宽松 JSON decoder、合成 fixture 和 capability manifest 维护。

## Alternatives

1. 直接依赖 Codex 内部 Rust crate：类型更精确，但版本耦合和发布复杂度过高。
2. 直接查询 Codex SQLite：查询成本低，但依赖私有 schema，且违背 JSONL durable history 主链路的设计事实。
3. 先实现 App Server Live Adapter：能展示 transient delta，但多 subscriber 生命周期与 request 行为尚不足以作为默认安全前提。
4. 前后端分离构建：UI 开发体验更强，但会增加首个纵向切片的工具链和部署复杂度。

## Consequences

- 首版可以离线、只读、可重放地验证历史浏览价值；
- unknown variant 不阻塞导入，但其语义投影需要后续 compatibility fixture；
- 当前不捕获 transient delta、approval 或 question；UI 必须显示 durable completeness；
- cursor 签名、blob、retention、App Server Live Adapter 和性能门槛需要后续 V1 切片补齐；
- SQLite 单 writer 和 projection rebuild 为后续加固保留了明确接口。
