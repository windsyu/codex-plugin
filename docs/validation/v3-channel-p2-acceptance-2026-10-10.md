# V3 P2 增量验收：文件范围、失效入口与只读报告

> 2026-10-10，macOS / Apple Silicon；运行实现基线为本地 `da13e12`。本片新增测试与汇总报告示例，未改变渠道运行、配置 schema 或收件格式。P2/P3 仍未整体验收；阶段状态只在 [V3 计划](../v3-implementation-plan.md)维护。

[接入契约](../v3-telegram-channel-contract.md) · [排队取消记录](v3-channel-queue-cancel-2026-10-09.md) · [专项复现](../development/debugging.md#v3-渠道专项与离线验收报告)

## 1. 本片范围

核对现有证据后补齐两类直接验收：原版 CLI 的四种文件操作，以及 Relay/Session/Service 之间三种不同失效入口。TUI 断线、epoch、捕获溢出、撤销和不确定写入已有组合证据，本片不为每一层重复整套故障矩阵。

同时提供独立的运行时汇总报告示例，可在不创建监听的情况下生成可查看 HTML。它呈现经过核对的证据与用户清单，不能替代真实 Telegram、真实模型、用户同时输入或跨设备验收，不改变 P0 历史页面。

## 2. 原版 CLI 文件范围

`installed_cli_production_telegram_file_scope_path` 使用实际 Application、原版 CLI 0.156.1、生产绑定/发送/收件/callback；只有 Bot/model 是 loopback 合成端点。子进程通过 env_clear 隔离 HOME、USERPROFILE、CODEX_HOME、TMPDIR，复用 kernel peer/组/birth identity 和 socket inode 清理保护，不接触私人目录。

同一已绑定 Thread 的两个渠道任务分别拒绝、允许一份包含 Add、Update、Delete、Update-and-Move 的完整 patch：

- 审批前核对两个目录的准确文件集合和字节，新增及移动目标均不存在。
- 提示完整包含四项源路径、操作种类和移动目标，不要求原生 map 顺序；只有 `Allow once` / `Deny`，没有 diff、文件正文或 shell heredoc。
- 拒绝后所有原始字节和文件集合不变；允许后准确产生新增、更新和移动目标，删除目标及移动源消失，另一目录不变。
- 错误使用者、错误提示消息、旧引用和重复 callback 不推进执行。CallbackReceived 在确认和模型工具继续前已持久存在，并关联准确任务输入身份。
- 最终模型内容故意延迟，原生整轮完成前不外发 final；同一目标及上下文延续。收件没有正文、路径、引用或原生 ID。

此专项不伪造 grantRoot 请求，不证明更广范围授权可远程处理；该 schema 保持回本机。实际 TUI 与渠道同时响应、真实模型及 Telegram 仍须单列验证。

## 3. Service 失效入口

| 入口 | 直接断言 | 证据边界 |
| --- | --- | --- |
| `Service.unregister` | 真实执行器已弹出的 busy 等待任务与剩余队列失效，无 Submitting；重新注册并确认后的新绑定不被旧 generation 输入影响 | 仅证明替换身份保持有效，不代表新会话已完成一次对话 |
| 原生 `thread/closed`、TUI `thread/unsubscribe` 请求 | 原始 wire 经过 Relay；来源资格失效后，迟到的 Bot 提示 ACK 不激活旧引用，旧完成证明失败，旧 callback 不能回答；新订阅/绑定不受旧输入或 Cleanup 影响 | 退订在请求发出时即保守失效，不等同原生已完成退订；该用例直接调用 offer，未运行完整 final 投递路由 |
| 畸形 JSON | 原始字节继续转发到 native；真实 `Session.observe` 报 `channel_protocol_invalid`，生产 execute_job 暂扣晚到 final，确认过的交互不能回答、排队正文失效 | 不把解析失败转换成原生错误；不宣称观察可恢复旧资格 |

独立 GPT-6 审查发现退订夹具原用 id-less notification，改为准确 request ID，并验证资格在响应前失效、迟到响应不能恢复。生产 Tracker 未改变。解析用例实际运行 execute_job，补足上表直接 offer 用例没有独立证明的 final 暂扣路径。

## 4. 验证结果

初轮受管 artifact `d43e4006-d4cf-4ea3-8bb7-4c53d0846feb` 为 success：前端构建、渠道 228/228（4.37 秒）、原版文件专项 1/1（4.09 秒）通过。此轮早于退订 request envelope 修正和报告示例加入，不能作为最终全检。

修正后的最终定向 artifact `659599f1-49cf-4cf3-bf3a-e2c78d090d35` 为 success：前端构建、渠道 228/228（4.44 秒）、报告安全测试 11/11（0.02 秒）、报告示例构建、原版文件专项 1/1（3.86 秒）通过。原版专项要求两份完整四操作审批、4 个普通模型请求、2 个 callback 收件及准确文件结果；没有运行真实 Telegram、模型或实际 TUI 同时响应。

报告的独立 GPT-6 审查核对严格七项 schema、有界文本/计数、输出分支先于监听、create_new/0600、HTML 转义、只读路由与负向测试。schema 校验只验证格式，不验证 artifact 存在或计数真实性；汇总作者仍须关联实际元数据及日志。CGNAT 范围检查不证明 Tailscale 身份，离线 meta CSP 不提供 HTTP frame-ancestors 保证。

标准全检 `node scripts/dev.mjs check --offline` 的 artifact `c7cf90d5-bd8c-4c3f-9a2f-0c3012270fcd` 为 success，完成时间与 lease 已核对：117 份文档、80 项工具、274 项前端，typecheck/前端构建、fmt、Clippy 通过；Rust all-targets 的 13 组结果共 714 passed / 0 failed / 63 ignored，两个 binary 的 debug（0.10 秒）和 release（21.60 秒）构建通过。63 个 ignored 不计作通过；本片原版专项单列，其余历史原版专项以各自记录的源码边界为准。缓存约 43.7 GB 仅提示，未清理用户历史或未登记资料。

离线报告在受管 artifact `061ff737-d999-4fe5-9586-b5a16d4279a1` 准备，JSON 与 HTML 为新建 0600 文件。准备器先核对实际 success/完成/lease 元数据，再从标准日志复核 714/274/80 和三项失效测试及原版专项；页面只含经过核对的合成汇总、CLI/产品版本、明确限制和用户清单，没有原始日志或正文。最终 `report.html` 使用本片提交身份；原型保留其基线身份，不将研究 checkout 当作产品 commit。

本机系统 Chrome 对 1100 px 桌面和 390 px 窄屏执行 DOM 与全页视觉检查：七项证据、七步用户清单、714/0/63 等准确计数和 P2/P3 待验收标识可读，没有横向溢出、输入/控制表单或脚本。截图 provider 的路径配置拒绝将 PNG 写入当前受管目录，改用原生内联全页截图检查，未保留 PNG 文件；不将截图写盘计为通过。离线报告生成没有启动监听，具体 Tailscale 地址仍待人类授权。

提交前复验：117 份文档、工作树与暂存区差异检查通过；暂存区 12 个文件、候选树 714 个文件的二进制策略均为 0 errors。报告与合成日志保留在受管目录，未提交凭证、私人会话或构建产物。

## 5. 用户验收与剩余任务

真实配置与操作仍按[使用说明](../guides/usage.md#命令参数)在本机完成；请勿将 bot token 发到报告或聊天中。待验收至少包含：

1. 确认准确已有 Thread 和私聊身份，单独启用必要交互权限。
2. 通过当前已确认提示拒绝/允许明确的文件范围，核对实际文件结果。
3. 核对选项与显式 Reply 文字回答、旧提示不能再操作；模型不提供追问能力时记录不可用，不视为已验收。
4. 取消 busy/queued 及慢最终发送确认窗口中的等待任务；提交开始后回原生处理取消。
5. 撤销、Run 停止及失去来源后，确认旧消息不能操作新绑定；重启后不恢复授权或正文任务。

精确已提交 Turn 取消、实际 TUI 同时响应、真实 Telegram/模型和跨会话管理仍未完成。本片没有 push、Issue/PR、合并或发布。新 Tailscale 报告监听的具体地址授权仍未收到；不会替换已有 P0 入口或绕过自动审批拒绝。
