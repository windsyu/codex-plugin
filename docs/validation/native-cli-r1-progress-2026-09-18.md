# R1 阶段证据：输入仲裁与原生 PTY

日期：2026-09-18，补充更新 2026-09-19。切片状态统一维护于 [V2 实施计划](../v2-implementation-plan.md)。本文保留输入仲裁/PTY 的首次阶段证据；后续 WebSocket 和真实 Chrome 结果见[浏览器记录](native-cli-r1-browser-2026-09-19.md)，用户/模型基本聊天见[聊天验收](native-cli-r1-user-chat-2026-09-19.md)。正式 Launcher 与工具/命令仍未完成。

## 1. 已实现内容

### 1.1 输入仲裁

[control.rs](../../src/workbench/control.rs)实现独立于旧 Worker/InputLease/TurnOwner、数据库和 command ledger 的单 PTY 输入仲裁。它现由 [Terminal actor](../../src/workbench/terminal.rs)独占，检查与 PTY 写入在同一条串行命令中执行，不能在网络路由中预先授权再无条件写入。

- 页面连接默认只读；显式 claim 或确认 takeover 才获得输入权。最多 32 个连接。
- 接管、断线、重新绑定和释放都会使旧 generation 失效；排队输入在执行时再次检查，旧页面的已排队字节不能沿用失效权限。
- 断线保留 30 秒重连窗口，私有 reconnect secret 每次成功绑定都轮换；公开状态没有 secret。刷新可在旧 socket 报告断开前重新绑定，之后旧 socket 断开不能收回新连接的权限。
- 输入按 generation 内严格递增序号接收，单帧上限 64KiB；部分写入失败也消费该序号，避免重试不确定的字节。断线前输入不自动重放。
- 只有 controller 可以改变尺寸；重复尺寸不调用 PTY resize，非法或失败尺寸不更新状态。
- CLI 结束后所有 claim、takeover、reconnect 和输入均拒绝；查看结束状态不会重新启动进程。

### 1.2 真实 PTY actor

- 独立 Unix 线程持有普通子进程、PTY、仲裁、VT 状态和有界订阅；模型转发、数据库与页面不参与输入许可。命令队列 128 项，每次输入最多 64KiB，最多 32 个终端订阅，每订阅最多 64 个输出事件（单次原始读取 8KiB）。
- 同一步捕获终端快照并注册后续输出，避免快照/订阅间丢字节。复用[通用终端工具](../../src/terminal/mod.rs)中独立的 VT/filter，保留屏幕和输入模式；内存尾部最多 1MiB/60 秒，压缩或 resize 后明确标截断，不承诺完整 scrollback。该复用没有引入旧控制内核，已在 R5 准备工作中移至独立的 `src/terminal/`，通用前端工具同样移至 `web/src/terminal/`。
- 慢订阅满后移除并释放其输入连接，CLI 输出继续；重连必须重新快照。接管/重连后的旧输入 generation 被拒绝；新连接不重发旧字节。刷新只附着原 actor，不 spawn。
- PTY 使用非阻塞读写。原生进程不读输入时，单次写入最多等待约 100ms；部分写入错误仍消费输入序号，客户端应显示传递不完整并让用户检查终端，不能自动重发。
- 原生终端能力/光标查询由固定终端协议应答，CPR 使用查询位置的光标；过滤 OSC 剪贴板、链接、标题及文件副作用。异常记录 source、runEpoch、outputSeq 和安全错误码，不输出输入正文。
- 原生进程退出后保持可读终端状态、拒绝新输入，不启动 Shell。显式停止/host 退出先向本次拥有的进程组发送 TERM，750ms 后必要时 KILL，并回收子进程；不会扫描/终止其他 Codex。

目前验证覆盖 macOS 和安装版 CLI 0.154.0；Unix 实现不等于其他平台已经验收。2026-09-18 的检查没有 WebSocket/xterm；它们已在次日接入并完成独立浏览器验证，不能以原 PTY 单测替代该证据。

## 2. 检查与结果

```sh
cargo test --locked --offline --lib workbench::control
cargo test --locked --offline --lib workbench::terminal
cargo test --locked --offline --lib workbench::terminal::tests::native_cli -- --ignored --nocapture
cargo test --locked --offline --lib workbench::
cargo clippy --locked --offline --lib --tests --examples -- -D warnings
rustfmt --edition 2024 --check src/lib.rs
```

验证结果：

| 范围 | 结果与限制 |
| --- | --- |
| 原有输入仲裁 | 8 项通过：单写接管、旧 generation/排队输入拒绝、重连令牌轮换、窗口到期、部分写入、重复/乱序、resize、结束状态和私有令牌 |
| 新增 PTY 集成 | [8 项通过](../../src/workbench/terminal/tests.rs)：中文多行真实字节、拒绝只读/旧写者、重复输入不再写、快照后连续尾流、同进程重连/resize、慢页不阻塞、CPR/OSC、原生退出、强制清理、原生停止读取时的有界失败。使用临时目录中的 shell 夹具，不是产品提供任意 Shell |
| VT/filter 复用 | 12 项通过：UTF-8/宽字符、SGR/DEC、光标/能力应答、OSC 过滤、超长序列、快照属性及缺失标记 |
| 安装版 CLI | [1 项单独通过](../../src/workbench/terminal/tests/native_cli.rs)：0.154.0，隔离 home/项目，普通 CLI 初始化、中文未提交草稿、同进程重新附着及输入权恢复、Ctrl-U 清草稿和 Ctrl-D 原生退出。没有提交模型任务，没有新建 sessions 历史；不替代网页/真实模型验收 |
| 工作台常规回归 | 73 项通过、0 失败、7 项 opt-in 忽略；其中上述 1 项安装版检查已单独运行，其余既有浏览器/模型 opt-in 本次未重跑 |
| 静态检查 | Clippy（lib/tests/examples）、rustfmt 与 `git diff --check` 通过 |

常规回归首次在运行沙箱内有 16 项因 loopback bind 被拒而失败；使用获准的沙箱外本机回环测试重跑后全通过，没有改断言或降低门槛。测试仅使用合成数据、临时 home/目录，不修改真实用户配置或调用真实模型。

2026-09-19 已完成受认证 WS、持续挂载 xterm、三列正文界面和浏览器信任/中文粘贴/确认接管/同进程刷新检查；修复 resize 宽字符 panic 并隔离屏幕重建异常。范围和限制见[后续记录](native-cli-r1-browser-2026-09-19.md)；独立短断线、多行编辑、picker/审批和完整聊天仍需验收。

2026-09-19 后续已接入请求 metadata、请求/响应模型名和 HTTP 辅助/未知请求隔离，见[阶段记录](native-cli-r1-chat-metadata-2026-09-19.md)；继而接通原生用户 reader、真实用户气泡和[独立聊天验收](native-cli-r1-user-chat-2026-09-19.md)。工具/命令卡和执行结果仍未接入。[聊天契约](../codex-native-cli-workbench-detailed-design.md#chat-item-contract)和[C01–C10](../v2-implementation-plan.md#chat-acceptance)明确 R1 角色与 R2 工具范围；本页早期 PTY 检查和原型示例不替代后续证据。

## 3. 交付范围

当前仍为 `codex/native-cli-live-workbench` 未提交工作树。未执行 migration、用户服务切换、全局配置修改、提交或远程操作；既有工作树修改保留。
