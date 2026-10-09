# V3 P2 增量验收：已准入渠道回答与实际 TUI 重叠

> 2026-10-10，macOS / Apple Silicon；本片起始实现基线为 `7cf5593`，只补充测试、测试探针及文档。生产控制 helper 为测试传输增加私有泛型，原有协议、来源门和无重试语义保持。配置、旧数据库及 journal v5 不变；P2/P3 状态仍由 [V3 计划](../v3-implementation-plan.md)维护。

[接入契约](../v3-telegram-channel-contract.md) · [有序审批证据](v3-channel-tui-order-2026-10-10.md) · [专项复现](../development/debugging.md#v3-渠道专项与离线验收报告)

## 1. 可确定的边界

公开协议没有逐个 response 的原生采纳 ACK；`serverRequest/resolved` 不给出获胜端，`WrittenUnconfirmed` 只表示本机 flush 成功。无法从这些信号证明两份回答到达时原生仍未决，也不能以增加第二个 RPC 客户端代替实际 TUI。

`installed_cli_production_telegram_inflight_answer_path` 使用实际 Application、原版 CLI 0.156.1、生产 Service、实际持久收件与原版 PTY。仅 Bot/model 为 loopback 合成端点。子进程 env_clear，HOME、USERPROFILE、CODEX_HOME 和 TMPDIR 分别隔离；沿用准确 kernel peer/进程组/birth identity、socket inode 与回收保护，不修改或升级官方 CLI。

测试在独立渠道控制 UnixStream 上加入单实例、一次性的 `cfg(test, macOS)` 写屏障：初始化和未启用状态直通，读方向直通。发送路径先完成真实 fresh validation，再同步激活屏障并 `start_send`，中间没有 await。探针记录成功准入与实际 `poll_write`/vectored 写入，保存的只有本次 typed RPC ID 与有界标量，不保存 request/response 正文，也不公开原生身份。

每次 Pending 保存最新 waker，释放与登记使用同一短锁，锁外唤醒；Notify 在读取状态之前登记。门的 Drop 释放，控制任务退出或停止关闭探针；超时/停止后的已尝试写保持 Unknown，不重试。该测试屏障不修改原生 relay、来源资格、capture 水位或 Service 授权。

## 2. 实际 TUI 与结果断言

- 首先观察本次完整 Add/Update/Delete/Move 的准确原版菜单，再发送 Telegram Deny。只有 `admitted=1`、实际写受阻且 `bytesWritten=0`，同时 CallbackReceived 已实际持久存在、callback ACK 为零，才进入本机操作。
- 再次观察准确当前菜单及 `Yes, proceed (y)`，只按一次真实 PTY y。要求匹配 typed RPC ID 的 TUI raw Accept 和原生 resolved 各一次，并检查原生工具 Success、四种操作的准确文件集合与字节；此时渠道仍为零写入、零 flush/ACK，模型 final 受阻、task 为 Submitting 且没有 Closed。
- 释放独立写屏障后，渠道已准入的迟到 Decline 实际写出并 flush 一次，结果为 WrittenUnconfirmed、callback ACK 一次。准确工具与文件结果保持，没有第二次工具/模型 continuation；旧 reference 的新 update 被持久拒绝，没有第二次准入或 ACK。
- 最后释放模型 final，核对准确渠道原生完成后唯一最终文本、收件各一次、相同 run/thread/transport/subscription target。任何子进程或 HTTP handler panic 都设置固定安全标记，父、子成功出口均拒绝该标记；不能因 handler panic 被 Tokio 隔离而误判没有重试。

TUI raw 捕获在 relay 转发之前，单独只能证明原版生成了答案。工具 Success 和准确文件变化发生在渠道仍为零写入时，才构成本机实际执行的因果证据；resolved 仍不当作获胜端 ACK。原始屏幕、正文、文件路径及原生 RPC ID 不进入诊断输出，诊断仅含静态阶段、固定布尔字段和有界计数。

## 3. 验证结果

受管 artifact `23675062-6a47-4796-afcd-098e7873f8a1` 实际登记 success、完成时间且 lease 释放：前端构建通过，控制路径 22 passed / 0 failed / 2 ignored（0.22 秒，含本片 5 项新传输测试），渠道 228/228（4.37 秒），新增原版 in-flight 专项 1/1（2.92 秒），原版有序审批回归 1/1（3.95 秒）。两个原版专项都是显式 ignored 执行，不能从常规结果推断；控制过滤器的另两项 ignored 不计通过。

新增专项的最终安全诊断为 admitted 1、blockedWrites 2、bytesWritten 46、flushed/WrittenUnconfirmed/TUI Accept/resolved 各 1，NotSent/Unknown/TUI other 为零，released 为 true、closed 为 false。零字节、durable callback、原生成功及 final 暂停等中间状态由专项断言验证，最终日志不复制屏幕或响应正文。

独立 GPT-6 审查核对实际依赖 0.29 的 start_send/flush 行为、同锁 waker 登记与释放、Notify 登记、typed ID 区分和关闭/超时语义，未发现冻结版本的阻断缺陷。夹具审查曾发现 handler panic 在计数前发生可能被吞，从而弱化无重试断言；已在实际运行前补齐 ANY-panic 标记与父、子成功出口检查。本片没有失败的定向运行，不能把这项静态修正写成运行故障定位。

标准全检 `node scripts/dev.mjs check --offline` 的受管 artifact `d37839f6-d131-427d-9f8f-5afa65a0582b` 为实际 success，完成时间和 lease 已核对。固定 Node 22.23.2/Rust 1.95.0，119 份文档、工具 80 项、Web 274 项及 typecheck/构建、fmt、Clippy 通过；Rust all-targets 的 13 组共 719 passed / 0 failed / 65 ignored，两个 binary 的 debug（0.09 秒）与 release（21.67 秒）构建通过。65 项 ignored 不计通过，本片两个原版专项的显式结果以上述 artifact 为准。约 46.1 GB 主缓存仅提示，未清理。

本片专项提交为 `91b84d827caa47300ffa740012fb4d01e8dfdfa0`。最终文档 119 份、差异、14 文件暂存区与 719 文件候选提交树的二进制检查均通过，没有新增许可依赖或私人 fixture。

离线报告实际登记在受管 artifact `7e92141f-68b6-4fac-baee-80f84a59302b`，result 为 success、完成时间存在且 lease 已释放。报告固定该受测产品提交，JSON（4785 字节）与 HTML（6790 字节）均为新建 0600 文件；准备器核对各引用的实际 metadata 和专项日志。扩展文件项采用本片有序回归与 in-flight 原版结果，其他历史原版专项保持各自 artifact，未冒充本片重跑。

系统 Chrome 的 1100 px 桌面、390 px 窄屏 DOM 与原生内联截图复核通过：7 项证据、9 步待验清单、719/0/65 等准确计数、正确提交身份、P2/P3 待验状态，无横向溢出、输入/控制/脚本。窄屏另检查实际 viewport 截图；没有保存 PNG、启动监听或上传页面。Codex 文件面板请求返回 queued，不称为已打开。交接文档提交不改变报告的受测产品身份。

## 4. 交接与未证明范围

该用例只验证已经准入、尚未写出的渠道回答，与实际 TUI 执行重叠时的具体交错；不证明双方均在原生未决期间到达、无控制的同时输入、公平性、全部交错或 in-flight 回答可撤回。真实 Telegram/模型、精确已提交 Turn 取消、第一阶段整体验收与跨会话管理仍未完成。

没有新配置、迁移或生产控制接口。生成产物按受管任务登记，不提交私人数据或构建产物；远端 push、merge 和 release 未执行。新 Tailscale 监听仍待具体地址的人类授权，不替换旧入口。
