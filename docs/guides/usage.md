# 工作台使用指南

[文档总索引](../README.md) · [支持范围](../codex-native-cli-workbench-support.md)

构建和测试入口见[开发指南](../development/README.md)。本指南描述操作；阶段完成状态只在[V2 实施计划](../v2-implementation-plan.md)维护。

<a id="从项目目录启动-v2-工作台"></a>

## 打开全部历史首页

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

无参数启动打开“全部历史”，不创建 CLI，也不检查模型路由；缺少 CLI 或某个历史来源不可用不妨碍首页就绪。同一配置目录和数据根重复执行会验证私有实例入口并复用已有服务，不新增 CLI。启动器保持前台运行，重新打开页面仍使用同一应用。

首页可按项目、来源和主会话/子代理筛选，并搜索会话或消息；点击项目只筛选，点击会话只阅读用户、模型和工具记录。正文连续滚动，上下文或调用详情按需加载；更新时提示重新读取，不拼接不同版本。项目按最近记录排序，同名目录保持分开；“未归属项目”保留没有可靠项目归属的记录，不从会话执行目录猜项目。

首次显示“正在整理历史”时，可以先读已经出现的记录；来源故障、目录覆盖与正文/搜索覆盖分别提示。“全部历史”表示浏览范围，不保证全部来源都已扫描或消息搜索覆盖全部正文。搜索无结果时仍可选会话按需阅读；缓存预算触顶不删除原始历史。设置从同页展开。

### 开始新对话或继续所选会话

1. 已有项目：在左侧选择项目，点击“开始新对话”，核对显示的真实目录后再点击启动。尚未收录的目录：点击“打开其他目录”选择已有文件夹，或用“输入路径”填写绝对路径（支持 `~/`），检查后明确启动。选目录和取消都不创建 CLI，不代建文件夹。
2. 继续任务：打开具体原生会话，点击“继续此会话”，核对工作目录和可展开的 Codex 数据目录后明确继续。只有来源、`SessionMeta.id`、原工作目录和文件身份均可验证的记录提供此入口；未归属会话若满足同样条件也可恢复。旧 Observer 副本、工作台记录和非空 `history_base` 的继承历史可读，但不从网页恢复；直接 `--resume` 同样拒绝未支持的继承历史。
3. 进入工作台后，在右侧普通 CLI 中完成目录信任、输入、审批和设置。开始新对话不自动续最近会话，继续所选会话不重放历史正文、不发送 Enter；仍须用户实际提交输入。

macOS 系统文件夹窗口支持 `⌘⇧G` 输入路径，按钮与侧栏随系统语言显示；窗口不可用时回退手动输入。运行时使用内嵌助手，无需安装编译器；实际窗口验证与首次响应边界见[目录选择记录](../validation/native-cli-r6-e-picker-responsiveness-2026-09-23.md)。Windows 完整运行尚未接入。

同目录已有活动工作台时，项目入口显示“进入工作台”，不创建第二个 CLI；若要恢复另一会话，先明确停止该项目的当前 CLI，再回到所选历史继续。启动结果暂未确认时使用“查询启动结果”；刷新或短暂断线只查询同一操作，不重新发起启动。应用重启后不会根据旧操作自动创建 CLI。

### 另一个项目、返回首页、停止和退出

从工作台顶栏点击“← 全部历史”返回首页，当前 CLI 继续运行。再选择另一个项目并明确启动，会打开新的工作台标签页，原项目保留；最多 4 个活动项目，满额时先停止不用的项目。各项目可以同时输入，文件、模型代理、用量和保存互相隔离；“在此输入”只切换同一 CLI 的重复页面。弹窗被阻止或刷新后可点击“打开工作台”链接，不再次启动。

首页顶部工作台卡片可以重新进入。关闭网页标签不停止 CLI；在首页对应项目点击“停止”并确认，或在该工作台“用量与状态”中确认“停止当前运行”，只结束这个项目，其余项目与首页继续。“停止中”表示请求已受理；“本次运行已结束”提示出现后可继续查看记录或返回首页，不代表所有内容都已保存。

已结束运行最多保留 4 项内存阅读；同项目成功开始下一次运行或超出保留数量后，旧工作台入口会失效，已保存记录仍可从首页读取。从首页再次启动不会复活旧 CLI。需要结束整个应用时，在原启动器终端按 Ctrl-C；SIGTERM/SIGHUP 同样回收全部自有 CLI、监听和私有入口，外部 Codex 不受影响。SIGKILL 无法执行正常清理，见[退出验证](../validation/native-cli-r5-process-cleanup-2026-09-21.md)。

上述行为的实现与分片证据见[启动契约](../codex-native-cli-workbench-history-home.md#r6-e-launch)、[多项目运行](../codex-native-cli-workbench-history-home.md#r6-f-multi-project)和[验证索引](../validation/README.md)；整片验收状态以实施计划为准。

### 命令参数

| 命令或参数 | 用途 |
| --- | --- |
| `codex-view` / `codex-view --no-open` | 打开或复用首页；后者仅输出地址和私有入口位置，不自动开浏览器 |
| `codex-view --project <directory>` | 明确启动该现有目录的新对话；同目录已有 Run 时复用；相对路径以命令 cwd 为基准 |
| `codex-view --resume <UUID>` | 验证并恢复启动 cwd 中的指定原生会话，不自动输入 |
| `codex-view --project <directory> --resume <UUID>` | 同时验证指定目录与原生会话身份后恢复 |
| `codex-view open <entryFile>` | 使用启动输出中的私有入口文件打开现有应用，接受新 v2 与旧 v1 格式；不创建 CLI |
| `--profile <name>` | 使用原生 `$CODEX_HOME/<name>.config.toml`，不是另一份工作台配置 |
| `--provider-profile unmanaged-custom` / `--codex-bin <path>` | 选择已验证接入适配器 / 已安装官方 CLI，不修改模型或安装程序 |
| `--config-dir <directory>` / `--data-dir <directory>` | 明确指定工作台 JSON 目录 / 私有历史根，目录说明见下文 |

`--profile`、`--provider-profile`、`--codex-bin` 单独出现只定义应用启动偏好，不隐式创建 CLI。显式设置与已有实例冲突会报错；修改启动偏好需退出原应用后重新启动。命令行覆盖文件值但不反写。

## 原生 CLI 与模型接入

当前运行支持 macOS 和已安装的官方 CLI，版本号不作为启动白名单。已有 `0.154.0`、`0.155.1`、`0.156.1` 与 `0.159.2` 的受测记录；`0.156.1` 的多项目、新建/恢复和兼容回归见[R6-F](../validation/native-cli-r6-f-multi-project-2026-09-23.md)与[R6-G3](../validation/native-cli-r6-g3-compatibility-2026-09-29.md)，`0.159.2` 的 24 项安装版 CLI + 合成上游综合回归见[G4 版本记录](../validation/native-cli-r6-g4-cli-01592-2026-09-30.md)。其他版本会提示尚未回归并继续检查配置，升级后无需降级或添加绕过参数。工作台不自动升级、降级或修改 CLI；版本回归不扩大 provider、Windows 或手机支持，当前支持边界以[支持范围](../codex-native-cli-workbench-support.md)为准。

模型接入仍使用已验证的 `unmanaged-custom` 路由适配器。原生配置须已有 `custom` provider、Responses、显式上游地址与静态 bearer；`requires_openai_auth` 可以为 `true` 或 `false`，原生 File API Key 配置可保持原样。其他 provider、ChatGPT/管理配置、环境或命令认证及真实 WS profile 尚未纳入支持。无法确认路由或认证时，本次 CLI 启动失败并显示具体原因，历史首页仍保留；不调整模型、权限或原生传输。版本检查与配置检查分别处理，边界见[详细设计 §2](../codex-native-cli-workbench-detailed-design.md#2-启动与-provider-配置)。

明确启动项目时，启动器保留原 `CODEX_HOME` 和指定 cwd，模型接入仅对本次 CLI 覆盖模型代理地址。网页中的原生 CLI 继续负责主题、目录信任、发送和设置；这些原生操作可能按 CLI 自身规则保存偏好。默认打开本机浏览器；`--no-open` 时用 `codex-view open <entryFile>` 打开输出中指定的私有配对文件。普通输出只有本机地址、应用 ID、存在时的 Run/PID 和 CLI 版本，以及文件位置，不包含配对密钥。可用 `--codex-bin <path>` 指定已安装 CLI；`--resume <UUID>` 只接受明确的原生会话 ID，不自动提交。

网页终端保留原生 ANSI 颜色、加粗、斜体、代码与表格边框。为避免非交互启动环境把网页终端误判成无色日志，CLI 子进程使用 `TERM=xterm-256color` / `COLORTERM=truecolor` 并清除继承的 `NO_COLOR`；按 2026-09-19 用户要求传 `-c tui.animations=false`，关闭原生欢迎动画、微光与旋转提示。正式入口和 R1 调试入口一致，不添加动画识别/裁剪逻辑，不写全局配置；已有 CLI 进程需下次启动才使用这组显示选项。[验证记录](../validation/native-cli-terminal-display-2026-09-19.md)包含原因、范围和截图。

输入确认丢失时提醒会跨重连/刷新保留，检查原生终端后可手动清除，不自动重发。正式入口后台保存安全观察副本，重启后可从首页或项目“历史记录”读取；未保存尾部可能丢失，异常退出和保存缺口会明确显示。原生审批/追问、键盘操作、短断线和明确 `--resume` 的早期本机 Chrome 证据见[交互验收](../validation/native-cli-r1-native-interactions-2026-09-19.md)。

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
    index.sqlite          # 项目工作台的可重建运行索引
    library/
      catalog.sqlite      # 全部历史的派生目录、脱敏搜索与读取进度
      writer.lock         # 目录单写者锁
    usage-v1/             # 占用缓存
    cleanup/              # 清理任务及暂存
  runtime/<scope>/
    instance.lock         # 进程独占锁；保留同一个文件身份
    instance.json         # 私有应用发现与配对；正常退出移除
```

自 ADR 0049 更换默认值后，旧 `$CODEX_HOME/workbench/config` 和 `$CODEX_HOME/workbench-data-v1` 保留原位，不自动迁移或合并。新默认历史列表不包含旧位置的记录；仍需读取时，用 `--config-dir <原配置目录>` 和 `--data-dir <原历史目录>` 明确指定。已有 JSON 中的绝对数据路径继续有效，`storage.dataDir: null` 则使用新默认位置。原生 Codex 的配置、认证和会话保持原处。目录选择逻辑覆盖 Windows 约定，完整 Windows 运行仍受当前 Unix 终端/文件锁实现限制，尚未验收，见 [ADR 0049](../decisions/0049-workbench-user-directory.md)。

[R3.1](../v2-implementation-plan.md#r31-history-settings) 提供 JSON 配置、同页展开的设置面板、历史占用、按运行/批量清理和可选保留期限。配置默认保存在 `~/.codex-web/config/config.json`，可用 `--config-dir <directory>` 指定目录；缺文件时创建[安全默认值](../configuration/workbench.config.example.json)，已有文件不覆盖。首页和项目工作台顶栏的“设置”读写同一共享文件，收起保留未保存草稿，保存后保持阅读位置与终端，没有独立设置页面。设置只供电脑所有者使用，手机入口不提供共享配置。显式启动参数覆盖文件值且不反写；程序位置、profile、浏览器偏好和数据根需退出并重新启动应用后生效，不搬迁历史、不重启当前 CLI。

设置首先展示“历史清理”，用中文说明开关、天数和影响。默认手动删除和自动清理都关闭；全部历史首页只读，删除在对应项目工作台的“历史记录”中进行。每条已结束记录提供“删除”；点击“批量删除”后可勾选最多 100 条。删除确认框列出记录、大小和保留原因，点击“永久删除 N 条记录”后才执行。尚未开启时可从确认框进入现有清理设置，保存后再回来确认。当前操作直接显示进度与结果，列表下方“删除操作记录”可回查；支持停止剩余删除，仍占空间和失败会明确显示。运行中记录、其他项目记录、原生 Codex 会话和旧 Observer 数据不在本项目清理范围内。

自动清理另需开启“自动清理过期记录”（初值 90 天）。保存后检查已有到期记录，工作台存活时每小时再检查；只处理当前项目有可信结束凭证的正常结束记录，旧记录、异常退出及缺失凭证不自动清理。关闭策略或配置失效会暂停尚未开始的删除；已移入暂存的单次运行完成当前删除单位，服务退出后的恢复也重新校验策略。没有后台常驻清理进程。

占用按文件逻辑大小估算，按当前项目隔离，单列活动运行、待清理暂存和共享管理数据；部分统计不等于完整容量，实际磁盘释放可能不同。设置显示已有历史文件夹 `<当前数据目录>/runs`，展开“本次运行日志的位置”可查看对应运行目录；日志按段保存，没有单独的归档导出文件。Codex 程序位置、启动配置名称与更改历史保存位置收进“高级启动设置”；日常面板不展示配置文件路径。配置损坏时保留 CLI 和阅读，显示需修复文件的位置，修复后重载；启动时配置损坏仍可打开首页查看修复提示；不覆盖文件，创建 CLI 和清理保持禁用。同一配置目录供各项目共享，各工作台只管理自己的项目。验证范围见[R3.1 验收记录](../validation/native-cli-history-settings-2026-09-20.md)。

1. 从同一项目目录运行正式 `codex-view --project .`，在右侧正常完成一轮对话，查看底栏“已保存”与 Token 摘要。
2. 在启动器按 Ctrl-C 结束，再运行无参数 `codex-view`；从首页选择保存的工作台记录，只阅读旧消息、工具结果和上下文，不启动 CLI 或重发原生输入。
3. 进入项目工作台后，右侧终端始终属于当前运行；点击“返回实时阅读”恢复原面板和滚动位置。需要原生继续任务时，由用户明确操作 CLI 或传 `--resume <UUID>`。
4. 底栏显示简短保存状态；“保存异常”或“历史有缺失”时可展开查看说明，技术水位在“诊断详情”中。记录失败时终端照常可用；恢复后的内容从新分段保存，连续水位不跨过缺口。历史卡片显示“保存时”状态，服务异常退出时说明未保存尾部数量未知。

两次正式进程启动与保存故障的隔离验收命令、截图和边界见 [R3 验收记录](../validation/native-cli-r3-acceptance-2026-09-20.md)。测试使用临时 home、合成上游和本机 Chrome，不提交真实模型任务。本段步骤验证工作台 Recorder 历史。统一首页另通过只读目录登记原生会话和受支持的旧 Observer 来源，不迁移原始数据；当前范围见[支持说明](../codex-native-cli-workbench-support.md)。

## 文件、搜索与 Git 阅读

正式入口的左侧“文件 / 搜索代码 / Git”提供当前启动目录的只读阅读。展开文件树、点搜索命中可在中央定位到代码行；Git 区分已暂存与未暂存/未跟踪文件，另可查看影响当前目录的提交记录。左侧 Git 与中央对话可同时显示，关闭文件恢复原阅读位置，右侧 CLI 始终保持同一进程。工具卡中的本机文件路径可打开当前文件，不把历史路径变成历史文件快照。

搜索使用已安装的 `rg`，Git 使用已安装的 `git`；缺少某个程序时只显示相应阅读能力不可用。文件在中央连续滚动阅读，支持行号跳转、跨屏复制和 Ctrl/⌘ F 文件内查找，长行可横向滚动；文件与 Git Diff 都保持只读。页面只绘制可见区域附近的内容，单文件仍最大 1 MiB，超限会明确提示。项目搜索最多 200 处匹配并在 5 秒后停止；二进制、链接及常见凭证文件不提供正文预览。Git Diff 对比文件内容，模式/重命名信息看左侧状态，未解决冲突不合成单一 Diff。面板显示读取时间和截断，当前可见内容每 5 秒有界刷新，不承诺读取之后没有其他程序修改。详情与截图见 [R4 验收](../validation/native-cli-r4-acceptance-2026-09-20.md)。

## 读取旧 Observer 历史

`codex-view` 已提供统一后台目录和来源登记，schema 20/25 通过独立只读 reader 接入，三类历史阅读界面已在 R6-D 接入。旧 `codex-observerd import/serve` 的迁移上限仍为 20，会拒绝 schema 25；不要改库版本号绕过。下面的独立旧网页命令只适用于当前受支持的旧库。

在首页“设置 → 全部历史的来源 → 接入旧历史”选择历史类型。可以直接填写数据库/目录的完整绝对路径并“加入待保存来源”；旧 Observer 也可展开“从旧版配置中读取位置”，填写 `observer.toml` 的完整路径，点击“读取位置”，核对数据库与附件位置，再“确认加入待保存来源”。最后点击设置面板的“保存”才登记；保存后的后台目录会在任何启动 cwd 发现该来源，无需另开 Observer 服务或重复配对。旧配置只提取历史路径，不继承登录、网络或控制设置。

默认自动接入当前 `CODEX_HOME` 的原生会话与当前工作台历史根；最多登记 8 个额外来源。移除来源并保存会撤销其目录/正文访问，但不删除原始历史。当前 JSON 为 schema 2；旧 schema 1 只读加载，用户保存时备份为 `config.previous.json` 后升级，原生 rollout、旧库及已有工作台记录不迁移。已省略的未变化正文不会仅因增加搜索缓存预算自动补回；需重建派生目录缓存，按需源阅读仍可用，边界见[目录缓存契约](../codex-native-cli-workbench-history-home.md#10-r6-c-目录与来源的实现契约)。

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

旧配置中的 `[controller]` 会给出退役提示并被忽略，包括 `enabled=true`、`session_kernel=tui/preview` 和旧 fixture 路径；不会启动 CLI、App Server 或 remote TUI。`doctor` 不再要求安装 Codex。旧 `app_server_socket` / `live_mode` 同样只接受为兼容输入，不连接任何服务。旧网页不再提供“新建对话”或“继续终端会话”；使用原生工作台时运行 `codex-view` 进入首页选择项目，或在项目目录用 `codex-view --project .` 明确启动。

## 数据与 API 边界

- `/v1` 继续提供历史、项目、Thread/Turn/Item、搜索、健康和流查询；配对 POST 只兑换本机认证。旧数据和审计记录保留。
- 旧 `/v2` 路由全部移除并返回 404；新工作台继续使用独立 `/workbench/v1`，请求提示词/调用详情、终端、历史及设备接入不受影响。
- Application 的目录/设置/启动接口属于全局范围；运行接口必须显式使用 `/workbench/v1/runs/{runId}/…`，停止为 `/workbench/v1/runs/{runId}/stop`。旧无 Run 前缀的运行路径返回 404，不默认选某个项目。
- 新运行时不以旧租约、command ledger、CAS 或同步入库作为转发前提；记录故障允许原生 CLI 继续，必须如实显示缺口与未保存状态。
- 新工作台默认 loopback，设备访问须显式开启，共用配对不承接旧 Tailscale 控制模型；官方 CLI 不打补丁、不自动升级。
- 旧 schema 1–20 migration 与 audit 表保留；不再执行旧 command/worker/租约重启恢复或暂存图片清理，不迁移、不清理真实历史。服务切换和发布仍需另行安排。
