# cc-viewer 实际试用与实现对照

## 1. 范围和基线

日期：2026-09-18。根据用户要求，实际运行已安装的 `ccv`，在系统 Chrome 操作；同时核对网页介绍和本地源码。此记录是产品研究证据，不是 Codex Gateway 发布验收。

逐项功能、具体源码入口与技术路线见[cc-viewer 独立说明 F01–F12](cc-viewer-function-and-implementation.md)；本项目的范围、功能映射与验收统一见 [V2 实施计划](v2-implementation-plan.md)，架构见[方案](codex-native-cli-workbench.md)与[详细设计](codex-native-cli-workbench-detailed-design.md)。

| 对象 | 本次基线 |
| --- | --- |
| [介绍网页](https://weiesky.github.io/cc-viewer/) | 访问时显示 1.6.341；与安装版不同 |
| cc-viewer 源码 | `/Users/windsyu/magicproject/cc-viewer`，`5b544c6ee112bb14a80670480d3c4c900b17d307`，package 1.8.18 |
| 实际启动程序 | `/opt/homebrew/bin/ccv`，安装包 1.8.18 |
| 原生 Claude CLI | 界面显示 2.1.267 |
| Gateway 比较基线 | `main@930172493c163a903621ca14c540d1ca10dd4f3e`，schema 20 |
| 官方 Codex 研究源码 | `/Users/windsyu/magicproject/codex`，`633ab199cfd724aa78013c006b27a2b3d049fc3b` |
| 本机 Codex CLI | `codex --version` 返回 0.154.0；只检查版本，未做新的运行验收 |

使用隔离临时目录内的 Tiny Garden 合成 Git 项目：README、app.js、index.html，不含真实工作代码或用户会话。配置目录与日志目录独立；只复用用户已配置的模型连接环境，未拷贝全局插件、Hooks 或历史。实际调用的是已有模型服务配置，网页响应标识为 `gpt-5.6-sol`，原生模型选择器显示配置别名；不能将结果归为官方 Anthropic 模型兼容证据。

启动形式如下，省略临时目录随机值和全部凭证：

```text
cd <isolated-trial>/workspace
CLAUDE_CONFIG_DIR=<isolated-trial>/claude-config \
CCV_LOG_DIR=<isolated-trial>/logs \
CCV_HOST=127.0.0.1 CCV_START_PORT=7318 CCV_MAX_PORT=7328 \
CCV_NO_OPEN=1 DISABLE_AUTOUPDATER=1 ccv --no-open
```

浏览器访问本机 loopback。只在合成目录完成原生 trust 提示；未修改真实项目或上游源码。下列证据来自本次页面、终端与合成仓库的观察；不保存原始模型请求、认证头、私有配置或用户身份截图到仓库。

## 2. 实际操作结果

| 编号 | 实际操作 | 观察到的结果 | 结论边界 |
| --- | --- | --- | --- |
| T01 | 启动 ccv，打开页面并完成隔离 CLI 初始化 | 活动栏、文件/搜索区域、中央对话、右侧真实终端 | 验证本机已安装版本能启动 |
| T02 | 在原生终端要求只读 README.md 和 app.js，用三条中文说明 | 网页显示用户输入、两次 Read、三条完成回复 | 输入来自原生终端 |
| T03 | 在原生终端要求 totalPrice 遇到负数抛出 `Error("negative price")` | 真实 app.js 改动，网页显示前导说明、Edit Diff `+7/-1` 与结果 | 此次未观察到独立权限审批，不算审批流程验收 |
| T04 | 要求先输出 LIVE_BEGIN，再写 20 条改进想法，最后 LIVE_END | 中间态已出现第 1–9 条、第 10 条部分文字及流式光标，尚无 LIVE_END；随后完整 20 条和 LIVE_END 出现 | 证明增量呈现，未测毫秒延迟 |
| T05 | 文件树打开 app.js | 中央代码视图展示修改后的文件，终端仍在右侧 | 未使用网页编辑/保存 |
| T06 | 打开 Git 变更，再点 app.js | 显示 app.js、预置变更 index.html、合计 `+8/-2`；app.js Diff `+7/-1` | 只读，没有 commit/push/checkout |
| T07 | 搜索 `negative price` 并点结果 | 显示 1 个结果、1 个文件，标注 ripgrep；跳转 app.js | 只验证小项目搜索与跳转 |
| T08 | 打开网络面板与主请求上下文 | 请求列表显示 MainAgent/SubAgent、状态、时长、用量；上下文显示工具清单（24 项）与消息步骤 | 未采集认证头；非完整网络/缓存分析验收 |
| T09 | 返回对话页面 | 能继续阅读原对话并保留当时侧栏操作状态 | 面板切换不需要重新启动 CLI |
| T10 | 原生终端输入 `/model`，打开选择器后 Escape 取消 | 原生 picker 可用，界面提示保留原模型 | 不证明网页具有独立设置协议 |
| T11 | 要求 AskUserQuestion 选择名称/价格；最小化弹层，再选名称提交 | 网页出现可响应表单；原生终端等待；提交后终端确认问题已答，模型回复“名称” | cc-viewer 接管为网页表单，本项目不照搬此输入层 |
| T12 | 空闲时刷新浏览器 | 结构化消息、Diff、已回答问题和原终端画面恢复 | 未验证处理中断网、服务冷重启或重放幂等 |
| T13 | 打开 UltraPlan 配置入口 | 看到代码专家、调研专家、自定义专家与输入区域 | 只检查入口，未运行多专家任务 |

T04 捕捉的中间态要点：`LIVE_BEGIN` 已出现，第 10 条停在“为缺货植物显示明确状态，并暂时禁用”，末尾有流式光标，`LIVE_END` 尚未出现。随后观察到最终标记，排除了只在请求完成后一次性呈现全文的情况。网络面板的单请求耗时与终端整轮耗时范围不同，不据此推算流式延迟或效率收益。

T10 切换 picker 后一次紧接着的输入没有形成新请求；在确认终端空闲、粘贴文字已进入输入框后再 Enter，才执行 T11。试用不能将“发送了键盘动作”等同于“原生提交已发生”；这也是新方案以协议实际 Item 确认为准的原因。

## 3. 源码实现与可借鉴部分

以下文件相对 cc-viewer 源码根目录，定位对应上述 commit：

| 能力 | 源码证据 | 对 Codex 的意义 |
| --- | --- | --- |
| 原生 CLI + 模型代理 | `packages/app/server/pty-manager.js:277` 设置 `ANTHROPIC_BASE_URL`，`:335` 附近通过 CLI settings 确保代理配置生效 | 新方案采用普通 CLI + 同层模型代理，适配 Codex provider/认证 |
| 模型请求捕获 | `packages/app/server/interceptor.js:779` 包装 fetch | 解释网络/上下文来源，也是新 Codex 方案的主要捕获层 |
| 持久日志和派生视图 | `packages/app/server/lib/v2/layout.js`、`v2-writer.js`、`live-feed.js` | 借鉴流式临时展示与历史分工；新方案异步 journal/索引，不强制旧 SQLite/raw |
| 网页发送 | `apps/web/src/components/chat/ChatView.jsx` 中 PTY 写入与延时 Enter 路径 | 本次不采用；原生键盘由用户直接操作 |
| 文件内容搜索 | `packages/app/server/lib/code-search.js` 使用 ripgrep、结果限制及 fallback | 后续限定 cwd 的只读搜索 |
| UltraPlan | `apps/web/src/utils/ultraplanTemplates.js` 包含专家提示模板 | 不把提示模板视为性能保证或原生协议能力 |

介绍网页、源码和安装版之间存在版本差异；本记录优先以实际操作与对应源码说明实现，不将网站列出的所有功能标记为已试用。

## 4. Codex 协议与 main 差距

官方证据：[App Server 文档](https://developers.openai.com/zh-Hans/docs/app-server)；本地上述 Codex commit 中：

- `codex-rs/app-server/README.md:1769` 起描述 Turn、Item、delta 与最终快照。
- `codex-rs/app-server-protocol/schema/json/v2/AgentMessageDeltaNotification.json`、`ItemStartedNotification.json`、`ItemCompletedNotification.json`、`CommandExecutionOutputDeltaNotification.json` 固定身份与字段。
- `codex-rs/app-server-protocol/src/protocol/common.rs:1867` 起声明 Turn/Item 通知；`codex-rs/tui/src/lib.rs:438` 的 remote 初始化不排除这些通知。
- `codex-rs/tui/src/app/thread_event_buffer_tests.rs:62` 验证只合并相邻、身份匹配的 agent delta；`codex-rs/tui/src/app/tests.rs:928` 起有 snapshot/replay 渲染测试。

main 代码另有实时投影缺口：`src/session/proxy/event_sink.rs:850` 保存 raw，但 Item ID/type/status/text 为空；`src/store/mod.rs:2127` 的更新依赖 `item_type`。proxy 同步调用 Writer，后者等最多 50ms 凑批后提交；这说明旧链路有等待点，不是端到端时延测量。用户后续授权完全重构，因此不再以补齐旧 Item 投影作为实施主线。

这些源码和测试只是被读取，没有在本次运行官方 Codex 测试或 Gateway 联合验收。安装版 Codex 的接入/时延实测属于新方案 R0，完整设备验收属于 R5。

## 5. 清理、未测项与结论

试用后已停止本次 ccv 进程，7318 端口无监听；清理隔离配置（包括复制的连接凭证）与原始日志。原 `~/.claude/settings.json` 和 `~/.claude.json` 的 SHA-256 与启动前一致。保留的临时项目仅含合成代码；没有把原始请求或私人配置加入本仓库。

未测试：真实手机/软键盘、图片上传、独立权限审批、IM、服务重启恢复、处理中断网、复杂多代理/UltraPlan、Git 写操作、大仓库性能。未衡量 token 节省、速度提升或多设备可靠性。

在本记录之后，用户明确要求进一步靠近 cc-viewer 的实现机理并允许完全重构。当前方向为：**目录启动普通官方 CLI，以模型代理取得流式观察，网页直接推送，历史异步记录。** 本次操作证据未改变；新设计及其可靠性取舍按 ADR 0039，不等于已完成 Codex 验证。 完整架构、范围、契约与分片见[重写后的工作台方案](codex-native-cli-workbench.md)。
