# R2 增量验证：上下文、并发、拒绝与中断

日期：2026-09-20。本次补齐 compact/fork/resume、同轮 steering、并发结果与 WebSocket 来源的浏览器证据，并验证原生拒绝及取消未完成参数。阶段结论和限制汇总见 [R2 验收](native-cli-r2-acceptance-2026-09-20.md)，唯一状态表仍为[实施计划](../v2-implementation-plan.md)。

## 1. 环境与变更范围

- 分支 `codex/native-cli-live-workbench`，HEAD `930172493c163a903621ca14c540d1ca10dd4f3e` 加未提交工作树；无 commit、远程 Issue/PR、push 或发布。
- 安装版官方 CLI `0.154.0`，本机 Chrome `153.0.8010.50`，headless；没有下载浏览器。官方只读参考 commit 本次仍为 `633ab199cfd724aa78013c006b27a2b3d049fc3b`。
- 实际使用的 `target/debug/codex-view` SHA256 为 `357c5e41acd7eec32e96bed47e609683922e42490dc8a4ed0bd199b95fd1898b`，与[统一 ViewItem 验证](native-cli-r2-view-items-2026-09-20.md)相同。
- 本增量仅增加测试、浏览器探针及文档，没有改变生产行为、协议、配置或数据库。前端生产文件未变，因此复用上一增量的 TypeScript/Vite 构建和 133 项前端测试结果，没有重复构建来冒充新增证据。
- 三组原生场景均使用正式 launcher、临时 CODEX_HOME/项目及合成 HTTP/SSE 模型；并发场景使用真实 decoder/observer/Web UI、合成 HTTP/WS 字节和 cat PTY。没有真实模型请求、私人会话读取或用户配置写入。
- 现有三个用户调试服务未重启。只读检查其公开前端脚本均已没有“启用输入 / 释放输入权”，仅保留多页面冲突时的“在此输入”。

<a id="native-context"></a>

## 2. compact、fork 与显式 resume

[原生场景](../../src/workbench/proxy/tests/native_cli/contexts_browser.rs)与[Chrome 探针](../../web/e2e/r2-context-history-probe.cjs)执行以下路径：

1. 原生提交一次合成中文输入，取得第一条回复；执行 `/compact`，模型 fixture 返回带字面 script 的摘要。
2. 再次提交完全相同的文字，执行 `/fork`，第三次提交同文输入。
3. 正常结束原 CLI；用正式 launcher 的 `--resume` 显式恢复 fork 后的原生 Thread，确认没有自动请求或新用户气泡，再第四次提交同文输入。

实际结果：共 4 次对话请求和 1 次 compaction 请求；首两次对话同 thread、不同 turn，fork 改变 thread，resume 保留 fork 后的 thread。摘要确实存在于之后的模型输入和“网络 → 上下文与用量”，不成为新用户消息，也未执行脚本。首 Run 为 3 用户 / 3 模型；resume 的新 Run 初始为 0 条，实际提交后为 1 用户 / 1 模型。原生 rollout 共 4 条不同提交记录，逐一匹配请求的 thread/turn，没有读取诊断。

同 Run 刷新保持 PID/epoch 和卡片身份；显式新 Run 具有新 epoch，不以新运行的用户气泡重放旧上下文。原生恢复历史不等于网页已经具备 R3 持久历史。

参考源码事实：`core/src/responses_metadata.rs` 声明 compaction 用途，`core/src/compact.rs` 安装摘要到模型历史；`core/tests/suite/compact_resume_fork.rs` 验证 compact 历史在 resume/fork 后保留。上述为固定 commit 的实现事实；本项目是否正确关联，以这里的安装版测试为证据。

## 3. 流式回复期间追加输入

[steering 场景](../../src/workbench/proxy/tests/native_cli/steering_browser.rs)让第一条模型回复保持 receiving，通过真实 xterm 粘贴第二条输入。草稿阶段中央仍只有 1 条用户消息；原生 Enter 提交后才放行后续响应。

两次模型请求确实使用同一 thread 和 turn，第二次 input 包含追加说明。原生记录是同 turn 的 2 个不同 UserMessage item，页面为 2 用户 / 2 模型；第一张模型卡保持同一节点，焦点和刷新后的 itemKey、PID/epoch 不变。两条用户消息继续注明同轮内容按来源归组、与模型片段的精确先后未确认。测试没有把不同来源的观察顺序宣称为原生全序。

<a id="concurrency"></a>

## 4. 逆序结果、重复与 WebSocket

[合成捕获场景](../../src/workbench/web/tests/concurrency_browser.rs)与[浏览器探针](../../web/e2e/r2-concurrency-probe.cjs)覆盖实际 decoder 到 DOM：

| 输入及操作 | 结果 |
| --- | --- |
| HTTP 模型说明 → 两个同时生成参数的工具 → 结果按 B/A 顺序且各重复一次 → 模型后续回复 | 仍为 2 工具 / 2 模型，结果各回原卡；工具获得正式 wire ID 不换 itemKey，卡片顺序为模型、A、B、模型 |
| 展开参数、上滚阅读并聚焦终端后发布结果 | 原工具 DOM、展开、焦点、scrollTop 和可见锚点保持；结果只是已有内容更新 |
| 同一 WS 连接先观察两个 masked response.create，响应以相反次序开始 | 请求来源保留 create 1/2 序号，响应分别按显式 response ID 归属；不按到达顺序建立 create→response 关系 |
| 两个 WS 响应使用相同 call ID、wire item ID 和 output index，交错输出 | alpha/beta 参数及正文互不串接；一个完成时另一个仍 receiving，完成重放不增加卡片 |
| 第三个 create 携带 previous_response_id 和工具 output | 请求详情可读实际 previous_response_id 和 output，工具仍 unobserved；没有用前序响应 ID 猜当前 create 的响应归属或结果匹配 |
| 两个完成响应报告不同 usage；另一个完成事件缺响应 ID | 明确响应分别显示 21/5/26 与 34/8/42 的输入/输出/总 token；缺身份的 usage 不借用上一响应，不汇总为任务用量 |
| 未归属参数增量、图片 base64、加密 reasoning、已知秘密与 HTML | 无身份增量不进入内容；媒体/加密字段省略，已知秘密脱敏，HTML 作为字面文本；0 条伪造用户消息，pageErrors=0 |
| 按需详情、面板切换、320/736/1024px、刷新 | 流式状态更新未重复 GET 上下文，详情按手动刷新更新；终端节点/PID/epoch 和最终卡片身份保持，页面无整体横向溢出 |

这验证的是合成 WS 捕获与界面行为，不是安装版 CLI 或真实 provider 的 WS 兼容声明。固定源码 `core/src/client.rs` 的 WS 请求路径使用 previous_response_id 和增量 input；该字段表达前序响应关系，不足以证明当前 create 对应哪个新响应。本实现继续保留未确认关联。

<a id="native-lifecycle"></a>

## 5. 原生拒绝、未知工具与取消参数

[原生生命周期场景](../../src/workbench/proxy/tests/native_cli/lifecycle_browser.rs)与[Chrome 探针](../../web/e2e/r2-lifecycle-probe.cjs)验证：

- 临时原生规则禁止单条 `touch must-not-exist.txt`；CLI 返回拒绝错误，目标文件未创建。另给出未知工具和缺 `cmd` 的命令，均有各自后续 function_call_output。
- 三张卡均保留后续模型请求来源，结果可以键盘展开，执行状态保持 `result_observed`。未知工具为普通工具卡，不冒充命令。
- 第二轮让 apply_patch 停在参数生成中，原生 Esc 断开未结束的 upstream。原卡变为 `argumentsState=incomplete`、`execution=unobserved`，展开和焦点保留；没有虚构“已取消”或“已拒绝执行”，也没有生成拟议完整 Diff。
- 第三轮新输入正常完成。最终 3 用户 / 2 模型 / 4 工具，6 次主请求；刷新不重发，保持同一 CLI。
- 正常结束 CLI 后检查 rollout：有对应 turn_aborted，没有上述调用的 CommandExecution/FileChange 终态；两个测试目标文件都不存在，原配置和规则文件语义未变。

固定源码 `protocol/src/approvals.rs::default_available_decisions` 的普通命令审批默认提供批准/Abort；TUI 将 Esc/Cancel 映射为 Abort。`tools/events.rs` 还说明底层 Rejected 可同时来源于人工拒绝与运行准备失败。因此 `declined` 不能自动解释为“用户拒绝”，也不能从错误文字或 turn_aborted 推导出来。

**实际能力边界：** 规范 declined 的字段映射有[原生记录 fixture](../../src/workbench/rollout/tests/native_tools.rs)和[卡片渲染测试](../../web/src/workbench/ToolCard.test.tsx)，当前原生试验没有取得此终态，不宣称人工拒绝自动显示 declined；没有可靠 started 也不补执行中。C09 的当前验收是正确展示缺失/未知并继续使用 CLI。未来 provider/原生流程若提供新事实，仍需版本受测后才能扩大支持。

最初含重定向的 printf 夹具实际执行成功，未作为拒绝证据；最终改为单条被规则命中的 touch 并核对文件未创建。另一次探针误依赖固定英文错误句，改为检查实际拒绝类别、逐 call 输出与原生记录。compact 探针的精确按钮名和 steering 夹具的缺失合成凭证也分别修正后通过。这些是 fixture/探针修正，没有修改产品结果判定或降低安全断言。

## 6. 检查与复现

| 检查 | 最终结果 |
| --- | --- |
| compact/fork/resume，安装版 CLI + Chrome | passed，7.15s；两个 Run 均 pageErrors=0 |
| 同轮 steering，安装版 CLI + Chrome | passed，3.33s；pageErrors=0 |
| 并发 HTTP/WS 捕获 + Chrome | passed，2.12s；pageErrors=0 |
| 原生拒绝/未知/参数中断 + Chrome | passed，3.42s；pageErrors=0 |
| Rust 库回归 | 158 passed / 23 ignored，7.76s；ignored 不算通过，上述四项另显式运行 |
| 静态检查 | cargo fmt、包含 tests 的 Clippy、四个新增探针语法通过；6 份文档的 215 个本地链接/锚点、标题/代码块及变更空白检查通过 |

```sh
cargo test --locked --offline --lib
cargo clippy --locked --offline --lib --bin codex-view --examples --tests -- -D warnings
cargo fmt --all -- --check
cargo test --locked --offline --lib native_compact_fork_and_resume_keep_context_out_of_new_user_bubbles -- --ignored --nocapture
cargo test --locked --offline --lib native_steering_keeps_both_submissions_and_same_turn_without_optimistic_messages -- --ignored --nocapture
cargo test --locked --offline --lib browser_keeps_reverse_tool_results_and_websocket_contexts_in_their_proven_scope -- --ignored --nocapture
cargo test --locked --offline --lib native_tool_refusals_and_cancelled_parameters_remain_distinct_from_execution_facts -- --ignored --nocapture
```

需要允许本地监听和启动本机 Chrome；WORKBENCH_TEST_SCREENSHOT 可设为临时截图前缀。正式 binary 内嵌前端，生产前端变化后必须先构建 Web 再构建 binary。本次无生产文件变化，未重复此前通过的工具/文件执行用例，也未重测性能分位数。

## 7. 已检查截图

[fork 后仅显示三次真实提交](native-cli-r2-contexts-2026-09-20.fork.png)、[resume 原生历史与当前请求上下文](native-cli-r2-contexts-2026-09-20.resume.png)、[同轮追加输入及顺序说明](native-cli-r2-contexts-2026-09-20.steering.png)、[WS 请求/响应分别呈现](native-cli-r2-contexts-2026-09-20.websocket.png)、[原生拒绝/未知工具结果](native-cli-r2-contexts-2026-09-20.refusals.png)、[取消未完成参数](native-cli-r2-contexts-2026-09-20.cancelled.png)。均为合成数据截图；数量、状态、来源和焦点以测试断言为准。
