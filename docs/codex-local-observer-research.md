# Codex 本地会话观察器 / Gateway：研究结论与实现方案

## 1. 目标

希望实现一个运行在本机的 **Codex Local Observer / Gateway**，能够：

- 发现并读取本机 Codex 的历史会话；
- 实时观察正在运行的 Codex 会话；
- 展示比普通聊天 UI 更完整的上下文：
  - Thread / Turn / Item
  - 用户消息与 Assistant 输出
  - Codex 对客户端公开的 reasoning / reasoning summary
  - Tool / MCP 调用与结果
  - Shell 命令
  - 文件修改
  - Approval / User Question
  - Token usage
  - Model / reasoning effort / sandbox / cwd
  - Sub-agent
  - Error / interrupt / completion
- 在本地 Web 前端中浏览、搜索和实时查看会话；
- 后续通过统一 API / Event Bus 接入 Telegram、飞书、企业微信、Discord 等 IM；
- 最终可扩展到：
  - 继续已有 Thread
  - 发送消息
  - interrupt
  - approve / deny
  - answer question
  - fork thread

核心原则：

> Observer 应该是 sidecar。即使关闭 Observer，原有 Codex CLI / App / IDE 仍应正常工作。

---

## 2. 需求边界

“完整看到模型和本地所有会话”建议定义为：

> 完整采集 Codex 官方接口、本地持久化存储和 runtime event 对客户端公开的数据。

不应把“模型未公开的隐藏 chain-of-thought”作为必须获取的数据。

目标数据可以分三层：

### L1 Transcript

- User message
- Assistant message

### L2 Execution

- Tool calls
- MCP
- Shell
- File edits
- Approval
- Errors
- Interrupt / completion

### L3 Agent Telemetry

- Reasoning summary / exposed reasoning
- Model
- Reasoning effort
- Token usage
- Thread settings
- Sub-agent
- Timing

---

## 3. 推荐总体架构

```text
Codex CLI ───────────────┐
Codex macOS App ─────────┼──────────────┐
Codex IDE Extension ─────┘              │
                                        │
                         ┌───────────────▼───────────────┐
                         │       Codex Observer          │
                         │                               │
                         │  1. App Server Adapter        │
                         │  2. Thread Store Adapter      │
                         │  3. Hook Adapter              │
                         │                               │
                         └───────────────┬───────────────┘
                                         │
                                  Normalized Event Bus
                                         │
                   ┌─────────────────────┼─────────────────────┐
                   │                     │                     │
                   ▼                     ▼                     ▼
                SQLite / DB          WebSocket / SSE        REST API
                   │                     │                     │
                   └──────────────┬──────┴───────────────┬────┘
                                  ▼                      ▼
                              Web Viewer              IM Adapter
                                                     Telegram
                                                     Feishu
                                                     WeCom
                                                     Discord
```

---

## 4. 推荐数据源优先级

优先级：

```text
1. codex app-server
2. thread/read / thread/list / thread/items/list / thread/turns/list
3. Codex local thread store / rollout store
4. filesystem watcher
5. hooks
6. runtime logs
7. HTTP / model API interception
```

### 为什么 App Server 优先

`codex app-server` 已经提供了官方的：

```text
Thread
  └─ Turn
      └─ Item
```

以及实时事件：

```text
thread/*
turn/*
item/*
```

它适合作为 Observer 的 canonical protocol。

### 为什么不优先 HTTP MITM

HTTP interception 看到的是“模型请求层”，未必完整包含：

- Codex thread lifecycle
- tool execution
- file edits
- approval
- MCP
- sub-agent
- Codex runtime state

因此 HTTP proxy 最多作为高级 debug adapter，而不是核心数据源。

---

## 5. Canonical Schema 建议

Observer 内部不要只保存：

```ts
messages: Message[]
```

建议沿用 Codex 的抽象：

```ts
interface Thread {
  id: string;
  cwd?: string;
  project?: string;

  model?: string;
  reasoningEffort?: string;

  status: string;

  createdAt?: string;
  updatedAt?: string;

  raw?: unknown;
}

interface Turn {
  id: string;
  threadId: string;

  status: string;

  usage?: unknown;

  raw?: unknown;
}

interface Item {
  id: string;
  threadId: string;
  turnId?: string;

  type: string;

  payload: unknown;

  // 永远保留 Codex 原始 payload
  raw: unknown;
}
```

重要原则：

> Normalization 不能丢失原始 Codex payload。

Web UI 可以显示标准化视图，同时提供 Raw JSON Inspector。

---

# 6. 当前最重要的源码研究问题

接下来研究 `openai/codex` 源码时，最重要的是回答下面这些问题。

## A. macOS Codex App 与 app-server 的关系

### Q1

**macOS Codex App 是否直接使用开源仓库中的 `codex app-server`？**

需要找到：

- App 如何启动 Codex backend；
- backend executable 是什么；
- 是否启动 `codex app-server`；
- 启动参数是什么。

---

### Q2

**macOS App 使用独立 app-server，还是连接共享的 app-server daemon？**

理想情况：

```text
Codex App ─────────┐
                   ▼
            app-server daemon
                   ▲
Observer ──────────┘
```

如果成立，Observer 可以作为第二个 client attach。

---

## B. App Server 是否支持旁路观察

### Q3

**同一个 app-server / daemon 是否允许多个客户端连接？**

重点检查：

- connection/session ownership
- transport accept loop
- client state
- subscriptions
- Thread manager 是否 process-global

---

### Q4

**`thread/list` 能否看到其他客户端创建的 Thread？**

需要区分：

```text
stored threads
loaded threads
threads owned by current connection
```

如果 `thread/list` 是全局 store 查询，则非常适合 Observer。

---

### Q5

**实时事件是全局广播还是 connection-scoped？**

例如：

```text
item/started
item/completed
item/agentMessage/delta
turn/completed
```

关键问题：

> Observer 未创建这个 thread 时，是否能够收到它的 events？

---

### Q6

如果 events 默认不是全局广播：

**是否可以对已有 Thread 执行某种 subscribe / resume / read 操作，从而无侵入监听？**

需要确认：

- `thread/resume` 是否改变 session 状态；
- 是否可能影响官方 App；
- 是否允许多个 client resume 同一个 thread；
- 是否存在专门 subscribe API。

---

## C. Codex 本地 Thread Store

### Q7

**Thread / Turn / Item 最终持久化在哪里？**

重点寻找：

- `$CODEX_HOME`
- SQLite
- rollout
- sessions
- JSONL
- state DB
- archive

---

### Q8

**CLI、macOS App、VS Code 是否共享同一个 CODEX_HOME / Thread Store？**

这是整个方案最关键的问题之一。

理想状态：

```text
CLI ──────────┐
App ──────────┼── shared thread store
VS Code ──────┘
```

如果成立，即使实时事件订阅有限，也能通过 watcher 做最终一致的完整发现。

---

### Q9

**一个正在运行中的 Turn，会以什么频率写入 store？**

需要判断：

```text
只在 Turn 完成后写入
vs
流式持续写入
```

这决定 filesystem watcher 能否用于 realtime。

---

## D. app-server daemon / control socket

### Q10

重点研究：

```text
$CODEX_HOME/app-server-control/
app-server-control.sock
```

需要回答：

- daemon 是谁启动的？
- socket 生命周期如何管理？
- App 是否使用它？
- 是否允许任意本地 client attach？
- 是否有 authentication / ownership check？
- 一个 daemon 管理多少 app-server session？

---

### Q11

研究：

```text
codex app-server proxy
```

明确它到底代理：

```text
observer
  ↓
control socket
  ↓
existing app-server
```

还是仅用于内部 daemon control plane。

---

## E. Remote Control

### Q12

重点阅读：

```text
codex-rs/cli/src/remote_control_cmd.rs
```

需要搞清：

- Remote Control 如何发现 active Codex；
- 如何获取 live threads；
- 如何把 approvals / terminal / diff / state 暴露出去；
- 是否直接复用了 app-server；
- 是否使用 daemon / relay；
- 本机是否已经存在可直接利用的状态接口。

Remote Control 很可能是理解“如何旁路访问 live Codex state”的最佳参考实现。

---

# 7. 建议优先追踪的源码目录

从以下目录开始：

```text
codex-rs/app-server/
codex-rs/app-server-daemon/
codex-rs/app-server-client/
codex-rs/app-server-protocol/
codex-rs/app-server-transport/

codex-rs/thread-store/
codex-rs/rollout/

codex-rs/core/src/session/

codex-rs/cli/src/remote_control_cmd.rs
```

重点搜索关键词：

```text
app-server-control
sock
unix
listen
daemon

thread/list
thread/read
thread/resume
thread/loaded/list

subscribe
broadcast
notification
connection

ThreadManager
SessionManager

CODEX_HOME

rollout
state_db
sqlite

remote_control
relay

item/started
item/completed
turn/completed
agentMessage/delta
```

---

# 8. 推荐的 V1

第一阶段只做 Read-only Observer。

目标：

```text
✓ 自动发现本机 Codex Thread
✓ 历史 Thread 列表
✓ Thread / Turn / Item 展示
✓ User / Assistant
✓ reasoning summary / exposed reasoning
✓ tool / MCP
✓ shell
✓ file edit
✓ approval
✓ token usage
✓ model / reasoning effort
✓ sub-agent
✓ realtime update
✓ raw JSON
✓ full-text search

× 暂不发送消息
× 暂不 approve
× 暂不接 IM
```

V1 的核心验收标准：

> 无论我从 Terminal、IDE 或 macOS App 使用 Codex，只要这些数据被 Codex 持久化或公开，Observer 都能够最终发现该 Thread，并尽可能实时重建完整的 Thread → Turn → Item 时间线。

---

# 9. V2：Gateway / Control Plane

确认 Observer 稳定之后，再增加：

```text
create thread
resume thread
send message
interrupt
approve / deny
answer user question
fork thread
change model
change reasoning effort
```

对外统一提供：

```text
REST
WebSocket / SSE
Webhook
```

---

# 10. V3：IM Bridge

IM 不直接理解 Codex protocol。

增加：

```ts
interface ConversationBinding {
  platform:
    | "telegram"
    | "feishu"
    | "wechat"
    | "discord";

  externalChatId: string;

  codexThreadId: string;

  cwd?: string;
}
```

数据链路：

```text
Telegram / Feishu / ...
          │
          ▼
      IM Adapter
          │
          ▼
     Codex Gateway
          │
          ▼
      thread/start
```

Codex events：

```text
Codex
  ↓
Observer Event Bus
  ↓
IM Adapter
  ↓
Telegram / Feishu / ...
```

---

# 11. 最理想的最终形态

如果源码研究证明：

1. macOS App 使用 app-server；
2. 存在共享 daemon；
3. Observer 可以作为第二客户端连接；
4. thread store 是共享的；

那么最终架构可以非常简单：

```text
               Codex macOS App
                      │
Codex CLI ─────── app-server / daemon ────── Codex IDE
                      │
                      │
                Codex Observer
                      │
              ┌───────┴───────┐
              ▼               ▼
          Web Viewer        IM Bridge
```

此时：

- 不需要 fork Codex；
- 不需要修改官方 App；
- 不需要 model API MITM；
- hooks 只作为 discovery / wake-up 补充；
- store watcher 作为历史补全 / fallback。

---

# 12. 如果理想情况不成立

退化方案：

```text
App Server Client
       +
Thread Store Reader
       +
Filesystem Watcher
       +
Hooks
```

形成最终一致的 Hybrid Observer：

```text
              App Server events
                    │
Codex Store ─── Normalizer ─── Event Bus
                    │
                  Hooks
```

即：

> realtime 尽量依靠 app-server，完整性依靠 thread store。

---

# 13. 当前阶段最需要确认的五个结论

基于当前仓库版本，结论不宜简单归并为全局 Yes / No，而应区分“同一 app-server 进程”和“共享 CODEX_HOME、不同进程”：

```text
[部分确认] 1. macOS Codex App 是否使用 codex app-server？
           仓库证明 desktop/mobile remote client 使用 app-server daemon，
           但不包含 macOS App 本身的启动代码，无法证明本机 App 的具体启动拓扑。

[条件成立] 2. 是否存在可被第二客户端 attach 的共享 daemon/socket？
           Unix control socket 和多连接 accept loop 已实现；
           但不能假定 CLI、IDE、App 都连接该 daemon。

[Yes] 3. thread/list / thread/read 是否能够读取其他客户端创建的 thread？
      它们查询进程配置的 ThreadStore，不按创建连接隔离；
      前提是各进程解析到相同 CODEX_HOME / SQLite home。

[条件成立] 4. 是否能够订阅其他客户端正在运行 thread 的实时 events？
           同一 app-server 进程内可以；跨 app-server 进程不可以。

[源码默认成立，产品资料未确认] 5. CLI / App / IDE 是否共享同一个本地 thread store？
                         开源客户端默认使用 ~/.codex，CODEX_HOME 可覆盖；
                         官方资料未说明 macOS App 是否覆写该值。
```

因此当前可执行结论是：

> `codex-observer` 可以完全建立在官方 app-server protocol + local ThreadStore 之上，且不需要 HTTP interception；但 V1 必须是 Hybrid Observer，不能只依赖 attach 一个全局 daemon。

---

## 项目暂定名称

推荐：

```text
codex-observer
```

或：

```text
codex-gateway
codex-viewer
codex-bridge
codex-local-gateway
```

其中架构层最准确的名字是：

> **Codex Local Observer & Gateway**

---

# 14. 源码研究结论（2026-08-11）

## 14.1 研究基线

- 仓库：当前工作区中的 `openai/codex` 官方源码；
- commit：`41ece455b7fa7166f4fc38522952afdaa2604e18`；
- 结论只描述该 commit 的实现，不承诺未来版本兼容；
- macOS Codex App 的产品源码不在本仓库内，因此涉及 App 启动方式的结论会明确标为“未由源码证明”。

## 14.2 Q1～Q12 汇总

| 问题 | 结论 | 对 Observer 的含义 |
| --- | --- | --- |
| Q1 macOS App 是否直接使用 app-server | **协议层高度相关，启动拓扑未证明** | 不应写死 App 的子进程名或启动参数；通过 socket/store 运行时探测 |
| Q2 App 是否连接共享 daemon | **仓库存在 daemon，但不能证明 App 默认使用** | daemon adapter 是 opportunistic fast path，不是唯一入口 |
| Q3 是否支持多客户端 | **Yes，同一 app-server 进程支持** | 一个 Unix socket 可同时连接官方客户端和 Observer |
| Q4 list/read 是否全局 | **对当前 ThreadStore 全局，不按 connection 隔离** | 可读取同一 store 中其他客户端创建的历史 Thread |
| Q5 events 是否全局广播 | **不是无条件全局广播，而是 thread subscriber scoped** | Observer 必须建立订阅；不能只 initialize 后等待 |
| Q6 能否监听已有 Thread | **同进程已加载 Thread 可以通过 resume 附着；跨进程不可以** | attach 前先 `thread/loaded/list`；跨进程只读 store |
| Q7 数据持久化位置 | **JSONL 是 canonical durable replay；SQLite 是索引/投影** | 完整性 fallback 以 rollout JSONL 为准，SQLite 用于发现与搜索 |
| Q8 各客户端是否共享 store | **相同 CODEX_HOME 时共享；默认是 `~/.codex`** | 需要记录每个 source 的实际 `codexHome`，不能只假设默认值 |
| Q9 运行中写入频率 | **按可持久化 item/event 增量 append，并 flush** | 文件 watcher 可做到 item/event 级最终一致，但拿不到 delta 和临时请求 |
| Q10 control socket | **Unix、多连接、权限 0600、默认固定路径** | 本机同用户可 attach；Windows 不能依赖该 daemon 实现 |
| Q11 app-server proxy | **单连接字节代理，不是额外控制协议** | 可用于 stdio 客户端复用现有 socket，但 Observer 直接 WebSocket over UDS 更合适 |
| Q12 Remote Control | **将完整 app-server JSON-RPC 封装进远端 relay stream** | 可参考其连接、重连、分片、ack 设计；它不是本地只读状态 API |

## 14.3 关键源码证据

### app-server transport 与 daemon

- app-server 默认 transport 是 stdio；Unix socket 是显式 `--listen unix://`，默认路径为 `$CODEX_HOME/app-server-control/app-server-control.sock`。`codex app-server proxy` 只把一个 stdio raw stream 代理到该 socket：[app-server/README.md](../codex-rs/app-server/README.md#L20-L44)。
- Unix socket accept loop 会为每次 accept 启动独立 WebSocket connection，因此明确支持多个客户端；socket mode 是 `0600`：[unix_socket.rs](../codex-rs/app-server-transport/src/transport/unix_socket.rs#L21-L88)、[unix_socket.rs](../codex-rs/app-server-transport/src/transport/unix_socket.rs#L158-L167)。
- daemon 当前仅支持 Unix，定位是为 SSH 机器上的 desktop/mobile remote client 管理 app-server；其默认启动参数确实是 `app-server --listen unix://`，按配置可加 `--remote-control`：[app-server-daemon/README.md](../codex-rs/app-server-daemon/README.md#L1-L15)、[pid.rs](../codex-rs/app-server-daemon/src/backend/pid.rs#L412-L421)。

### 多连接与订阅语义

- app-server 进程维护 `HashMap<ConnectionId, ConnectionState>`，每个 transport connection 有独立 initialize/capabilities 状态：[app-server/lib.rs](../codex-rs/app-server/src/lib.rs#L911-L997)。
- Thread 状态显式维护 `live_connections`、`threads` 和 `thread_ids_by_connection`，通知目标不是天然全局：[thread_state.rs](../codex-rs/app-server/src/thread_state.rs#L302-L320)、[thread_state.rs](../codex-rs/app-server/src/thread_state.rs#L472-L564)。
- 当同一进程新建 Thread 时，app-server 会把所有已初始化连接附着到该 Thread，因此较早连入的 Observer 能看到其他连接随后创建的 Thread：[app-server/lib.rs](../codex-rs/app-server/src/lib.rs#L1128-L1149)。
- 对同一进程内已经运行的 Thread，`thread/resume` 会把新 connection 加入订阅，并向该 connection 重放 pending server requests：[thread_lifecycle.rs](../codex-rs/app-server/src/request_processors/thread_lifecycle.rs#L650-L662)、[thread_lifecycle.rs](../codex-rs/app-server/src/request_processors/thread_lifecycle.rs#L725-L750)。
- `thread/unsubscribe` 是正式 API；最后一个订阅者离开后 Thread 也不会立刻卸载，而是空闲 30 分钟后关闭：[app-server/README.md](../codex-rs/app-server/README.md#L193-L198)。

这里有一个不能忽略的行为：approval、request user input、MCP elicitation 等 server-initiated request 会发送给 Thread 的所有 subscriber；这些发送共用同一个 request ID 和 callback，因此任一先到的有效 response 会解决请求：[outgoing_message.rs](../codex-rs/app-server/src/outgoing_message.rs#L295-L359)、[outgoing_message.rs](../codex-rs/app-server/src/outgoing_message.rs#L383-L402)。Read-only Observer 必须采集但不得自动响应，控制能力必须通过单独授权层开启。

另一个边界是 `thread/resume` 并非纯 subscribe：它会生成 resume snapshot、重放 pending request、改变 subscriber/unload 状态，并在空闲时触发 thread idle lifecycle。它没有启动新 Turn，但仍可能被 hooks/extensions 观察到。因此 live attach 必须可配置、通过官方 app-server 集成测试，并在断开前调用 `thread/unsubscribe`；不能把它描述成绝对零副作用。

### ThreadStore 与持久化

- LocalThreadStore 的源码注释明确规定：rollout JSONL 是 durable replay format，SQLite 是可查询 metadata index；live append 仍先写 canonical JSONL：[local/mod.rs](../codex-rs/thread-store/src/local/mod.rs#L90-L110)。
- 默认 rollout 路径是 `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<time>-<thread-id>.jsonl`；archive 在 `$CODEX_HOME/archived_sessions/`：[recorder.rs](../codex-rs/rollout/src/recorder.rs#L1549-L1572)。冷文件还可能被压缩成 `.jsonl.zst`，官方 line reader 会透明处理两种物理表示：[compression.rs](../codex-rs/rollout/src/compression.rs#L18-L55)。
- SQLite 默认位于 SQLite home（默认等于 CODEX_HOME），当前相关文件为 `state_5.sqlite` 和 `thread_history_1.sqlite`；文件名带版本号，Observer 不应把表结构视为稳定 API：[state/sqlite.rs](../codex-rs/state/src/sqlite.rs#L29-L47)。
- 每次可持久化 append 都会先过滤，再写 recorder 并 flush；paginated history 随后投影到 SQLite，源码明确称 SQLite 为 rebuildable view：[live_writer.rs](../codex-rs/thread-store/src/local/live_writer.rs#L280-L332)。
- store 不保存所有 runtime event。approval request、request user input、item started、stream delta、terminal delta、warning/error 等大量事件被明确标记为 transient/non-durable：[rollout/policy.rs](../codex-rs/rollout/src/policy.rs#L121-L182)。因此 watcher 能提供“最终历史”，不能还原错过的完整实时过程。
- Thread 有跨进程 writer lock。另一个进程 cold-resume 正在运行的 Thread 会得到 `already has an active writer` 冲突：[writer_lock.rs](../codex-rs/thread-store/src/local/writer_lock.rs#L17-L69)。这正是不能跨进程用 `thread/resume` 冒充 subscribe 的原因。

### 官方 API 覆盖

- `thread/list` 查询 stored threads，`thread/loaded/list` 只反映当前 app-server 进程内存，`thread/read` 不 resume 即可读历史；`thread/turns/list` 和 `thread/items/list` 提供分页历史，但 items list 依赖 store 支持：[app-server/README.md](../codex-rs/app-server/README.md#L172-L180)。
- 默认 `thread/list` 只查询 interactive sources。要完整发现 exec、app-server、sub-agent 等 Thread，Observer 必须显式枚举 `sourceKinds`，不能使用默认参数：[filters.rs](../codex-rs/app-server/src/filters.rs#L6-L35)。
- 实时 item 生命周期是 `item/started → delta* → item/completed`，completed item 才是结果权威；Turn 完成通知只携带最后一条 agent message 作为 fallback：[app-server/README.md](../codex-rs/app-server/README.md#L1555-L1569)、[app-server/README.md](../codex-rs/app-server/README.md#L1593-L1621)。
- 当前公开 item 类型已经覆盖 user/agent message、plan、reasoning、command、file change、MCP、collab/sub-agent、web search、image generation/view、sleep、review 和 compaction：[app-server/README.md](../codex-rs/app-server/README.md#L1571-L1591)。

### CODEX_HOME 与共享范围

开源实现统一通过 `CODEX_HOME` 解析配置和会话根目录，未设置时为 `~/.codex`：[home-dir/lib.rs](../codex-rs/utils/home-dir/src/lib.rs#L5-L17)、[home-dir/lib.rs](../codex-rs/utils/home-dir/src/lib.rs#L52-L60)。SQLite 还允许用 config/requirements 或 `CODEX_SQLITE_HOME` 单独覆盖。因此 Observer 的 source identity 至少应包含：

```text
codex_home
sqlite_home
app_server_socket
app_server_version
process/connection epoch
```

不能只用 `threadId` 判断两个数据源是否完全等价。

---

# 15. 修订后的 V1 架构

## 15.1 结论

V1 应实现两条并行采集链路，外加一个可选唤醒链路：

```text
Path A：同进程实时
existing app-server Unix socket
  -> initialize
  -> thread/loaded/list
  -> 对“该进程已加载”的 thread/resume
  -> thread/* + turn/* + item/* + server requests

Path B：跨进程最终一致
CODEX_HOME/sessions + archived_sessions
  -> discover / watch
  -> incremental JSONL tail
  -> .jsonl.zst cold import
  -> periodic thread/list/read reconciliation（可用时）

Path C：可选 wake-up
hooks / writer-lock / directory event
  -> 只触发 rescan
  -> 不作为 canonical payload
```

归一化后的事件统一进入 Observer 自己的 append-only event log，再生成 Thread/Turn/Item projection 和 WebSocket/SSE 输出。

## 15.2 为什么不是“只启动一个 app-server”

Observer 自己启动的 app-server 可以读取共享 store，但看不到其他 app-server 进程中的 transient events。反过来，只 attach daemon 又会漏掉使用默认 stdio app-server 或直接运行 core/TUI 的会话。Hybrid 不是保守 fallback，而是满足 V1 验收标准的必要结构。

## 15.3 数据源能力矩阵

| 能力 | 同进程 app-server attach | `thread/read/list` | JSONL / SQLite | Hook |
| --- | --- | --- | --- | --- |
| 历史 Thread 发现 | Yes | Yes | Yes | No |
| 完整 durable item | Yes | Yes | Yes，以 JSONL 为准 | No |
| agent/reasoning delta | Yes | No | No | No |
| command output delta | Yes | No | No | No |
| pending approval/question | Yes，可重放 | No | No | 部分事件可提示，但不完整 |
| 最终 command/file/MCP 结果 | Yes | Yes | 通常 Yes | 部分 |
| token usage | Yes | 可从持久化快照恢复一部分 | TokenCount 持久化 | No |
| live thread status | Yes，process-local | 仅被查询 app-server 的 process-local status | 推断值 | No |
| 跨 CLI/App/IDE 进程 | No | Yes，前提是共享 store | Yes，前提是共享 store | 取决于配置 |
| 原始 payload | Yes | API 转换后的 payload | Rollout 原始行 | Hook 自身 payload |

`captureCompleteness` 必须成为 UI 可见字段，至少区分：

```text
live_complete       # 从 turn 开始持续订阅，包含 delta/request
live_partial        # 中途 attach，可能缺早期 transient event
durable_complete    # durable rollout 已完整导入
durable_partial     # 文件仍在增长、存在坏行或版本未知
metadata_only       # 只有 thread metadata
ephemeral_lost      # 已知存在 ephemeral thread，但无法从 store 补回
```

---

# 16. Observer 内部协议与存储设计

## 16.1 Event Envelope

原先的 Thread/Turn/Item schema 应保留，但还需要一个不可变事件层，避免把 delta、request 和 snapshot 强行塞进 Item：

```ts
interface ObserverEvent {
  eventId: string;              // Observer 生成，单调写入
  sourceId: string;             // codexHome + socket/process identity
  sourceKind: "app_server" | "rollout" | "sqlite" | "hook";
  sourceEpoch: string;          // 每次连接/文件 incarnation 变化
  sourceSeq?: number;           // 同一连接内接收顺序或 JSONL ordinal

  observedAt: string;
  eventAt?: string;

  threadId?: string;
  turnId?: string;
  itemId?: string;
  requestId?: string;           // 必须与 sourceEpoch 一起使用

  method: string;               // 原 app-server method 或 rollout item tag
  phase: "snapshot" | "started" | "delta" | "completed" | "request" | "resolved";
  durability: "transient" | "durable" | "derived";

  payload: unknown;             // 标准化 payload
  raw: unknown;                 // 原始 JSON-RPC message / JSONL line decoded JSON
  sourceFingerprint: string;   // 安装级 keyed hash，用于去重
  storedRawHash: string;       // 脱敏后 payload 的完整性 hash
}
```

Observer 的 `Thread` 还应补充当前官方字段：

```ts
interface ThreadProjection {
  id: string;
  sessionId?: string;
  parentThreadId?: string;
  forkedFromId?: string;
  source?: string;
  threadSource?: string;
  historyMode?: "legacy" | "paginated";
  path?: string;
  cwd?: string;
  modelProvider?: string;
  model?: string;
  reasoningEffort?: string;
  approvalPolicy?: string;
  approvalsReviewer?: string;
  sandbox?: unknown;
  activePermissionProfile?: unknown;
  status?: string;
  createdAt?: number;
  updatedAt?: number;
  recencyAt?: number;
  captureCompleteness: string;
  raw: unknown;
}
```

不要把 `model`、`reasoningEffort` 和 sandbox 只放在 Thread 上。它们可以被 turn override 或 settings update 改变，建议增加 `TurnExecutionContext` snapshot，并记录有效时间范围。

## 16.2 Observer SQLite 最小表

```text
sources
source_checkpoints          # socket cursor / file identity + byte offset
raw_events                  # append-only，raw_json + hash
threads
turns
items
pending_requests            # approval / question / elicitation
item_deltas                 # 可配置保留；大输出可 spill 到 blob
thread_source_coverage
conversation_bindings       # V3 IM binding
```

推荐启用 FTS5，但索引只包含用户可见文本、reasoning summary、命令展示文本和最终输出摘要。raw JSON、凭据、环境变量、图片 base64 不进入 FTS。

## 16.3 去重与合并规则

1. raw event 永远 append，不覆盖；projection 才做 upsert。
2. app-server connection 内按接收顺序排序；不能用 wall clock 重排 delta。
3. Item 主键优先使用 `(threadId, turnId, itemId)`；缺 turnId 时保留 source scope，等待后续关联。
4. `item/completed` 覆盖 started/delta 构造出的展示字段，但不删除 delta 原始事件。
5. 同一 durable JSONL record 用 `(storeSourceId, threadId, ordinal, sourceFingerprint)` 去重；文件 rename/archive 后保持 logical rollout identity。
6. server request ID 只在 connection/source epoch 内唯一，不能全局以 `requestId` 去重。
7. app-server snapshot 与 JSONL 冲突时按字段合并：runtime 状态取 live，durable history 取 JSONL，metadata 取最新官方 API/SQLite，所有原始值保留 provenance。
8. `turn/completed` 不是完整 items snapshot，不能用它删除此前收集的 items。

---

# 17. Adapter 详细实现

## 17.1 AppServerAdapter

连接流程：

```text
1. 发现 $CODEX_HOME/app-server-control/app-server-control.sock
2. 校验 owner、file type、权限；拒绝跟随不可信 symlink
3. WebSocket over Unix socket
4. initialize（声明 clientInfo；V1 默认不开 experimentalRawEvents）
5. initialized
6. 分页调用 thread/loaded/list
7. 仅对该返回集合调用 thread/resume
8. 新建 Thread 依靠 app-server auto-attach
9. 正常退出时对已附着 Thread 调用 thread/unsubscribe
10. 断线后重新 initialize，并从 thread/loaded/list 重建订阅
11. 周期性 thread/list/read 做 durable reconciliation
```

重要约束：

- 不对“只在 store 中存在、但不在该 socket 的 `thread/loaded/list` 中”的 Thread 调用 resume；
- V1 对所有 server request 只记录，不 response；
- 不启用会改变 Thread 配置的 resume overrides；
- 将 live attach 作为独立开关；未完成目标 App/IDE 版本的副作用回归前，可默认关闭，只运行 store observer；
- attach 后标记为 `live_partial`，只有完整观察到一个新的 `turn/started` 到 `turn/completed` 才能把该 Turn 标记为 `live_complete`；
- 显式枚举全部 `sourceKinds`，并分别查询 active/archived；
- 对不认识的 method/union variant 仍保存 raw JSON，不能因 schema decode 失败断开整个 source。

## 17.2 RolloutStoreAdapter

监视范围：

```text
$CODEX_HOME/sessions/**/rollout-*.jsonl
$CODEX_HOME/sessions/**/rollout-*.jsonl.zst
$CODEX_HOME/archived_sessions/**
$CODEX_HOME/thread-writer-locks/*.lock    # 仅作 active hint
```

增量读取必须处理：

- 最后一行尚未完成：缓存 partial bytes，直到出现换行；
- rename/archive：按 ThreadId 和 SessionMeta 识别 logical file，不按绝对路径新建 Thread；
- truncate/replace/inode change：创建新的 source epoch，从头校验并按 `sourceFingerprint` 去重；
- `.jsonl.zst`：作为 cold immutable import，不尝试 byte tail；
- plain 与 compressed sibling 同时存在：遵循官方逻辑，优先 plain；
- 单行解析失败：记录 ingest error 和 offset，继续后续行，不让整个扫描停止；
- 未知 rollout item：保存 raw，projection 标记 `unknown`；
- 定期全量 rescan：filesystem notification 只用于降低延迟，不能作为发现完整性的依据。

Observer 不应直接写入 Codex 的 JSONL、SQLite、writer lock 或 daemon state。若需要利用 SQLite，只用 read-only URI 打开，并先识别 `PRAGMA user_version` / 文件名版本；不认识的版本降级到 JSONL。

## 17.3 HookAdapter

Hooks 适合提供 `SessionStart`、`SessionEnd`、`UserPromptSubmit`、tool/sub-agent lifecycle 的低延迟 wake-up，但它会执行用户配置的命令、可能受 trust/managed policy 限制，也不覆盖所有 delta。V1 可以完全不依赖 hooks；若启用，只把 hook payload 写成 source event，然后触发对应 CODEX_HOME rescan。

---

# 18. 对外 Read-only API（V1）

建议最小 API：

```text
GET  /v1/sources
GET  /v1/threads?cursor=&limit=&source=&status=&q=
GET  /v1/threads/{threadId}
GET  /v1/threads/{threadId}/turns
GET  /v1/threads/{threadId}/items
GET  /v1/threads/{threadId}/events?afterEventId=
GET  /v1/search?q=
GET  /v1/health
WS   /v1/events
SSE  /v1/events
```

Event stream 至少支持：

- `afterEventId` 断点续传；
- bounded replay window；
- slow consumer backpressure / disconnect；
- filter by thread/source/method；
- payload size cap，大型 command output、diff、image 结果使用 blob reference；
- schema version 与 raw payload 并存。

Web UI 的 Thread 页建议按以下顺序渲染：

```text
Thread header（source/cwd/model/status/capture completeness）
  -> Turn timeline
     -> Item cards
        -> live delta / final state
        -> approval/question request state
        -> raw JSON inspector
  -> provenance / ingest diagnostics
```

---

# 19. V2 Control Plane 的额外约束

V2 不应直接把 app-server JSON-RPC 原样暴露给 IM 或浏览器。增加 capability-based command layer：

```text
observer.read
thread.create
thread.send
thread.interrupt
request.answer
approval.accept
approval.decline
thread.fork
```

每个 mutation 必须绑定到确定的 live source：

```ts
interface CommandTarget {
  sourceId: string;
  sourceEpoch: string;
  threadId: string;
  expectedTurnId?: string;
  expectedRequestId?: string;
}
```

若原 source 已断线，Gateway 不得悄悄在自己的 app-server 中 cold resume 一个可能仍由别的进程持有的 Thread。应返回 `SOURCE_NOT_LIVE`，由用户明确选择“等待原 source”或“在确认无 active writer 后接管”。

Approval / question 需要 compare-and-set：只有 pending request 仍存在、source epoch 匹配时才允许 response；所有 IM 操作必须带幂等键和审计记录。

---

# 20. 安全与隐私基线

Observer 会集中展示 shell、文件路径、diff、MCP 参数/结果和对话，敏感度高于普通聊天记录。最低要求：

- 默认只监听 loopback 或 Unix socket，不绑定 `0.0.0.0`；
- Web/REST 即使在 loopback 也使用随机 bearer token，并设置严格 Origin 校验；
- Observer DB 和 socket 权限限制为当前用户；
- 不记录 auth token、完整环境变量、MCP OAuth secret；
- 图片/audio/base64 和大型 tool result 放入受权限保护的 blob store；
- UI 默认转义 Markdown/ANSI/HTML，raw inspector 禁止执行内容；
- 外部 IM 默认只发送摘要，不自动外发 cwd、diff、命令输出或 reasoning；
- read-only 与 control 进程/令牌分权；
- 任何 approval response 都写 audit event；
- 提供 retention、按 Thread 删除 Observer 副本、暂停采集与数据导出能力。

本机 control socket 的 `0600` 只提供 OS 用户级隔离，不等于 Observer Web API 已安全。把 Observer 暴露到 LAN/公网前必须另做认证、CSRF/Origin、防重放和速率限制。

---

# 21. 分阶段实施计划与验收

## Phase 0：官方契约冻结与能力发现

当前研究范围只使用官方源码和官方 OpenAI 文档，不把桌面 App 逆向、进程注入或未公开运行参数作为证据。实现开始前先冻结一份版本化 capability manifest：

```text
Codex source commit
目标 Codex CLI version
app-server stable JSON Schema hash
observer 支持的 method / notification / request 清单
experimentalApi = false 时的可用能力
rollout 顶层 type 与兼容版本清单
```

运行时只通过官方接口做 feature detection，例如 initialize、`thread/loaded/list`、`thread/list` 和 schema generation；未公开的 macOS App 启动拓扑继续标记为未知，不作为 V1 正确性的前提。官方文档说明生成的 TypeScript/JSON Schema 与执行生成命令的 Codex 版本精确匹配，因此可以将 schema hash 纳入兼容性测试：[OpenAI Docs：Codex App Server](https://learn.chatgpt.com/docs/app-server)。

## Phase 1：Historical Import

- 扫描 JSONL / `.zst`；
- 建立 raw event log 与 Thread/Turn/Item projection；
- 支持 archive、sub-agent parent relation、reasoning/tool/file/usage；
- 支持断点与幂等重扫。

验收：重复导入结果完全相同；随机终止后重启不丢、不重；历史列表与官方 `thread/list` 对账。

## Phase 2：Realtime Attach + Watcher

- attach 已存在 daemon socket；
- loaded/resume subscription；
- JSONL tail；
- WebSocket/SSE；
- completeness 与 provenance UI。

验收：

1. 同 socket 的另一客户端启动 Turn，Observer 收到 started/delta/completed；
2. 独立 CLI stdio 运行 Turn，Observer 至少在 durable append 后更新；
3. Observer 关闭或崩溃不影响官方客户端；
4. Observer 不响应 approval 时，官方客户端仍能正常响应；
5. 中途 attach、断线重连、archive、压缩转换均不会复制 Item。

## Phase 3：Read-only Web Viewer

- timeline、search、raw inspector、capture health；
- 明确标识 transient 缺失、unknown variant、source disconnected；
- 加入 retention 与访问控制。

## Phase 4：Gateway Control

只在 read-only 稳定后实现 send/interrupt/approve/question/fork，并先限定为本机已连接 source。Remote Control 的 relay envelope/ack/reconnect 可以作为设计参考，但不直接复制其云端认证假设。

---

# 22. 官方资料尚未确认、实现中需保留的问题

以下问题不能由当前官方源码和官方 OpenAI 文档关闭。实现测试可以验证 Observer 自身的兼容与降级行为，但不得把桌面 App 逆向或私有运行参数当作正式产品事实：

1. 当前安装的 macOS App 是否创建/连接默认 `app-server-control.sock`，其 backend binary 与参数是什么；
2. App、IDE、CLI 实际解析出的 `codexHome` 和 `sqliteHome` 是否一致；
3. macOS App 使用的 `SessionSource` 是 `vscode`、`custom("chatgpt")` 还是其他值；
4. Observer 作为第二 subscriber 时，官方 App 对重复收到/竞争响应的 pending request 的实际处理是否符合预期；
5. daemon 重启与 App 升级时 socket/process epoch 如何变化；
6. 默认新建 Thread 当前采用 legacy 还是 paginated history，`thread/items/list` 在目标安装版本上的可用性；
7. ephemeral Thread 是否存在任何产品级 discovery API；若没有，Observer 未在创建时 attach 就无法补回；
8. 大型 command output/diff 的真实峰值与保留策略；
9. JSONL compression worker 对活跃/冷文件的时间阈值，以验证 watcher 的 rename 处理；
10. Windows 目标是否需要以自建 stdio app-server + store watcher 替代 daemon socket。

---

# 23. 最终建议

当前源码已经足以确定技术路线：

```text
官方 app-server v2 protocol = 实时与控制的 canonical protocol
官方 rollout JSONL          = durable completeness 的 canonical source
官方 SQLite                 = discovery/search accelerator，不是唯一事实源
hooks                       = optional wake-up
HTTP MITM                   = 不进入主方案
```

最小可落地版本应先实现 `RolloutStoreAdapter + Observer event log + Web Viewer`，再增加对用户显式配置的 Unix app-server endpoint 的 `AppServerAdapter`。这样即使官方资料始终不说明 macOS App 是否使用共享 daemon，V1 仍然成立；未来若官方确认共享 endpoint，也无需改变数据模型，只会把相应 Thread 的 capture completeness 从 durable 提升到 live。

---

# 24. 可直接开发的 V1 实现规格

## 24.1 证据边界与依赖政策

截至 2026-08-12，官方 OpenAI 文档已经明确：App Server 是 Codex 用于 rich clients 的开放集成接口，提供 authentication、conversation history、approvals 和 streamed agent events；实现源码也公开在本仓库中：[OpenAI Docs：Codex App Server](https://learn.chatgpt.com/docs/app-server)。因此 V1 可以把 app-server v2 当作官方公开集成边界；具体稳定性仍以 method 的 experimental 标记和目标版本生成的 schema 为准。

同一官方页面当前也把 app-server command / WebSocket transport 标为 experimental，且未承诺 production workload support。Unix socket transport 本质上仍是 WebSocket over UDS，因此 live adapter 必须保持 opt-in、版本固定和 fail-closed；store-first importer 才是 V1 的生产完整性基础。

实现采用以下依赖政策：

1. **协议依赖**：使用 app-server JSON-RPC v2 stable surface；默认不声明 `experimentalApi`。
2. **持久化依赖**：只读取 rollout JSONL / `.jsonl.zst`；不写入 Codex store。
3. **索引依赖**：不直接读取 Codex 的版本化 SQLite 表。发现失败时扫描 JSONL，不与 `state_5.sqlite` 等文件名绑定。
4. **代码依赖**：Observer 生产包不直接依赖 `codex-core`、`codex-thread-store` 或其他内部 crate，避免跟随内部重构发布。
5. **schema 依赖**：开发和 CI 使用目标 Codex 版本生成的 JSON Schema 做兼容性测试；运行时先保存原始 JSON，再做容错投影。
6. **未知字段政策**：未知 method、未知 enum variant、未知 rollout `type` 全部落入 `unknown` projection，不丢弃、不导致 source 断线。

源码中的 stable/experimental 方法注册集中在 [common.rs](../codex-rs/app-server-protocol/src/protocol/common.rs#L675-L695)，完整通知 union 可由生成的 [ServerNotification.ts](../codex-rs/app-server-protocol/schema/typescript/ServerNotification.ts#L76-L79) 对账。rollout 顶层格式是 `type + payload` tagged union：[history/src/lib.rs](../codex-rs/history/src/lib.rs#L27-L39)。

## 24.2 技术选型

V1 推荐实现为独立 Rust sidecar：

| 领域 | 选择 | 原因 |
| --- | --- | --- |
| async runtime | Tokio | 适合 socket、watcher、API 并发任务 |
| HTTP / WS / SSE | Axum | 一个进程内提供只读 API 和静态 UI |
| Unix WebSocket | tokio-tungstenite + UnixStream | 直接连接官方 control socket |
| JSON | serde_json | 允许 raw-first、typed-later 的容错解析 |
| SQLite | rusqlite，单独 writer thread | 避免多个 async task 竞争写锁，事务顺序清晰 |
| filesystem hint | notify | 只触发 rescan，不承担完整性 |
| compression | zstd | 流式读取官方 `.jsonl.zst` |
| hash | BLAKE3 | 生成 raw hash、文件指纹和 dedupe key |
| IDs | UUIDv7 或 ULID | Observer event id 可排序且不依赖墙钟唯一性 |
| logs | tracing | source/thread/epoch 结构化诊断 |

建议单独建立 `observer/` 工程，不把产品代码放进 `codex-rs/core`：

```text
observer/
  Cargo.toml
  migrations/
  fixtures/
    app_server/<codex-version>/
    rollout/<codex-version>/
  src/
    main.rs
    config.rs
    supervisor.rs
    domain/
      event.rs
      projection.rs
      completeness.rs
    adapters/
      app_server.rs
      rollout.rs
      hook.rs
    ingest/
      normalizer.rs
      projector.rs
      redaction.rs
    storage/
      writer.rs
      reader.rs
      migrations.rs
    api/
      rest.rs
      stream.rs
      auth.rs
    commands/
      doctor.rs
      import.rs
      serve.rs
  web/
```

V1 保持一个 daemon 二进制 `codex-observerd`，CLI 子命令只控制相同核心：

```text
codex-observerd doctor
codex-observerd import
codex-observerd serve
codex-observerd rebuild-projections
```

## 24.3 进程内模块和数据流

```text
SourceSupervisor
  ├─ RolloutScanner ──┐
  ├─ RolloutWatcher ──┤
  ├─ AppServerClient ─┼─> bounded IngestQueue
  └─ HookReceiver ────┘          │
                                 ▼
                         Normalizer / Redactor
                                 │ IngestBatch
                                 ▼
                       single SQLite DbWriter
                          │             │
                          │ commit      │ projection update
                          ▼             ▼
                     CommittedBus    QueryPool
                          │             │
                          ├────> WS/SSE │
                          └────> REST/Web UI
```

关键事务原则：

> raw event、projection 更新和 source checkpoint 必须在同一个 SQLite transaction 中提交；只有 transaction commit 后才能向 WS/SSE 发布事件。

这保证：

- crash 发生在 commit 前：checkpoint 未前移，重启后重读并由 dedupe 去重；
- crash 发生在 commit 后：projection 和 checkpoint 同时存在，不会出现 API 已推送但数据库没有记录；
- API 慢消费者不会反向阻塞 Codex socket reader，超过队列上限时断开 API consumer，而不是丢采集事件。

采集队列必须 bounded。默认建议：

```text
ingest queue: 4096 events
DB group commit: 100 events 或 50ms
API consumer queue: 512 events / connection
raw JSON max: 64 MiB / event，可配置
inline blob max: 256 KiB
```

当 ingest queue 达到高水位时，AppServerClient 暂停读取产生 WebSocket backpressure；RolloutScanner 暂停扫描但不丢 checkpoint。若持续超过健康阈值，`/v1/health` 返回 degraded。

---

# 25. Adapter 可编码规格

## 25.1 SourceRegistry

配置只接受显式 `codexHomes`，未配置时使用当前官方默认 home。不要遍历所有用户目录。每个 store source 生成稳定标识：

```text
storeSourceId = blake3("store\0" + canonicalCodexHome + "\0" + canonicalSqliteHome)
socketSourceId = blake3("app-server\0" + canonicalSocketPath)
```

Codex Thread ID 在单一 store 中可以直接使用；Observer API 使用复合键，避免多个 home 发生身份混淆：

```text
observerThreadKey = base64url(storeSourceId + "\0" + codexThreadId)
```

必须同时保存原始 `codexThreadId`，不能让 UI 或 IM 把 Observer key 回传给 app-server。

启动顺序采用 scan-watch-rescan，避免 watcher 注册窗口丢文件变化：

```text
1. 打开 Observer DB、执行 migration、取得单实例锁
2. 初次全量扫描 sessions / archived_sessions
3. 注册 filesystem watcher
4. 再次扫描并对账
5. 启动周期性 rescan
6. 若允许 live mode，再连接已配置的 app-server socket
7. API readiness = DB ready；source 未连接只表现为 degraded
```

## 25.2 AppServerAdapter 状态机

配置将实时模式拆成三个清晰选项：

```text
off            # 完全不连接 app-server，默认值
observe_new    # initialize 后观察同进程随后创建的 Thread
attach_loaded  # 额外对 thread/loaded/list 返回值执行 thread/resume
```

`off` 是最保守默认值，因为连接成为 subscriber 本身可能改变 Thread 的卸载时机。`attach_loaded` 必须显式开启，并受第 30 节研究门禁约束。

连接状态机：

```text
Disabled
  -> Discovering
  -> Connecting
  -> Initializing
  -> Ready
  -> Backoff
  -> Connecting
```

每次成功完成 `initialize` + `initialized` 都创建新的随机 `sourceEpoch`，`sourceSeq` 从 1 递增。退避建议为带 jitter 的 `0.5s, 1s, 2s, 4s ... 30s`。

Ready 后按顺序执行：

1. 保存 initialize response、目标 Codex version 和 capability raw JSON；
2. 若模式是 `attach_loaded`，分页获取 `thread/loaded/list`；
3. 对每个返回 ID 调用无 override 的 `thread/resume`；
4. resume 失败只标记该 Thread 的 live capability，不使整个 source 失败；
5. 定期调用 `thread/list`，分别查询 archived=false/true，并显式传全部已知 `sourceKinds`；
6. 正常退出时对本连接已附着的 Thread 调用 `thread/unsubscribe`；
7. 断线后把该 epoch 的 runtime projection 标记为 stale，不能把 stale active 状态继续展示为当前状态。

接收路径必须先保存 envelope，再解释类型：

```text
WebSocket frame
  -> validate UTF-8 / JSON object
  -> assign sourceSeq
  -> persist raw-redacted envelope
  -> classify: response | notification | server request
  -> minimal typed extraction
  -> projection
```

Server request 处理规则：

- approval、request user input、MCP elicitation 记录到 `pending_requests`；
- V1 绝不发送 result/error response；
- `serverRequest/resolved` 或 source epoch 结束时关闭本地 pending 状态；
- request ID 的作用域是 `(socketSourceId, sourceEpoch, requestId)`；
- 不能仅按 `requestId` 做全局去重。

官方文档明确 `thread/read` 不 resume、不加载 Thread、也不订阅事件，因此 durable reconciliation 只能使用 `thread/read`，不能偷偷替换成 `thread/resume`：[OpenAI Docs：Read a stored thread](https://learn.chatgpt.com/docs/app-server#read-a-stored-thread-without-resuming)。

## 25.3 RolloutStoreAdapter 增量算法

### Plain JSONL

对每个 logical rollout 保存：

```text
thread_id
current_path
file_identity
source_epoch
committed_byte_offset
committed_ordinal
last_complete_line_hash
scan_status
```

读取算法：

1. 从 `committed_byte_offset` 打开文件；
2. 只处理以换行结束的完整 record；
3. 为 record 计算 BLAKE3，生成 dedupe key；
4. 按最多 100 条组成 `IngestBatch`；
5. batch 中最后一个完整换行后的 byte offset 与 ordinal 随 transaction 提交；
6. EOF 的不完整行不移动 checkpoint，下次从旧 offset 重读；
7. 单行 JSON decode 失败时保存 ingest error、offset、hash 和有限 preview，并继续后续完整行；
8. 超过 `maxRawEventBytes` 时记录 oversize error，不把整行载入 API；原 Codex 文件保持事实源。

`file_identity` 在 Unix 使用 device + inode，在 Windows 使用 volume/file id；拿不到稳定 identity 时使用 `size + mtime + first-4KiB-hash` 作为弱指纹并标记 `identityConfidence=weak`。

### Rename、archive、truncate

- path 变化但 SessionMeta 的 Thread ID 与已知 rollout 一致：更新 location，不创建新 Thread；
- inode 变化、size 小于 checkpoint、首 record hash 变化：结束旧 epoch，创建新 epoch，从 0 重扫；
- 同一 Thread 同时出现 active 与 archived path：保存两个 location，选择 plain active 作为 current，后台对内容 hash 对账；
- watcher 事件只 enqueue 父目录 rescan，并做 200ms debounce；每 30s 仍执行全量 rescan。

### `.jsonl.zst`

`.zst` 视为 cold immutable source：

1. 若同名 plain 文件存在，优先读取 plain；
2. 流式解压并按 ordinal 处理，不尝试 byte tail；
3. 每 500 条提交一次 checkpoint ordinal；
4. crash 后从头解压并快速跳过已提交 ordinal；
5. 完整读到 zstd stream end 后标记 `importComplete=true`；
6. compressed file 指纹变化时创建新 epoch并重新验证。

官方 reader 本身也会透明选择 plain 或 compressed representation：[compression.rs](../codex-rs/rollout/src/compression.rs#L41-L55)。Observer 自己实现相同优先级，但不调用会 materialize/修改文件的 append API。

### Rollout 最小解析

第一版只要求识别顶层：

```text
session_meta
response_item
inter_agent_communication
inter_agent_communication_metadata
compacted
turn_context
world_state
event_msg
```

SessionMeta 至少抽取 id、session_id、parent/fork、cwd、originator、cli_version、source、thread_source、model_provider、history_mode 和 history_base；当前源码字段定义见 [protocol.rs](../codex-rs/protocol/src/protocol.rs#L2849-L2911)。每个 payload 仍完整保留为 raw-redacted JSON。

## 25.4 HookAdapter

Hook 不是 V1 必选项。启用后使用一个很窄的本机 Unix datagram/HTTP loopback receiver：

```text
hook event -> authenticate local secret -> persist wake-up event -> enqueue source rescan
```

Hook payload 不覆盖或替代 rollout/app-server 事件。相同业务动作即使同时来自 hook 与 rollout，也保存两条有不同 provenance 的 raw event；projection 层再关联。

---

# 26. SQLite 物理模型与事务

## 26.1 最小 DDL

下面是逻辑 DDL；实际 migration 可拆分索引和 FTS：

```sql
CREATE TABLE sources (
  source_id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  stable_identity TEXT NOT NULL UNIQUE,
  config_json TEXT NOT NULL,
  status TEXT NOT NULL,
  last_seen_at_ms INTEGER,
  last_error_json TEXT
);

CREATE TABLE source_epochs (
  source_id TEXT NOT NULL,
  epoch_id TEXT NOT NULL,
  opened_at_ms INTEGER NOT NULL,
  closed_at_ms INTEGER,
  capability_json TEXT,
  close_reason TEXT,
  PRIMARY KEY (source_id, epoch_id)
);

CREATE TABLE raw_events (
  event_seq INTEGER PRIMARY KEY AUTOINCREMENT,
  event_id TEXT NOT NULL UNIQUE,
  source_id TEXT NOT NULL,
  epoch_id TEXT NOT NULL,
  source_seq INTEGER,
  dedupe_key TEXT NOT NULL UNIQUE,
  observed_at_ms INTEGER NOT NULL,
  event_at_ms INTEGER,
  thread_key TEXT,
  codex_thread_id TEXT,
  turn_id TEXT,
  item_id TEXT,
  request_id TEXT,
  method TEXT NOT NULL,
  phase TEXT NOT NULL,
  durability TEXT NOT NULL,
  source_fingerprint TEXT NOT NULL,
  stored_raw_hash TEXT NOT NULL,
  raw_json TEXT,
  blob_id TEXT,
  redaction_json TEXT,
  decode_status TEXT NOT NULL,
  decode_error TEXT
);

CREATE TABLE source_checkpoints (
  checkpoint_key TEXT PRIMARY KEY,
  source_id TEXT NOT NULL,
  epoch_id TEXT NOT NULL,
  file_identity TEXT,
  byte_offset INTEGER,
  ordinal INTEGER,
  updated_event_seq INTEGER NOT NULL,
  extra_json TEXT
);

CREATE TABLE threads (
  thread_key TEXT PRIMARY KEY,
  store_source_id TEXT NOT NULL,
  codex_thread_id TEXT NOT NULL,
  session_id TEXT,
  parent_thread_key TEXT,
  forked_from_thread_key TEXT,
  status TEXT NOT NULL,
  runtime_status_stale INTEGER NOT NULL DEFAULT 1,
  capture_completeness TEXT NOT NULL,
  created_at_ms INTEGER,
  updated_at_ms INTEGER,
  projection_json TEXT NOT NULL,
  provenance_json TEXT NOT NULL,
  last_event_seq INTEGER NOT NULL,
  UNIQUE (store_source_id, codex_thread_id)
);

CREATE TABLE turns (
  thread_key TEXT NOT NULL,
  turn_id TEXT NOT NULL,
  status TEXT NOT NULL,
  capture_completeness TEXT NOT NULL,
  started_at_ms INTEGER,
  completed_at_ms INTEGER,
  execution_context_json TEXT,
  projection_json TEXT NOT NULL,
  provenance_json TEXT NOT NULL,
  last_event_seq INTEGER NOT NULL,
  PRIMARY KEY (thread_key, turn_id)
);

CREATE TABLE items (
  thread_key TEXT NOT NULL,
  turn_scope TEXT NOT NULL,
  item_id TEXT NOT NULL,
  item_type TEXT NOT NULL,
  status TEXT NOT NULL,
  started_at_ms INTEGER,
  completed_at_ms INTEGER,
  projection_json TEXT NOT NULL,
  provenance_json TEXT NOT NULL,
  last_event_seq INTEGER NOT NULL,
  PRIMARY KEY (thread_key, turn_scope, item_id)
);

CREATE TABLE pending_requests (
  source_id TEXT NOT NULL,
  epoch_id TEXT NOT NULL,
  request_id TEXT NOT NULL,
  thread_key TEXT,
  request_type TEXT NOT NULL,
  state TEXT NOT NULL,
  request_event_seq INTEGER NOT NULL,
  resolved_event_seq INTEGER,
  payload_json TEXT NOT NULL,
  PRIMARY KEY (source_id, epoch_id, request_id)
);

CREATE TABLE blobs (
  blob_id TEXT PRIMARY KEY,
  stored_hash TEXT NOT NULL UNIQUE,
  media_type TEXT NOT NULL,
  size_bytes INTEGER NOT NULL,
  relative_path TEXT NOT NULL,
  redaction_json TEXT,
  created_event_seq INTEGER NOT NULL,
  expires_at_ms INTEGER
);

CREATE INDEX raw_events_thread_seq
  ON raw_events(thread_key, event_seq);
CREATE INDEX raw_events_method_seq
  ON raw_events(method, event_seq);
CREATE INDEX turns_thread_time
  ON turns(thread_key, started_at_ms, turn_id);
```

`turn_scope` 在有 turnId 时等于 turnId；缺少 turnId 时使用 `@unassigned:<source-id>:<epoch>`，后续通过明确证据关联，禁止只靠时间猜测。

## 26.2 Dedupe key

```text
app-server = "app:" + sourceId + ":" + epochId + ":" + sourceSeq
rollout    = "rollout:" + storeSourceId + ":" + threadId + ":" + ordinal + ":" + sourceFingerprint
hook       = "hook:" + hookInstanceId + ":" + deliveryId
derived    = "derived:" + ruleVersion + ":" + sorted(parentEventIds)
```

不要使用 timestamp 作为去重键。相同文本、相同 delta 或同一毫秒内的两个事件都可能合法出现。

## 26.3 Projection 优先级

采用 field-level provenance，而不是整行“最后写入获胜”：

| 字段 | 优先来源 |
| --- | --- |
| 当前 runtime status | 当前连接 epoch 的 app-server notification/read response |
| completed item 内容 | app-server `item/completed`，随后与 durable rollout 对账 |
| 历史 transcript | durable rollout / `thread/read` |
| delta 展示 | app-server 当前 epoch |
| Thread metadata | SessionMeta + 最新官方 list/read response |
| archive/location | filesystem + official archived notification |
| pending request | app-server server request + resolved notification |

source 断线后 runtime status 设置 `runtimeStatusStale=true`，展示为 last-known，不转换成 idle。durable 事件不得覆盖一个更新的 live status；live snapshot 也不得删除 rollout 中已经存在的历史 Item。

## 26.4 Completeness 计算

每个 Turn 保存以下 coverage flags：

```text
live_started
live_terminal
live_epoch_contiguous
durable_started
durable_terminal
durable_eof_reached
decode_error_count
unknown_event_count
source_disconnect_count
```

计算规则：

```text
live_complete    = live_started && live_terminal && live_epoch_contiguous
live_partial     = 任一 live flag 存在但不满足 live_complete
durable_complete = durable_started && durable_terminal && durable_eof_reached
durable_partial  = 任一 durable flag 存在但不满足 durable_complete
metadata_only    = 只有 Thread metadata
```

一个运行中的 Turn 正常显示 `durable_partial`，不能把“尚未结束”误报为 ingest failure。Thread completeness 是各 Turn coverage 的汇总，API 同时返回原因数组，而不是只有一个枚举值。

## 26.5 SQLite 运行参数

```text
journal_mode = WAL
foreign_keys = ON
busy_timeout = 5000ms
synchronous = FULL（默认，保护不可重放的 transient event）
temp_store = MEMORY
```

DbWriter 独占写 connection；REST 查询使用只读 connection pool。数据库目录、WAL、SHM、blob 目录均限制为当前用户权限。projection 可从 raw durable event 重建，但 app-server transient event 不可重放，因此 raw event log 不应被视为无价值缓存。

---

# 27. API 的具体契约

## 27.1 通用响应

所有 snapshot response 返回提交水位：

```json
{
  "apiVersion": "v1",
  "asOfEventSeq": 18273,
  "data": {},
  "nextCursor": null
}
```

分页 cursor 是带版本的 opaque base64url payload，至少包含 sort key、tie-breaker 和 query fingerprint。调用方改变 filter 后不得复用旧 cursor。

主要 endpoint：

```text
GET /v1/sources
GET /v1/threads
GET /v1/threads/{observerThreadKey}
GET /v1/threads/{observerThreadKey}/turns
GET /v1/threads/{observerThreadKey}/items
GET /v1/threads/{observerThreadKey}/events
GET /v1/events?afterEventSeq=
GET /v1/search
GET /v1/health
GET /v1/blobs/{blobId}
GET /v1/meta/capabilities
WS  /v1/stream
SSE /v1/stream
```

`/v1/meta/capabilities` 必须公开：

```json
{
  "liveMode": "off",
  "experimentalApi": false,
  "supportedMethods": [],
  "unknownMethodsObserved": [],
  "schemaHashes": {},
  "researchConstraints": []
}
```

## 27.2 Event stream

客户端订阅：

```json
{
  "type": "subscribe",
  "afterEventSeq": 18273,
  "filters": {
    "threadKeys": [],
    "sourceIds": [],
    "methods": []
  }
}
```

服务端只发送已提交事件。若 cursor 早于 retention window，HTTP 返回 `410 CURSOR_EXPIRED`，WS 发送 gap frame 并关闭；客户端随后重新获取 snapshot 和新的 `asOfEventSeq`。

慢消费者队列溢出时返回 `SLOW_CONSUMER` 并断开，不能让它阻塞 ingest。大型 payload 返回 blob reference：

```json
{
  "blobId": "...",
  "size": 928312,
  "mediaType": "application/json",
  "sha256": "...",
  "redacted": true
}
```

## 27.3 错误码

```text
SOURCE_OFFLINE
LIVE_ATTACH_DISABLED
LIVE_ATTACH_UNSUPPORTED
THREAD_NOT_FOUND
CURSOR_INVALID
CURSOR_EXPIRED
BLOB_NOT_FOUND
SCHEMA_UNSUPPORTED
INGEST_DEGRADED
UNAUTHORIZED
ORIGIN_REJECTED
SLOW_CONSUMER
```

V1 没有 mutation endpoint。即使底层 app-server 支持 approve/send/interrupt，REST router 也不注册这些路由。

---

# 28. 配置、隐私和运维

## 28.1 配置样例

```toml
[server]
bind = "127.0.0.1:4765"
bearer_token_file = "observer-data/token"
strict_origin = true

[storage]
database = "observer-data/observer.sqlite"
blob_dir = "observer-data/blobs"
synchronous = "full"
raw_event_retention_days = 30
delta_retention_days = 7
blob_retention_days = 14

[[sources]]
name = "default"
codex_home = "~/.codex"
sqlite_home = "~/.codex"
app_server_socket = "~/.codex/app-server-control/app-server-control.sock"
live_mode = "off"
scan_interval_seconds = 30

[capture]
max_raw_event_bytes = 67108864
inline_blob_bytes = 262144
keep_deltas = true
keep_reasoning = true
keep_raw_json = true

[privacy]
redact_known_secrets = true
external_summary_only = true
fingerprint_key_file = "observer-data/fingerprint.key"
```

`~` 必须由 Observer 自己展开一次并 canonicalize；运行中不因 cwd 改变重新解析。配置 reload 只允许增加/暂停 source 和调整 retention，不在热加载中更换数据库路径。`fingerprint.key` 与数据库构成同一备份单元；丢失或轮换 key 后必须显式执行全量重建，不能在旧数据库上静默产生另一套 dedupe key。

## 28.2 Raw 与脱敏

“保留 raw”定义为保留官方 payload 的结构和非敏感字段，不等于无条件保存秘密。写入 `raw_events` 前执行确定性 redactor：

- Authorization、Cookie、API key、OAuth token、known secret field 替换为 typed marker；
- 环境变量默认只保存 key，value 只允许显式 allowlist；
- base64 image/audio 和超大 tool result 移到 blob store；
- 保存 `redaction_json`，说明规则版本和被修改的 JSON Pointer；
- 未脱敏输入使用安装时生成的 256-bit secret 计算 keyed BLAKE3 `sourceFingerprint`，用于 dedupe；数据库同时保存脱敏后 payload 的 `storedRawHash`。不保存无 key 的 pre-redaction hash，避免对低熵 secret 做离线猜测。

如果未来需要法证级 byte-exact capture，应作为单独加密模式设计，不能悄悄改变 V1 默认行为。

## 28.3 Health 与诊断

`doctor` 和 `/v1/health` 至少报告：

```text
DB migration / WAL 状态
每个 source 的连接、epoch、last event、last error
rollout scan backlog / decode errors / oversize records
ingest queue depth / DB commit latency
WS/SSE consumer 数与 dropped consumer 数
projection lag = committedEventSeq - projectedEventSeq
unknown method / variant 计数
retention 最近执行结果
```

日志不输出完整用户消息、命令输出或 raw payload；默认只记录 ID、method、size、hash prefix 和错误分类。

---

# 29. 实施切片与验收门槛

## Slice A：Observer Core

交付：配置、migration、SourceRegistry、raw event log、DbWriter、projection rebuild、`doctor`。

验收：

- migration 重复执行幂等；
- transaction 中 event/projection/checkpoint 原子提交；
- kill -9 后重启无重复 projection；
- unknown event 可以存储并查询。

## Slice B：Historical Import

交付：plain JSONL / `.zst` importer、archive/rename 识别、Thread/Turn/Item 基础投影、CLI 查询。

验收：

- 同一目录连续导入两次，raw event 和 projection 数量不变；
- 任意 byte chunk boundary、半行、坏行、truncate、inode replace 不会导致后续数据永久丢失；
- active → archived rename 不创建重复 Thread；
- SessionMeta parent/fork/session relation 与 raw 对账；
- 与官方 `thread/list` / `thread/read` 抽样对账，差异带 provenance 解释。

## Slice C：Read-only API 与 Web Viewer

交付：REST、WS/SSE、timeline、raw inspector、capture completeness、source health。

验收：

- snapshot + `afterEventSeq` 不丢不重；
- cursor 过期能恢复；
- slow consumer 不影响 ingest；
- HTML/ANSI/Markdown 均不能在 raw inspector 中执行；
- 多 CODEX_HOME 的同名/同 ID 数据不会串线。

## Slice D：App Server Live Adapter

交付：UDS WebSocket、initialize、通知采集、server request 展示、断线重连、可选 loaded attach。

验收：

- 使用官方 app-server 作为 test server，而不是 mock 掉整个协议；
- primary + Observer 两个 connection 同时接收 Thread 事件；
- Observer 从不响应 approval/question；
- 中途断线产生 `live_partial`，不会伪装成 `live_complete`；
- reconnect 创建新 epoch，不按重复 requestId 合并；
- paginated/unsupported resume 只降级单个 Thread；
- 正常退出发送 unsubscribe，异常退出可由 server 清理 connection。

## Slice E：可选 Hooks 与 Retention

交付：hook wake-up、数据删除/导出、delta/blob retention、rebuild 工具。

验收：

- hook 丢失不影响历史完整性；
- retention 不删除 projection 当前引用的 blob；
- 删除 Observer 副本不修改 Codex 原始 store；
- rebuild 后 durable projection 与删除前一致，transient coverage 明确降级。

测试层次：

```text
unit       JSON classifier、dedupe、completeness、redactor
property   任意 chunk/换行/truncate/重复 delivery
golden     版本化 app-server 与 rollout fixtures
integration 官方 codex app-server + 临时 CODEX_HOME
e2e        importer + live + API + reconnect + archive
compat     当前版本 schema 与上一支持版本 schema
```

每次支持新的 Codex 版本时，先运行该版本的 `generate-json-schema`，将 hash 和兼容报告加入 fixture manifest；不认识的 stable breaking change 必须让 live adapter fail closed 到 store-only，而不是继续发送可能有副作用的请求。

---

# 30. 保留的重点研究问题与临时决策

这些问题不能用当前官方源码或官方 OpenAI 文档完整回答。它们必须保留在发布清单中，但除明确标注外不阻塞 store-first V1：

| ID | 待研究问题 | 当前官方证据状态 | V1 临时决策 | 阻塞范围 |
| --- | --- | --- | --- | --- |
| R1 | macOS App 是否默认创建/连接 control socket | 官方未公开具体本地启动拓扑 | 不假设；socket adapter 仅连接用户显式配置的 endpoint | 不阻塞 Store；阻塞“自动覆盖 App 实时事件”的承诺 |
| R2 | App、CLI、IDE 是否共享 CODEX_HOME / SQLite home | 源码说明默认与 override，产品实际值未公开 | source 显式配置，identity 包含实际 home | 不阻塞 |
| R3 | macOS App 的 SessionSource 值 | 官方未明确 | 枚举全部官方 `sourceKinds`，未知值保留 raw | 不阻塞 |
| R4 | 第二 subscriber 收到 pending request 时，官方客户端的产品级行为 | 源码证明多播与共享 callback，产品 UX 未承诺 | `live_mode=off` 默认；开启后 Observer 永不 response | 阻塞默认开启 live attach |
| R5 | daemon/App 升级后的稳定进程身份 | socket path 已知，公开协议没有稳定 process id | 每次 initialize 生成新 epoch；断线状态一律 stale | 不阻塞 |
| R6 | 目标版本默认 historyMode 与分页 API 可用性 | 官方文档说明 paginated 支持仍有限且相关 API 可实验 | feature detection；失败回退 JSONL | 不阻塞 |
| R7 | ephemeral Thread 的全局 discovery | 公开 API 只保证当前进程 loaded memory | 只承诺已连接时捕获；否则标记不可恢复 | 阻塞“所有 ephemeral Thread”承诺 |
| R8 | 大型 command output、diff、media 的真实分布 | 官方未给产品级上限 | 64MiB event cap + blob + 可配置 retention；上线前用合成边界测试 | 不阻塞原型，阻塞生产容量定标 |
| R9 | compression worker 的实际调度阈值是否稳定 | 源码是实现细节，官方文档未承诺 | watcher 不依赖阈值，周期性 rescan + 双表示支持 | 不阻塞 |
| R10 | Windows 是否会提供等价本地多连接 transport | 当前 Unix daemon 不能代表 Windows 产品承诺 | Windows V1 使用 store-only；未来按官方 transport 增加 adapter | 阻塞 Windows live capture |
| R11 | App Server transport 何时达到 production-supported maturity | 官方文档当前明确标为 experimental / unsupported for production workloads | live adapter opt-in、pin schema、失败即退回 store-only | 阻塞 live mode 的生产 SLA，不阻塞 V1 store-first |

## 30.1 研究问题关闭标准

只允许以下证据关闭问题：

1. 官方 OpenAI 文档明确描述并可稳定引用；
2. 当前官方源码中的公共协议、测试和实现三者一致；
3. 官方 schema 将能力标为 stable，且 Observer 兼容测试通过。

桌面 App 的二进制逆向、私有日志猜测、社区帖子和未公开参数不能把问题状态改成“已确认”。如果官方资料长期不公开，问题应保持 `Not publicly specified`，架构继续采用临时决策。

## 30.2 发布时允许声明的能力

V1 可以声明：

```text
读取配置的 Codex local store 中可持久化的 Thread 历史
增量观察 rollout 写入并展示最终一致视图
在用户显式配置且官方 app-server 可连接时增强实时事件
保留公开 payload、provenance 和 completeness 状态
Observer 关闭不影响 Codex 原客户端
```

V1 不应声明：

```text
无配置发现所有 Codex 产品进程
捕获所有客户端的所有 transient delta
恢复未连接时产生的 ephemeral Thread
获取隐藏 chain-of-thought
在所有平台提供相同实时能力
对未来 Codex 内部 SQLite/rollout shape 永久兼容
```
