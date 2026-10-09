# V3 P2 授权交互与 callback 接入（2026-10-09）

[验证索引](README.md) · [V3 实施计划](../v3-implementation-plan.md) · [接入契约](../v3-telegram-channel-contract.md#5-输出与必要交互)

## 1. 本片结果与边界

本地生产代码已接入本机独立交互授权、连续 request 状态、严格 typed answer、确认发送后才激活的一次性 Telegram reference，以及无正文 callback 收件。普通 V2、旧只读 `/v1`、官方 CLI 和旧数据库不变。P2 整体尚未完成；真实 Telegram、自由文本回答、独立排队取消和精确原生整轮取消仍待完成或解决。

环境为 macOS / Apple Silicon、原版 `codex-cli 0.156.1`、Rust 1.95.0、Node 22.23.2，锁定依赖。只读源码实查 commit `2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4`，不将它当作安装版 binary 的已证实来源。全部原生子进程隔离 HOME、USERPROFILE、CODEX_HOME、TMPDIR；合成服务只监听 loopback，无真实 Telegram、凭证、模型或私人会话。

## 2. 实现契约

- POST confirm 的 `interactiveConsent` 缺省 false；共享上下文勾选不授予交互权限。绑定 generation、candidate、目标或 revision 变化不继承授权；设备 token 不能修改它。
- 提示和回答必须匹配 Accepted Turn ID、完整 own-user 生命周期、准确 Thread/Turn/item、连续原生来源和原子捕获水位。每个 attempt 的请求身份与完整文件范围分别累计最多 32 项，不驱逐；请求十分钟过期，任务三十分钟过期。
- 命令仅允许普通单次动作与原生 availableDecisions 的交集；环境只允许缺省/null 或保留 `local`。额外权限、网络授权、远程环境及未知 schema 回本机。文件先有完整 started scope，最多八个路径操作，diff 立即丢弃；grantRoot 广授权不答。
- 问题当前支持 1–3 题、每题 2–4 个选项。先逐题持久收件和换发新 reference，集齐后只写一个完整答案。秘密、自由文本、isOther、非阻塞和定时问题回本机；等待问题或资格不确定时，文本持久拒绝并提示，不误排成下一任务。按钮过期及回答写入都不证明原生问题已解决，分流保护继续等准确本机 result/error、resolved 或终态。
- 提示发送 Unknown/Rejected 不激活 reference、不重试，显示静态本机核对状态。callback 核对 bot/user/chat/确认后的 message ID、generation、task、request、期限和动作；取走全部同提示 reference 后才 await fsync。本机原始 result/error、resolved、终态、撤销或缺口使旧按钮失效。
- 提交前即登记文本分流门，ACK 或完整来源证明未齐时拒绝入队，不外发交互。提交 Unknown，或未决/不确定交互退出时，按准确 generation/target 使旧绑定失效并清除该窗口队列和引用，保留不确定收件；重新继续需本机确认可用目标，不误撤后来替换的绑定。
- 控制连接等到 WebSocket 就绪后同步复验并调用 `start_send`，两者之间没有 await。接纳后的字节可能仍在缓冲区，撤销不能追回在途回答。结果区分 NotSent、WrittenUnconfirmed、Unknown；没有应用层采纳 ACK，不以 resolved 或 spinner ACK 证明远程决定生效。
- receipt 新写 v3，读取 v1/v2/v3 混合记录；callback 仅留 bot/update、query 的 BLAKE3 摘要、task/interaction UUID。与原任务和拒绝身份共享 4096 项及 4 MiB 上限，恢复只去重，不重放回答。旧读取器不兼容 v3，旧行不重写，旧数据库不迁移。

## 3. 原版 CLI 生产操作链

专项 `installed_cli_production_telegram_interaction_path` 的 artifact `3f705727-a8dd-4eef-a286-61a4c0fd1c4f` 为 success，1 passed / 0 failed，运行 3.87 秒。它实际经过 Application owner configure/pair/confirm、生产 polling/execute actor、原版 remote TUI 与同一已加载 Thread；没有手工替代交互发送循环。

两轮分别验证无副作用 `printf` 的 Allow once 与完整单文件 Add 的 Deny，文件没有产生；模型工具输出证明前者退出码 0、后者被拒绝。callback 收件摘要和原 task/message key 在 spinner ACK 及模型工具继续前已经存在并符合完整性摘要；原生 Turn 未结束前没有最终回复，结束后只有对应最终文本。还验证错误用户、旧提示 reference 和重复 callback 无法再次释放工具，并保留上一轮上下文与同一绑定目标。普通安装版命令提供的动作交集只有 Allow once，未伪造 Deny 或把 cancel 转成拒绝。

原 P1 `installed_cli_production_telegram_path` 本轮另行通过，1 passed / 0 failed，3.27 秒；省略交互字段明确返回 false，来源隔离、同 Thread 文本闭环和本机输入继续保留。两项都是原版 CLI 加合成 Bot/model，不是实际 Telegram 或人的同时按键验收。

## 4. 失败记录、修正与审查

所有失败 artifact 保留为 failure，不计作通过：

- `7eb963df-b0a3-419b-bb90-b70f64bef9bc`：编译成功，126 项通过、39 项因沙箱禁止临时 Unix/TCP 监听失败；允许本机合成监听后复验。
- `f937072e-b9ff-4e6d-8291-baa20d6b6200`：164 项通过，1 项提示进度文案断言失败；补齐仅在 UTF-16 限额内追加的进度文案。
- `ef9120d0-561c-4547-818c-a2d2da3a5efe`：166 项渠道、17 项控制及 P1 专项通过，P2 首轮回本机，未通过。
- `0d105523-1290-40af-9a5b-70e12a268907`、`7ffa47de-78c7-4163-a501-cdc576da07fc`：有界静态诊断确认实际请求为 command、保留 local 环境、无网络或额外权限；原 parser 将任何非 null 环境拒绝。临时 schema 诊断已经删除，不进入生产观察路径。
- `ab91ed0a-75dc-47f3-a95e-6c50af9bb0e0`：UI 追加结果文案后未及时重建，标准 build guard 拒绝旧 Web 资源；先重建 Web 再复验，没有绕过检查。
- `7c6591c4-744f-4d42-983a-d28d6c84fcbc`：fixture 错误要求原生命令一定提供两个动作；改为校验真实支持交集，生产动作限制没有放宽。
- `aea1c606-1e54-4ab8-8f3c-310cf99257d3`：80 项工具和 271 项前端测试通过，Rust 导出排序未满足 formatter；修正排序后重新运行标准检查，未绕过格式门。
- `a14d08c4-3b65-44bc-8bd6-65b6ca4cbfc7`：工具、前端、格式检查通过；Clippy 拒绝一个可简化布尔表达式，采用等价表达式后复验，不添加 lint 豁免。

`local` 修正有固定源码与负向测试依据：[EnvironmentManager](https://github.com/openai/codex/blob/2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4/codex-rs/exec-server/src/environment.rs#L287)只把保留 local 构造为本机环境，拒绝 provider 和动态注册冒用；[负向测试](https://github.com/openai/codex/blob/2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4/codex-rs/exec-server/src/environment.rs#L1440)验证 remote websocket 不能占用该 ID。普通审批沿用选定 environment ID；本项目新增 local 正例和其他环境负例，不依赖维护者源码路径运行。

独立 GPT-6 审查指出原先先校验再 await `send` 的就绪窗口，随后修正为先就绪、同步校验并 start_send。在途字节不能撤回的限制明确保留。最终审查另发现追问按钮过期后错误解除文本分流，现已将原生问题退休与远端回答授权期限分开；消费回答授权后也不提前解除分流。新增 session 过期/写入回归及经过真实 service 收件路径的过期文本拒绝、旧按钮无效、准确 resolved 后才可排新任务测试。审查结论不能代替测试。

审查继续发现 ACK 前和 Unknown 后的分流窗口：现将登记移至提交前，缺少 ACK/完整来源时保守拒绝文本；未知或带未决交互退出时仅撤销仍匹配的旧绑定。生产 execute_job 回归覆盖 ACK 前文字、ACK 前已观察问题、Unknown 后文字均持久拒绝、不入队、不发按钮，且原生 relay 继续转发。

## 5. 浏览器与剩余验收

系统 Chrome 在 1440 与 390 宽度检查实际 TelegramPanel 和工作台 CSS，使用合成 API 的临时 harness。artifact `e625ee17-6aaa-41d6-9fec-1563448441dc` 为 success：默认两项勾选 false、仅交互勾选不能激活、实际点击发送 true 与匹配 revision、活动启用/禁用文本正确；无 page error 或横向溢出。检查发现通用 settings label 把复选框排成列，已显式修正为 row。截图只含合成身份，临时 harness 和监听已清理。

随后五个交互结果代码和未知理由安全兜底的定向 UI 测试通过，TelegramPanel 36/36，typecheck 通过；前端已重新构建。该浏览器检查不代替原生生产启动或真实 Telegram。精确取消缺口见[接入契约 §5.4](../v3-telegram-channel-contract.md#54-已提交-turn-的取消缺口)，未通过阻断 TUI、伪造错误或批准 cancel 绕过。

## 6. 最终检查与本地交付

标准 `node scripts/dev.mjs check --offline` 的 artifact `a6b9b422-9e00-40c8-b707-3338146a74ce` 为 success，完成时间和租约按受管工具记录：

| 检查 | 结果 |
| --- | --- |
| 文档链接/结构 | 114 份，0 错误；文档最终更新后另行复验 |
| 维护工具 | 80 passed / 0 failed |
| 前端 | 271 passed / 0 failed，typecheck 和构建通过 |
| Rust all-targets | 647 passed / 0 failed / 61 ignored |
| formatter / Clippy | 通过；没有降低 warnings 门槛 |
| debug / release 两个 binary | 构建通过，Web 资源先重建 |
| Git 二进制策略 | 实际暂存区 36 项及待提交树 705 项，均 0 错误 |

全量检查之后只再增加“旧任务和错误 target 不得撤销替换绑定”的测试，生产源码未改变。最终 artifact `bc52e79b-9481-4159-86aa-a11455b42312` 为 success：fmt/Clippy 再次通过，service 19/19，通过真实 socket 的 P1、P2 原版 CLI 专项各 1/1，分别 3.74 与 3.79 秒。这两项显式 ignored 专项单列执行，不将标准检查中其余 61 项 ignored 当作通过，也不重复加总定向用例。

没有远端 push、Issue/PR、合并或发布，没有私人数据上传。旧数据库不迁移；独立 receipt 的新写版本为 v3，旧读取器不兼容，交互授权默认关闭。Tailscale 只读报告的具体监听仍待人类授权，不能将已有 P0 地址当作本片验收地址。
