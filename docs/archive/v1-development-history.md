# Codex Local Observer V1 开发历史归档

> 状态：Archived / Non-normative  
> 归档日期：2026-08-29  
> V1 基线提交：`2dd0f6b59abfe5824af0bf940041b1bfb961ab82`  
> 当前开发约束：[`../codex-local-gateway-v2-development-constraints.md`](../codex-local-gateway-v2-development-constraints.md)

## 1. 归档用途

本文压缩记录 Codex Local Observer V1 的调研、设计、ADR、实现结果、验证证据和已知限制，用于：

- 解释现有 V1 代码和 schema 的历史原因；
- 为 V2 兼容性、migration 和回归测试提供背景；
- 避免重复研究已经确认的 Codex durable/live 数据事实；
- 在排查历史行为时定位原始 Git 版本。

本文不是当前需求、详细设计或执行指南。V2 开发不得从本文恢复已经被新约束替代的范围、优先级或安全决定。发生冲突时，必须以根目录 `AGENTS.md` 和 V2 核心约束为准。

原 V1 文档已从活动文档目录移除；需要完整历史文本时，通过 Git 查看 V1 基线提交，不要把旧文件恢复到 `docs/` 根目录。

## 2. V1 产品目标与最终状态

V1 的目标是在 macOS 优先、单用户、本机环境中，以 store-first、read-only 方式验证 Codex 会话观察价值：

```text
Codex rollout store
  → append-only raw event
  → Thread / Turn / Item projection
  → SQLite query / search
  → REST / SSE / WebSocket
  → local Web Viewer
```

V1 最终完成的主要能力：

- 扫描一个或多个 `CODEX_HOME` 的 active/archived rollout；
- 增量读取 plain JSONL，并流式重放 `.jsonl.zst`；
- 保留 EOF 半行、坏 JSON 审计占位和 unknown variant；
- 事务化提交 raw event、projection、FTS 和 checkpoint；
- 投影 Thread → Turn → Item、关系、运行上下文和 capture completeness；
- watcher debounce rescan 加周期全量扫描；
- 只读 REST、签名 cursor、SSE、WebSocket 和搜索；
- Vite + Preact Web Viewer、对话优先时间线、Raw Inspector 和诊断摘要；
- 内容入库前 redaction、内容寻址 blob、retention、export 和 Observer-copy purge；
- 本机配对 Cookie、bearer token、严格 Origin 和可选 Tailscale Serve；
- 可选、默认关闭的只读 App Server Live Adapter；
- 合成 fixture、release binary E2E、10k Thread/Item 查询冒烟和安全负向测试。

V1 没有任何 Codex mutation route。唯一 POST 是本机 Viewer 配对，`purge` 也只删除 Observer 自身副本。

## 3. 关键事实与兼容基线

### 3.1 Codex 数据事实

- durable rollout 是 V1 可重建历史的主要事实源；
- Codex 内部 SQLite 私有表没有成为 Observer 正确性依赖；
- rollout 可包含未知字段和未来 variant，decode 失败不能导致原始证据丢失；
- transient delta、approval、question 和 ephemeral 事件无法仅凭 durable history 完整恢复；
- App Server live 连接只能补强同一连续 epoch 中实际观察到的事件；
- source reconnect 会产生新 epoch，不能假设跨连接 sequence 连续；
- cwd 是执行上下文，但不总是 Codex App 的真实 project identity；
- Codex Desktop projectless Thread 可能有 cwd，却仍应显示在“最近”而不是项目树。

### 3.2 Capture completeness

V1 对外使用以下状态诚实表达数据覆盖：

```text
live_complete
live_partial
durable_complete
durable_partial
metadata_only
ephemeral_lost
```

基本原则：

- durable clean EOF 只能证明持久化记录覆盖；
- live complete 要求同一 epoch 中观察到 Turn start 和 terminal；
- disconnect、sequence gap、unknown event 和 decode error 会降低完整性；
- 后续成功事件不能清除先前已确认的缺口；
- UI 和 API 不得把 metadata-only 或不可恢复 delta 伪装为完整对话。

### 3.3 研究源码基线

V1 调研参考本地 Codex 源码：

```text
/Users/windsyu/workspace/codex
41ece455b7fa7166f4fc38522952afdaa2604e18
```

该路径只用于研究。产品运行时、测试和 fixture 从未依赖这个绝对路径。

## 4. 最终架构快照

### 4.1 进程与模块

V1 交付单个 Rust daemon：`codex-observerd`。

主要模块边界：

| 模块 | 历史职责 |
| --- | --- |
| `domain/` | 无 IO 的 classify、identity、normalize、project、redact 和模型规则 |
| `store/` | migration、SQLite pool、projection、query、blob 和 maintenance |
| `ingest/` | rollout 发现、增量读取和历史导入编排 |
| `live/` | App Server transport、只读 reconcile 和 live envelope ingestion |
| `http/` | 认证、cursor、query、handler 和 stream |
| `writer` | 单 DbWriter、有界 ingest channel 和 committed event bus |

依赖方向保持为接口层 → store/domain，domain 不依赖 IO 模块。

### 4.2 提交不变量

- 外部记录先形成 append-only raw event，再更新 projection；
- raw event、projection 和 checkpoint 在同一 SQLite 事务中提交；
- checkpoint 只能在数据已提交或错误已明确持久化后推进；
- dedupe 不使用 wall clock 作为唯一身份；
- unknown/decode error 形成可查询证据，不静默跳过；
- 只有提交成功的事件进入 `CommittedEventBus`；
- API 请求路径只查询 Observer SQLite，不直接扫描 Codex store；
- projection 可以从保留的 raw event 重建。

### 4.3 并发与恢复

- 一个进程只持有一个 DbWriter 和一个写连接；
- advisory instance lock 防止两个 daemon 写同一数据库；
- 查询使用只读连接池；
- ingest queue 和 consumer queue 有界；
- watcher 只提供 rescan hint，周期扫描负责最终对账；
- stream 采用 DB replay → live bus 切换，慢消费者被隔离；
- crash 后通过 checkpoint、dedupe 和 migration 恢复，不修改 Codex 原始状态。

## 5. 数据与 API 历史契约

### 5.1 核心投影

- `ObserverEvent`：source、epoch、sequence/offset、method、raw/blob、decode status 和 provenance；
- `ThreadProjection`：Codex Thread ID、source、状态、归档、关系、project、context 和 completeness；
- `TurnProjection`：Turn 状态、时间、model、effort、approval、sandbox 和 coverage；
- `ItemProjection`：消息、command、file change、MCP、collaboration、reasoning、usage、status 和 unknown；
- `PendingRequest`：source epoch 内的 approval/question/elicitation 只读状态；
- `ProjectionConflict`：durable/live 不一致的显式诊断记录。

### 5.2 V1 API

活动 V1 兼容接口包括：

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

Thread、Turn、Item 和 Search 使用签名 keyset cursor，并绑定 endpoint、filter 和 `asOfEventSeq`。V2 不得破坏这些已发布读取契约。

### 5.3 Project 与 projectless

- V1 最初使用规范化 cwd 作为降级 project key；
- 后续确认 Codex Desktop 自动生成目录不等价于项目；
- 对已知 Desktop originator 且路径符合 `*/Documents/Codex/YYYY-MM-DD/<name>` 的 durable Thread，`project` 返回 `null`；
- projectless Thread 在 Viewer 的“最近”分区显示；
- cwd 仍保留在 runtime context，不因分类变化丢失 provenance；
- 该规则只是 macOS 优先降级，不宣称恢复完整 Codex 项目注册表。

## 6. 安全与隐私历史基线

### 6.1 本地文件与网络

- HTTP 只监听 loopback；
- Tailscale Serve 只把 Tailnet HTTPS 转发到 loopback，不启用 Funnel；
- Observer 数据目录为 `0700`，数据库、token、key、lock 和 blob 为 `0600`；
- 文件检查 owner、mode、regular-file 和 no-follow/symlink 条件；
- rollout 只读打开，不写 Codex SQLite、writer lock 或 session 文件；
- blob 路径由服务端生成，下载强制 attachment 并限制 Range/并发。

### 6.2 认证

- `serve` 每次启动轮换 bearer secret；
- 同一次启动使用稳定、可重复兑换的 pairing URL；
- pairing URL 只含签名 pair code，不直接包含 bearer secret；
- 兑换后使用 `HttpOnly; SameSite=Strict` Cookie；
- daemon 重启会使旧 pair code、bearer、Cookie 和签名 cursor 失效；
- bearer 保留给本机 API/CLI 和故障恢复；
- Tailscale 模式依赖 Serve 注入并校验的身份、Host 和 HTTPS 转发信息。

V2 已明确改变“只读身份”的能力范围；当前 mutation 权限以 V2 核心约束为准。

### 6.3 Redaction 与保留

- raw/Markdown/ANSI/HTML/SVG 一律视为不可信输入；
- redaction 在写 raw、blob 和 projection 前执行；
- credential、auth header、URL token/signature、JWT 形态被移除或标记；
- image/audio data URI 正文不持久化，只保留 media type、大小估计和 keyed fingerprint；
- 大型已脱敏 payload 使用内容寻址 blob；
- legacy redaction 记录不会自动重写，health/Viewer/export 必须显示风险；
- raw、delta 和 blob retention 独立；
- export 只导出脱敏 Observer 副本；
- purge 使用抑制墓碑，避免 rescan 静默恢复已删除的 Observer Thread。

## 7. Web Viewer 历史结果

V1 Viewer 从只读诊断页演进为对话优先界面，最终包含：

- 项目树、projectless“最近”和子代理折叠分区；
- 响应式列表/详情导航；
- 用户/Assistant 消息作为主阅读流；
- command、tool、file、reasoning、usage 和诊断按 Turn 折叠；
- renderer registry 处理已知和 future item variant；
- 搜索结果精确定位 Turn/Item；
- Raw Inspector 延迟加载、分页和安全复制；
- Cookie SSE 与 bearer 轮询降级；
- Markdown 经 DOMPurify 清洗，禁止脚本、iframe、style 和本地文件链接；
- 390、820、1280、1440px 响应式与键盘路径测试。

保留但未作为 V1 发布阻塞项的工作：完整 URL state、浏览器前进/后退恢复，以及 10,000 Thread/Item 下更深入的虚拟化与交互性能基线。

## 8. ADR 决策压缩

原 `docs/decisions/0001`—`0012` 的 Accepted 决策压缩如下：

| ADR | 决策 | V2 保留意义 |
| --- | --- | --- |
| 0001 | Rust + SQLite + store-first 纵向切片 | 保留 durable-first 和可重建 projection |
| 0002 | App Server Live Adapter 默认关闭且只读 | V2 改为可控 actor，但仍保留显式启用和 capability fail-closed |
| 0003 | 已脱敏内容寻址 blob | 继续用于大型已脱敏证据，不用于保存原始附件 |
| 0004 | purge 使用持久化抑制墓碑 | V2 不得让 rescan 恢复已 purge 的 Observer 副本 |
| 0005 | 单 DbWriter + CommittedEventBus | V2 command/event 提交继续遵守单写者和 committed-only stream |
| 0006 | pairing code + 签名 Viewer Cookie | 已被 0011 的启动期可复用 pairing 语义扩展 |
| 0007 | 规范化 cwd 降级项目分组 | 仅作为缺少上游 project identity 时的兼容规则 |
| 0008 | Vite + Preact Viewer | V2 Composer 继续使用现有前端栈 |
| 0009 | domain/store/ingest/live/http 分层 | V2 新 command/controller 模块必须保持依赖方向 |
| 0010 | Tailscale Serve → loopback | V2 继续复用网络边界，但已认证 Tailnet 用户拥有 mutation |
| 0011 | 启动期稳定、重启轮换的本机 token | V2 复用同一认证，不增加 control token |
| 0012 | 保留 Codex projectless Thread | V2 导航和新对话不得重新伪造项目身份 |

完整 ADR 文本仍可从 Git 的 V1 基线提交读取。

## 9. 验证历史

### 9.1 自动化门禁演进

已记录的最后一组 V1 核心门禁包括：

```text
cargo test --all-targets                         74 passed, 1 capacity test ignored
cargo clippy --all-targets --all-features        passed with -D warnings
cargo fmt --all -- --check                       passed
git diff --check                                 passed
```

Viewer 加固阶段曾记录：

```text
npm exec tsc -- --noEmit                         passed
npm test                                         25 passed
npm run test:e2e                                 8 passed
npm run build                                    passed
cargo test --all-targets                         68 passed, 1 capacity test ignored
```

这些数字是历史快照，不是当前分支测试数量承诺。后续以当前测试运行结果为准。

### 9.2 Release E2E 覆盖

历史 release binary E2E 验证过：

- fixture import、重复 import 幂等、API 分页和搜索；
- SSE/WS replay → live 切换；
- 无 token 401、非法 Origin 403、安全响应头；
- pairing fragment 清除、Cookie 鉴权和 bearer 不进入 URL；
- Tailscale Serve 身份头替换、tailnet-only 状态和 loopback backend；
- 启动期 token 多次兑换、重启轮换和旧凭证失效；
- redaction、symlink/no-follow、blob Range、pending request 只读；
- retention、snapshot export、purge suppression 和 projection rebuild。

### 9.3 容量边界

- 常规测试包含 10,000 Thread + 10,000 Item/FTS 查询冒烟；
- 2,000,000 event capacity test 是显式 ignored 测试；
- V1 没有对 10 GiB、2,000,000 event 或 100 并发客户端承诺正式 SLA；
- 已验证的是索引、稳定 cursor 和有界分页，不等于目标机器完整容量规划。

## 10. V1 已知限制

- store-first 只能保证 durable history 的最终一致；
- App Server attach 是实验性、默认关闭能力；
- `attach_loaded` 调用 `thread/resume`，可能影响 loaded 生命周期；
- 断线期间的 transient delta、approval 和 question 不可恢复；
- reasoning 是否保留取决于 capture policy，且不得宣称获得隐藏 chain-of-thought；
- projectless 分类是可审计降级，不是完整 project registry；
- legacy redaction 记录需要显式 purge/reimport 才能消除历史风险；
- 大规模容量 SLA、Windows live、完整 URL state 和列表虚拟化没有在 V1 冻结。

## 11. V1 时代的 V2/V3 预研

V1 阶段曾维护一份约 970 行的 V2 Control Plane / V3 IM Bridge 未来设计。它确认了若干后来继续保留的方向：

- mutation 必须绑定确定的 live source 和 source epoch；
- command 应有幂等键、状态转换、CAS 和审计；
- approval/question 需要一次性 action 和竞争处理；
- Web、API 和未来 IM 不应直接透传 App Server JSON-RPC；
- source disconnect、timeout 和 outcome unknown 必须显式表达；
- IM 平台应只做身份、消息和 delivery 适配，业务语义由 Gateway 统一定义；
- 外部渠道默认不披露 reasoning、cwd、完整 diff、命令输出或 raw JSON。

该历史设计中的以下方案已被当前 V2 决定替代，不得继续执行：

- read token 与 control token 分离；
- 为首版引入 principals、roles 和通用 policy engine；
- 完整 takeover 流程作为 V2 主线；
- 首版同时覆盖所有未来 command 和 IM 扩展接口；
- 将 V2 仍视为不影响当前开发的“未来设计”。

V3 的旧预研包括 Platform Adapter、External Identity、Conversation Binding、delivery ledger、交互卡片和 disclosure policy。这些只作为历史方向保留。将来启动 V3 时必须基于当时的官方协议和 V2 实现重新创建当前设计，不能直接恢复旧文档。

## 12. V2 必须保留的兼容性

V1 虽已归档，但以下实现契约仍是 V2 的兼容基础：

- `/v1` 保持只读且现有 response/cursor 语义不破坏；
- rollout、Codex SQLite 和 writer lock 仍不被 Gateway 修改；
- raw event、projection 和 checkpoint 事务一致；
- unknown variant、decode error、provenance 和 completeness 继续保留；
- V2 mutation 必须绑定确定的 live source 和 source epoch；
- Controller 关闭时，V1 import/query/Viewer 行为保持可用；
- V2 event stream 不得广播未提交状态；
- 现有 redaction、blob、文件权限、Origin 和 loopback/Tailscale 边界继续生效；
- projectless 和 cwd provenance 不因新增 Composer 而丢失；
- V1 fixture 和回归测试继续作为 V2 发布门禁的一部分。

这些兼容性只描述已有行为。V2 的控制权限、命令、附件和安全例外必须以当前 V2 核心约束为准。

## 13. 被压缩的原始文档

本归档替代以下活动文档：

```text
docs/codex-local-observer-research.md
docs/codex-local-observer-detailed-design.md
docs/codex-local-observer-web-viewer-improvements.md
docs/codex-local-observer-future-design.md
docs/v1-validation.md
docs/decisions/0001-rust-sqlite-store-first-mvp.md
docs/decisions/0002-opt-in-read-only-live-adapter.md
docs/decisions/0003-content-addressed-redacted-blobs.md
docs/decisions/0004-purge-suppression-tombstones.md
docs/decisions/0005-single-db-writer-committed-event-bus.md
docs/decisions/0006-one-time-pairing-cookie.md
docs/decisions/0007-cwd-project-grouping.md
docs/decisions/0008-vite-preact-web-viewer.md
docs/decisions/0009-module-layering.md
docs/decisions/0010-tailscale-serve-loopback-forwarding.md
docs/decisions/0011-startup-scoped-reusable-local-token.md
docs/decisions/0012-projectless-thread-grouping.md
```

删除这些工作区文件不会删除 Git 历史。需要审计原文时使用：

```bash
git show 2dd0f6b:<path>
```

不要把旧文档重新加入当前开发入口；若历史结论需要重新成为规范，应在 V2 文档或新的 ADR 中重新验证和表述。
