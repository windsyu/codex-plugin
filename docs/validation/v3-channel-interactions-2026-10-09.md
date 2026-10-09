# V3 P2 原生必要交互边界（2026-10-09）

[验证索引](README.md) · [V3 实施计划](../v3-implementation-plan.md) · [接入契约](../v3-telegram-channel-contract.md#5-输出与必要交互)

## 1. 范围与环境

本记录证明隔离原版 CLI 的必要交互机制，并新增可供生产消费者使用的实时来源谓词。Telegram callback、交互提示、持久交互收件与本机授权界面尚未接入，不表示 P2 已完成或验收。

- macOS / Apple Silicon，安装版 `codex-cli 0.156.1`，未修改、升级官方 binary。
- Rust 1.95.0、Node 22.23.2，锁定依赖。
- 只读官方源码实查 commit：`2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4`；不是该安装版 binary 的已证实构建来源。
- 每次独立临时 HOME/USERPROFILE/CODEX_HOME/TMPDIR、项目和 Unix socket；合成模型只监听 loopback，无真实凭证、Telegram、私人文件或真实模型调用。

## 2. 来源门与九个原生用例

`OriginProof.live_owned_turn()` 要求匹配 Accepted Turn ID、Turn started、同一个 own-user item 的 started/completed、无混入或其他失效且尚未原生终止。它只证明来源，调用者仍须复验 request、binding generation 和捕获水位，不构成通用 RPC 权限。三个新增单元测试覆盖 ACK 先到/后到、不完整和冲突输入、gap 及所有终态；专项 artifact `cc92943d-c03d-44d3-9e6c-c971786e7547` 三项通过。

探针在原版 TUI 初始化的同一已加载 Thread 上执行；TUI relay 是唯一观察来源。两个独立原生 RPC 客户端不 resume、不 queue、不订阅通知。回答前核对精确 Thread/Turn/item 以及实时来源门。并发写入由同一 barrier 释放，双方均不等待对方写入或 resolved；这不能证明真实人的 TUI 按键竞速、网络到达顺序或哪个客户端获胜。

成功 artifact `a05b17d6-9230-4805-a662-96b35a8c323d` 的 `report.json` 为 `passed: true`，以下九个业务用例均单列 passed：

| 用例 | 实际断言 |
| --- | --- |
| `commandConcurrentAllowDeny` | 两连接同时 accept/decline，一次 resolved、一个 declined 工具结果，迟到答复未反转 |
| `commandConcurrentDenyAllow` | 反向分配答复，一次 resolved、一个 completed 结果，退出码 0 且合成 literal 输出符合预期 |
| `fileApprovalAllow` | 审批前精确单文件 Add scope 已知且 grantRoot=null；允许后只有临时文件产生并包含合成内容 |
| `fileApprovalDeny` | 同一审批前范围；拒绝后文件不存在，原生 item=declined |
| `fileConcurrentAllowDeny` | 同一范围，两连接并发答复，结果为一次拒绝，文件不存在，迟到答复未反转 |
| `questionConcurrentDifferentAnswers` | 同一阻塞 Plan 问题同时 Yes/No，模型只收到其中一个完整答案，未合并或被迟到答复替换 |
| `questionStaleIdDoesNotConsumeCurrent` | 其他 request ID 的答复不消费当前问题，随后准确 ID 的 Yes 到达模型一次 |
| `questionMalformedFirstConsumesRequest` | `answers: []` 消耗 raw callback，模型收到空 answers；稍后的合法答复不能修复 |
| `questionUnknownIdPassesThrough` | 形状合法但未知 question ID 的答案被原生透传；生产必须自行核验 ID |

每个用例还核对成功原生 Turn completed 与自己的显式 final_answer，证明交互之后来源未失效。三个文件用例分别验证完整 pre-approval scope；这些检查不额外计作业务用例。版本和 normal peer/wrapper/socket 回收也单列，不混入九项交互数。

原生 callback 消费 raw response/error 后再解析 typed answer，没有应用层采纳 ACK。WebSocket 写成功和 `serverRequest/resolved` 都不能显示为“远程批准已生效”；工具结果、整轮完成和 Telegram 投递必须分开。

## 3. 失败诊断与修正

前两次 artifact `446cf556-7f82-4fb2-a54f-a933c2af0d36`、`76f688d0-ca47-4b95-a366-e5e8e8ee4090` 均记录 failure，未计为通过。编译、合成 TUI 和正常原生退出通过，但首个命令用例在来源解析前失败。

新增静态 failedStep、解析前生命周期计数及布尔诊断后，成功运行证明首个用例还捕获到其他 Thread 的事件，其中有一个 agent item 的 phase 不属于该来源解析器支持范围。探针原先在过滤目标 Thread 前调用严格解析器，误将外线程事件作为目标解析错误。修正为先过滤准确 Thread，再解析；目标 Thread 的未知 phase 继续严格拒绝，没有放宽来源门。生产 session decoder 本来就先按目标 Thread 过滤，本次未修改该生产行为。

模型标题 schema 单独返回有效标题 JSON；按 Turn 存储首个准确 call ID，只提取该 call 的 function_call_output，不使用全局请求序号减一或历史答案。报告只保留静态名字、布尔和计数，不输出原始 error、正文、临时路径或 native IDs。

probe 使用新进程组，核对内核 socket peer 的 PID/UID/PGID，macOS 额外核对启动时间。WNOWAIT 保留组 leader 至确认 peer 消失后才 reap；失败 fallback 只作用于该独有组。物理 socket 记录 UID/dev/inode，仅在确认 peer 消失后删除身份匹配的叶，保留共享父目录和 lock。正常 cleanup 已通过；不声称任意 detached 工具也已结束。

## 4. 文件范围与精确取消的源码边界

公共 FileChange approval 参数没有 changes。固定源码的 apply_patch handler、tool events 与 App Server 测试一致证明：完整 `item/started(fileChange)` 先于审批，包含准确 item 身份和 changes。安装版三个文件用例补证单文件 Add 的该顺序。`patchUpdated` 可能是流式预览，不能单独作为完整审批范围；rename、多文件、未知操作、超限与 grantRoot 更广授权尚须生产负向验证。原始 diff 不进入渠道摘要。

固定源码的 `turn/interrupt` 在检查准确 Turn ID 之后异步提交不带 Turn ID 的 Interrupt；approval cancel 经过异步路径转换为 Abort，core 中断当前 task。已被取出的旧响应无法由 pending callback 清理追回。前置 ID 检查不能排除旧 Turn 结束、新本机 Turn 开始之后中断错误任务的窗口。问题 error/畸形答案生成空 answers 继续执行，也不是取消。详细来源链接和限制见[接入契约 §5.4](../v3-telegram-channel-contract.md#54-已提交-turn-的取消缺口)。精确整轮取消继续保持未完成项，不能用有限运行样本或渠道撤销代替。

## 5. 复现、标准检查与剩余工作

先按开发指南通过标准构建，登记新的受管 artifact。以下命令只写入尚不存在的报告文件；`WORKBENCH_TEST_CODEX` 应指向自行安装且受测的 binary：

```sh
WORKBENCH_TEST_CODEX=/absolute/path/to/codex \
  cargo run --locked --example v3-native-channel-probe -- \
  --interaction-races --interaction-report /absolute/path/to/managed-run/report.json
cargo test --locked --lib live_owned_turn -- --nocapture
```

`node scripts/dev.mjs check --offline` 完整通过，artifact `ec2691f7-0d61-46c1-ab8c-c6c2d66b0c71` 为 success：113 份文档、80 个工具测试、Web typecheck/262 个测试/build、Rust fmt/Clippy、all-targets 合计 588 passed / 0 failed / 60 ignored，以及两个 binary 的 debug/release 构建。九个原生用例单独来自上述 example 实跑，不算进 Rust 588 项。60 个条件专项本轮未逐项执行；未改变网页行为，未重复 Chrome 场景。另行核对实际暂存区 10 个文件和对应完整 Git 树 696 个 blobs，二进制检查均无发现；正式 HEAD 在交付时复验。

本轮修改来源谓词、其回归、隔离 probe 和权威契约/索引；生产 callback、配置 schema、receipt 格式、旧数据库和 migration 均未改变。没有远端 push、合并或发布，也没有真实 Telegram 调用和私人数据上传。

P2 下一片需接入默认关闭的本机必要交互授权、连续 request 状态、有界摘要、严格 typed response、绑定原始 Telegram 消息的一次性 callback，以及无正文收件与失效/故障矩阵。尚未证明真实 Telegram、本机实际按键同时回答、callback 发送确认前到达、撤销后迟到、慢发送/存储故障或生产恢复。P1/P3 真实 Telegram 用户验收仍独立待完成，不由本探针替代。
