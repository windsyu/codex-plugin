# V3 P2 增量验收：原版 CLI 宿主重启

> 2026-10-10，macOS / Apple Silicon；起始源码 `0750b38`。本片补充两个独立宿主的原版专项及精确取消能力门，不修改生产行为、配置或 journal v5。P2/P3 仍未整体验收，状态只在 [V3 计划](../v3-implementation-plan.md)维护。

[接入契约](../v3-telegram-channel-contract.md) · [合成 Service 重建](v3-channel-service-restart-2026-10-10.md) · [原版同会话重绑](v3-channel-input-rebind-2026-10-10.md) · [专项命令](../development/debugging.md#v3-渠道专项与离线验收报告)

## 1. 两个独立宿主与真实持久历史

`installed_cli_production_telegram_host_restart_path` 顺序启动两个独立 Rust 宿主测试进程，使用同一套私有临时 HOME/USERPROFILE、CODEX_HOME、TMPDIR 和项目。各宿主启动真实 Application、Service/receipt worker、独占原版 App Server 和 remote TUI；CLI 固定为安装版 0.156.1，Bot/model 均为 loopback 合成端点，不访问真实凭证或模型。

父进程从本任务的 socket alias 取得 kernel peer credentials，记录同 UID 的准确 peer/wrapper PID、birth identity、进程组及 socket dev/inode。每个宿主取得父进程 guard 确认后才执行渠道任务；第一宿主退出后，父进程必须在 guard Drop 和第二次 spawn 前证明旧 peer/wrapper 已消失。guard 的后备清理不能制造成功证据。

第一宿主通过 owner HTTP 配置、配对及显式交互确认，真实 Telegram polling 提交文件任务；实际 typed Deny 后原版工具报告拒绝，准确四种文件内容保持，原生完成后仅发送一次 final。观察该 task 的 Closed 和后续 polling 进度，再提交第二个任务，令其实际模型 final 保持未释放；第三个输入实际 Received/排队并获得取消引用，始终没有 Submitting。

随后显式 Drop Application，再让宿主进程退出。私有 0600 交接文件保存合成旧 cookie、instance、Thread/target、引用和三个 task 身份，不写通用日志。父进程在第一宿主所有线程已随进程消失后只读校验 journal 的摘要/实际行，按准确 task 归约最终恢复计数；正常 Drop 可能为旧 actor 追加 Closed，不把其固定假设为仍 Submitting，也不把 Closed 当作原生任务已取消。

## 2. 显式恢复与旧权限失效

第二宿主初始 configured=false，无 active/candidate、队列、lastTask 或 Run；凭证/journal 字节保持，250 ms 内零 bot inspection/polling、普通模型请求和发送。新服务器 GET 使用旧 cookie 得到 401；新有效 cookie 携旧 instanceId 的 POST 得到准确 409/stale_instance，分别验证凭证和 instance 边界。

显式 configure 不提供 token，才从保存文件加载凭证并实际完成 getMe/getWebhookInfo。恢复统计与第一宿主退出后的实际 journal 逐 task 归约一致，仍无授权或正文。通过生产本机 RunWorker 的 Connect.resume 显式恢复准确旧 Thread；这是本机操作，不是 Telegram 恢复冷 Thread。实际 TUI 订阅产生资格后才重新配对/确认，binding generation 改变，新的 Run/transport/subscription generation 均为非空 UUID 且与旧值不同。

旧三条 message key、已消费 query、旧 callback reference 和旧取消 reference 通过实际 polling 重新到达；offset 至少 107 证明已处理。旧 message/query 不增加任务或消费身份，两个旧引用准确写 Rejected 105/106；原 journal 前缀保持，无普通模型请求、任务、发送或 callback ACK。新输入 107 取得唯一 Received/Submitting，模型上下文包含准确旧输入、实际文件拒绝输出和已完成旧 final；未执行第三个排队正文，没有旧任务重放。

新 final 暂停时 nativeCompleted=false、没有 Closed 或发送；释放后准确 TaskView 为 nativeConfirmed/nativeCompleted、completed/confirmed，唯一新 final 与 Closed 对应同一 task。模型普通请求第二宿主仅一次、Bot 发送仅一次、callback ACK 为零；Thread 保持准确旧 UUID，文件仍为拒绝结果。

## 3. 证据边界与实现准入

本片证明主动 Application Drop 后宿主退出、准确旧 peer/wrapper 消失、重新加载凭证和本机显式恢复后重新绑定的正常闭环。不区分生产 reaper 的 NativeExited/强制 fallback，不证明所有工具已结束、真实崩溃、断线重连、旧清理竞态或所有同时输入交错。child 的最后步骤为显式 process::exit(0)，不宣称所有 Rust 析构均执行。没有修改或升级官方 CLI，源码研究 commit 也不作为该二进制构建来源。

所有 child/HTTP handler panic 都留下固定失败标记，父子成功出口检查；诊断只输出白名单阶段、受限源码行号和布尔证据，不打印合成正文/凭证。活动写者存在时只读 journal，不重开第二写者；完整第一宿主退出后才开始第二配置。收件检查排除正文、query、cwd/HOME、命令/patch 和 token 标记。

精确已提交 Turn 取消另有[尚未满足的原生准入门](../v3-telegram-channel-contract.md#精确取消的实现准入门尚未满足)。只读源码实际 commit `2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4`、工作树干净；公开 params、processor、core handler 与公共测试交叉核对，未找到公开的原子 Turn 条件中断。能力门规定迟到取消不能中断新的本机 Turn，客户端提高复验频率不解除阻塞；本片没有增加取消接口或 journal 版本，也没有缩小该验收项。

## 4. 执行与交付

独立 GPT-6 静态审查要求补强第一 task 的 Closed 等待、第二宿主零 callback ACK 和完整 target 身份断言。首轮 artifact `efa4ae5c-1b18-4b8d-adb7-8f230d6f852b` 实际 success，原版 1/1（6.42 秒），是补强前结果。冻结补强后的 artifact `19e5fb1e-24ac-4edb-8ed6-153a6bd46b7a` 实际 success、完成时间存在且 lease 释放：先 Web 构建，原版专项 1 passed / 0 failed（6.27 秒）。静态复核不运行测试，不替代此结果；其他 ignored 原版专项本片没有重跑。

完整标准检查 `node scripts/dev.mjs check --offline` 实际退出 0；artifact `b0c14901-e040-4b3f-ad90-b391dbdbcf98` 为 success，完成时间存在、lease 释放，耗时 286.876 秒。固定 Node 22.23.2 / Rust 1.95.0；122 份文档、工具 80、Web 274、Rust all-targets 13 组汇总 732 passed / 0 failed / 67 ignored，fmt、Clippy（`-D warnings`）、类型检查与 Web 构建通过。ignored 不能计作执行，新增原版专项的实际结果单列在上一段。debug/release 两个 binary 构建通过，增量构建耗时分别 0.09 / 21.54 秒。主缓存提示 50,280,415,249 字节，仅提示，未清理。

真实 Telegram、跨设备用户验收、精确已提交 Turn 取消与第二阶段管理仍未完成；只读 Tailscale 入口仍待准确地址/端口授权，自动续跑不解除此前自动审批拒绝。
