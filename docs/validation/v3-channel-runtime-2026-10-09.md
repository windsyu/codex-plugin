# V3 实验运行接入与订阅边界

2026-10-09，macOS / Apple Silicon，本地开发增量。阶段状态只在 [V3 实施计划](../v3-implementation-plan.md)维护。P1 的 Service/API/UI 文本路径已接入源码，仍在验证、未验收；本文区分原生运行、合成专项、旧完整检查和真实 Telegram 验收。

[文档总索引](../README.md) · [接入契约](../v3-telegram-channel-contract.md) · [ADR 0055](../decisions/0055-v3-channel-native-protocol.md) · [P0 历史证据](v3-channel-origin-2026-10-09.md)

## 1. 接入范围

本地源码新增 `codex-view --channel-ready` 和启动偏好 `launch.channelReady`，默认 `false`。保存配置仅改变下次应用启动后的新 Run；现有普通 Run 不转换。实验 Run 拥有原版 App Server、私有 TUI relay 及原版 remote TUI，继续使用现有模型代理、PTY、网页观察和异步历史。原版 CLI 不修改、不升级；旧 `/v1` 继续只读。

安装版 `0.156.1` 的 App Server 不提供完整 named profile loader，公共 `ThreadStartParams` 也没有对应字段；当前源码研究 commit 为 `2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4`，不是已证明的二进制构建来源。实验模式明确拒绝 named profile，不复制原生配置或将凭证展开到参数。普通模式仍支持原有 named profile 路径。

配置继续使用 schema 2；旧 schema 1/2 中缺失 `channelReady` 按 `false` 读取。只增加启动偏好，不在普通配置中写 bot token、配对 nonce 或激活状态，无旧数据库迁移。

## 2. 原生订阅资格与只读 API

订阅资格来自同一 relay 捕获的 TUI `thread/start/resume/fork` 请求及匹配成功响应；必须区分字符串和数字 RPC ID，确认精确 Thread UUID 与已知普通来源。全局 `thread/started`、loaded 列表、历史正文及网页的最后用户气泡均不能授予资格，不猜测 TUI 焦点。

`unsubscribe`、关闭、notLoaded/systemError、解析或捕获缺口使资格失效。每个 pending 请求保存有界、请求之后观察到的状态证据；迟到 ACK 不能覆盖终态、错误或更新状态，也不继承另一个请求的旧状态。pending 64、候选 32、已用订阅请求 ID 256；不驱逐去重记录来继续，达到上限关闭渠道资格。

本机 owner 的 `GET /workbench/v1/application/channel-runs` 当前返回 `instanceId`、`runs` 和 `telegramImplemented: true`；true 只表示代码入口可用，不代表 P1/P2/P3 已验收。各项包含 `runId`、`supportsChannel` 和可选 `source`；source 仅返回 revision、transport epoch、phase、候选 Thread UUID/busy 和静态 failure。普通 Run 的 source 为 null。该只读接口没有正文或凭证；六个本机 Telegram 管理 API 另见[接入契约](../v3-telegram-channel-contract.md#3-本机绑定与凭证)，设备权限不能代替 owner。

消费者单帧解析上限 1 MiB，不保留请求或历史正文。资格失败不参与 relay 转发等待；当前消费者在缺口或断线后永久关闭该 Run 的渠道资格，不通过历史补齐或重连自动恢复它。

安装版 App Server 会将本 Run 的 `--listen` 地址发布为别名，真实 socket 位于原版 CLI 的共享受保护临时目录。源码见 [`unix_socket.rs`](https://github.com/openai/codex/blob/2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4/codex-rs/app-server-transport/src/transport/unix_socket.rs) 与 [`daemon_directory.rs`](https://github.com/openai/codex/blob/2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4/codex-rs/uds/src/daemon_directory.rs)。本实现只解析本 Run 私有目录中指定的别名，核对物理目录/叶子的 owner 和权限；连接在发送协议字节前验证内核 peer PID、独占进程组及 inode，初始化后再次验证。生产 relay 的每次连接固定同一 peer PID、进程组和物理 inode，替换目标时关闭资格。

原版 CLI 自己在共享临时目录创建本 Run 的 hash 叶子及锁，不属于测试 HOME/TMPDIR 的独占子树。清理仅删除已证明归属且 inode 匹配的物理 socket，以及本 Run 私有目录；不递归删除官方共享目录或锁。运行使用独立进程组，并保留 leader 身份直至 kill/reap；TUI 停止后立即启动有界 App Server 关闭，不等待历史 reader 淘汰。仅实验 App Server 覆盖 `thread_unload_delay_secs=0`。

## 3. 早期原生接入增量检查

- 前端类型检查、设置页 9 项测试和构建通过，初始 artifact `3f4cb4ca-853d-41a9-a673-c6ff3dbce253`；后续帮助与错误文案已在 `87076b51-e016-41a2-97a2-4f1b4696e10d` 重建。
- 迟到 start/fork 响应回归先在旧逻辑中实际失败（exit 101，`thread/start: notLoaded`），修复后订阅模块 16 项通过；artifact `d59e8d43-67ca-4a91-a275-4941841ddb5f`。
- Session 快照锁顺序回归先在旧逻辑中实际失败（exit 101），修复后先固定解码 epoch，再读原子捕获状态。渠道相关 80 项测试通过，包括绑定状态机 9 项；artifact `87076b51-e016-41a2-97a2-4f1b4696e10d`。这些是当时源码的定向结果，不覆盖后续原生启动改动，也不加到 P0 的旧全回归数量中。
- 原生入口专项 artifact `aecfcbda-6784-43db-862c-a6a2a7b5c1d8`：启动相关 23 项通过，原生 relay 15 项通过，单列 ignored 的真实 CLI 专项 1 项通过。该专项隔离 HOME/USERPROFILE/CODEX_HOME/TMPDIR，两次启动原版 remote TUI，均经精确请求关联取得 `source=vscode` 候选；取得 loaded-only 提交 ACK，合成模型 loopback 连接到达，丢弃捕获不停止原生转发，包装进程与原生子进程退出，匹配的物理叶子删除、官方父目录保留。还验证异步启动后的第二次授权复验拒绝时回收本 Run 目录。此专项使用生产 Owner 与 TerminalHost，不代替生产 Application 界面的完整操作链路或 Telegram 验收。
- 上述专项的 Clippy 当时因订阅模块两处建议失败，artifact 总结果为 failure；随后已按建议修正，完整检查纳入复验。首次订阅红绿尝试因跨模块 `stop_channel` 尚未补齐而编译失败，artifact `63340e4e-e36b-49c0-9b18-6654e062f7bf`，不算负例复现或通过。首次真实 App Server 启动因拒绝官方 socket 别名而失败，也不算通过；上述专项是在确认并修复别名机制之后执行。

独立源码评审发现并修正了：TUI 停止后 owner 留在历史 reader 中、异步启动后漏授权复验、start/fork 卸载先于 ACK 的资格反弹，以及 Session 比较不同时刻 epoch 的竞态。评审不是运行时成功证据，仍须验证最终源码。

## 4. 早期完整检查的时间边界

P0 的 Tailscale 页面继续只提供当时已验证的合成聚合结果，不含生产配置或控制权限。后续新增的 P1 bot 配置、绑定、撤销、收件、发送 actor 和 UI 不属于 P0 验收范围，也不属于下列旧完整检查。

原生接入增量冻结后的完整检查 `node scripts/dev.mjs check --ci` 通过，artifact `702fe763-69ca-40d7-b509-2e1167267890`：维护工具 80 项、前端 235 项、Rust all-targets 544 项通过，57 项 ignored 未计作通过；类型检查、Web 构建、fmt、Clippy 与 debug/release binary 构建均成功。提交树二进制检查覆盖当时的 HEAD，新增源码仍须在暂存后单列检查。后续 P1 actor 改动和工具进程组专项不属于这次冻结结果。

上述早期真实 CLI 生命周期检查只证明包装进程与 App Server peer 回收。后续工具专项见下一节，不能把父进程退出当作所有工具结束。所有真实 CLI 测试隔离 HOME/USERPROFILE/CODEX_HOME/TMPDIR，仅用合成配置，不写真实用户目录。本轮增量没有据此宣称推送、合并或发布。

## 5. 后续 P1 集成专项：红灯、修复与当前证据

这些是各自执行时源码的专项，不相加为新完整回归数量，也不覆盖执行后仍在进行的整改。

| 专项 | 实际结果与 artifact | 边界 |
| --- | --- | --- |
| 来源 attempt、ACK 与原子捕获水位 | `3b6e9c72-98c9-411c-b33d-a4bdf1affbc9`：显式禁用 ACK 门禁的负例 exit 101；恢复最终实现后 11 项 Session、2 项水位测试通过，fmt 通过 | 这是故障注入验证，不宣称历史 Session 已有相同接口；后续 sourceRevision 原子确认整改另验 |
| Telegram 本机面板 | `e2431021-5417-435f-a9f8-7a4e1997221a`：11 项 UI 专项、TypeScript 与 Web 构建通过 | 覆盖凭证清空、不回显错误正文、精确候选确认、来源 revision 变化、未知结果不重发和状态分离；不是浏览器/Telegram 端到端验收 |
| 重启后显式读取保存凭证 | `6a0ed543-554e-4b2b-973e-b37d558d9b82`：修复将 configured=false 误当成无保存凭证的按钮条件；12 项面板测试、TypeScript 与 Web 构建通过 | 新负例确认 POST configure 不带 token；不自动读取凭证，不调用真实 bot |
| Service 状态与重启后忘记凭证 | `b7be4a8d-2247-4401-b93f-080b2b32ea85`：27 项面板测试、TypeScript 与 Web 构建通过 | 补齐 confirmed/partialUnknown/noReply、waiting/abandoned/uncertain/rejected，并允许 configured=false 时明确 forget。先增回归实际 8 项失败（`7adc3d26-3576-4fae-be4b-4297b795e8b9`，当时 PATH 为 Node 26.7.0）；最终通过使用本机 Node 22.22.2，不冒充固定工具链 22.23.2 的完整检查 |
| receipt v2、拒绝记录与 Service | `28ee58f4-deeb-4a4b-b69c-4d7cc6c8ac3f`：Store 17、receipts 5、Service 8，共 30 项定向通过 | 拒绝回归先在 `cccde6e5-11ce-47de-a616-68cefb0498f1` 实际失败：已拒 update 被重新解释；未知提交/轮询切窗先在 `bbf094e2-a271-48b4-8c51-c99353ba45e7` 两项失败，再修复至左列结果 |
| 原生运行中工具退出 | `b5f947a6-c500-4dfc-be9e-41e23cc6705f`：24 项 launch 通过（2 ignored 另列），ignored 工具专项 1 项内含 3 个实际原版 CLI 场景，另有 Owner 专项 1 项通过 | 正常 pipe/PTY 为 NativeExited，受测工具 stopped=true；强制场景为 ForcedUnknown，组外 pipe 仍存活，测试使用精确已知身份清理。全部场景 unrelated_alive=true，cleanup_confirmed=true；不能推广为所有工具结束 |
| 同 HOME 多 Run 关闭门 | 旧 first-handle 逻辑在 `a98907fb-3d5b-41f7-91cd-1ea7ae38277d` 实际失败：漏掉 pending B 后启动了新 CLI；修复后的 Application 26 项通过（1 ignored），最终 `00abc477-701d-4908-bf75-e67712ad7b06` 门禁专项 1 项通过 | 矩阵覆盖 normal+pending、normal+unknown、all-normal、pending+unknown；不是所有派生工具回收证明 |
| 确认原子复验及渠道状态 | `281d10d3-4ae8-4aae-ad86-fb0527fcbe0e`：Service 9、Session 11、binding 9 项通过 | 修复后定向验证，不代替最终 P1 全量检查或真实 Telegram 验收 |

同 HOME 多个 Run 的关闭结果已改为聚合，并取得上述专项证据。confirm 的来源 revision 与 qualified target 已改为在同一 Session 锁内复验，避免分两次快照形成竞态；修复后的定向结果见上表。`f89160aa-af69-4883-b7f9-e4b0028b6b7f` 的 Service/Session/binding 尝试因前端源比 dist 新而在 build.rs 阶段阻断，三个过滤器均未执行，不能计为测试失败复现或通过。

### 5.1 集成标准检查：分段执行及修复边界

以下检查使用 Node 22.23.2、Rust 1.95.0。它们是标准检查分段完成的实际结果，不是一条完整 `check` 命令成功；已通过的维护工具和 Web 检查没有无条件重复。

| 执行 | 结果 |
| --- | --- |
| `e63fb6c6-ceb8-4908-8015-de512e2fa9de` | 标准 check 的文档 112 文件、HEAD 提交树 678 blobs 二进制检查、维护工具 80 项、前端 262 项、TypeScript 与 Web 构建通过；随后 fmt 因 channel/mod 模块排序失败。HEAD 树检查不代表全部未提交文件已被检查 |
| `aff68db9-f0e9-4672-b770-f63b36df7d54` | 修正格式后继续检查，Clippy 因 telegram_request 的 Result 错误 Response 体积过大而失败；随后改用项目已有静态 `(StatusCode, &str)` 错误模式 |
| `1eaad55f-882c-440d-8261-b721e33222f4` | fmt、Clippy、Rust all-targets 575 项、debug/release binary 构建通过；58 项 ignored 未计作通过 |

这些结果截至该次源码冻结。其后发现 Application 宿主在 drop RunWorker 后立即退出，可能切断 detached 的 15 秒原生回收任务。源码已加入正常退出的并行回收等待、同一 ShutdownHandle 完成检查，以及未发布 Owner 取消/失败的 Drop 等待；这些新增修复不在上述 575 项或 binary 构建结果内。统一 18 秒是 reaper 阶段的等待策略，不是整个 Application Drop 的硬时间上限，也不证明所有工具结束或多个真实 Run 并发退出已验收。

真实宿主退出回归在 `0591f4fd-8a22-49a7-a5e9-68f6c0aa828d` 实际失败：强制场景的 `reaped_before_exit=false`、`host_waited=false`。修复后的 `cf763032-2b18-492c-8a77-1485f7e27a2a` 中，宿主专项 1 项含 normal/forced 两个真实 CLI 场景通过，两个 witness 均打印 true；另一个 Owner 专项 1 项覆盖未发布 Owner 取消后回收。但该次测试的 witness 当时仅打印、未纳入最终成功条件；独立评审后已把 `pending_witness` 与 `reaped_before_exit` 纳入断言。

最终检查 `1de84f91-3ea0-474f-bb05-78384c495da4` 已完成，metadata 为 success：使用 Node 22.23.2 / Rust 1.95.0，fmt、Clippy、Rust all-targets 575 项通过、0 失败、59 项 ignored，以及 debug/release binary 构建通过。单列执行的 ignored 宿主退出专项 1 项通过（16.32 秒，内含 normal/forced 两场景），两个 witness 已纳入断言且均为 true；Owner lifecycle 专项 1 项通过（0.69 秒），覆盖取消未发布 Owner、第二次 verify 错误，两次 owned_primary_processes_reaped=true。59 项 ignored 不整体计为通过，仅单列上述实际执行结果。先前 575/58 记录保留其源码时间边界，P1 仍未验收。

本轮分段标准检查与最终 Rust 复验已完成；工作内容二进制检查覆盖当时的 42 项、发现禁止二进制 0 项。补充 CHANGELOG 后，`node scripts/dev.mjs binaries` 暂存区检查覆盖 43 项、发现 0 项；提交后应另行核验最终提交树。此记录不据此宣称已推送、合并或发布。

### 5.2 本机 Chrome 页面检查

`ae54fe86-3cd2-4b43-9d69-36c94560a9bc/ui-probe.log` 使用 Node 22.23.2 与系统 Chrome，在 1440 和 390 像素宽度检查当时最新 binary 的首页设置及 Telegram 未配置面板；页面错误数为 0。GET 状态没有创建 channel 目录或原生 HOME，没有启动 CLI 或调用 Bot API。面板截图已由主执行者查看。此检查证明未配置界面的实际渲染与只读惰性行为，不覆盖真实凭证、配对、投递或宿主退出修复。

## 6. 当前实现契约与未验收项

Service、Application owner API 和设置页已接通文本路径；六个 Telegram API 的 Host/Origin/cookie/instance 边界、lazy 显式凭证加载、一次 nonce 候选与本机确认按接入契约实现。启动或 GET 状态不会因此读取 token 或调用 bot API。撤销授权不回滚已开始的 token 保存，forget 是单独操作。必要审批和问题目前回本机处理，P2 未完成。提交前允许等待短暂捕获积压；发送分块前有积压则保守暂扣或记录部分投递，不自动补发，发送前有界等待与重验尚未纳入本片。

独立 journal 新写 v2、读取 v1/v2 混合；Rejected 与 Received 身份共享 4096 上限，总字节限制 4 MiB。拒绝和收件都须 fsync 后才能推进 offset；旧 polling 授权窗口的消息不得在新绑定下重新执行。无正文持久化、无自动 replay；损坏、容量和持久化失败均停止渠道。

`NativeExited` 仅确认 native peer 消失、拥有的 wrapper 正常退出并回收，且未使用强制 fallback；不是所有工具终止证明。`ForcedUnknown` 和关闭进行中的状态保留本机同 HOME 启动门。源码研究基线 `2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4` 显示：连接关闭允许已启动 handler 继续，[对应测试](https://github.com/openai/codex/blob/2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4/codex-rs/app-server/src/connection_rpc_gate.rs#L122)；全局线程关闭超时只[记录 warning](https://github.com/openai/codex/blob/2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4/codex-rs/app-server/src/request_processors/thread_processor.rs#L1286)；统一工具清理调用[非等待终止路径](https://github.com/openai/codex/blob/2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4/codex-rs/core/src/unified_exec/process.rs#L219)。因此不把 loaded 空列表、固定等待、请求 ACK 或 exit 0 提升为完整清理证明。

分段标准全量检查和最终退出修复复验已完成；完整本机生产操作链、真实 Telegram 上下文延续与本机来源隔离、P2 必要交互和 P3 用户验收仍未完成。安装版 `0.156.1` 不是已证实来自上述源码 commit；不扩大为其他 CLI 版本、Linux 或 Windows 支持。
