# ADR 0055：V3 单会话渠道的原生语义输入候选

## Status

Proposed。隔离来源探针通过，尚未采纳为生产启动方式，尚未完成用户验收。V3 阶段状态只在 [V3 实施计划](../v3-implementation-plan.md)维护；V2 默认运行方式和验收不变。

## Date

2026-10-09

## Context

V3 要为本机指定的已有受管原生会话增加 Telegram 输入、限定回复和必要交互。本机对话不自动外发；相同正文、PTY ACK 和模型 `response.completed` 都不能证明渠道来源及整轮完成。

当前 launcher 用 `-c` 为本次普通 CLI 覆盖模型代理地址。安装版 CLI `0.156.1` 在隔离 HOME 中确实访问模型代理，但该 HOME 没有可附加 socket。只读官方源码 `2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4` 的 [daemon 启动排除规则](https://github.com/openai/codex/blob/2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4/codex-rs/tui/src/daemon_startup.rs)也排除这类配置覆盖。该源码 commit 不是安装版二进制的已证实构建来源。

[隔离探针](../../examples/v3-native-channel-probe.rs)验证另一候选：项目显式启动原版 `app-server --listen unix://...`，原版 TUI 用 `--remote` 连接；第二客户端给同一个已加载 Thread 增加队列消息，原生回显 `clientUserMessageId → userMessage.clientId → turnId`，最终收到 `turn/completed`。每个渠道 Turn 的两次合成模型响应不提前结束任务。此证据没有证明生产认证、审批竞速、断线恢复或任意 CLI 版本兼容。

## Decision

本 ADR 提议下一片采用**显式启用的渠道能力运行方式**，先完成受控纵向闭环，再决定是否扩大支持：

1. 保留普通 CLI 的 V2 默认启动方式。渠道能力运行方式使用本次 Run 独占的私有 Unix socket、原版 App Server 与原版 TUI，模型 HTTP/SSE/WS 代理、PTY 和异步观察仍独立。不开公网协议端口，不修改官方 CLI，不自动升级。
2. 先由本机 TUI 建立正常会话，再在本机明确选择其已有 Thread、绑定 Telegram 私聊。不是为每条消息新建或恢复另一会话。现有无语义端点的普通 Run 明确显示不支持热附加，不自动停止、恢复、迁移或偷偷替换目标。
3. 渠道通过原生队列提交文本，逐任务串行调度。每次实际提交使用新 UUID client ID，提交前登记任务；ID 仅关联来源，不提供原生幂等。写入后超时或 ACK 丢失不自动重放，后续可信原生回显可以消除不确定性。
4. 只在不中断的订阅连接中，观察到准确 Thread/Turn 开始、同一个 UserMessage 的 started/completed 及 client ID 后，建立外发资格。混入任何其他用户 item，包括复用同一 client ID 的另一个 item，均使该 Turn 的自动回复外发失效。
5. 初片仅允许明确 `final_answer` 的原生 assistant 文本，并等到成功的原生 `turn/completed` 后一次性释放。completion 中的 items 是摘要，不能用它证明没有本机输入。模型完成、assistant item 完成、工具完成均不代替 Turn 完成。不发 reasoning、工具输出、终端、历史快照或 diff。
6. 断线、解析失败、观察溢出、订阅变化、epoch/Thread 变化、本机撤销或无法解释的事件顺序均关闭旧外发资格。重连不能恢复未完成任务的来源证明；失效 generation 的待发送内容不得继续发送。已经外发的内容不能召回。
7. Telegram 只匹配一个绑定 user ID 和 private chat ID；不得按用户名、转发来源或第一个 `/start` 自动授予权限。凭证保存在本机独立配置范围内，具体存储、绑定确认、审批与追问支持须在下一片契约中补齐，未实现前不开放生产入口。

这是对 ADR 0039 原生唯一输入入口的有限候选扩展，不恢复旧 Session Kernel、持久 ThreadOwner、command ledger/CAS、raw-first 转发或 `/v2`。生产接入实现前，须将本 ADR 状态和实际支持边界同步更新。

## Alternatives

- **向 PTY 粘贴正文并 Enter：** 无法确认草稿、picker、审批或追问状态，也不能可靠关联渠道来源，未采用。
- **共享 daemon + 当前启动参数：** 当前代理覆盖被排除；改写用户默认配置以绕开限制会扩大影响，未采用。
- **新建 `exec resume` 或独立 App Server 恢复同一 rollout：** 不是同一个正在运行的受管会话，不能作为已有会话接入。
- **所有 Run 默认转为 remote TUI：** 在生产兼容和回归未验证前扩大 V2 改动，暂不采用。

## Consequences

新模式会增加一个受管子进程和私有协议连接，需要单独验证启动、停止、进程清理、认证、profile、原生设置、审批与设备访问。不能因探针通过就替换默认运行入口或宣称支持 Linux/Windows。

最终文本延迟到整轮完成后发送，以避免稍后的本机 steer 混入同一 Turn。任务完成和 Telegram 发送确认分别表达；发送超时可能已送达，不能承诺 exactly-once 或已读。

来源隔离不等于上下文保密过滤。渠道沿用同一 Thread，模型为渠道生成的回复可能引用本机先前对话；本机绑定时须明确说明共享上下文，不能承诺这些回复绝不包含历史内容。

本片新增纯来源门和 Telegram 输入校验，以及隔离探针、只读验收页；生产入口、配置、数据库和 migration 均未改变。剩余原生审批、追问、交错输入、提交不确定和恢复验证见[验证记录](../validation/v3-channel-origin-2026-10-09.md)。
