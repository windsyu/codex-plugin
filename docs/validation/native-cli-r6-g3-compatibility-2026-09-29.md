# R6-G3：当前 CLI、历史兼容与设备边界验证

日期：2026-09-29。代码起点 `main@f6dabba`，本地工作分支 `codex/r6-g3-compatibility`。实施状态只在[实施计划](../v2-implementation-plan.md#r6-history-home)维护。本报告区分合成兼容回归、安装版 CLI 实验和真实旧库只读抽查，不替代整片用户试用。

## 1. 环境和隔离

- macOS 26.5.2（25F84）/ Apple Silicon；Rust 1.95.0、Node 22.23.2；安装版 CLI 0.156.1、系统 Chrome 154.0.8037.58。
- 使用 `node scripts/dev.mjs doctor/build/check/e2e`；Cargo 使用 `--locked --offline`。先构建前端，再构建内嵌页面的 Rust binary。
- 原生自动化使用临时 HOME、USERPROFILE、CODEX_HOME 和合成项目；模型仅为本机 HTTP/SSE fixture，不使用真实凭证或调用真实模型。Chrome 无头运行，未下载浏览器，未操作桌面鼠标或原生目录窗口。
- 实际旧库使用独立只读探针，不启动 Observer writer/importer、应用历史扫描或 CLI，不改用户来源配置。用户运行实例未替换。
- 日志和合成截图登记于 `target/artifacts/<UUID>`；成功诊断按工程流程保留 7 天，失败诊断保留 30 天。日志不是永久随仓库提交的 fixture。

## 2. 真实 schema 25 限定抽查

来源为本次现场 `observer.toml` 中已经登记的 `observer-data/observer.sqlite`。这是 2026-09-29 的现场证据，不是贡献者必须拥有的路径。

新探针 [legacy_readonly_probe.rs](../../tests/legacy_readonly_probe.rs)要求显式传入绝对来源路径与预期 schema，不扫描用户主目录、不自动选择数据库。通过生产 `LegacyReader` 和专用只读 VFS 读取；每次查询沿用 2 秒预算，每页最多 20 条，抽取首 20 条会话元数据及其中前 10 个会话的条目。所有页必须属于同一来源修订。

最终正文门槛只统计条目投影中明确的 message/text/content/output 等文本字段；ID、状态、非空元数据对象及仅有 summaryText 不能代替正文证据。对应合成反例测试通过。

| 集合 | 查询页数 | 返回记录 | 范围 |
| --- | --- | --- | --- |
| 会话 | 1 | 20 | 仍有后续页 |
| 项目 / 来源 / epoch / gap | 各 1 | 20 / 3 / 20 / 0 | 项目和 epoch 仍有后续页 |
| 上下文 / 关系 | 各 10 | 10 / 10 | 前 10 个会话 |
| turn | 10 | 42 | 1 页仍有后续 |
| item | 10 | 172 | 7 页仍有后续；121 条投影包含已识别文本，共 464,741 字节 |
| raw event | 10 | 185 | 8 页仍有后续 |

上述查询没有 page/record issue。schema 为 **25**，reader 标记为受验证结构。未读取附件；这不是全库完整性、搜索覆盖或真人正文语义审查。

读取前后分别流式计算数据库及 `-wal`、`-shm`、`-journal` 的完整 BLAKE3，并比较存在性、设备/inode、长度、权限/归属、mtime/ctime；读取访问时间不作为写入证据。即使查询断言失败，也先执行关闭 reader 后的第二次比较。现场存在 3 个文件，共 **2,101,368,400 字节**，前后全部相同，未创建 journal。报告仅输出计数、布尔值和 schema，不输出私人正文、ID、游标或文件 hash。

最终受管记录为 `792423b6-fce5-48d1-a352-d59f10590308`：真实抽查和正文证据反例测试共 2 项通过。较早记录 `011947c5-6fd8-4090-8596-1780d370ac9a` 的“非空投影”门槛偏弱，不能单独当作正文已读依据；最终记录替代该项结论。

## 3. 兼容和权限的具体断言

- schema 20/25 合成夹具对所有已支持集合进行分页、脱敏和未知值保留，比较源文件字节及写入元数据；另外覆盖 WAL、热 journal、缺辅助文件、锁、取消、来源替换、附件边界。实际 schema 25 抽查与这些合成故障实验分开。
- entry v1/v2 保留现有 URL/私有文件规则；补齐 v2 缺 instanceId、混入旧 runEpoch/cliPid、nil runId 和混合格式的拒绝断言。
- 配置 1 只加载不重写；显式保存升级至 2，previous 精确保留旧内容。补充 Observer 来源保存/重载一致性，来源目录不因保存而创建，删除策略仍关闭。
- shared device listener 对全局启动、目录选择、来源预览/刷新和设置 mutation 返回 404；跨 Run 的有效请求明确为 401/404，完整 WebSocket Upgrade 被拒为 401，撤销一 Run 不影响另一 Run。
- 新增同一个 Application 注册 Run、开启设备并实际配对的测试。先证明该 Cookie 能读取授权 Run，再将它发送至该 Application 的本机入口：10 条全局读写返回固定 401/403，Run 列表、CLI 调用记录与配置字节不变，不产生配置备份。
- V1 合成库上的查询/导出和所有 mutation 拒绝断言保留；真实 Observer binary 加载退役 controller 配置后不创建 CLI、不改控制账本，旧 `/v2` 路由 404。

## 4. 回归发现及修复依据

安装版 CLI 的标题生成请求实际包含 `request_kind=turn, thread_source=thread_title`，旧测试把所有非 system 请求当作对话，产品则把这个新来源归为 unknown。新增生产 decoder 回归先失败（期望 auxiliary，实际 unknown），再将这一明确元数据变体纳入辅助调用。仍兼容旧 system；用户请求即使带 title 输出 schema 也仍是 conversation，未知/冲突元数据不猜测用途。没有改变转发、原生认证或权限。

旧专项探针还有三类漂移：R6 后仍请求已退役的无 Run 路由；新版 CLI 的 `Trust this folder?` 与保留原模型提示未被处理；键盘测试未计入新增“全部历史”链接。修正为按页面实际 Run 构造接口和 WebSocket 拦截，并校验 HTTP 状态，原生模型提示明确选择保留已有模型。没有给产品增加旧路由别名，没有去掉输入/丢 ACK/同线程恢复等验收目标。

另补齐三处旧原生 PTY fixture 的 HOME/USERPROFILE 隔离。取消实验使用“原生已显示中间内容、观察副本已到达、上游仍未结束”的证据触发 Escape，继续要求上游被取消并能完成下一次输入；不再依赖已变化的终端快捷键提示文字。原生持久化与异步观察分别等待各自结果，不能用同一调度时刻的快照冒充同步完成。

本节记录调查依据，最终通过范围以完成后的命令结果为准。G1 独立索引线程 CPU、广泛正文吞吐和缓存释放后的补全边界，以及 G4 整片交付与用户试用不由本报告提前关闭。

## 5. 阶段提交时的验证范围

`a1dbc93` 按用户要求提交当时改动，以下保留提交时的运行结果和待复验项；后续收尾证据见 §6，不能将本节旧失败与最终复验混为一次运行。

| 检查 | 结果 | 受管记录 UUID |
| --- | --- | --- |
| 修改前标准 `check --offline` | Rust 439 通过 / 53 忽略、前端 234、维护工具 29；不能代替最终改动验证 | `b6c5100b-4be9-49e8-a0f0-8bbae204ed5a` |
| 提交前标准 `check --offline` | Rust 442 通过 / 54 忽略、前端 234、维护工具 29；fmt/Clippy/类型/前端构建及 debug/release binary 构建通过 | `22691447-e555-4fba-8f58-fbf5c5b3b602` |
| V1 系统 Chrome `e2e` | 10 通过 | `bca00bb9-8e4c-4046-880c-76fb2e24f2c9` |
| entry / 配置专项 | 3 / 1 通过 | `79b165fb-a462-4906-bf82-a8fdbdfb23c2` / `9c189f45-72a0-48ef-9c4b-dfc9514efeab` |
| shared listener / 同实例手机权限 | 各 1 通过 | `688ae4f9-8d1c-4322-96c4-a56163cf0897` / `fe2feaae-226c-44c7-ba42-964aa57e8136` |
| 请求用途 decoder | 6 通过；新增标题来源回归曾先失败 | `ca5b7981-f913-4f96-8f9a-2d223f451ac2` |
| 安装版 CLI 综合专项 | 20 通过 / 4 失败，见下述后续结果 | `789ada0c-277a-472a-9694-196e66b222bd` |
| 原生 FileChange 失败与取消专项复验 | 1 通过，修复测试助手假就绪 | `8aed7b29-ad60-4208-80e1-1bbf91062910` |

综合专项命令为 `cargo test --locked --offline --lib workbench::proxy::tests::native_cli:: -- --ignored --nocapture --test-threads=1`。通过项包含原生键盘/审批/追问/丢 ACK、两次用户输入与标题分离、配置优先级、取消后恢复、保存后重启、信号退出清理、历史首页新建/恢复、多项目独立输入与 QR 作用域、工具卡和 FileChange 等；模型请求均指向合成上游。

综合专项四项失败及提交时状态：

- `contexts_browser::native_compact_fork_and_resume_keep_context_out_of_new_user_bubbles`：初次 compact/fork 的浏览器断言通过，第二次 `--resume` 启动等待 Run 超时；原因尚未确认，下一步检查 Application 的 `launchError` 与可恢复历史契约。
- `launcher::product_launcher_preserves_native_profile_and_browser_flow_and_cleans_owned_run`：视口、草稿恢复及 terminalFaults 断言通过，随后截图因路径没有图片扩展名失败。已补 `.png`，细化失败阶段并使用实际第二页面诊断；最终修复尚未复跑。
- `patch_native::installed_cli_file_change_failure_and_cancel_have_distinct_native_evidence`：旧助手将背景欢迎界面误判为输入就绪，草稿实际遇到模型通知。助手现要求可见光标位于输入行，观察保留原模型被选中后再确认；上述专项复验通过。
- `rollout_tools::native_command_finishes_after_last_model_request_and_updates_the_original_card`：同样停在旧助手草稿验证；助手已修正，此项仍待复验。

终端 actor、零 CLI 首页复用/信号、三来源历史浏览和设备二维码专项四组尚未在本轮最终改动上执行。当前改动无数据库 migration、新配置字段或权限扩张，未修改用户运行实例、未推送或发布。

提交前另外运行文档校验、19 个变更 CJS 文件的 `node --check` 和 `git diff --check`，均通过。常规检查中的 54 项 ignored 保持显式专项，不能计作通过。

## 6. 综合回归收尾

后续工作以 `a1dbc93` 为代码起点，修改限于测试探针与文档，没有新生产行为变化。

### 已定位的兼容差异

1. `resume_history_unsupported` 是实际 Application 返回的拒绝，不是后台卡死。R6 设计 §12.2 明确拒绝非空 `history_base`；安装版 CLI fork 会话符合这个条件。测试助手现在立即检查固定错误码，不等到通用就绪超时。
2. 保留原生 compact/fork 的实时聊天断言，并增加三个阶段：compact/fork 后正常显示 3 次用户/模型内容；显式恢复 fork 被拒绝、零 Run/CLI 且配置不变，首页仍能搜索并读取该会话的回复、没有恢复按钮；再显式恢复原会话，确认摘要与 reply2 保留，fork 独有的 reply3 不进入原会话上下文。恢复前直接核对合成 rollout 首行中相应 ID、fork 关系与 `history_base` 有无。没有将继承历史改写成新 prompt，也没有放宽恢复资格。
3. 安装版 CLI 会在命名 profile 写入原生初始化结果 `tui.screen_reader_detection_done=true`。探针只接受这一精确布尔值及既有原生 trust；其他 profile、provider、项目配置和工作台 JSON 仍执行原有一致性断言。
4. 无扩展名的受管截图前缀导致早期探针截图失败，功能断言已先通过。相应探针统一生成 PNG 文件，保留已有 `.png` 后缀以兼容调试命令的完整输出路径，并区分截图阶段和真实视口/输入错误。

`history_base` 的含义还核对了固定研究基线中的[官方 SessionMeta/HistoryPosition 协议](https://github.com/openai/codex/blob/633ab199cfd724aa78013c006b27a2b3d049fc3b/codex-rs/protocol/src/protocol.rs#L2791)：它引用另一 rollout 的前缀。该源码说明与安装版实验分别记录；本轮没有验证通用继承链恢复，旧 R2 的 fork-resume 证据不扩展为当前 R6 支持承诺。

### 结果与证据

| 验证 | 结果 | 受管记录 UUID |
| --- | --- | --- |
| 安装版 CLI 综合矩阵 | 24 通过 / 0 失败；各浏览器阶段无页面错误 | `48a209b5-a580-4eda-aafb-70d609eaf111` |
| 继承历史拒绝及阅读加强复验 | 1 通过，包含 header 前提、历史正文及禁止恢复按钮 | `50a762a0-8ff4-4398-9702-822fc8738726` |
| 原生迟到命令独立复验 | 1 通过，仅 2 次请求，原工具卡回填 exit7；亦纳入上述综合矩阵 | `e85fba58-72ff-45c2-8d7e-4195329950bd` |
| 原生 terminal actor | 1 通过 | `6242a176-5ddd-4669-82a3-cb8490330a21` |
| 首页复用与退出信号 | 1 通过，4 个视口、零 WebSocket/页面错误 | `3abc290e-dee8-44d9-93be-3a045413f3ba` |
| 三来源历史阅读 | 1 通过，4 个视口、最多 48 个正文节点、零外部请求/页面错误 | `ab9017b8-6992-40af-97ac-aa6810ff0b0e` |
| 设备二维码、流式输入与撤销 | 1 通过；中文输入、刷新、离线、390/320 宽度和 Run 隔离；errors=0 | `26492422-637d-4313-9c6c-86f9ef6bd0c0` |
| 合成工作台浏览器矩阵 | 首轮 7 通过 / 1 截图失败；R0 截图修复后单项通过，8 项均有通过证据 | `b6045858-2d8c-44cc-becf-d14fd5c1ef3a` / `266de5a4-5a35-4837-a556-1bafc90d7202` |
| 最终标准 `check --offline` | Rust 442 通过 / 54 忽略，前端 234、维护工具 29；fmt/Clippy/类型/Web 构建及 debug/release 构建通过 | `dc2841c4-216b-4831-b169-cff7f9b4b88f` |

设备实验测得 enable→QR 581ms、重新展开→QR 968ms，属于当次合成环境，不是所有设备的性能承诺。以上专项复用系统 Chrome；真机、真实 provider、其他平台和 G1 尚缺资源指标没有在本轮补验。此次没有桌面鼠标自动化或原生目录窗口操作。

合成工作台矩阵覆盖流式中间态、安全转义、unknown、内容身份/快照缺口、逆序和重复工具结果、未归属 WS、按需上下文、保存故障恢复、清理/设置、用量、文件/搜索/Git 和窄屏。文件实验实际读取 30,000 行、渲染 39 行节点；密集样本 500,000 行仍为 39 节点，并验证完整选择复制、20,013 字符长行和关闭释放。它不能替代 G1 大历史索引资源测量。

独立 GPT-6 审查确认恢复实验与现有契约一致，并提出异常错误类型假阳性、真实 header 前提、拒绝后可读证据三项补充；均已修正，最终加强专项通过。仅审查意见不计作运行证据。

最终文档检查 99 个文件无错误，变更 CJS 语法和 diff 空白检查通过；`binaries --tree HEAD` 检查已提交的 648 个文件无禁止产物，新增收尾 diff 为文本。已查看继承历史拒绝后的阅读、三来源桌面阅读和手机 390px 截图。旧库真实数据未再读取，用户运行实例未替换；本次收尾修改留在工作树，未自动提交或推送。产物为 `target/debug/codex-view` 和 `target/release/codex-view`，完成后按用户要求暂停供阶段试用，完整 R6 仍按实施计划保留未完成项。
