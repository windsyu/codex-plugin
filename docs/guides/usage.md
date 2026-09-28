# 工作台使用指南

[文档总索引](../README.md) · [支持范围](../codex-native-cli-workbench-support.md)

构建和测试入口见[开发指南](../development/README.md)。本指南描述操作；阶段完成状态只在[V2 实施计划](../v2-implementation-plan.md)维护。

## 从项目目录启动 V2 工作台

目录助手现以完整临时 macOS 应用包运行，已从真实窗口确认中文按钮和侧栏，并验证取消、目录选择及自动清理。[实际窗口验证](../validation/native-cli-r6-e-picker-bundle-2026-09-23.md)纠正了此前只验证语言资源、未覆盖实际面板的缺口。

系统目录窗口的本地化由原生 AppKit 助手提供，随 macOS 语言选择资源；macOS 构建需 Apple SDK/Clang（构建脚本复用 `cc`），助手嵌入单个产物，用户运行时无需安装编译器。新建项目只确认项目路径，恢复时才显示官方 CLI 的会话来源；工作台配置、历史和元数据继续使用 `~/.codex-web`，与官方默认 `~/.codex` 分开。[修正与验证](../validation/native-cli-r6-e-picker-language-2026-09-22.md)。

macOS 首页的“打开其他目录”会弹出系统文件夹选择窗口；旁边“输入路径”保留小型对话框输入（支持 `~/`）。选中目录后先检查，再明确点击“开始新对话”；取消不会启动 CLI。原生窗口不可用时可手动输入；Windows 运行支持尚未接入。实现与验证边界见[目录入口改进](../validation/native-cli-r6-e-folder-picker-2026-09-22.md)。

当前入口支持 macOS 和已安装的官方 CLI，版本号不作为启动白名单。`0.154.0` 与 `0.155.1` 是当前受测基线；其他版本会提示尚未回归并继续启动，升级后无需降级或添加绕过参数。`0.155.1` 已用本机合成模型完成原生对话、审批/追问、重连、显式 resume 与历史恢复回归，范围见[版本兼容验证](../validation/native-cli-version-compatibility-2026-09-20.md)。工作台不自动升级、降级或修改 CLI。

模型接入仍使用已验证的 `unmanaged-custom` 路由适配器。原生配置须已有 `custom` provider、Responses、显式上游地址与静态 bearer；`requires_openai_auth` 可以为 `true` 或 `false`，原生 File API Key 配置可保持原样。其他 provider、ChatGPT/管理配置、环境或命令认证及真实 WS profile 尚未纳入支持。无法确认路由或认证时，本次 CLI 启动失败并显示具体原因，历史首页仍保留；不调整模型、权限或原生传输。版本检查与配置检查分别处理，边界见[详细设计 §2](../codex-native-cli-workbench-detailed-design.md#2-启动与-provider-配置)。

```bash
# 在本仓库构建；首次先运行 bootstrap。
node scripts/dev.mjs build

# 默认打开/复用首页，不启动 CLI；不要求已安装 Codex。
<repository>/target/debug/codex-view
# 直接进入当前项目；首页也可在核验目录后启动。
cd <project>
<repository>/target/debug/codex-view --project .
# 使用原生命名 profile（$CODEX_HOME/named.config.toml）
<repository>/target/debug/codex-view --project . --profile named --no-open
```

同一配置目录和数据根重复执行会验证私有实例入口并复用已有服务，不新增 CLI。`--profile`、`--provider-profile`、`--codex-bin` 单独使用只设置启动偏好；显式设置与已有实例冲突会报错。首页可按项目、来源和子代理分组筛选，并搜索会话或消息；点击会话即可阅读用户、模型和工具记录，逐条查看已保存的上下文或调用详情。正文连续滚动、详情按需加载，更新时提示重新读取，不拼接不同版本。设置仍从同页展开。选择项目或输入绝对路径（支持 `~/`）后先执行只读核验，再开始新对话；历史“继续此会话”只对可验证的原生记录开放。启动响应遗失时页面只查询同一操作，绝不自动再次启动。同项目新对话进入已有 Run，若要恢复另一会话须先停止它；其他项目可在新标签并行启动，最多 4 个活动项目，满额时先停止不再使用的工作台。首页逐项目停止需要明确确认，停止一个项目不影响其余项目。弹窗被阻止或刷新恢复时提供“打开工作台”链接，不再次启动。详见 [R6-E 启动契约](../codex-native-cli-workbench-history-home.md#r6-e-launch)；验证结果见[验证报告索引](../validation/README.md)。

明确启动项目时，启动器保留原 `CODEX_HOME` 和指定 cwd，模型接入仅对本次 CLI 覆盖模型代理地址。网页中的原生 CLI 继续负责主题、目录信任、发送和设置；这些原生操作可能按 CLI 自身规则保存偏好。默认打开本机浏览器；`--no-open` 时用 `codex-view open <entryFile>` 打开输出中指定的私有配对文件。普通输出只有本机地址、应用 ID、存在时的 Run/PID 和 CLI 版本，以及文件位置，不包含配对密钥。可用 `--codex-bin <path>` 指定已安装 CLI；`--resume <UUID>` 只接受明确的原生会话 ID，不自动提交。

网页终端保留原生 ANSI 颜色、加粗、斜体、代码与表格边框。为避免非交互启动环境把网页终端误判成无色日志，CLI 子进程使用 `TERM=xterm-256color` / `COLORTERM=truecolor` 并清除继承的 `NO_COLOR`；按 2026-09-19 用户要求传 `-c tui.animations=false`，关闭原生欢迎动画、微光与旋转提示。正式入口和 R1 调试入口一致，不添加动画识别/裁剪逻辑，不写全局配置；已有 CLI 进程需下次启动才使用这组显示选项。[验证记录](../validation/native-cli-terminal-display-2026-09-19.md)包含原因、范围和截图。

关闭网页不结束 CLI。在“用量与状态”中确认“停止当前运行”、原生退出或调用本 Run 的停止 API 会结束终端，保留本次内存阅读；在启动器按 Ctrl-C、发送 SIGTERM，或启动终端挂断并发送 SIGHUP 时，会清理本次 CLI、监听和私有入口。强制结束（SIGKILL）无法执行同样的退出清理，见 [R5 退出验证](../validation/native-cli-r5-process-cleanup-2026-09-21.md)。输入确认丢失时提醒会跨重连/刷新保留，检查原生终端后可手动清除，不自动重发。正式入口后台保存安全观察副本，重启后可从“历史记录”读取；未保存尾部可能丢失，异常退出和保存缺口会明确显示。配置/退出边界见[启动器验收](../validation/native-cli-r1-launcher-2026-09-19.md)，原生审批/追问、键盘操作、短断线和正式 `--resume` 的本机 Chrome 证据见[交互验收](../validation/native-cli-r1-native-interactions-2026-09-19.md)。

macOS 本地图片可在右侧原生终端中粘贴图片的完整路径，看到 CLI 的 `[Image #1]` 附件标记后，再输入问题并手动 Enter。当前 CLI 0.155.1 已通过真实模型识图验证；这不代表浏览器直接粘贴图片文件、拖入上传或手机相册已经支持。图片仍由 CLI 读取和发送，工作台不会自动提交。

手机使用方式：在电脑工作台顶栏点击 **“手机接入 → 开启设备访问”**，用手机扫描二维码或复制配对链接。面板同时列出局域网 IP 和可用的 Tailscale MagicDNS 地址，共用一个端口及同一 Run/CLI；切换二维码不会断开另一地址。局域网需在同一可互访网络，MagicDNS 需手机连接相应 Tailscale；无需配置 Serve。配对码在本次程序运行期间保持固定，可重复扫码，刷新电脑页面也不换码；Run 结束后设备授权失效，新 Run 生成新码。不同项目共用设备端口，但配对码和权限独立；A 的手机入口只能访问 A，不能访问其他项目、全局历史或共享设置。可逐个断开或关闭当前项目设备访问，其他项目与电脑工作台继续运行。

默认仅本机访问，每次启动需手动开启。可选 `access.port` 在 `.codex-web/config/config.json` 或“设置 → 高级启动设置”修改，0 自动选择，1024–65535 固定端口，下次开启接入生效。接入不自动修改防火墙，局域网 HTTP 不加密。手机复用现有终端，冲突时点击“在此输入”；手机相册上传仍未提供。浏览器自动化与真机待验项目见[接入验证](../validation/native-cli-device-access-2026-09-21.md)。

生成过程中按 Esc 后，若响应流结束但未收到模型终态，中央保留已收到的正文并标“本次响应不完整”，不再一直显示“正在接收”；这不代表工具一定取消成功。需要回到普通 CLI 时，先在启动器结束 `codex-view`、确认本次运行退出，再在项目目录运行 `codex`，不要让两个入口同时控制同一会话。[R5 阶段记录](../validation/native-cli-r5-desktop-2026-09-21.md)包含实际模型、图片、旧历史与隔离回退证据。

左下角显示本次运行的已知 Token、输入/输出和缓存摘要，点击后查看精确用量与推理明细。未上报显示 `—`，缓存/推理不重复加入总量，异常或冲突用量不会计入合计。统计包括当前启动后的辅助请求，切换历史不会把旧运行消耗加进来；保存异常继续可见，运行 ID、进程号和保存序号放入默认折叠的“诊断详情”。设计与范围见[用量概览](../codex-native-cli-workbench.md#15-用量概览与用户状态)，实际界面和检查结果见[试用改进验收](../validation/native-cli-usage-overview-2026-09-20.md)。

<a id="r3-历史与保存状态试用"></a>

## 历史保存、清理与配置

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
  runtime/<scope>/
    instance.lock         # 进程独占锁；保留同一个文件身份
    instance.json         # 私有应用发现与配对；正常退出移除
```

自 ADR 0049 更换默认值后，旧 `$CODEX_HOME/workbench/config` 和 `$CODEX_HOME/workbench-data-v1` 保留原位，不自动迁移或合并。新默认历史列表不包含旧位置的记录；仍需读取时，用 `--config-dir <原配置目录>` 和 `--data-dir <原历史目录>` 明确指定。已有 JSON 中的绝对数据路径继续有效，`storage.dataDir: null` 则使用新默认位置。原生 Codex 的配置、认证和会话保持原处。目录选择逻辑覆盖 Windows 约定，完整 Windows 运行仍受当前 Unix 终端/文件锁实现限制，尚未验收，见 [ADR 0049](../decisions/0049-workbench-user-directory.md)。

[R3.1](../v2-implementation-plan.md#r31-history-settings) 提供 JSON 配置、当前工作台内展开的设置面板、历史占用、按运行/批量清理和可选保留期限。配置默认保存在 `~/.codex-web/config/config.json`，可用 `--config-dir <directory>` 指定目录；缺文件时创建[安全默认值](../configuration/workbench.config.example.json)，已有文件不覆盖。点击顶栏“设置”展开表单，收起保留未保存草稿；表单读写同一文件，保存后仍留在当前阅读位置与终端，没有独立设置页面。显式启动参数覆盖文件值且不反写，启动项和历史目录修改在下次启动生效，不搬迁历史、不重启当前 CLI。

设置首先展示“历史清理”，用中文说明开关、天数和影响。默认手动删除和自动清理都关闭。历史页默认以阅读为主，每条已结束记录提供“删除”；点击“批量删除”后可勾选最多 100 条。删除确认框列出记录、大小和保留原因，点击“永久删除 N 条记录”后才执行。尚未开启时可从确认框进入现有清理设置，保存后再回来确认。当前操作直接显示进度与结果，列表下方“删除操作记录”可回查；支持停止剩余删除，仍占空间和失败会明确显示。运行中记录、原生 Codex 会话和旧 Observer 数据不在清理范围内。

自动清理另需开启“自动清理过期记录”（初值 90 天）。保存后检查已有到期记录，工作台存活时每小时再检查；只处理当前项目有可信结束凭证的正常结束记录，旧记录、异常退出及缺失凭证不自动清理。关闭策略或配置失效会暂停尚未开始的删除；已移入暂存的单次运行完成当前删除单位，服务退出后的恢复也重新校验策略。没有后台常驻清理进程。

占用按文件逻辑大小估算，按当前项目隔离，单列活动运行、待清理暂存和共享管理数据；部分统计不等于完整容量，实际磁盘释放可能不同。设置显示已有历史文件夹 `<当前数据目录>/runs`，展开“本次运行日志的位置”可查看对应运行目录；日志按段保存，没有单独的归档导出文件。Codex 程序位置、启动配置名称与更改历史保存位置收进“高级启动设置”；日常面板不展示配置文件路径。配置损坏时保留 CLI 和阅读，显示需修复文件的位置，修复后重载；启动时配置损坏仍可打开首页查看修复提示；不覆盖文件，创建 CLI 和清理保持禁用。同一配置目录供各项目共享，各工作台只管理自己的项目。验证范围见[R3.1 验收记录](../validation/native-cli-history-settings-2026-09-20.md)。

1. 从同一项目目录运行正式 `codex-view --project .`，在右侧正常完成一轮对话，查看底栏“已保存”与 Token 摘要。
2. 在启动器按 Ctrl-C 结束，再从同一目录启动，点击“历史记录”，选择上次运行。可阅读旧消息、工具结果和上下文；旧原生输入不会自动重发。
3. 进入项目工作台后，右侧终端始终属于当前运行；点击“返回实时阅读”恢复原面板和滚动位置。需要原生继续任务时，由用户明确操作 CLI 或传 `--resume <UUID>`。
4. 底栏显示简短保存状态；“保存异常”或“历史有缺失”时可展开查看说明，技术水位在“诊断详情”中。记录失败时终端照常可用；恢复后的内容从新分段保存，连续水位不跨过缺口。历史卡片显示“保存时”状态，服务异常退出时说明未保存尾部数量未知。

两次正式进程启动与保存故障的隔离验收命令、截图和边界见 [R3 验收记录](../validation/native-cli-r3-acceptance-2026-09-20.md)。测试使用临时 home、合成上游和本机 Chrome，不提交真实模型任务。本段步骤验证工作台 Recorder 历史。统一首页另通过只读目录登记原生会话和受支持的旧 Observer 来源，不迁移原始数据；当前范围见[支持说明](../codex-native-cli-workbench-support.md)。

## 文件、搜索与 Git 阅读

正式入口的左侧“文件 / 搜索代码 / Git”提供当前启动目录的只读阅读。展开文件树、点搜索命中可在中央定位到代码行；Git 区分已暂存与未暂存/未跟踪文件，另可查看影响当前目录的提交记录。左侧 Git 与中央对话可同时显示，关闭文件恢复原阅读位置，右侧 CLI 始终保持同一进程。工具卡中的本机文件路径可打开当前文件，不把历史路径变成历史文件快照。

搜索使用已安装的 `rg`，Git 使用已安装的 `git`；缺少某个程序时只显示相应阅读能力不可用。文件在中央连续滚动阅读，支持行号跳转、跨屏复制和 Ctrl/⌘ F 文件内查找，长行可横向滚动；文件与 Git Diff 都保持只读。页面只绘制可见区域附近的内容，单文件仍最大 1 MiB，超限会明确提示。项目搜索最多 200 处匹配并在 5 秒后停止；二进制、链接及常见凭证文件不提供正文预览。Git Diff 对比文件内容，模式/重命名信息看左侧状态，未解决冲突不合成单一 Diff。面板显示读取时间和截断，当前可见内容每 5 秒有界刷新，不承诺读取之后没有其他程序修改。详情与截图见 [R4 验收](../validation/native-cli-r4-acceptance-2026-09-20.md)。

## 读取旧 Observer 历史

`codex-view` 已提供统一后台目录和来源登记，schema 20/25 通过独立只读 reader 接入，三类历史阅读界面已在 R6-D 接入。旧 `codex-observerd import/serve` 的迁移上限仍为 20，会拒绝 schema 25；不要改库版本号绕过。下面的独立旧网页命令只适用于当前受支持的旧库。

默认配置：loopback `127.0.0.1:4765`、Codex source `~/.codex`、Observer 数据目录 `./observer-data`，不提供旧控制功能。可复制 [observer.example.toml](../../observer.example.toml) 调整；相对路径以配置文件位置为基准。本机配置、凭证和数据库不提交仓库。

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
