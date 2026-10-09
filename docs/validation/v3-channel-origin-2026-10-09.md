# V3 首片：来源隔离与原生完成探针

> 日期：2026-10-09。工作分支：`codex/v3-channel-origin-spike`，基线 `17187ec`。
> 范围：原版 CLI、合成模型、合成渠道的隔离协议验证和纯校验。生产 Telegram、绑定界面、远程审批与恢复尚未实现；不改变 V2 验收。

[文档总索引](../README.md) · [V3 实施计划](../v3-implementation-plan.md) · [候选 ADR 0055](../decisions/0055-v3-channel-native-protocol.md)

## 1. 环境与隔离

实际安装版 CLI 为 `0.156.1`，Rust `1.95.0`，Node `22.23.2`。只读研究 checkout 的 commit 为 `2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4`；没有证据证明它对应安装版二进制的构建来源。未升级或修改官方 CLI。

[探针](../../examples/v3-native-channel-probe.rs)为每次运行创建独立临时 HOME、USERPROFILE、CODEX_HOME 和项目，使用合成 provider 与合成 bearer；上游只监听 loopback，候选原生协议只监听临时 Unix socket。退出后清理本次专属进程与目录。不读取真实用户配置、会话或登录凭证，不调用真实模型、Telegram 或第三方消息服务。

macOS `/tmp` 的符号链接会影响原生 trust key；探针将项目路径 canonicalize 后再登记 trust，解决首次探针未发模型请求的问题。修正后重新实际运行成功，不把最初失败当作通过。

## 2. 实际通过的协议检查

| 检查 | 实际证据及边界 |
| --- | --- |
| 当前普通启动路径 | `-c` 覆盖下，CLI 经生产 `ProxyServer` 到达合成模型；临时 HOME 内没有 socket。这是当前无法直接附加的证据，不是渠道可用声明 |
| 同一个已有会话 | 候选私有 App Server 与原版 remote TUI 先创建本机上下文，第二 RPC 客户端附加同一个已加载 Thread；没有另开 `exec resume` 或另一会话 |
| 本机输入隔离 | 本机 UserMessage 有结构化身份证据，不纳入渠道 client ID 集合；不依据正文、ANSI 或时间归属 |
| 相同正文 | 两次相同合成文本使用不同 client ID，分别收到准确原生 UserMessage/Turn 回显，最终完成两个不同 Turn |
| 原生来源与完成门 | RPC 前创建 `OriginProof`，按有序 `turn/started`、UserMessage started/completed、client ID、明确 final phase 和原生终态处理；非终态不释放回复，两次终态均匹配合成 final |
| 多次模型响应 | 每条渠道 Turn 先生成合成 `update_plan` 调用，再进行第二次模型响应；两次 `response.completed` 不替代原生 `turn/completed` |

探针中的 ANSI 仅用于合成 TUI 首次就绪和输入一条本机消息，不能证明提交、来源、审批或完成。来源证据来自原生协议与结构化记录。

`turn/completed.items` 是摘要，不能用它证明 Turn 没有本机输入；`clientUserMessageId` 是来源关联，不是原生幂等键。源码研究发现 `turn/start` 可进入已有 Turn，因此不能用忙闲检查后发送来保证渠道独占；同 Turn 混入任何其他 user item 都关闭自动外发。

## 3. 新增纯边界与检查

[来源门](../../src/workbench/channel/origin.rs)每次只处理一个实际提交尝试，内存有界；只允许明确 final 文本，在成功原生整轮结束后释放一次。没有完整用户生命周期、来源混入、ID 冲突、未知终态、捕获缺口、异常顺序或回复超限时不释放。完成身份保留有界 tombstone，满时失效而不淘汰；重复 final item 不覆盖更新结果，同 item 正文变化失效。

[Telegram 校验](../../src/workbench/channel/telegram.rs)只接受准确绑定的 user ID、private chat ID 和非 bot 发送者；按 i64 精确解析身份，转发来源不授予权限。文本及 entity 使用 UTF-16 边界，控制字符、未知内容变体、媒体和编辑更新不能变为任务。callback 只接受本 bot 的可访问消息及有界 opaque reference；task、generation、过期和一次性消费仍属于未实现的任务 actor，不能只凭此校验执行审批。

纯边界测试覆盖同 Turn 本机 steer（包括 final 后但 Turn 完成前）、本机抢先、复用 client ID、缺少 started、回显不一致、重复终态、缺口、失败/中断/未知状态、先有未归属回复、乱序本机输入、已完成 Turn 重开、tombstone 溢出、final 重放，以及 Telegram 身份、Unicode、callback 负例。独立只读复核发现三条来源漏洞，已增加对应回归并修正；复核意见本身不算测试通过。

| 本次检查 | 结果 |
| --- | --- |
| `node scripts/dev.mjs check --ci` | 成功；维护工具 80、前端 234、Rust 462 通过，0 失败；Rust 56 ignored，未计作通过 |
| TypeScript、Web 构建、Rust fmt/Clippy | 通过；固定工具链和 `--locked` |
| 两个 binary 的 debug/release 构建 | 通过，Web 先构建 |
| 新增渠道纯边界 / 验收页专项 | 15 / 2 项通过，计入上述 Rust 总数；不是额外叠加计数 |
| 安装版 CLI 协议探针 | 最终源码实际复跑退出 0，七项检查通过；合成模型，不是 Telegram 实测 |
| 文档 / 工作差异内容检查 | 110 份 Markdown 通过；14 个修改或新增文件未发现禁止二进制；差异空白检查通过 |
| 只读验收 HTTP | 本机通过 Tailnet 地址读取页面与七项报告为 200，`no-store`；POST 为 405 |
| Chrome 页面检查 | 独立浏览器上下文、390×844 手机模拟视口；七项通过可见，宽度无水平溢出。未把模拟视口当作真实手机/Tailnet 验收 |

完整检查的受管产物为 `target/artifacts/89cec747-5367-4b1d-afe1-50f5877e5e63`；最终协议报告为 `target/artifacts/54629e2f-dc6d-4b1a-8c4f-59f4391b0a73`。先前 Clippy 布尔表达式检查失败的任务保留为失败诊断，修正后完整检查与协议复跑成功。产物和原始日志均不入 Git。

这些模块当前由隔离 example 使用，生产 launcher 没有启用渠道入口。未新增依赖、配置、schema 或 migration；旧 `/v1` 不变。

## 4. 复现与只读远程验收

使用开发指南的固定工具链和锁定依赖。专项命令为：

```sh
cargo test --locked --lib workbench::channel:: -- --test-threads=4
cargo test --locked --example v3-channel-acceptance
cargo clippy --locked --all-targets -- -D warnings
cargo run --locked --example v3-native-channel-probe -- --report <登记产物目录>/report.json
```

可用 `WORKBENCH_TEST_CODEX` 显式选择自行安装的官方 CLI；不把维护者机器路径作为贡献者前提。专项不会升级 CLI。报告使用 `0600`、禁止覆盖，只写版本、研究 commit、七项布尔检查和未验范围，不写正文、会话 UUID、路径或凭证。

[验收页](../../examples/v3-channel-acceptance.rs)严格读取安全汇总，提供 `/` 和 `/api/report`，可选 `/api/boundaries`、`/api/attachment`；没有输入或控制接口。正文 HTML 转义、禁止脚本、响应不缓存。默认监听 loopback，只有明确传入本机 Tailscale 地址才远程可达，不监听 `0.0.0.0` 或修改 Tailscale 设置。

```sh
cargo run --locked --example v3-channel-acceptance -- \
  --report <登记产物目录>/report.json \
  --listen <本机 Tailscale IPv4> --port 0
```

这份汇总不含私人资料，Tailnet 中允许连接该机器的设备可以阅读；不是带会话控制权的设备配对页面。用户手机的 Tailnet ACL、网络和实际可达性须单列验收，不能用本机 HTTP 成功代替。

## 5. P0b 后续原生边界专项

P0a 的 462 项 Rust 全回归及七项协议探针统计保持 §3 原记录；以下是**另一次定向运行**，不叠加进 462。受管任务 `target/artifacts/edb678e0-bb3e-48b3-bbcb-48b3094ca084` 的 `metadata.json` 记录 `result: success`，其 `report.json` 为 `schemaVersion: 1`、`kind: native-semantic-boundaries`，列出十项 `passed`。探针使用安装版 `codex-cli 0.156.1` 与合成模型；`sourceCommit` 为只读研究源码 `2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4`，不能证明安装版二进制对应这个提交。报告只含汇总检查，不含消息正文、会话 ID 或凭证。复现时另用新建的受管任务目录，报告路径必须尚不存在：

```sh
cargo run --locked --example v3-native-channel-probe -- \
  --boundaries --boundaries-report <新登记产物目录>/report.json
```

| P0b 检查 | 实际证据及边界 |
| --- | --- |
| `commandApprovalAllow`、`commandApprovalDeny` | 两个订阅原生协议的客户端收到同一个命令审批请求；选定允许或拒绝后，原生命令执行结果分别符合决定。候选来源门仍能为允许和拒绝两轮确认来源 |
| `resolvedStaleDenyCannotReverseAllow`、`resolvedStaleAllowCannotReverseDeny` | 另一客户端收到 `serverRequest/resolved` 后才提交相反的旧响应，未推翻已确认结果；此检查**不是**两个响应同时竞争的 race 测试 |
| `planQuestionRoundTrip` | 原生 plan mode 的阻塞提问按准确 request ID 回答，解决事件发生在 Turn 完成前；不代表 Telegram 已可远程回答 |
| `queueWaitsBehindOtherClientTurn` | 另一客户端的模型响应被合成上游暂留时，渠道候选输入留在原生队列；前一 Turn 完成后，自己的 UserMessage 生命周期才开始。这证明隔离候选的序列，不证明当前生产 `-c` 启动路径可安全附加已有 loaded Thread |
| `otherClientSteerWithheld`、`lateSteerAfterFinalWithheld` | 另一客户端输入混入同一 Turn，来源门以 `MixedInput` 暂扣最终回复；后一项在模型 final item 完成后、原生 Turn 完成前插话，仍被暂扣 |
| `unreadAckNoReplayAndGapWithheld` | 提交连接发出一次请求后**故意不读取其 ACK**并关闭；主观察连接持续运行，原生历史中仅有一条匹配 UserMessage。没有自动重发，人工标记的捕获缺口使回复暂扣。这不是实际网络丢包、服务崩溃或恢复试验 |
| `semanticQueuePreservesNativeDraft` | 原生 TUI 的未发送文本草稿在语义队列任务完成后仍可见；该任务未向 PTY 注入任务文本。未验证 picker 状态或重启后的草稿 |

新 [只读验收页](../../examples/v3-channel-acceptance.rs)可选加载此报告，在旧首片报告之外展示 P0b 当前证据；`/api/boundaries` 只在提供 `--boundaries-report` 时开放。本次 P0b 元数据和报告证明探针运行成功，尚不能代替用户手机跨设备查看或真实渠道验收。

## 6. P0c 严格已有线程附加与基础模块

最终源码的受管实测为 `target/artifacts/8eb25fcb-dd49-4a77-a897-0044d656eb5d`，退出 0、元数据 success，安全报告为 `kind: native-attachment-boundaries`、五项 passed。原版 `0.156.1` TUI 从启动时经过 [私有 relay](../../src/workbench/channel/native.rs)，独立控制连接只 initialize 和提交/回答请求，没有调用 resume 或增加原生队列。源流来自 TUI 自己已经建立的订阅，不依赖 loaded 列表或历史正文补建订阅。

| P0c 检查 | 实际证据 |
| --- | --- |
| `originViaTuiSubscriptionWithoutControlResume` | TUI 流含准确 Turn started、UserMessage started/completed/client ID、final_answer 与 Turn completed；来源门正常释放合成最终文本 |
| `approvalViaTuiSubscriptionWithoutControlResume` | 未订阅的控制连接按 TUI 捕获的原生命令请求回答；原生命令执行退出 0，输出含准确合成字面量，随后正常终态。不是文件审批或同时竞答证据 |
| `questionViaTuiSubscriptionWithoutControlResume` | 未订阅的控制连接回答准确阻塞 Plan question；TUI 流见 resolved 与正常终态 |
| `captureConsumerLossDoesNotBlockTuiAndInvalidatesProof` | 实际丢弃观察消费者产生缺口；原生任务继续完成，旧来源证明保持失效，没有借历史重建外发资格 |
| `unloadedTurnStartRejectsWithoutColdLoadOrQueue` | TUI 断开、目标实际卸载后，turn/start 返回 thread-not-found；loaded 仍无目标，原生队列仍空 |

relay 的 14 项回归覆盖原文、二进制转发并标缺口、字节预算释放、慢/失效消费者、独占 TUI 与新连接 epoch、退出清理、负向路径、心跳、关闭握手、双向大帧背压、epoch 快照交错及关闭后迟到 final。独立复核发现的公开 watch 锁、重复 Pong、过早取消关闭握手、串行双向写等待、epoch 混读和关闭资格窗口均修正。旧单循环在确定背压用例中超时，最终双向驱动通过；不能把默认缓冲下未复现的早期用例说成已复现。

观察状态的 closed 表示资格立即撤销，底层握手可能继续有界收尾；它不是“新 TUI 已可接入”的 ACK。消费者在 await 后及外发前重新检查 closed/gap/epoch，不能只检查等待开始时状态。

[Telegram HTTP client](../../src/workbench/channel/telegram/client.rs)十项合成测试覆盖不重定向/不自动 retry、未读确认、超时/5xx/错 chat 的 unknown、1 MiB 响应限制、纯文本、Unicode、webhook 拒绝接管及 callback ACK。[收件存储](../../src/workbench/channel/store.rs)十四项测试包含独立子进程在实际 fsync 后退出 73、不执行析构，再开存储仍为 Submitting、重复消息不提交、日志不重放；也覆盖损坏、身份变化、权限、并发锁、容量和追加失败。测试数包含一个普通执行时惰性的子进程 helper。Closed 仅表示收件处理关闭，不代表原生完成或 Telegram 送达。未验证断电耐久性或同 UID 离线恶意完整记录截尾。

| 最终集成检查 | 结果 |
| --- | --- |
| `node scripts/dev.mjs check --ci` | `target/artifacts/a5388850-2136-4a2b-a4e8-da632b0e82b5` success；工具 80、前端 234、Rust 505 通过，0 失败；Rust 56 ignored 未计作通过 |
| fmt、Clippy all-targets、类型与构建 | 通过；两个 binary debug/release，Web 先构建；无新增依赖或锁文件变化 |
| 最终 relay / client / store / 验收页测试 | 14 / 10 / 14 / 7，已经计入 Rust 505，不额外叠加 |
| P0c 安装版最终复跑 | 最终关闭资格与消费端复查修正后五项通过；未使用真实模型或 Telegram |
| 最终只读远程页面 | 本机读取 `/api/attachment` 为 200、no-store，POST 为 405；Chrome 390×844 视口显示 22 项通过、0 失败、scrollWidth 390，无水平溢出。未把本机和模拟视口当作用户手机实际验收 |

复现与三份汇总一起展示：

```sh
cargo run --locked --example v3-native-channel-probe -- \
  --attachment-boundaries --attachment-report <新登记产物目录>/report.json
cargo run --locked --example v3-channel-acceptance -- \
  --report <P0a报告> --boundaries-report <P0b报告> \
  --attachment-report <P0c报告> --listen <本机 Tailscale IPv4> --port 0
```

基础存储有自己的 receipt 格式版本，但尚未接入生产配置；旧数据库、配置 schema、默认启动、`/v1` 和 migration 不变。

## 7. 未验与后续

- 审批两个客户端同时响应的 race、真实 Telegram 审批/追问，以及未支持交互的用户提示。
- 真正的网络 ACK 丢失、提交不确定性恢复、旧操作、断线、崩溃与重启；picker 状态和重启后草稿。
- 生产 Run 的启动/退出、认证与 profile、设备配对、绑定撤销、异步发送及持久化。
- 真实 Telegram 和模型、其他 CLI 版本以及 Linux/Windows。

渠道沿用同一 Thread 的上下文；模型为渠道生成的回复可能引用本机历史。来源门阻止自动转发本机发起的 Turn，不能承诺模型回答不会引用共享历史。

P0a/P0b/P0c 的自动化结果不代表完整第一阶段通过。P0b 的 queue/resume 不是生产附加方法；P0c 的 relay/turn-start 候选已有实际证据，但目前生产仍只以普通 CLI 的 `-c` 覆盖启动。下一步交付 P1 的显式运行方式、本机配置绑定撤销、持久收件和异步文本闭环，再补齐 P2 的生产必要交互与故障矩阵；跨会话管理继续独立确认。
