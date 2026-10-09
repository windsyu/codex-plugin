# V3 P2 增量验收：输入身份与同会话重新绑定

> 2026-10-10，macOS / Apple Silicon；起始实现基线 `f82ba53`。本片修复渠道输入领取顺序，并补充同一原版 Run/Thread 的撤销后重新绑定。配置、旧数据库和 journal v5 不变；P2/P3 仍未整体验收，阶段状态只在 [V3 计划](../v3-implementation-plan.md)维护。

[接入契约](../v3-telegram-channel-contract.md) · [先前失效证据](v3-channel-p2-acceptance-2026-10-10.md) · [专项复现](../development/debugging.md#v3-渠道专项与离线验收报告)

## 1. 可复现缺陷与修复

原入口只预查 Rejected。callback 或文字回复先移走当前 Pending，再由持久写者发现 Received update、Received message key 或已消费 query 的冲突并返回 Duplicate，导致仍有效的审批或下一题失去回答入口。普通旧按钮重放测试不能覆盖这个当前提示仍有效的情形；本片不将它描述为未经授权执行。

修复前 artifact `9a4e8ad4-80e1-4956-ab4b-551ec3292d9e` 出现三处目标断言失败，随后测试断言持有 mutex 导致清理二次 panic、SIGABRT，不能计为三项正常执行失败。夹具改为复制状态、释放锁后断言；运行时保持原基线。第二次 artifact `28d1a0a5-f24d-46e5-9f46-05b1cdd25b63` 正常完成为 0 passed / 3 failed / 658 filtered，准确复现三个当前提示丢失问题。两次均实际登记 failure、完成时间和已释放 lease，失败不计通过。

当前入口在任何 async lookup 前，以短锁保留 bot/update、可安全提取的用户 message key、固定 32 字节 callback 摘要；独立 owner UUID 只拥有该输入保留，不授予交互权限。随后通过独立 OS receipt worker 查询全部五种消费域，再执行原有授权、来源及引用校验。冲突直接返回，不移走提示、不写入、不学习第二身份；实际写者的原子校验仍保留。callback 提示消息 ID 不作为用户 message key。

只有 owner 的正常 Ok 释放保留；错误或调用取消保留至 Service 结束，撤销和重新配置不清空。最多 17 项，满时停止渠道并给出 `channel_input_capacity`，不能淘汰旧身份或重新授予不确定输入。这个容量与独立取消的内部保留分别有界。网络/PTY 转发不等待保留或收件。

Store 的新增查询只读，不改变历史写入、Event/schema、格式版本、回放验证或旧文件。旧 v1/v5 中同 update、不同 message 的合法 Received 行仍可读取；当前 Service 的全域预查更保守，不借修复拒绝历史行。

## 2. 并发与存储断言

七项 `interaction_duplicate_identity_` 回归通过生产 `process_update`：三个修复前失败场景、同时有效审批的 update/query 冲突、callback→文字及文字→文字的跨身份域冲突、等待 lookup 时丢失调用者、17 个遗留保留与第 18 个容量拒绝。暂停实际 receipt worker 时，重复调用必须单次 poll 就返回 Ok，两份当前提示及 journal 保持；释放后核对准确 typed native answer，另一提示使用新身份仍能成功。重复输入不学习的新 query/update/message 在后续准确回答中独立验证。

丢失调用者测试通过 `writer.load` 排空屏障后再次重放旧身份，仍不能消费提示；新身份可以回答。容量测试每次单独排空已取消查询，区分身份保留上限和 receipt 队列满。该新增丢失调用者用例覆盖 lookup 等待点，不单独宣称验证 fsync 后丢 ACK；后者仍按既有 receipt 故障证据阅读。

四项 Store/Worker 回归覆盖全部 update/message/query 域、跨 bot/chat 隔离、不学习冲突身份、无追加、重开保持、历史 Received 行，以及实际独立 OS 线程和取消排队查询。验证包含 journal 字节、sequence 和 task counts 不变；等待查询时单线程 Tokio 继续运行。正文、原始 query、按钮引用和原生身份不进入收件。

独立 GPT-6 静态审查核对入口 reserve-before-lookup、owner 释放、所有消费索引、并发测试及原版夹具，未发现冻结版阻断问题。审查没有运行测试，实际结果以下方执行记录为准。

## 3. 同一原版 Thread 的恢复

`installed_cli_production_telegram_rebind_recovery_path` 使用实际 Application、生产 Service/receipt 和原版 CLI 0.156.1、实际 PTY；仅 Bot/model 为 loopback 合成端点。父子 HOME、USERPROFILE、CODEX_HOME、TMPDIR 隔离，保留准确 kernel peer/进程组/birth identity、socket inode 与回收保护。任何子任务/HTTP handler panic 均留下固定失败标记，父子成功出口都检查；不升级或修改官方 CLI。

- 旧渠道任务进入完整 Add/Update/Delete/Move 原版菜单后撤销；准确旧 channel task 为 Closed/abandoned、nativeCompleted=false，原生菜单仍可用且文件不变。Closed 不当作原生取消或完成。
- 显式重新配对、按最新 source revision 确认同一完整 run/thread/transport/subscription target，binding generation 改变且重新授权交互。旧 callback 持久拒绝；旧 message key 的重放通过实际 polling offset 确认已处理，没有新 Received/Submitting 或 callback ACK。
- 新绑定任务在旧原生审批仍 busy 时 Received、发出静态等待提示且没有 Submitting。再次观察准确菜单，只按一次实际 PTY y；旧工具 Success 与四种文件结果正确，新任务仍等待，旧模型 final 尚未释放。
- 释放旧 final 后，新任务自动继续同一上下文；模型请求准确包含旧输入、旧工具输出、旧 final 和新输入。旧 final 不发往 Telegram，新 final 暂停期间 task 仍 Submitting、nativeCompleted=false；准确原生完成后才有唯一新 final。

最终模型普通请求和 Bot 发送各 3 次；Received/Submitting/Closed 各 2，CallbackReceived 与 callback ACK 均零，Rejected update 准确为 1/3/4，总收件 9 行。没有第二次工具执行或隐蔽重试；原生身份/引用不入 journal。

这是同进程、同一已加载 Thread 的显式 revoke→rebind 恢复。旧渠道 actor 关闭后才确认新绑定，不能扩称旧 Cleanup 与 confirm 竞态；没有进程/Service 重启、传输重连、跨 Thread 或真实 Telegram。Bot HTTP 成功与新 polling 不当作私有按钮激活屏障。

## 4. 执行结果与交付

artifact `94e6d3e1-1e33-412f-be9c-7d3b55c34ca0` 实际为 success，完成时间存在、lease 已释放：先构建 Web；渠道 239 passed / 0 failed / 0 ignored（7.13 秒），含本片 11 项新增回归。显式 ignored 原版专项分别为重绑 1/1（4.50 秒）、文本 1/1（3.50 秒）、审批/混合问答 1/1（6.58 秒）、排队取消 1/1（4.58 秒）、四种文件范围 1/1（3.96 秒）。其他原版专项没有在本片重跑，不从常规通过推断它们已执行。

标准 `node scripts/dev.mjs check --offline` 的 artifact `372e9ff5-0676-4e2b-a99b-dd5476684e15` 实际 success、完成时间存在且 lease 释放（297.779 秒）。固定 Node 22.23.2 / Rust 1.95.0；120 份文档、工具 80 项、Web 274 项及 typecheck/构建、fmt、Clippy 通过。Rust all-targets 13 组共 730 passed / 0 failed / 66 ignored；两个 binary 的 debug（0.09 秒）和 release（21.61 秒）构建通过。ignored 结果与上方五项显式专项分开，其他未执行原版用例不计通过。约 47.8 GB 主缓存只提示，未清理。

本片没有增加依赖、许可、配置或 migration；输入保留及静态容量诊断为实验渠道行为修正，V2 默认路径与旧 `/v1` 只读契约保持。真实 Telegram、用户跨设备、全部同时交错、精确已提交 Turn 取消和第二阶段跨会话管理仍未完成。
