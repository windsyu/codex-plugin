# 旧 Observer：只读兼容与数据边界

本文是仍保留的 `codex-observerd`、`/v1` 和旧历史数据的技术参考。当前工作台架构以 [ADR 0039](../decisions/0039-cc-viewer-style-runtime.md)及[本机架构](workbench.md)为准；本页不恢复旧 Controller、Session/App Server、remote TUI 或 `/v2` 控制协议。旧 ADR 的历史理由可从 [V1 固定基线](https://github.com/windsyu/codex-plugin/tree/2dd0f6b59abfe5824af0bf940041b1bfb961ab82/docs)追溯。

## 1. 两种旧数据入口

| 入口 | 职责与边界 |
| --- | --- |
| `codex-observerd import/serve` | 读取原生 rollout，写入 Observer 自有 SQLite/附件，再提供旧网页与 `/v1` 读取；保留 schema 1–20 migration，不接受 schema 25 |
| `codex-view` 的旧 Observer 来源 | 使用独立零写 reader 读取已登记的 schema 20/25 数据库及附件，加入统一历史目录；不启动旧 writer、迁移或导入，不改写原库 |

原生 rollout 是可重建持久历史的事实来源；Codex 内部 SQLite 私有表不是 Observer 正确性依赖。普通 `.jsonl` 与 `.jsonl.zst` 可导入；EOF 半行留待后续完整后提交，坏行和 unknown variant 保留诊断，不因单条失败中断整个来源。后台目录与 HTTP 请求分离，读取 API 查询已发布索引，不在请求内全库扫描原生 store。

旧库读取必须考虑 WAL 一致性，不把活动数据库当作 immutable 单文件；缺必要 WAL/SHM 或需修复时返回来源不可用。`codex-observerd doctor` 是只读诊断，`import/serve` 会写 Observer 自有数据，二者不能替代零写 reader。使用方法见[旧历史接入](../guides/usage.md#读取旧-observer-历史)。

## 2. 旧 `/v1` 读取协议

当前路由以 [http/mod.rs](../../src/http/mod.rs) 为准，业务接口保持只读；`POST /v1/auth/pair` 只兑换本机网页授权，不是 Codex 控制入口。

```text
GET  /v1/health
GET  /v1/sources
GET  /v1/projects
GET  /v1/threads
GET  /v1/threads/{threadKey}
GET  /v1/threads/{threadKey}/turns
GET  /v1/threads/{threadKey}/items
GET  /v1/threads/{threadKey}/events
GET  /v1/events
GET  /v1/blobs/{blobId}
GET  /v1/search
GET  /v1/meta/capabilities
GET  /v1/meta/settings
GET  /v1/stream
GET  /v1/stream/ws
POST /v1/auth/pair
```

Thread、Turn、Item 及搜索分页使用签名 keyset cursor，绑定 endpoint、筛选及 `asOfEventSeq`，不能换条件后复用或自行构造。SSE/WS 基于已提交事件提供回放与后续事件；慢消费者隔离，不能用未提交状态冒充已保存事件。现有响应及 cursor 语义继续保留，不向此命名空间加入发送消息、审批、interrupt 或启动 CLI 的路由。

服务每次启动轮换 bearer，配对码在本次服务运行内稳定、可重复兑换；网页使用 `HttpOnly; SameSite=Strict` Cookie，bearer 不进入 URL。服务重启后旧 Cookie、配对码及签名 cursor 失效。默认 loopback；旧 Tailscale Serve 路径仍检查 HTTPS、Host 和可信身份头，不因此新增公网账号或工作台 RemoteOwner 权限。

## 3. 提交、来源与完整性

Observer 的 raw event、Thread/Turn/Item projection 和 source checkpoint 在同一事务提交；只有提交成功的事件进入已提交事件流。`source/epoch/sequence` 或文件 offset 标识来源，wall clock 不能单独作为去重身份；索引可由保留的脱敏 raw 重建。文件通知只是 rescan 提示，不能替代周期对账或提交水位。

旧记录与 API 保留以下 `captureCompleteness` 值：

| 值 | 解释边界 |
| --- | --- |
| `durable_complete` | 持久记录的开始、终态和 EOF 证据满足旧覆盖规则；不代表恢复了全部瞬时事件 |
| `durable_partial` | 持久记录缺少必要覆盖证据，或覆盖材料保留不完整 |
| `live_complete` | 同一连续 epoch 中有开始与终态证据，且满足连续性、断线及解码检查 |
| `live_partial` | live 证据不完整、断线或存在相应解码缺口 |
| `metadata_only` | 没有足够的正文或生命周期覆盖证据 |
| `ephemeral_lost` | 旧记录明确标记无法恢复的瞬时内容缺失 |

完整性与生命周期状态独立：模型响应结束不能推导整轮任务完成，clean EOF 也不能重建 transient delta、approval 或 question。来源重连产生新 epoch，不能假定跨连接 sequence 连续；后续成功事件不抹除已确认缺口。旧 completeness 值只约束旧兼容层，不强制成为新工作台的实时状态模型。

旧 live/App Server 字段仅用于解释已有记录和兼容配置。当前运行不重新连接旧 live adapter；旧 `[controller]`、`app_server_socket`、`live_mode` 不恢复控制或自动执行。

## 4. Projectless 与 cwd

原生 cwd 是执行上下文，不能直接当作完整的 Codex 项目身份。旧降级规则见 [domain/project.rs](../../src/domain/project.rs)：缺失有效 cwd 时无展示项目；有 cwd 时，仅对已知 Desktop originator（`Codex Desktop` / `codex_work_desktop`）及 `*/Documents/Codex/YYYY-MM-DD/<name>` 形态目录，将展示项目判为 `null`。普通 CLI 和其他明确目录不因同名形态自动取消归属。该规则是有界兼容判断，不声称读取了完整项目注册表。

Projectless 记录仍保留原 cwd 和来源信息，不从 cwd 生造用户项目。新工作台进一步分离 `recordedCwd` 与展示归属，显式恢复只使用经过验证的原 cwd、SessionMeta 和来源身份，见[历史首页](../codex-native-cli-workbench-history-home.md)与[项目会话整理](../codex-native-cli-workbench-session-organization.md)。

## 5. 脱敏、导出、保留与 purge

脱敏在写入 raw、blob 和 projection 前执行。认证头、已知凭据、URL token/signature、JWT 等按规则处理；image/audio data URI 不作为原始附件持久化，只保留类型、大小估计及带密钥指纹。大 payload 使用已脱敏内容寻址 blob。Raw JSON、Markdown、ANSI、HTML 和 SVG 都是不可信内容，不能因来自本机数据库就执行或直接注入页面。

旧记录不会因升级而自动重新脱敏。health、Viewer 和 export 保留 legacy-redaction 风险提示；导出仅包含 Observer 副本，不能将其视为已按最新规则重处理。后续修复旧脱敏数据需明确维护方案和授权，不能自动删除原库或私人来源。

保留策略分别处理 raw、delta 与 blob；覆盖材料被删除后要反映不完整性。显式 `purge` 仅删除 Observer 自有副本，要求确认，并在事务内写入按来源/原生 thread 身份绑定的抑制墓碑及维护审计；后续 rescan/import 不得悄悄恢复该副本。purge 不删除原生 rollout，不向官方 CLI 发出任何控制操作，也不等于“重新导入即可恢复”。相关实现见 [store/mod.rs](../../src/store/mod.rs)。

数据目录保持私有权限（Unix 目录 `0700`、数据库/secret/附件等 `0600`），打开敏感路径时检查 owner、普通文件、mode 和链接边界；导出、blob 下载与范围读取继续保持既有预算及权限检查。数据维护和 schema 迁移只针对 Observer 自有数据，工作台的统一历史读取不自动执行这些写操作。

## 6. 维护约束

- 保留 schema 1–20 migration、旧 raw/audit、provenance、unknown/decode 诊断及读取契约；不要通过修改 `user_version` 强行让旧 writer 接受新库。
- 新 CLI/PTY/模型代理与异步 Recorder 不依赖旧 Writer、App Server 或控制账本；旧事务约束不得回流并阻塞新转发热路径。
- 不把旧局域网身份、配对码或 bearer 自动升级为全局工作台、公网账号或跨电脑授权。
- 兼容修改继续验证只读路由、分页/cursor、来源边界和内容安全；测试使用合成数据及隔离 HOME/CODEX_HOME，不以私人历史作为可写 fixture。

旧验证不构成当前平台、容量或 SLA 承诺；实际支持和仍未实测的组合见[支持范围](../codex-native-cli-workbench-support.md)。
