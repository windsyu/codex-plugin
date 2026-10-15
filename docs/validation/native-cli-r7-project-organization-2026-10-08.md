# R7 项目会话整理验证（2026-10-08）

[实施计划 R7](../v2-implementation-plan.md#r7-session-organization) · [整理契约](../codex-native-cli-workbench-session-organization.md)

## 1. 基线与范围

源码基线 `b0ae129` 加本地分支 `codex/session-project-clarity` 的未提交增量；macOS / Apple Silicon，Node 22.23.2、Rust 1.95.0。本报告逐项记录已执行证据，不从分片通过推断完整 R7 验收。所有历史均为临时合成来源，浏览器仅使用已安装系统 Chrome，没有读取或整理私人历史。

R7-A 保持完整路径项目身份，新增独立 `projectQ`、不依赖当前筛选/分页的 `selectedProject`、全匹配 `totalRecords`、未归类原因，以及可确认的扫描完成代次。没有新持久状态、原始来源 migration、恢复 cwd 或设备权限变化。

## 2. 回归与独立复核

先运行新增回归：后端两项因未知 projectQ、缺失 totalRecords/selectedProject 失败；前端四项复现导航耦合、标题取错/丢失、缺目录启动未禁和原因未解释，既有用例仍通过。

独立复核进一步发现摘要加载/失败期间新建按钮会借项目页路径重新启用，以及快速无变化扫描可能错过 indexing 后一直显示扫描中。新增三项失败回归后修正：启动必须当前摘要 available；刷新使用请求/完成代次而非采样中间状态。

发布阶段 SQLite revision 查询失败曾静默留下 indexing；真实 SQLite 缺表 prepare 故障回归先失败，再修复为 `cache_revision_failed`。完成成功仅接受来源已终结 ready/partial，队列为空不能冒充发布成功。

## 3. 已执行检查

| 检查 | 结果与范围 |
| --- | --- |
| `node scripts/dev.mjs doctor` | Node、Rust、Cargo、npm、Git、Apple SDK 通过 |
| `cargo test --locked --offline history::library:: --lib -- --test-threads=4` | 45 passed，10 ignored；含项目摘要、过滤、同名路径、目录消失、撤销、路径限定项目搜索、全量计数、扫描完成/失败与发布故障，以及现有三来源/正文/恢复/只读边界回归 |
| Web 全量 Vitest | 31 files / 247 passed；其中 HistoryLibraryPanel 33 项最后另行复跑通过 |
| TypeScript typecheck、Rust fmt/Clippy | 通过；Clippy 使用 all-targets、`-D warnings` |
| `node scripts/dev.mjs build --offline` | 先 Web 后两个 Rust debug binary 构建通过；未宣称本片 release 或全项目 check 已通过 |
| 系统 Chrome / 正式 debug binary | 合成三来源，临时 HOME/USERPROFILE/CODEX_HOME；项目不在导航页且 source/q 无匹配仍有正确摘要，独立 projectQ/后退、键盘路径展开、扫描完成、未归类原因、旧链接纠偏、分页、正文锚点、设置 Esc/焦点及 1440/1024/736/390/320px 通过 |
| Chrome 安全/隔离断言 | 0 WebSocket、0 page errors、0 external requests、0 Run/CLI 启动；源文件/旧库/Run 元数据前后不变，挂载正文卡片上限 48 |

浏览器首次在沙箱内启动工作台提前退出；允许本机服务与 Chrome 后，同一隔离测试通过。未将沙箱启动失败算作产品功能通过。

## 4. 产物与复现

截图由受管任务生成，位于 `target/artifacts/62fe916e-a182-477d-ad40-bacc683a44f2/`，包括桌面列表、320px 项目导航/未归类详情与长正文阅读；执行时检查桌面和窄屏截图，未见横向溢出或关键控件遮挡。它们是本次本机诊断产物，按保留策略可过期，不是源码分发依赖。

先执行标准 build，再运行：

```sh
cargo test --locked --offline --lib \
  binary_history_reading_chrome_all_sources_and_bounded_dom \
  -- --ignored --nocapture --test-threads=1
```

测试内部隔离用户目录，并使用本机合成来源。环境需要能监听 loopback 和启动系统 Chrome。截图可指定 `WORKBENCH_TEST_SCREENSHOT` 为本次受管任务目录中的前缀。

## 5. R7-B 分层与来源查询

类型使用封闭证据码，unknown 仍可见；Workbench Run 单列。分组先检查全部授权候选的身份/home/项目/类型/来源唯一性，再成员级筛选、统计和分页。全组摘要不读取正文；搜索命中只属于自己的来源，Observer 单独命中不会打开未命中的原生记录。catalog 投影升至 3，只重建派生目录与 checkpoint。

失败先行：分类字段缺失、默认主要区/惰性展开、Observer 首选和未认证路由；独立审查后另复现类型冲突仍合组、UUID 大小写全组漏员、裸组响应不兼容与 409 错映 503。各自修正后回归通过。首次 Chrome 数量断言误将旧库已确认代理当 unknown，按 fixture 的实际类别核对各区并保留至少一个明确子代理的要求，未放宽产品分类。

| 检查 | 实际结果 |
| --- | --- |
| History library 常规测试 | 53 passed / 10 ignored；新增分组 5 项、分类 9 项，包含过滤前唯一性、不同 home/同名不同项目/类型/身份冲突、仅 Observer 搜索命中、全匹配分区数量与过期游标 |
| Owner/API/手机专项 | 组查询、严格参数、全组摘要、来源撤销、Host/Origin、手机 cookie 禁用各项通过；错误映射 1 项通过 |
| Web 全量、类型检查 | 32 files / 259 passed，typecheck 通过 |
| Rust fmt/Clippy 与标准 debug build | all-targets `-D warnings`、Web→两个 binary 通过 |
| 系统 Chrome / 正式 debug binary | 三来源默认主要区、惰性辅助/运行展开与单区切换、section URL/后退、裸全组摘要及全部 A/R6 reader 窄屏/锚点/只读断言通过；0 WebSocket/page errors/external requests/CLI 调用 |

最终 B Chrome 产物为 `target/artifacts/90a902fe-7e7c-4e3e-8288-7e66d2941f25/`。本片不宣称归类持久化、全局 check、release、10k 性能或真实 CLI 已验证。

## 6. R7-C 单条覆盖与故障隔离

实现独立 organization schema 1 状态库、首次意图 FULL 事务与不覆盖发布、事务内 CAS、每次查询的新鲜共享快照，以及连接内有效归属 TEMP 视图。catalog 主库与所有原始来源保持只读；覆盖不能改变 recordedCwd、来源身份或 resume thread。强 If-Match、owner/Origin/instance/sourceRevision 校验、失败重读与恢复自动规则已接入。

独立审查先发现读连接不能恢复 hot journal、用户偏好故障阻断来源阅读、首次失败留下正式空库，以及重新序列化 DTO 丢失归属说明，修正并回归后再次独立审查未发现新的阻塞项。状态故障明确 overrideState=unavailable，独立正文和原 cwd 恢复可用，项目/分组/编辑及新项目启动保持严格失败。

| 检查 | 实际结果 |
| --- | --- |
| 状态库测试 | 12 passed；两 Store 首次 absent/同令牌并发、四个真实子进程提交前后退出、ACK 遗失/陈旧 CAS、生产 snapshot hot-journal 回滚恢复、真实 SQLite FULL、预算、mode/link/hardlink、损坏/缺失/替换/版本/锁负向 |
| 有效归属集成 | 5 passed；全部查询/正文 DTO/同 data peer/restart/catalog v2 重建/原 cwd launch 一致，坏首次目标/过期来源不创建状态，故障降级，撤销/dormant 重接；另分组覆盖拆组/旧摘要 409 通过 |
| Owner/API/手机 | 强引用 ETag、缺失 428、弱/重复/*拒绝、严格 body/字段/实例/来源/目标、跨 Origin 与手机 GET/PUT 禁用通过 |
| Web 全量与类型 | 最终增补 413 诊断后 33 files / 274 tests passed，typecheck 通过；含保存、确认、409/ACK 重读、不盲写、正文/滚动45/URL不变及故障提示；413 回归先失败，修正真实 error.code envelope 后通过 |
| Rust 与 debug 产物 | all-targets Clippy `-D warnings`、fmt、标准 Web→两个 binary build 通过；build 任务 `9bd3b489-66d4-466c-885f-1b8f0bac9ce6` |
| Chrome 功能 | 正式 debug binary、三来源与旧 reader；单条从未归类保存到项目并恢复自动，736/320弹窗、确认/保存滚动可达、正文/URL/关闭焦点不变；0 WebSocket/page error/external request/CLI startup；任务 `2b9ad94b-9cdf-400f-9310-b285f0a5526c` |

首次 private pending 失败文件保留诊断，不自动清理。上述 crash 是真实进程退出，不等于物理断电；没有实测设备 fsync 故障、未知 UID 或只读文件系统，不能将对应失败返回测试当成真实硬件保证。C 旧窄屏截图底部观察到重复绘制；补充明确状态、两帧与根/header/dialog 单例及 bounds 断言后，正式 Chrome 再次通过，亲看新320/736弹窗、1024列表已无重复（任务 `f7a0e7a5-0b77-4796-98fa-fad634990b36`），原异常图保留。证据不支持 DOM 重复，具体底层栅格根因未证明。

## 7. R7-D 万条记录性能

新增 `r7_10000_group_and_organization_warm_queries`：直接发布合成 catalog，8,000 native + 1,000 Observer + 1,000 Workbench，包含 1,000 native/Observer 明确组、1,000辅助记录与一次人工覆盖；不扫描私人来源，也不以旧 R6 用例替代新增查询。全域资格核验与全部计数先完成，DTO 只在排序/分页后构造。

每个分组/搜索场景预热 3 次、测量 25 次，响应小于 1 MiB，p95 硬门槛仍为 ≤200ms。与 R6 G1 相同 production release profile 的本机结果：

| 查询 | p50 | p95 | 最大值 |
| --- | --- | --- | --- |
| 分组 | 55.62ms | 56.77ms | 56.81ms |
| 搜索 needle | 62.78ms | 63.66ms | 63.80ms |
| 单条归属 | 未单列 | 0.585ms | 未单列 |

release 专项 1 passed，任务 `4c094f9f-51c9-4821-bcc7-00abf9471929`；没有引入缓存或放宽全域核验/阈值。此前同测试在 debug profile 分组 p95=534.64ms **失败**，任务 `fd0cbc61-9a0b-4c84-9e75-b37d4dab86a8`；debug 不作为 production 性能通过证据，搜索/归属因首项失败未执行。以上不是端到端浏览器延迟或后台初次扫描性能。

```sh
cargo test --release --locked --offline --lib \
  r7_10000_group_and_organization_warm_queries \
  -- --ignored --nocapture --test-threads=1
```

## 8. R7-D 集成与收尾

R7-D 两 Application 与双 Run 精准测试已 2 passed，并在最终标准 check 中再次通过：不同 configDir/同 dataDir 的首次并发保存一200一409，双方保存后立即读到同修订；真实 SQLite BEGIN IMMEDIATE 导致整理 PUT503，同时两个实际 PTY 消费第二输入，各自真实 per-Run HTTP/SSE 响应，CLI PID/复用与原rollout不变。这是同一测试进程内的两个独立 Application；Store 的既有修订并发、重启/跨 peer 查询与真实 crash 子进程另有 C 证据，不冒充双独立 OS 进程压力实测。

新增三来源零写入专测 1 passed（任务 `3e382bcb-f155-48c8-8a38-434a0ce4257c`）：schema20/25 外部子进程 writer保持真实非空 WAL/SHM，8条来源24次 project/unassigned/automatic 与按需读正文后，完整 native/旧库目录/Run目录的文件列表与全部字节不变，包含主库/audit、WAL/SHM、附件及非空 observations journal；各来源正文与recordedCwd不变。首次fixture未发布观察而journal为空导致测试失败，增加合成观察并等待实际保存水位后通过，未降低零写门槛。

安装版官方 CLI 0.161.0 专项通过两次；受管任务 `c4bff1ca-4af9-4e84-bc4c-094d4d9a82ca`，3.58s，outer 与隔离 helper各1 passed。子进程清空环境，仅保留执行所需白名单，HOME/USERPROFILE/CODEX_HOME 都是临时目录，模型为合成 loopback 上游。使用公开 `--no-daemon` seed 与 `[features].daemon_auto_start=false` profile（CLI 自身 help/features list核对）；真实 SessionMeta.source=cli，分类ordinary/native_source_cli_v1。归入不同的服务端已有项目后，idle native全部 regular files字节不变；Application 显式 resume同UUID/原canonicalPath，实际PTY提交后，官方CLI执行pwd的真实工具结果为原目录而非展示目标；共3个会话模型请求，无自动重放。不是原生私有锁清理或CLI补丁。

初期0.161默认daemon尝试出现source=vscode、原生会话占用与helper退出等待；真实socket不属于regular file也曾使快照fixture失败，均保留诊断。最终公开no-daemon profile通过，不宣称默认daemon已兼容。精确终止仅本次临时root的7个daemon与1个挂起helper，核对无残留，既有用户daemon未操作；没有修改用户CLI或全局配置。

最终标准 check 首轮工具80 passed、Web33/274 passed、typecheck/fmt通过；随后新CLI测试的 Clippy collapsible_if 导致失败，任务 `89da2db9-924a-48a0-ba2d-28d147651c59`，未算全量通过。修正后 `node scripts/dev.mjs check --offline` **完整通过**，任务 `e69fbde4-05c1-468e-8320-55e92aba96cf`：工具80passed、Web33files/274passed、typecheck、fmt、all-targets Clippy -D warnings、Rust all-targets 479passed/59ignored（其中lib377passed/54ignored），Web→两个debug/release binary均通过。ignored未算通过；本轮明确另跑的专项只有报告列出的Chrome、release新查询和安装版CLI。

文档109项零发现，git diff --check通过。标准binary检查暂存区为空（checked0），不能作为增量二进制审查；另用同一classifyContent对实际修改+未跟踪45文件核对，零禁止内容。人工核对没有新依赖、复制第三方实现、私人fixture、认证信息或生成产物；参考token-monitor只借鉴概念并固定链接。分支`codex/session-project-clarity`，未提交、未推送。

最终新debug binary Chrome再次1 passed（16.82s），任务 `6a98bcc6-409a-45f3-9a7e-d956150f9f61`；主要/辅助/运行、全关联摘要、归类/撤销、320/736/1440等宽度、正文锚点/焦点/URL、旧来源与原生文件零变、48卡片预算及0 WebSocket/page error/external request/CLI spawn均通过。截图DOM单例/bounds断言通过；早先异常图保留，新图没有重复布局。完整阶段状态在实施计划记录，不新增并行状态表。

## 9. 验证限制与交付边界

guardian 专属分类仍缺受测 rollout/公共协议证据；当前没有读取私有 SQLite 来补足。未声称物理断电/fsync设备故障、双OS进程CAS压力、0.161默认daemon、新provider/真手机/其他平台或所有历史内容的完整搜索覆盖；debug性能未达标事实保留。真实历史未变，未提交、推送、创建 Issue/PR、切换用户实例或发布。R7后下一步为用户试用，不自动清理或批量整理历史；阶段状态仅在实施计划维护。
