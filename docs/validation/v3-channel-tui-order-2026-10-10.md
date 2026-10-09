# V3 P2 增量验收：原版 TUI 与渠道的有序审批

> 2026-10-10，macOS / Apple Silicon；运行实现基线为本地 `c58ce1b`。本片只新增测试与文档，不改变运行行为、配置或 journal v5。P2/P3 仍未整体验收；阶段状态只在 [V3 计划](../v3-implementation-plan.md)维护。

[接入契约](../v3-telegram-channel-contract.md) · [文件与失效验收](v3-channel-p2-acceptance-2026-10-10.md) · [专项复现](../development/debugging.md#v3-渠道专项与离线验收报告)

## 1. 范围与原生语义

`installed_cli_production_telegram_tui_response_order_path` 使用实际 Application、原版 CLI 0.156.1、生产 Service 和无正文收件；只有 Bot/model 是 loopback 合成端点。子进程 env_clear，显式隔离 HOME、USERPROFILE、CODEX_HOME 和 TMPDIR，沿用 kernel peer/组/birth identity 与 socket inode 回收保护。本机审批只经过真实 PTY，不使用第二个 RPC 客户端代替 TUI。

只读研究 checkout 的实际 commit 为 `2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4`。其 [TUI 文件菜单](https://github.com/openai/codex/blob/2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4/codex-rs/tui/src/bottom_pane/approval_overlay.rs#L1015)将 No/Esc 映射到 Cancel；[公开 schema](https://github.com/openai/codex/blob/2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4/codex-rs/app-server-protocol/src/protocol/v2/item.rs#L115)区分 Decline 与 Cancel。因此本片不把本机 No 称为单项 Deny，也不测试 Cancel 或精确整轮取消。研究 checkout 不证明安装版二进制来源；专项必须独立断言安装版版本、当前菜单的本次目标与 `Yes, proceed (y)`，才能发送 y。

## 2. 两个有序方向

同一绑定 Thread、同一精确 target、连续上下文中，分别使用独立的 allowed/denied 合成目录；每次原生审批仍包含完整 Add、Update、Delete、Update-and-Move 范围。

- 本机先允许：实际菜单可见后只输入一次 y，原生工具报告成功且准确文件集合/字节改变。在最终模型流仍受阻、task 已 Submitting 而没有 Closed 时，发送已观察 Telegram 提示中的旧 Deny；核对持久 Rejected、没有 CallbackReceived 或 callback ACK，文件结果不变，然后释放最终回复。
- 渠道先拒绝：先观察本次原版 TUI 菜单，再发送准确 Telegram Deny；核对 CallbackReceived 在继续前持久存在、准确原生工具拒绝和文件完全不变。在 final 仍受阻时，当前屏幕的菜单标题及 Allow 选项在至少 300ms 的观察窗口内保持缺席，随后释放最终回复；不向退场菜单发送按键。

两轮要求 4 个普通模型请求、4 个 Bot 消息、2 个 Received/Submitting/Closed、1 个 CallbackReceived/ACK 和准确旧输入的 1 个 Rejected。同一来源与上下文贯穿两轮，receipt 无正文、路径、引用、原生身份或 token。后台标题请求单列，不计作普通模型请求。

第一方向没有单独读取 Service 内部按钮激活状态，不能仅凭 polling 周期宣称本机输入前引用已激活；已激活引用的本地响应退休另有 Service 定向测试。本片直接证明已观察提示的迟到输入在原生成功、整轮未完成时被拒绝。

## 3. 检查、失败与修正

首轮受管 artifact `3f902c19-40d1-49ca-8b7f-03a0facf066f` 为 failure：前端构建、渠道 228/228（4.42 秒）通过，原版专项在 second_turn 屏幕条件等待处超时（18.28 秒，固定诊断行 49）。此轮不能计为专项通过。

独立 GPT-6 审查指出两项测试可靠性问题：事件订阅仅在屏幕等待时消费，其他等待可能填满 64 项队列并失去 grant；单帧菜单缺失可能只是重绘中间态。修正为持续消费同一订阅，通过 watch 唤醒并保留 Drop 中止；退休判断移除可能已滚出画面的启动 banner，以普通 UI 辅助标记和至少 300ms 的菜单缺席窗口观察，空白重置窗口，候选稳定窗口内菜单重现则失败。原始屏幕不输出、不写盘，诊断仅允许固定布尔字段与静态条件名。这两项修正不等于已经单独定位首轮超时的唯一原因。

修正后受管 artifact `350f116b-03a5-422a-b4b5-334976ee6ac0` 为 success：前端构建、渠道 228/228（4.33 秒）和原版专项 1/1（3.75 秒）通过。专项确实观察安装版两个准确菜单、只输入一次本机 y，执行两份完整四操作 patch 的允许与拒绝，并核对上述文件、来源、收件及最终回复断言。专项为 ignored 测试的显式执行，不从常规检查推断其结果。

独立复审确认持续消费任务、watch 有记忆的唤醒和 Drop 中止合理，没有遗留循环引用。辅助 UI 标记可能匹配当前屏幕中的历史文本，watch 也会合并事件；它们不代表新的原生状态 ACK。结果限定为实际工具拒绝后、当前快照观察窗口的菜单缺席，不能写成逐帧无重现或内部审批对象已不可操作。

标准全检 `node scripts/dev.mjs check --offline` 的 artifact `6c22d403-403c-4600-8a84-018ef63f8ffd` 为 success，完成时间与 lease 已核对：固定 Node 22.23.2/Rust 1.95.0，118 份文档、工具 80 项、Web 274 项及 typecheck/构建、fmt、Clippy 通过。Rust all-targets 的 13 组结果共 714 passed / 0 failed / 64 ignored；两个 binary 的 debug（0.09 秒）与 release（21.10 秒）构建通过。64 个 ignored 不计通过，原版有序专项仍以上述显式执行为准。约 45.1 GB 缓存仅提示，未清理。

安全诊断实际显示 remote_retired 的 header/allow/menu 为 false、working/interrupt/composer/stable 为 true；仅这些固定布尔字段进入日志。原始画面、正文与私人路径没有外发。

增量离线报告在受管 artifact `dfb21037-b2a4-473e-8bbd-41039b0005be` 准备。准备器核对所有引用 artifact 的实际 success/完成/lease、原版专项日志和标准计数；更新四操作项为本片实际 TUI 证据，保留其余源码未变的历史定向证据，并显示首轮失败及真正同时响应缺口。原型使用运行基线 `c58ce1b`，最终输出须采用本片产品提交身份，不能将研究 checkout 当作产品版本。

原型在系统 Chrome 的 1100 px 桌面及 390 px 窄屏完成 DOM 与原生内联全页截图检查：7 项证据、8 步用户清单、714/0/64 等准确计数和 P2/P3 待验收可读，没有横向溢出或输入/控制/脚本。JSON 与 HTML 新建为 0600，没有创建监听，没有保存 PNG 或提交生成产物。

## 4. 证据边界与交接

有序响应不等于真正同时输入，也不证明公平性、所有交错或 in-flight 回答能撤回。公开 resolved 只有 thread/request 身份，不提供获胜端；WrittenUnconfirmed 仍不能表示原生已采纳。本片的结果依据真实工具结果、准确文件内容和当前 TUI 画面。

真实 Telegram/模型、同时响应、精确已提交 Turn 取消和跨会话管理仍未完成。原版 CLI 未修改或升级，旧数据库与配置无迁移，未推送、合并或发布。既有离线报告保留当时提交与证据；新 Tailscale 报告监听仍未取得具体地址的人类授权，不替换旧 P0 入口。
