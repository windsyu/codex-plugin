# 原生 CLI 工作台：支持范围与使用边界

本页汇总实际验证过的环境、产品限制及尚未实测的组合，范围记录截至 2026-10-16。当前实现契约见[本机架构](../architecture/workbench.md)、[本机交互](../interactions/workbench.md)和[中转架构](../architecture/relay.md)；构建通过不等于新平台、provider 或真实设备已经支持。历史逐次验证可从[清理前固定版本](https://github.com/windsyu/codex-plugin/tree/f686d4a3e1ba2847a2530fc78540bea773b54b19/docs/validation)追溯。

## 1. 运行环境与模型接入

| 项目 | 当前范围 |
| --- | --- |
| 本机工作台 | macOS 26.5.2 / Apple Silicon 已验证；Linux、Windows 尚未完成本机工作台运行验收。Windows 的 `.codex-web` 路径约定不等于平台已支持 |
| 公网中转 | 静态 `x86_64-unknown-linux-musl` 产物已在现有 Linux 服务验证，包含 TLS 门户、电脑出站连接与目录浏览；该结果不代表 Linux 本机 CLI/PTY 工作台已支持 |
| 官方 CLI | 0.154.0、0.155.1、0.156.1、0.159.2 有原生交互及历史相关验证；0.159.2 综合回归覆盖审批、追问、取消、工具、恢复、多项目隔离与退出。版本号不是启动白名单，其他版本提示后继续检查实际配置和路由；不自动升降级 |
| 静态凭据接入 | `unmanaged-custom`：原生 `custom` provider、Responses、显式 upstream、静态 bearer；支持 `requires_openai_auth=true/false`，保留 File API Key。现有 HTTPS/SSE 配置已有真实模型证据，独立 API Key 等新增组合另有合成 provider 验证；不自动改模型、权限或全局配置 |
| 命令认证接入 | 原生 `auth.command`：Responses、显式 upstream、`requires_openai_auth=false`；命令和 token 由本机 CLI 执行、缓存及刷新。工作台要求 File credential store，并快照缺失或 API-key 形状的 `auth.json`，以排除 cloud-managed 路由补值；不从中取得命令 token。与静态 bearer、env key/header、AWS、`requires_openai_auth=true` 互斥。CLI 0.162.1 与系统 Chrome 的合成 provider 回归已通过，真实 provider/公网发送仍待验，见 [ADR 0060](../decisions/0060-native-provider-command-auth.md) |
| 未纳入的模型路由 | managed/ChatGPT、其他 provider、环境认证、AWS 和真实 provider WS 尚未纳入支持。通用 WS 字节转发的合成验证不能替代真实接入验证 |
| 浏览器 | 系统 Chrome 153/154 系列已有验证，覆盖原生交互、历史、二维码和桌面/窄屏布局；Safari、Firefox 没有完整验收 |
| 手机 | 用户已确认单 Run 局域网接入和输入成功；多 Run 授权与窄屏布局有合成验证，真手机尚未复测。具体系统、锁屏恢复、双网络及移动网络组合未逐项确认，进一步体验优化暂缓 |

CLI 0.161.0 另有公开 no-daemon profile（`--no-daemon` / `[features].daemon_auto_start=false`）验证：跨展示项目整理后仍恢复原线程、原 cwd，工具与零自动重放通过。这不代替 0.159.2 综合回归，也不承诺 0.161 默认共享 daemon 模式或完整新版本兼容。

中转账号登录只保护网页和网关访问，不为模型服务登录。本机官方 CLI 按 provider 配置提供模型认证，代理透明转发 `Authorization`，认证头不进入观察、页面或 journal。工作台不读取动态命令 token，无法保证按精确值脱敏任意正文主动回显的 token；不要要求模型或工具输出凭据。

## 2. 项目、目录与日常操作

- `codex-view` 无参数打开零 CLI 的项目概览，全部历史在 `/history`；同配置/数据根复用应用。已有项目或输入的目录先做只读检查，再明确新建或恢复；同项目已运行时进入已有工作台。`--project .` 可直接从当前项目启动普通官方 CLI；右侧原生终端处理对话、Slash、初始化、审批与追问。
- 本机支持 macOS 原生文件夹窗口，并保留手动输入；远端完整 owner 可通过网页浏览电脑当前系统用户可读的目录。选择后填回规范路径并校验，不直接启动 CLI；浏览权限不等于项目启动资格，也不扩大 Run 文件访问范围。旧后端缺少目录能力字段时保留手动输入。
- 目录窗口使用完整临时 AppKit 应用包，中文侧栏、取消、选择和键盘操作已有实际验证。启动采用标准事件循环、异步显示后激活；首次冷启动的焦点延迟尚未稳定复现，不能保证全部系统状态下均已消除。
- 中央区分用户输入、模型正文、工具参数和执行结果；流式中间态可见，取消或缺失不会伪装为完整。调用详情和用量展示脱敏后的请求、工具定义、响应及覆盖范围，不承诺无限保留原始上下文。
- 刷新、切换文件或历史、打开设置不另启 CLI。每个 Run 同时只有一个终端输入者，重复页面可能需要点击“在此输入”；未确认输入不自动重发，不承诺无缝自动接管。
- 当前项目的文件、搜索和 Git 均只读；文件/Diff 支持连续滚动、全文查找、行号跳转、跨屏复制及长行横向阅读，只渲染视口附近内容，关闭后释放文档实例。
- 历史首页提供项目搜索、稳定路径摘要及主要/辅助/运行记录分层，未确认类型不隐藏；有明确身份的来源可折叠展示，正文与用量仍独立。完整 owner 可单条归入已有项目、设为未归类或恢复自动；展示归属不改变原 cwd 或恢复身份。
- 历史异步保存，支持占用统计、按运行/批量删除及可选保留期限；手动删除和自动清理默认关闭。活动运行、其他项目、旧 Observer 和原生 Codex 记录受保护。
- 设置位于独立 `/settings` 或 Run 设置页；配置为 `.codex-web/config/config.json`，默认历史为 `.codex-web/history`。生效范围以界面说明为准。
- macOS 已验证原生图片本地路径粘贴、手动提交及真实模型识图；图片剪贴板、拖入上传和手机相册不在已验收范围。

最多同时运行 4 个活动项目；另项目可开新标签，同项目复用 Run，恢复该项目另一会话前需先停止原运行。每个 Run 的终端、文件、用量与保存独立；最多保留 4 个已结束 Run 的内存阅读，退役后从历史读取已保存内容。具体契约见[历史首页与启动](../design/history-home.md)和[项目会话整理](../design/session-organization.md)。

## 3. 后台运行与远程访问

macOS 用户级 LaunchAgent 提供 `start/status/open/stop/autostart`，见[后台服务使用](usage.md#后台开启远程注册和退出)。服务启动不自动创建项目；已有有效登记按原接入开关恢复连接，不重复要求登录。首次交互启动或授权明确失效时引导打开本机注册页；非交互和系统自启不弹浏览器，注册未完成不影响本机服务启动。暂时断网按重连处理，不当作授权失效。

真实 LaunchAgent 的手动/重复启动、异常限频重启、前台冲突及完整停止已有验证。异常恢复只重启工作台，不恢复项目或重放输入；`stop` 完整结束所属项目、代理和连接，保留下次登录的自启偏好。真实注销/重新登录自启与睡眠/唤醒恢复仍待实测；用户服务不保证注销、睡眠或关机期间在线。

默认只监听 loopback。显式开启某个 Run 的“本地局域网接入”后增加设备监听；LAN IP 与 Tailscale MagicDNS 是同一服务的多个 URL，同 Run 共用固定配对码，应用重启后更换。码、许可和撤销按 Run 隔离，共享设置与全局历史仅完整 owner 可用。LAN HTTP 不加密，Tailscale 策略和设备可达性由用户环境决定。

公网中转使用独立账号流程，浏览器与电脑按 CLI Run 加密，Application 独立，后端只转发业务密文；中央前端及授权方仍可信，不防恶意运营者修改脚本或授权。当前 UI/API 和传输为 3、中转 schema 仍为 3，三端须配套升级并从门户重新打开，账号、登记和历史保留；无明文回退，仅支持单 origin，见[中转部署运维](public-relay-deployment.md)与 [ADR 0066](../decisions/0066-run-scoped-encrypted-relay-channels.md)。真实手机、TLS、延迟、峰值内存和规模负载仍须独立实测，不由单元测试或桌面视口推导通过。

真实公网目录浏览与本机已有历史正文阅读已完成；截至上述范围记录，真实远端已有历史正文尚未完成专门的浏览器验收。完整公网模型消息与恢复仍需独立 Chrome 验证：明确启动 CLI、发送一次低风险消息、确认输入一次和流式回复，并在刷新、新标签接管、短断网及中转重启后核对同一 Run/CLI 和零输入重放。三电脑完整公网业务、真实手机/移动网络与证书续签仍待实际验收。窄屏 Chrome 和合成 provider 结果不代替这些场景。

## 4. 阅读、保存与进程边界

| 边界 | 用户可见行为 |
| --- | --- |
| 单文件 / Git 输出 | 最大 1 MiB；超限明确提示。正文没有 200 行分页，也不按 4000 字符截断长行；该上限不等于整个浏览器进程的内存上限 |
| 项目搜索 | 最多 200 处命中、2048 文件 / 32 MiB 扫描和 5 秒预算，截断原因可见；需要已安装 `rg` |
| Git | 需要已安装 `git`；暂存/未暂存 Diff 是当前文件内容对比，模式/重命名看状态；不提供暂存、提交、回滚或 push |
| 文件访问 | 固定启动目录、安全打开，拒绝路径越界、链接和常见凭证文件；不会自动识别任意源码中嵌入的秘密 |
| 保存失败 / 慢页面 | CLI 转发和终端继续运行，网页提示保存异常或观察缺口。实时显示不等于已落盘，模型响应结束不等于整个任务完成 |
| 重启与恢复 | 只恢复可信保存前缀，未保存尾部标为不确定；不自动 resume，不重发输入。显式恢复重查原生文件、`SessionMeta.id`、原 cwd 和来源身份；非空 `history_base` 的继承历史只读不恢复，直接 `--resume` 同样拒绝 |
| 关闭浏览器 | 不等于停止 CLI；重新打开有效入口可继续使用 |
| 停止与退出 | 网页停止仅结束对应 Run。后台 `stop` 或前台启动器正常退出、SIGINT/SIGTERM/SIGHUP 回收全部所属 CLI、监听与入口；SIGKILL、断电或工具自行脱离进程组不在同步回收保证内 |
| 回到普通 CLI | 先结束对应工作台运行并确认 CLI 已退出，再从项目目录启动 `codex`，避免两个入口操作同一原生会话 |

全局历史的目录、正文缓存和搜索覆盖分别报告。每会话搜索摘录有限，即使正文缓存完整也可能只搜索部分内容。缓存不足时仍可按需读源正文；仅增大预算不会自动补回未变化会话的省略缓存，目前没有网页重建或自动补全操作。活动来源文件变化可能返回 `409 source_revision_changed`，需重新取得匹配来源修订，不能绕过检查读取旧缓存。

整理视图在筛选前核验全部授权候选，最多 100,000 条、metadata+locator 累计 64 MiB；超限 413 并提示切换“全部来源记录”，缩小筛选不能绕过全域核验。每组最多 10 个来源，默认 30 / 最多 200 组一页，响应小于 1 MiB。本机 10k 合成目录的 release 暖查询曾测得分组 p95 56.77ms、搜索 63.66ms；debug 分组 534.64ms 未达 200ms 目标。这些样本不代表冷扫描、浏览器端到端延迟或所有负载的性能保证。

单条归类覆盖最多 1,024 条 / 8 MiB 快照，位于有效 dataDir 的 `organization/state.sqlite`（schema 1）；catalog 投影 4 可重建，JSON 配置仍为 schema 2，原始 rollout、旧 Observer 和 Run 不迁移。归类库损坏、缺失、替换、版本或锁异常时明确不可用，不自动清空；独立正文和原 cwd 恢复可降级继续，新项目启动不绕过验证。首次保存失败可能保留私有 pending 诊断文件，不自动删除；旧 binary 不应用覆盖，回退不保证相同整理视图。物理断电、设备 fsync 故障及双 OS 进程 CAS 压力未作实际保证。

任务标题优先有效已有名称，否则使用真实用户请求；交流预览最多 240 个 Unicode 字符，仅代表当前来源已观察范围，Observer/Workbench 未读取尾部不保证最新。只识别已确认完整上下文包装，未知或畸形内容保守保留。正文默认突出对话，技术记录可展开、错误和缺口可见；首个对话最多自动定位 8 个窗口 / 128 条，超限保留继续阅读入口，搜索命中窗口自动展开。详情保留原始脱敏字段，不推断任务完成。

历史清理预览曾单次出现 `Stale`，后续回归未复现，根因尚未确认；后来测试通过不代表该诊断已经结案。

## 5. 构建产物与旧历史兼容

应用页为 `/`、`/history`、`/launch`、`/settings` 和仅本机提供的 `/relay`；Run 页为 `/runs/{runId}/{chat|terminal|files|search|git|history|usage|settings|access}`，单 origin 中转加 `/g/{gatewayId}` 前缀。旧 `/?run=<runId>` 兼容并规范化，同 Run 页面切换保留终端，不启动 CLI 或重放输入。

`1.0.0` 提供 macOS / Apple Silicon View 安装包，包含上述后台服务、历史整理与按 Run 加密的可选公网连接器，不附带独立 Relay 服务器。`code-view` 无参数明确启动当前目录，也支持指定目录；`code-view --history` 和 `codex-view` 保留无 CLI 入口。安装见[安装指南](installation.md)，正式资产以 [GitHub Release](https://github.com/windsyu/codex-plugin/releases/tag/v1.0.0) 为准；旧安装包不会自动获得加密通道能力，公网三端必须配套升级至 API/wire 3。发布版本号不扩大本页的受测范围，原生历史与旧 Observer 不迁移；派生 catalog、独立归类库及跨端版本限制仍按上文处理。

`codex-view` 统一读取原生、工作台及 Observer schema 20/25 的目录、搜索和正文。旧库使用独立零写 reader，不调用迁移；实际 schema 25 只做过限定正文抽查，不代表全库完整性验证，缺必要 WAL/SHM 或需修复时返回来源不可用。独立 `codex-observerd serve/import` 的迁移上限仍为 20，不能直接打开 schema 25，见[旧 Observer 兼容参考](../architecture/observer-compatibility.md)。

先构建 Web，再构建 Rust；debug/release 各自内嵌前端，服务器中央前端也内嵌于 relay binary。仅更新磁盘 `web/dist` 不会改变运行实例，试用新版需重启新二进制。两组件分别构建、部署和核验，见[组件指南](component-deployment.md)。

```sh
node scripts/dev.mjs build --release --offline
# 无参数打开项目概览，全部历史在 /history；直接启动项目添加 --project <directory>
/path/to/codex-plugin/target/release/codex-view
```

- `codex-view`：原生工作台，也是 `cargo run` 的默认入口。
- `codex-observerd`：V1 只读历史网页/API、导入/导出与显式维护；schema 1–20 migration 和既有 audit 保留。
- 旧 `/v2` 控制 API、Session/App Server 内核和 remote TUI 已移除；旧控制客户端不转接到新内核。
