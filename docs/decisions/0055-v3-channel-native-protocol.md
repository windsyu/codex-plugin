# ADR 0055：V3 单会话渠道的原生语义输入候选

## Status

Proposed，正在实现并验证显式实验入口。隔离来源与必要交互探针通过，但其 resume/queue 附加方法不满足严格已有线程边界；后续透明 TUI relay 与 loaded-only 提交路径的五项实测已通过。本地源码已有默认关闭的原生运行接入及 Service/API/UI 文本闭环；P1 仍进行中、未验收，P2/P3 未完成。V3 阶段状态只在 [V3 实施计划](../v3-implementation-plan.md)维护；V2 默认运行方式和验收不变。

## Date

2026-10-09

## Context

V3 要为本机指定的已有受管原生会话增加 Telegram 输入、限定回复和必要交互。本机对话不自动外发；相同正文、PTY ACK 和模型 `response.completed` 都不能证明渠道来源及整轮完成。

当前 launcher 用 `-c` 为本次普通 CLI 覆盖模型代理地址。安装版 CLI `0.156.1` 在隔离 HOME 中确实访问模型代理，但该 HOME 没有可附加 socket。只读官方源码 `2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4` 的 [daemon 启动排除规则](https://github.com/openai/codex/blob/2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4/codex-rs/tui/src/daemon_startup.rs)也排除这类配置覆盖。该源码 commit 不是安装版二进制的已证实构建来源。

[隔离探针](../../examples/v3-native-channel-probe.rs)验证另一候选：项目显式启动原版 `app-server --listen unix://...`，原版 TUI 用 `--remote` 连接；第二客户端给同一个已加载 Thread 增加队列消息，原生回显 `clientUserMessageId → userMessage.clientId → turnId`，最终收到 `turn/completed`。每个渠道 Turn 的两次合成模型响应不提前结束任务。追加探针覆盖原生命令审批、Plan 问题、交错输入与故意不读提交确认。这些证据不等于生产认证、同时审批竞速、断线恢复或任意 CLI 版本兼容。

随后核对发现该探针的 `thread/resume` 附加有加载检查与实际调用之间的竞态：目标在两者之间卸载时，resume 可以重新加载它。原生队列也允许对磁盘上未加载的线程持久排队，不能替代严格已有线程附加。官方源码的 [队列处理器](https://github.com/openai/codex/blob/2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4/codex-rs/app-server/src/request_processors/thread_queue_processor.rs)及 [冷线程队列测试](https://github.com/openai/codex/blob/2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4/codex-rs/app-server/tests/suite/v2/thread_queue.rs)明确体现这一行为。因此既有探针的来源链成立，只能作为已加载并附加场景的有限证据，不能直接提升为生产绑定流程。

`turn/start` 通过 [原生 Turn 处理器](https://github.com/openai/codex/blob/2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4/codex-rs/app-server/src/request_processors/turn_processor.rs)查询内存中的线程，找不到即失败，但它不自动订阅通知，也能 steer 已忙碌的 Turn。`thread/read` 不订阅；warm resume 会订阅但并非 loaded-only。下列候选将观察与输入分开，并要求安装版实际验证这些边界。

## Decision

本地实验实现采用以下**显式启用的渠道能力运行方式**，ADR 保持 Proposed，先完成该片验证与验收，再决定是否扩大支持：

1. 保留普通 CLI 的 V2 默认启动方式。渠道能力运行方式使用本次 Run 独占的私有 Unix socket、原版 App Server 与原版 TUI。TUI 经项目透明 WebSocket relay 连接本次 App Server；relay 转发原始消息，不改写原生 RPC，也不等待解码、保存或渠道 ACK。有界观察副本只用于原生来源证据。模型 HTTP/SSE/WS 代理、PTY 和异步观察仍独立。不开公网协议端口，不修改官方 CLI，不自动升级。
2. 先由本机 TUI 建立正常会话，再在本机明确选择其已有且仍被该 TUI 订阅的 Thread、绑定 Telegram 私聊。精确绑定 Thread，不按第一个 loaded ID、cwd、活动状态或网页消息猜测 TUI 当前焦点，不自动跟随 TUI 切换。不是为每条消息新建或恢复另一会话。现有无语义端点的普通 Run 明确显示不支持热附加，不自动停止、恢复、迁移或偷偷替换目标。
3. 独立原生控制连接只用 `turn/start` 向已加载目标提交文本，不以 `thread/resume` 或 `thread/queue/add` 获取附加资格。渠道在自己的有界队列中等待可提交条件；本机输入仍可能在检查之后抢先进入同一 Turn，因此等待不能承诺渠道独占。每次实际提交使用新 UUID client ID，提交前保存渠道收件与提交意图；ID 仅关联来源，不提供原生幂等。写入后超时或 ACK 丢失不自动重放；没有 Accepted ACK 时不释放回复，即使已有原生回显也不自动重发。receipt v2 的收件、提交意图与拒绝记录边界见接入契约，不恢复旧控制内核。
4. 来源证据取自 TUI 自己不中断的原生订阅流，不能依赖第二客户端通过 resume 订阅。观察到准确 Thread/Turn 开始、同一个 UserMessage 的 started/completed 及 client ID 后，建立外发资格。混入任何其他用户 item，包括复用同一 client ID 的另一个 item，均使该 Turn 的自动回复外发失效。安装版 `0.156.1` 的隔离 P0c 已提供完整 v2 生命周期，也验证未订阅控制连接回答命令审批和 Plan 问题；不能据此忽略其他版本可能的通知筛选。
5. 初片仅允许明确 `final_answer` 的原生 assistant 文本，并等到匹配的 Accepted ACK 和成功的原生 `turn/completed` 后释放。completion 中的 items 是摘要，不能用它证明没有本机输入。模型完成、assistant item 完成、工具完成均不代替 Turn 完成。不发 reasoning、工具输出、终端、历史快照或 diff。
6. 断线、解析失败、观察溢出、目标退订/卸载、Run/transport epoch 或本机绑定目标变化、本机撤销及无法解释的事件顺序均关闭旧外发资格。重连不能恢复未完成任务的来源证明；失效 generation 的待发送内容不得继续发送。TUI 焦点是客户端内部状态，不能承诺能探测每次焦点切换。已经外发的内容不能召回。
7. Telegram 只匹配一个绑定 user ID 和 private chat ID；不得按用户名、转发来源或第一个 `/start` 自动授予权限。凭证保存在本机独立配置范围内，绑定、收件与必要交互规则见 [接入契约草案](../v3-telegram-channel-contract.md)。本机 owner 入口已接入，精确目标包含 subscription generation，设备 token 不得配置；真实 Telegram 与生产必要交互仍须实测。

这是对 ADR 0039 原生唯一输入入口的有限实验扩展，不恢复旧 Session Kernel、持久 ThreadOwner、command ledger/CAS、raw-first 转发或 `/v2`。本文同步本地实现决定，保留 Proposed 直至该片验证与验收完成。

## Alternatives

- **向 PTY 粘贴正文并 Enter：** 无法确认草稿、picker、审批或追问状态，也不能可靠关联渠道来源，未采用。
- **共享 daemon + 当前启动参数：** 当前代理覆盖被排除；改写用户默认配置以绕开限制会扩大影响，未采用。
- **新建 `exec resume` 或独立 App Server 恢复同一 rollout：** 不是同一个正在运行的受管会话，不能作为已有会话接入。
- **第二客户端 warm resume 订阅或原生队列附加：** 隔离探针已有证据，但卸载竞态与冷线程排队会越过第一阶段目标边界，不作为生产附加方法。
- **所有 Run 默认转为 remote TUI：** 在生产兼容和回归未验证前扩大 V2 改动，暂不采用。

## Consequences

新模式会增加一个受管子进程和私有协议连接，需要单独验证启动、停止、进程清理、认证、profile、原生设置、审批与设备访问。不能因探针通过就替换默认运行入口或宣称支持 Linux/Windows。

最终文本延迟到整轮完成后发送，以避免稍后的本机 steer 混入同一 Turn。任务完成和 Telegram 发送确认分别表达；发送超时可能已送达，不能承诺 exactly-once 或已读。

来源隔离不等于上下文保密过滤。渠道沿用同一 Thread，模型为渠道生成的回复可能引用本机先前对话；本机绑定时须明确说明共享上下文，不能承诺这些回复绝不包含历史内容。

P0 新增来源门、Telegram 输入校验与 HTTP 传输、私有原生 relay、隔离收件/凭证存储及探针和只读验收页。当时生产启动、配置、数据库和 migration 均未改变；历史证据见[验证记录](../validation/v3-channel-origin-2026-10-09.md)。P1 随后接入默认关闭的 `--channel-ready` / `launch.channelReady`、订阅候选消费者、本机 owner 六个 Telegram API、lazy 凭证加载、绑定 UI、持久收件/拒绝记录和串行文本 actor。代码可用不等于已验收。旧数据库和 migration 不变，receipt 新写 v2 且读取 v1/v2 混合；增量事实与限制见[实验运行接入记录](../validation/v3-channel-runtime-2026-10-09.md)。

### 原生退出保证的限定

正常宿主退出使用并行原生回收与 ShutdownHandle 等待，活动 Run 的 proxy/runtime 保留至等待结束；未发布 Owner 取消/失败也等待回收。统一 18 秒是 reaper 等待阶段的策略，不是整个 Application Drop 的硬时间上限。该实现已落源码，最终增强 witness 断言的宿主退出专项与 Owner lifecycle 专项已通过；当前真实场景不证明多个真实 Run 并发退出，也不扩大为所有工具结束保证。

仅实验 App Server 使用 `thread_unload_delay_secs=0`。停止先撤销控制/外发并关闭 relay，再对核验后的单个 native peer 发 SIGTERM；超时只回收自身进程组。选择 `NativeExited` 表达 native peer 消失、wrapper 正常退出并回收，允许据此结束 Application 当前的同 HOME 运行门。它不表示所有原生工具均已终止。`ForcedUnknown` 和进行中的关闭继续阻止该 HOME 立即重开，不能把强制 fallback 表述为正常退出。

未采用“所有请求有响应且 loaded 列表为空便证明所有工具已结束”。官方连接 close 允许已启动 handler 继续；全局 shutdown 超时只告警；统一工具终止包含异步、不等待结果的路径。因此不将进程退出、固定等待或重复空列表升级为完整清理证明，也不以扫描全机进程树补造该保证。固定源码与测试限制见[实验运行接入记录](../validation/v3-channel-runtime-2026-10-09.md)。
