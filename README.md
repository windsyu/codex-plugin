# Codex Local Gateway

V2 是参照 cc-viewer 机理实现的本机原生 Codex 工作台：**当前目录启动普通官方 CLI + PTY，模型 HTTP/SSE/WS 代理取得实时内容，网页直接推送，后台异步记录。** 文字、Slash、原生模型/权限设置、审批和追问继续由原生终端处理。

当前工作树的默认入口是 `codex-view`，提供原生终端、用户/模型聊天、工具结果/拟议 Diff、请求提示词与用量、异步历史、文件/搜索/Git 和手机配对。旧 Session/App Server 控制内核已退役；`codex-observerd` 仅保留 V1 历史导入、只读网页/API 及显式维护命令，schema 仍为 20。历史中的角色、来源和保存缺口继续保留，切换历史不切换右侧当前 CLI。

[支持范围与使用边界](docs/codex-native-cli-workbench-support.md)汇总当前可试用环境、功能、资源限制和退出方式；[R5 收尾验收](docs/validation/native-cli-r5-acceptance-2026-09-22.md)串联本次回归及此前真实模型/图片/性能证据。完整阶段状态以 [V2 实施计划](docs/v2-implementation-plan.md)为准。

## 当前文档入口

| 文档 | 职责 |
| --- | --- |
| [V2 实施计划](docs/v2-implementation-plan.md) | **从这里开始开发**：R0–R5 唯一状态表、需求追踪、逐片任务/验收、迁移和退役 |
| [工作台方案](docs/codex-native-cli-workbench.md) | 用户体验、范围、架构选择与可靠性取舍 |
| [支持范围与使用边界](docs/codex-native-cli-workbench-support.md) | 当前运行环境、已交付操作、资源边界、构建产物与旧历史兼容 |
| [详细设计](docs/codex-native-cli-workbench-detailed-design.md) | provider/代理、PTY、实时事件、存储与恢复契约 |
| [历史管理与统一配置方案](docs/codex-native-cli-workbench-history-settings.md) | R3.1：统一 JSON、点击展开设置、占用、按运行清理和可选保留期限 |
| [当前核心约束](docs/codex-local-gateway-v2-development-constraints.md) / [ADR 0039](docs/decisions/0039-cc-viewer-style-runtime.md) | 新实现边界及放弃旧控制内核的决定 |
| [交互原型与视觉基准](docs/codex-native-cli-workbench.md#ui-prototype) / [浏览器原型](docs/prototypes/native-cli-workbench.html) | 深色工作台、面板与状态交互；全部使用合成数据 |
| [cc-viewer 功能及实现](docs/cc-viewer-function-and-implementation.md) / [实际试用](docs/cc-viewer-hands-on-2026-09-18.md) | 独立参考研究、源码与 T01–T13 证据 |
| [项目执行指南](AGENTS.md) | 工作方式、测试与数据保护 |

旧 Controller/Session Kernel 详细设计、分片计划、旧 ADR 和重复说明已移除；历史与删除清单见 [V2 历史索引](docs/archive/v2-before-cc-viewer.md)。[V1 历史](docs/archive/v1-development-history.md)继续保留。旧验证不是新路径验收，旧计划不再指导新实现。

实施顺序：先做当前必要 provider 的模型代理实验，再接终端工作台、工具/请求阅读、异步历史、历史管理与统一配置、文件/搜索/Git，最后完成实机验收和旧内核退役。每片交付条件与状态只在实施计划维护，不在各文档重复记录。

## 构建与测试现有代码

服务端使用 Rust，Web 使用 Vite + Preact。构建包含原生工作台 `codex-view` 与只读历史兼容程序 `codex-observerd`；无 `--bin` 的 `cargo run` 默认启动工作台。

```bash
cd web
npm install
npm test
PLAYWRIGHT_USE_SYSTEM_CHROME=1 npm run test:e2e
npm run build
cd ..
cargo fmt --check
cargo test --all-targets -- --test-threads=4
cargo clippy --all-targets -- -D warnings
cargo build --release
```

浏览器测试使用本机 Google Chrome，不额外下载 Chromium。自动化使用临时目录和合成 fixture，不写真实 `~/.codex`。前端静态资源在 debug/release 均嵌入 Rust 可执行文件，避免运行中的旧后端读取新页面造成版本混用；修改前端后必须先 Web build 再 Rust build，并启动新构建的程序。

## 从项目目录启动 V2 工作台

当前入口支持 macOS 和已安装的官方 CLI，版本号不作为启动白名单。`0.154.0` 与 `0.155.1` 是当前受测基线；其他版本会提示尚未回归并继续启动，升级后无需降级或添加绕过参数。`0.155.1` 已用本机合成模型完成原生对话、审批/追问、重连、显式 resume 与历史恢复回归，范围见[版本兼容验证](docs/validation/native-cli-version-compatibility-2026-09-20.md)。工作台不自动升级、降级或修改 CLI。

模型接入仍使用已验证的 `unmanaged-custom` 路由适配器。原生配置须已有 `custom` provider、Responses、显式上游地址与静态 bearer；其他 provider、登录/管理配置、环境或命令认证及真实 WS profile 尚未纳入支持。无法确认路由或认证的配置会说明原因并退出，不调整模型、权限或原生传输。版本检查与配置检查分别处理，边界见[详细设计 §2](docs/codex-native-cli-workbench-detailed-design.md#2-启动与-provider-配置)。

```bash
# 在本仓库构建；已有依赖可使用 --locked --offline。
npm run build --prefix web
cargo build --locked --offline --bin codex-view

# 在需要操作的项目目录，运行上面构建的二进制。
cd <project>
<repository>/target/debug/codex-view
# 使用原生命名 profile（$CODEX_HOME/named.config.toml）
<repository>/target/debug/codex-view --profile named --no-open
```

启动器保留原 `CODEX_HOME` 和 cwd，模型接入仅对本次 CLI 覆盖模型代理地址。网页中的原生 CLI 继续负责主题、目录信任、发送和设置；这些原生操作可能按 CLI 自身规则保存偏好。默认打开本机浏览器；`--no-open` 时用 `codex-view open <entryFile>` 打开输出中指定的私有配对文件。普通输出只有本机地址、Run/PID、CLI 版本和文件位置，不包含配对密钥。可用 `--codex-bin <path>` 指定已安装 CLI；`--resume <UUID>` 只接受明确的原生会话 ID，不自动提交。

网页终端保留原生 ANSI 颜色、加粗、斜体、代码与表格边框。为避免非交互启动环境把网页终端误判成无色日志，CLI 子进程使用 `TERM=xterm-256color` / `COLORTERM=truecolor` 并清除继承的 `NO_COLOR`；按 2026-09-19 用户要求传 `-c tui.animations=false`，关闭原生欢迎动画、微光与旋转提示。正式入口和 R1 调试入口一致，不添加动画识别/裁剪逻辑，不写全局配置；已有 CLI 进程需下次启动才使用这组显示选项。[验证记录](docs/validation/native-cli-terminal-display-2026-09-19.md)包含原因、范围和截图。

关闭网页不结束 CLI。在“用量与状态”中确认“停止当前运行”、原生退出或调用本 Run 的停止 API 会结束终端，保留本次内存阅读；在启动器按 Ctrl-C、发送 SIGTERM，或启动终端挂断并发送 SIGHUP 时，会清理本次 CLI、监听和私有入口。强制结束（SIGKILL）无法执行同样的退出清理，见 [R5 退出验证](docs/validation/native-cli-r5-process-cleanup-2026-09-21.md)。输入确认丢失时提醒会跨重连/刷新保留，检查原生终端后可手动清除，不自动重发。正式入口后台保存安全观察副本，重启后可从“历史记录”读取；未保存尾部可能丢失，异常退出和保存缺口会明确显示。配置/退出边界见[启动器验收](docs/validation/native-cli-r1-launcher-2026-09-19.md)，原生审批/追问、键盘操作、短断线和正式 `--resume` 的本机 Chrome 证据见[交互验收](docs/validation/native-cli-r1-native-interactions-2026-09-19.md)。

macOS 本地图片可在右侧原生终端中粘贴图片的完整路径，看到 CLI 的 `[Image #1]` 附件标记后，再输入问题并手动 Enter。当前 CLI 0.155.1 已通过真实模型识图验证；这不代表浏览器直接粘贴图片文件、拖入上传或手机相册已经支持。图片仍由 CLI 读取和发送，工作台不会自动提交。

手机使用方式：在电脑工作台顶栏点击 **“手机接入 → 开启设备访问”**，用手机扫描二维码或复制配对链接。面板同时列出局域网 IP 和可用的 Tailscale MagicDNS 地址，共用一个端口及同一 Run/CLI；切换二维码不会断开另一地址。局域网需在同一可互访网络，MagicDNS 需手机连接相应 Tailscale；无需配置 Serve。配对码在本次程序运行期间保持固定，可重复扫码，刷新电脑页面也不换码；程序退出后失效，下次启动生成新码。可逐个断开或关闭设备访问，电脑工作台继续运行。

默认仅本机访问，每次启动需手动开启。可选 `access.port` 在 `.codex-web/config/config.json` 或“设置 → 高级启动设置”修改，0 自动选择，1024–65535 固定端口，下次开启接入生效。接入不自动修改防火墙，局域网 HTTP 不加密。手机复用现有终端，冲突时点击“在此输入”；手机相册上传仍未提供。浏览器自动化与真机待验项目见[接入验证](docs/validation/native-cli-device-access-2026-09-21.md)。

生成过程中按 Esc 后，若响应流结束但未收到模型终态，中央保留已收到的正文并标“本次响应不完整”，不再一直显示“正在接收”；这不代表工具一定取消成功。需要回到普通 CLI 时，先在启动器结束 `codex-view`、确认本次运行退出，再在项目目录运行 `codex`，不要让两个入口同时控制同一会话。[R5 阶段记录](docs/validation/native-cli-r5-desktop-2026-09-21.md)包含实际模型、图片、旧历史与隔离回退证据。

左下角显示本次运行的已知 Token、输入/输出和缓存摘要，点击后查看精确用量与推理明细。未上报显示 `—`，缓存/推理不重复加入总量，异常或冲突用量不会计入合计。统计包括当前启动后的辅助请求，切换历史不会把旧运行消耗加进来；保存异常继续可见，运行 ID、进程号和保存序号放入默认折叠的“诊断详情”。设计与范围见[用量概览](docs/codex-native-cli-workbench.md#15-用量概览与用户状态)，实际界面和检查结果见[试用改进验收](docs/validation/native-cli-usage-overview-2026-09-20.md)。

## R3 历史与保存状态试用

正式入口默认保存到 `~/.codex-web/history`，新目录与原生会话及旧 Observer 数据库分开；也可使用 `codex-view --data-dir <private-directory>`，父目录须已存在。数据目录由当前用户独占，权限不符合要求时提示保存失败。后台保存的是脱敏阅读预览和上下文，不是完整终端录像，截断内容不会凭空补齐。占用统计、删除和可选保留策略已在下述 R3.1 接入。

默认根目录在 macOS/Linux 为 `$HOME/.codex-web`，Windows 路径约定为 `%USERPROFILE%\.codex-web`，内部结构一致。默认位置独立于原生 `CODEX_HOME`：

```text
.codex-web/
  config/
    config.json           # 工作台配置
    config.schema.json    # 参数说明与校验
    config.previous.json  # 保存前的备份（首次保存后生成）
  history/
    runs/<运行编号>/       # 对话历史、分段日志、快照与上下文
    index.sqlite          # 可重建的历史索引
    usage-v1/             # 占用缓存
    cleanup/              # 清理任务及暂存
```

本次更换默认值，旧 `$CODEX_HOME/workbench/config` 和 `$CODEX_HOME/workbench-data-v1` 保留原位，不自动迁移或合并。新默认历史列表不包含旧位置的记录；仍需读取时，用 `--config-dir <原配置目录>` 和 `--data-dir <原历史目录>` 明确指定。已有 JSON 中的绝对数据路径继续有效，`storage.dataDir: null` 则使用新默认位置。原生 Codex 的配置、认证和会话保持原处。目录选择逻辑覆盖 Windows 约定，完整 Windows 运行仍受当前 Unix 终端/文件锁实现限制，尚未验收，见 [ADR 0049](docs/decisions/0049-workbench-user-directory.md)。

[R3.1](docs/v2-implementation-plan.md#r31-history-settings) 提供 JSON 配置、当前工作台内展开的设置面板、历史占用、按运行/批量清理和可选保留期限。配置默认保存在 `~/.codex-web/config/config.json`，可用 `--config-dir <directory>` 指定目录；缺文件时创建[安全默认值](docs/configuration/workbench.config.example.json)，已有文件不覆盖。点击顶栏“设置”展开表单，收起保留未保存草稿；表单读写同一文件，保存后仍留在当前阅读位置与终端，没有独立设置页面。显式启动参数覆盖文件值且不反写，启动项和历史目录修改在下次启动生效，不搬迁历史、不重启当前 CLI。

设置首先展示“历史清理”，用中文说明开关、天数和影响。默认手动删除和自动清理都关闭。历史页默认以阅读为主，每条已结束记录提供“删除”；点击“批量删除”后可勾选最多 100 条。删除确认框列出记录、大小和保留原因，点击“永久删除 N 条记录”后才执行。尚未开启时可从确认框进入现有清理设置，保存后再回来确认。当前操作直接显示进度与结果，列表下方“删除操作记录”可回查；支持停止剩余删除，仍占空间和失败会明确显示。运行中记录、原生 Codex 会话和旧 Observer 数据不在清理范围内。

自动清理另需开启“自动清理过期记录”（初值 90 天）。保存后检查已有到期记录，工作台存活时每小时再检查；只处理当前项目有可信结束凭证的正常结束记录，旧记录、异常退出及缺失凭证不自动清理。关闭策略或配置失效会暂停尚未开始的删除；已移入暂存的单次运行完成当前删除单位，服务退出后的恢复也重新校验策略。没有后台常驻清理进程。

占用按文件逻辑大小估算，按当前项目隔离，单列活动运行、待清理暂存和共享管理数据；部分统计不等于完整容量，实际磁盘释放可能不同。设置显示已有历史文件夹 `<当前数据目录>/runs`，展开“本次运行日志的位置”可查看对应运行目录；日志按段保存，没有单独的归档导出文件。Codex 程序位置、启动配置名称与更改历史保存位置收进“高级启动设置”；日常面板不展示配置文件路径。配置损坏时保留 CLI 和阅读，显示需修复文件的位置，修复后重载；启动时配置无效则在创建 CLI 前停止并报告字段位置。同一配置目录供各项目共享，各工作台只管理自己的项目。验证范围见[R3.1 验收记录](docs/validation/native-cli-history-settings-2026-09-20.md)。

1. 从同一项目目录运行正式 `codex-view`，在右侧正常完成一轮对话，查看底栏“已保存”与 Token 摘要。
2. 在启动器按 Ctrl-C 结束，再从同一目录启动，点击“历史记录”，选择上次运行。可阅读旧消息、工具结果和上下文；旧原生输入不会自动重发。
3. 右侧终端始终属于当前新运行；点击“返回实时阅读”恢复原面板和滚动位置。需要原生继续任务时，由用户明确操作 CLI 或传 `--resume <UUID>`。
4. 底栏显示简短保存状态；“保存异常”或“历史有缺失”时可展开查看说明，技术水位在“诊断详情”中。记录失败时终端照常可用；恢复后的内容从新分段保存，连续水位不跨过缺口。历史卡片显示“保存时”状态，服务异常退出时说明未保存尾部数量未知。

两次正式进程启动与保存故障的隔离验收命令、截图和边界见 [R3 验收记录](docs/validation/native-cli-r3-acceptance-2026-09-20.md)。测试使用临时 home、合成上游和本机 Chrome，不提交真实模型任务。历史只包含启用 Recorder 后保存的工作台运行，不自动导入全部原生旧会话或旧 `/v1` 数据。

## 查看合成阶段调试

正式入口的左侧“文件 / 搜索代码 / Git”提供当前启动目录的只读阅读。展开文件树、点搜索命中可在中央定位到代码行；Git 区分已暂存与未暂存/未跟踪文件，另可查看影响当前目录的提交记录。左侧 Git 与中央对话可同时显示，关闭文件恢复原阅读位置，右侧 CLI 始终保持同一进程。工具卡中的本机文件路径可打开当前文件，不把历史路径变成历史文件快照。

搜索使用已安装的 `rg`，Git 使用已安装的 `git`；缺少某个程序时只显示相应阅读能力不可用。文件在中央连续滚动阅读，支持行号跳转、跨屏复制和 Ctrl/⌘ F 文件内查找，长行可横向滚动；文件与 Git Diff 都保持只读。页面只绘制可见区域附近的内容，单文件仍最大 1 MiB，超限会明确提示。项目搜索最多 200 处匹配并在 5 秒后停止；二进制、链接及常见凭证文件不提供正文预览。Git Diff 对比文件内容，模式/重命名信息看左侧状态，未解决冲突不合成单一 Diff。面板显示读取时间和截断，当前可见内容每 5 秒有界刷新，不承诺读取之后没有其他程序修改。详情与截图见 [R4 验收](docs/validation/native-cli-r4-acceptance-2026-09-20.md)。

当前阶段已接上三列布局、受认证 WebSocket、原生 xterm、右侧“用户”气泡与左侧“模型”流式卡片，并在对话中交错呈现独立工具卡。用户提交来自明确匹配的原生记录，模型名有来源依据；HTTP 辅助/未知请求可在“用量概览 → 查看调用记录”中选中并展开。工具参数生成与执行状态分开，命令输出、退出码与耗时仅在存在关联证据时显示；Code Mode 保留代码工具卡。正式入口现已接入项目文件树、代码搜索及 Git 只读阅读；磁盘历史也使用上面的正式入口。下面保留的 R1 演示入口未安装 Recorder，使用普通安装版 CLI、临时项目/`CODEX_HOME` 和本机合成模型，只演示文字聊天，不访问真实模型：

```bash
npm run build --prefix web
cargo run --locked --offline --example r1-debug -- --state-file /private/tmp/codex-r1-debug.json
```

打开状态文件中的配对 `url`，即可直接在右侧完成官方 CLI 的主题/信任初始化并输入，无需启用或释放输入权；此模式不自动提交任务。中央展示合成模型的真实分片流。终端开关、调用详情打开/关闭和同进程刷新可直接操作；只有另一页面正在使用终端时才显示“在此输入”，点击一次即可切换，原页面随后只读。输出始终可读。正式用户配置和旧服务不变。

使用已安装 Chrome 自动检查同一条路径：

```bash
cargo run --locked --offline --example r1-debug -- \
  --state-file /private/tmp/codex-r1-probe.json --probe \
  --screenshot /private/tmp/codex-r1-probe.png
```

探针在隔离 CLI 内提交两次相同的中文多行文本，核对两组用户/模型消息、草稿排除、正文中间态、模型身份、辅助/未知隔离、上滚/跟随、同进程刷新、双页接管、窄屏和原生退出。用户 reader 在探针中刻意延迟 4 秒，验证迟到气泡不抢滚动锚点或终端焦点；正常调试没有此延迟。测试不下载浏览器；可用 `WORKBENCH_TEST_CODEX` / `WORKBENCH_TEST_CHROME` 指定已安装的可执行文件。证据和剩余范围见[聊天记录](docs/validation/native-cli-r1-user-chat-2026-09-19.md)。

R0 最小正文阅读入口也保留：

```bash
cargo run --locked --offline --example r0-debug -- --state-file /private/tmp/codex-r0-debug.json
```

工具卡可通过[实际 CLI/Chrome 验证](docs/validation/native-cli-r2-tools-2026-09-19.md)查看截图或复现。此测试由合成模型驱动安装版 CLI，在临时项目中真实读取、修改文件及运行退出码 7 的命令，分别覆盖 Code Mode 与直接命令模式；不会向现有用户调试会话提交任务：

```bash
WORKBENCH_TEST_SCREENSHOT=/private/tmp/codex-r2-tools-20260919 \
  cargo test --locked --offline --lib \
  workbench::proxy::tests::native_cli::tools_browser -- --ignored --nocapture
```

原生命令迟到退出、真实 `write_stdin` 轮询及长输出的验证见[增量报告](docs/validation/native-cli-r2-native-results-2026-09-19.md)。可用同样的临时环境测试，命令为 `cargo test --locked --offline --lib workbench::proxy::tests::native_cli::rollout_tools -- --ignored --nocapture`；结果分别标明模型请求或原生记录来源，输出预览上限 64KiB，完整日志未保存在工作台。

独立“模型请求/网络”页面已取消：模型回复和工具卡旁的“调用详情”打开中央侧面板；左下角“用量概览 → 查看调用记录”提供模型、用途、逐次 Token、状态与观察时长列表。辅助和未归属正文只在对应详情折叠展示，避免复制聊天。历史记录提供自己的调用入口，底栏入口始终属于当前运行。详情只在打开、分页或手动刷新时读取。系统说明、历史输入、工具 schema/custom format 与响应各自有来源；历史上下文不会新增用户气泡。接收新响应后提示刷新，分页过期不会混入其他版本；用量按响应列出，不将缺失视为零，也不汇总成任务总量。实时详情缓存只属于当前运行，R3 正式入口另有异步保存；历史详情通过历史 API 阅读。超限、淘汰或未保存内容不能冒充完整历史。当前入口与截图见[调用阅读验收](docs/validation/native-cli-call-inspection-2026-09-20.md)，底层详情契约另见[原请求详情验证](docs/validation/native-cli-r2-request-details-2026-09-19.md)。可在构建后运行合成 Chrome 场景：

```bash
WORKBENCH_TEST_SCREENSHOT=/private/tmp/codex-r2-request-details \
  cargo test --locked --offline --lib \
  browser_reads_context_on_demand_without_losing_terminal_or_reading_state -- --ignored --nocapture
```

两个入口的 `--state-file` 都必须是尚不存在的本地文件；本次浏览器配对 `url` 只写入权限 0600 的该文件，不输出到通用日志。R0 自动提交一次合成任务，R1 等待终端原生输入；刷新均不会再次提交。Ctrl-C 结束持续调试并清理临时目录和配对文件，内存阅读记录不保存。验证失败时残留的状态文件不表示服务仍在运行。

[三列交互原型](docs/prototypes/native-cli-workbench.html)可独立打开对照，其中的终端、文件、工具与 Git 内容为合成演示，不代表当前运行时已经接入。

## 读取旧 Observer 历史

默认配置：loopback `127.0.0.1:4765`、Codex source `~/.codex`、Observer 数据目录 `./observer-data`，不提供旧控制功能。可复制 [observer.example.toml](observer.example.toml) 调整；相对路径以配置文件位置为基准。本机配置、凭证和数据库不提交仓库。

```bash
cargo run --bin codex-observerd -- doctor
cargo run --bin codex-observerd -- import
cargo run --bin codex-observerd -- serve
```

`doctor` 是严格只读诊断。`import`/`serve` 读取 Codex rollout 并写 Observer 自有数据，不修改 Codex store。Viewer 就绪后输出本次服务运行期间有效的配对 URL；再次获取同一链接可运行 `cargo run --bin codex-observerd -- open`。使用自定义配置启动时，`open` 也必须传相同 `--config`。

隔离 fixture 演示：

```bash
cargo run --bin codex-observerd -- --config fixtures/observer.fixture.toml import
cargo run --bin codex-observerd -- --config fixtures/observer.fixture.toml serve
```

演示端口为 4766，输出位于忽略目录 `target/fixture-observer-data`。

旧配置中的 `[controller]` 会给出退役提示并被忽略，包括 `enabled=true`、`session_kernel=tui/preview` 和旧 fixture 路径；不会启动 CLI、App Server 或 remote TUI。`doctor` 不再要求安装 Codex。旧 `app_server_socket` / `live_mode` 同样只接受为兼容输入，不连接任何服务。网页不再提供“新建对话”或“继续终端会话”；使用原生工作台时在项目目录运行 `codex-view`。

## 数据与 API 边界

- `/v1` 继续提供历史、项目、Thread/Turn/Item、搜索、健康和流查询；配对 POST 只兑换本机认证。旧数据和审计记录保留。
- 旧 `/v2` 路由全部移除并返回 404；新工作台继续使用独立 `/workbench/v1`，请求提示词/调用详情、终端、历史及设备接入不受影响。
- 新运行时不以旧租约、command ledger、CAS 或同步入库作为转发前提；记录故障允许原生 CLI 继续，必须如实显示缺口与未保存状态。
- 新工作台默认 loopback，设备访问须显式开启，共用配对不承接旧 Tailscale 控制模型；官方 CLI 不打补丁、不自动升级。
- 旧 schema 1–20 migration 与 audit 表保留；不再执行旧 command/worker/租约重启恢复或暂存图片清理，不迁移、不清理真实历史。服务切换和发布仍需另行安排。
