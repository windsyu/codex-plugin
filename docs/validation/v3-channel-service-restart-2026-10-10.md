# V3 P2 增量验收：Service 重建与本机处理提示

> 2026-10-10，macOS / Apple Silicon；起始源码 `58dcc89`。本片补充实验渠道测试，不修改生产行为、配置、数据库或 journal v5。P2/P3 仍未整体验收，阶段状态只在 [V3 计划](../v3-implementation-plan.md)维护。

[接入契约](../v3-telegram-channel-contract.md) · [原版同会话重绑证据](v3-channel-input-rebind-2026-10-10.md) · [专项命令](../development/debugging.md#v3-渠道专项与离线验收报告)

## 1. Service 生命周期与凭证恢复

此前 Store/Worker 重开测试证明持久去重和阶段统计，原版 CLI 撤销后重绑证明同进程恢复；两者不能代替整个 Service 被销毁后重新配置、配对和完成新任务的证据。本片用临时 0700 目录、真实生产 Service/receipt worker、Relay→Session 和独立控制 socket，配合 loopback 合成 Bot。

旧 Service 通过公开 configure、begin_pair、confirm 和生产 process_update 取得授权并留下一个实际 Received 排队任务。旧 executor 故意不启动，保证排队正文仍在内存；另两个任务的 Submitting/Closed 和旧 callback 是通过实际 receipt worker 保存的合法合成元数据，不宣称曾由原版 CLI 执行。

测试真实 Drop 整个旧 Service，等待写者停止及独占存储锁释放；没有手动取走 worker。新 Service 注册同一合成 Session 并启动，最初没有配置、绑定、队列、原生提交或 Bot 请求，磁盘凭证和 journal 不变。显式 configure(None) 才读取已保存凭证，实际调用 getMe/getWebhookInfo，并恢复 Received=1 / Submitting=1 / Closed=1 统计；仍不恢复任何授权或正文。

新配置必须重新配对并按当前来源 revision 确认交互授权。真实 getUpdates 驱动新任务及当前审批回答，真实独立控制 socket 收到准确 turn/start 和 typed accept；Relay 上连续出现匹配的用户、审批、final item 和 turn/completed。旧 message key 的重放不提交任务，旧 query 的重放不移走当前提示；二者都不写 journal、不学习新 update 别名，这些别名随后用于准确新输入并成功。

最终新任务具有自己的唯一 Received/Submitting/Closed，旧排队任务仍 Received，合成 task 111 仍 Submitting；Closed 不代表旧任务被取消。final item 到 turn/completed 之间没有最终消息，准确原生完成后才有唯一 final 及 confirmed 投递。恢复统计为 Received=1 / Submitting=1 / Closed=2；journal 不含正文、query、引用、原生 request ID 或凭证。

Bot long poll 的取消由实际 TCP EOF 结算；测试结束须排空并检查服务器连接任务，异常不能被 detached task 或 Drop 掩盖。本片固定同一 Session/transport，未启动安装版 CLI 或重启 Application/宿主进程，不证明传输重连、实际崩溃、原生工具清理或旧 actor 清理与确认竞争。

## 2. 不支持交互的安全提示

一个表驱动 Service 测试覆盖 owned、foreign、revoked、unconsented 四种情况。请求来自真实 Relay 观察的合成命令审批，并带不支持的 additionalPermissions；通过生产 interactions::offer 和实际 Client HTTP 验证。

只有属于当前渠道 task、仍有效且明确授权交互的请求发送一次完整静态提示：需要在本机回答，当前渠道无法安全处理。再次 offer 不重复发送；外来请求、撤销和未授权场景均零 HTTP。四种情况都没有 native answer、按钮引用、排队或 journal 变更，命令、reason、cwd、正文和原生 request ID 不进入 HTTP/journal。

这是生产 offer 与合成 Session 的定向证据，不是整个执行器或原版模型工具的专项。本片没有扩大交互支持范围，也没有新增通用错误通知、对未知审批的默认响应或在失效后使用旧权限发送。

## 3. 验证与交付

独立 GPT-6 静态审查发现初版合成 Bot 的 detached supervisor 可能吞掉晚到的 handler panic；运行前改为明确停止、排空并检查连接与 supervisor。另补准确 task 生命周期及 final 前的 withholding 断言；冻结版增量复核未发现阻断问题。静态审查没有运行测试，不能单独计通过。

定向 artifact `1da29d73-9bb7-411d-ba7a-faf739c3a18b` 实际 success、完成时间存在且 lease 已释放：先构建 Web，再分别运行完整 Service 重建 1 passed（0.99 秒）、静态提示 1 passed（0.66 秒），最后渠道 241 passed / 0 failed / 0 ignored（6.87 秒）。两项新增测试没有 ignored；原版 CLI 专项在本片不重跑，仍按各自历史 artifact 阅读。

标准 `node scripts/dev.mjs check --offline` 的 artifact `f4100bd8-e539-45df-8884-2d7e5bb3d921` 实际 success、完成时间存在且 lease 释放（291.648 秒）。Node 22.23.2 / Rust 1.95.0；121 份文档、工具 80 项、Web 274 项、typecheck/构建、fmt 和 Clippy 通过。Rust all-targets 13 组共 732 passed / 0 failed / 66 ignored；两个 binary 的 debug（0.09 秒）和 release（23.13 秒）构建通过。未执行的 ignored 原版用例不计通过。约 48.9 GB 主缓存只提示，未清理；其后只修正文档，文档检查另行复验。

本片测试及导航本地提交为 `dd6e1ddc3b24ab0cdb11e1498400c1183c04acb8`，受检 tree 为 `5023e247afc8e29a9205ec13d67376e7c7c85cb5`。121 份文档、差异、11 文件暂存区和 726 文件候选提交树的检查通过；修改限于测试、测试注释和文档，没有推送、合并或发布。

离线报告 artifact `cf1e159d-e42b-4eaf-a78d-ba835693a5c1` 实际 success、完成时间存在且 lease 已释放，固定上述提交。JSON 5203 字节、HTML 7209 字节，均为新建 0600 文件；七项证据中本片失效/恢复引用当前定向 artifact，六项原版结果明确引用历史 `94e6d3e1-1e33-412f-be9c-7d3b55c34ca0`，不冒充重跑。

系统 Chrome 1100 px 桌面及 390×844 窄屏复核通过：7 项证据、10 步待验清单、8 项限制、准确提交和计数，没有横向溢出、输入/控制或脚本。初次 DOM 脚本误选计数 table，改选逐项证据列表后复验；原生内联截图确认桌面和手机当前 Service 证据/计数可读，没有改写页面或保存 PNG。文件面板请求返回 queued，不称为已打开。交接文档提交不改变报告标识的受测提交。

只读远程入口仍需要准确 Tailscale 地址/端口授权；此前自动审批拒绝不能被自动续跑视为同意。本片再次核对目标端口没有监听，先提供离线报告，不开放新监听。真实 Telegram、跨设备用户验收、精确已提交 Turn 取消和跨会话管理保持未完成。
