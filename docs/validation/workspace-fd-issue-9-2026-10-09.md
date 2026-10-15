# Issue #9：工作区假删除的复现、修复与验证

[验证索引](README.md) · [调试命令](../development/debugging.md#工作区-fd-诊断) · [ADR 0050](../decisions/0050-read-only-workspace.md)

## 结论与环境

2026-10-09，在独立 worktree、分支 `codex/issue-9-staged-diff`，以 `b0ae129` 为基线调查 [Issue #9](https://github.com/windsyu/codex-plugin/issues/9)。原始“revision 正确、staged patch 仅删除”已经复现，而非只在新版重复通过。最终 Git 实现避开 macOS `/dev/fd` 路径，使用请求自有私有快照；同条件回归已通过。最终标准检查与提交前检查另见下文。

实测 macOS 26.5.2 (25F84)、Apple Silicon、Darwin 25.5.0 / xnu-12377.121.10~1，Rust/Cargo 1.95.0、Node 22.23.2、Apple Git 2.50.1。doctor、离线 bootstrap 通过。测试使用合成临时目录，命令运行器显式隔离 HOME/USERPROFILE/CODEX_HOME，保留既有 Cargo/Rustup 缓存。未启动私人 CLI、模型调用或用户服务。

本次开始时 Issue 为 Open、无评论；[PR #11](https://github.com/windsyu/codex-plugin/pull/11)明确将其独立跟进。用户随后授权在验证修复后关闭 Issue，并创建本地 commit；没有授权 push、PR 或 merge。

## 可复现证据

### 原始异常与对照

原始 fixture 为 HEAD `old\n`、index `staged\n`、working `working\n`。失败时：

- staged patch 为 `@@ -1 +0,0 @@\n-old\n`，`truncated=false`；
- revision 为 `e98117b83ffb192e0fd32c6bb0ec83807296587a72d3957451c04272ec78ad8e`，匹配 BLAKE3(`old\n\0staged\n`)；
- 因而不能把正确 blob 读取与后续 Git 实际比较混为一谈。

新增矩阵完整复用原测试：FD 占用上限 0（无额外压力）、54、57、58、63、64、65、68、128、192，各运行 1/4 线程、每线程 2 次，共 20 个组合、100 次完整 fixture。保留原路径/扩展禁用/敏感文件断言，增加精确 hunk 和 revision 断言。

首次基线矩阵在 57:4 组合原样复现上述 staged 删除；另有 working/untracked 内容缺失及明确 Git 错误。随后用完全独立 Cargo target 编译未修改生产源码的 `b0ae129`，确认矩阵和新增 stat 干扰回归均失败，普通原始单测可通过。最终实现下两项回归均通过，workspace 共 19/19。

### 不依赖 Rust 的因果实验

独立合成探针不使用 Cargo 或项目进程封装，且不修改 Git index、测试内容或正在比较的文件偏移：

| 条件 | 结果 |
| --- | --- |
| 4 进程各对同编号 `/dev/fd/64` 执行 25,000 次 stat | 共 9,271 次 EBADF；原 FD 前后 fstat 正常、长度不变 |
| 改为不同编号 256–259，其余相同 | 100,000 次 stat 全部成功 |
| Git 比较 old/staged，同时另一进程只持续 stat 同号 `/dev/fd/65` | 首轮 40 次有 4 次原始假删除、2 次退出 128；另轮有 3 次原始假删除 |
| 干扰进程改为 stat `/dev/fd/66` | 40/40 正确 |
| Git 改用真正私有文件路径，保持同号 stat 干扰 | 40/40 正确 |

这证明同号伪路径并发 stat 本身足以触发所安装 Git 的原始症状，不需要假定 FD 被其它进程关闭、快照正文被改写或共享偏移错误。动态 FD 只避免同一父进程内活跃快照重号，不能保证不同工作台父进程之间不重号，因此最终 Git 修复完全绕开该路径。

[Apple XNU xnu-12377.1.9 的 fdesc 实现](https://github.com/apple-oss-distributions/xnu/blob/xnu-12377.1.9/bsd/miscfs/devfs/devfs_fdesc_support.c)按描述符编号和 mount 缓存伪 vnode，查找和属性读取会改变类型；[vn_stat_noauth](https://github.com/apple-oss-distributions/xnu/blob/xnu-12377.1.9/bsd/vfs/vfs_vnops.c)据其类型生成 stat，VNON 可返回 EBADF。[Git v2.50.1 diff.c](https://github.com/git/git/blob/v2.50.1/diff.c)中 binary 检查忽略内容加载错误；失败的 lstat 会缓存空内容，后续比较可把它当成空文件。此源码链与实验一致，但参考 XNU 不等同于运行内核的全部源码，未做内核指令追踪或完整 Apple Git 厂商差异审计。

### 另一个已证实的 FD 缺陷

原 `pre_exec` 固定 dup2 到 64/65 可覆盖 [Rust 1.95.0 的 exec 错误管道](https://github.com/rust-lang/rust/blob/1.95.0/library/std/src/sys/process/unix/unix.rs)。持有 FD 至 57/58，再 exec 不存在程序时，父进程误报启动成功、退出 1，错误信息写入 before/after 快照，长度变为 8。修复后所有 15 个布局都返回 `program_unavailable` 且快照不变。这与成功 Git 的假删除是两条独立证据，不把前者冒充后者根因。

macOS `/dev/fd` 副本共享偏移也已验证；EOF 探针返回 128/short-read，不是原始删除补丁。诊断不读正文、不 rewind，不靠重置偏移掩盖问题。

## 实际修改与安全取舍

- Git 对比读取后的自有快照：随机 `codex-workspace-diff-*` 目录创建时明确 0700，before/after 创建时 0600、create_new，拒绝覆盖/末端 symlink；不重新打开原项目正文路径。
- 子进程结束/取消/错误回收后，File 与 TempDir 按作用域清理。SIGKILL、崩溃或清理失败可能留下私有临时正文；不新增全局扫描删除，不声称命名快照具有匿名文件的崩溃清理保证。
- 所有快照写入、rewind 后提前持有 >=128 的 CLOEXEC FD；子进程仅清除其标记，不固定 dup2；父进程仍 CLOEXEC。rg 使用实际 FD 及归属映射，保留字面 query，不做字符串替换。
- Git 补丁仅从行首 `@@ -` 提取，避免 TMPDIR 含 `@@ ` 时泄漏临时路径。隔离测试故意使用这种 TMPDIR。
- `CODEX_WORKSPACE_DIAGNOSTICS=1` 默认关闭；本机 stderr JSONL 包含长度、偏移、FD 元数据、进程关联、启动 errno/退出码/信号、输出界限、stderr 字节数/静态类别。不记录正文、摘要、路径、argv、环境或原始 stderr，不进入 HTTP/历史。
- stderr 仅在诊断开启时保留最多 64 KiB 内存副本用于分类；等待 stdout/stderr 均 EOF。保留 1 MiB stdout、64 KiB stderr、取消、超时、进程组回收及全部原安全断言。

修改：[process.rs](../../src/workbench/workspace/process.rs)、[git.rs](../../src/workbench/workspace/git.rs)、[search.rs](../../src/workbench/workspace/search.rs)、[tests.rs](../../src/workbench/workspace/tests.rs)，以及[详细设计](../codex-native-cli-workbench-detailed-design.md)、[ADR 0050](../decisions/0050-read-only-workspace.md)、[调试指南](../development/debugging.md)、[验证索引](README.md)、本报告和 [CHANGELOG](../../CHANGELOG.md)。没有新增依赖、锁文件、migration 或对外 API 变化。

## 测试、诊断与限制

最终精确目标单测 1/1、workspace 19/19 通过，包含原始 fixture、100 次布局/线程矩阵、持续 stat 干扰下 20 次原始 fixture、64 次并发 staged/working 查询及 64 个独立读取、15 种 exec 失败布局、共享偏移/CLOEXEC、跨批次 rg 映射、0700/0600/拒绝覆盖/symlink/作用域清理、stderr 界限与脱敏负向测试。目标、workspace 和标准检查的可执行命令见[调试指南](../development/debugging.md#工作区-fd-诊断)。

最终完整 `node scripts/dev.mjs check --offline` 通过：工具 80/80、Web 234/234、Rust 454 passed / 0 failed / 56 ignored，fmt、Clippy（all-targets / -D warnings）、typecheck、前端构建、两个 binary 的 debug/release build 均通过。文档 107 项、实际暂存的 10 个文件二进制检查以及 `git diff --cached --check` 通过。56 项原有显式专项未执行，不计入通过。独立复审无剩余阻断项。

受管产物位于 `target/artifacts/<ID>`，成功任务保留 7 天、失败及独立构建保留 30 天，均不提交：

- 独立 stat/Git 探针源码、合成文件及计数：`88e93eb1-4c66-4de9-a946-764cb7482396/probe`。
- 初次原始 staged 删除复现：`b7942578-a43e-418e-aa30-ac148ec86abc`。
- 完全独立 target 基线：`1ca6af57-9ef6-4bfd-aa10-ff188825058f`；补齐 stat 干扰后 `c69559d9-b160-465e-b12d-7bd7c84c5ed4`（普通单测 2 通过，新增两项回归均失败）。
- 最终精确目标 1/1：`a5a7580f-5cf5-4be8-bed5-2ccba50d7108`；workspace 19/19（含特殊 TMPDIR）：`89aee10f-fde0-4cd4-a388-d71784224601`。
- 最终完整标准检查：`b9df31db-16de-4bd7-b01c-4d92fdf4e488`（外层隔离运行器 `8651c6c2-2030-4159-a54a-dd3c4f52a4c9`）。
- 新 debug/release 的两个 binary 在隔离 HOME 下各执行 `--version`，均退出 0、报告 0.3.0：`c2cf1328-ab3e-40c9-aa80-e87c88afd2d1`；这不是私人 CLI 或真实运行验收。
- 旧 exec 碰撞：`dadda5d3-9c5f-4917-82b2-b4b3d747761d`；修复后 `156e2870-51c4-4f58-aa5a-8f6c83e365a9`。
- 早期标准检查成功：`233bde40-f3bb-487b-a8e5-0dc03b2eb0c8`；沙箱限制导致本机监听失败的首轮为 `a3dfed11-fa09-41eb-b765-be7ba3b177b9`，未跳过，放行监听后完整重跑通过。

开发中失败也保留：初次共享 Cargo target 使切回主目录复用了基线测试二进制（9 项而非 19 项），`1e9444b0-f78d-4b52-b82f-e89c35e7cfd7` 和 `b82214c6-9ecc-4724-808b-ff8382093506` 不计作新版结果；已改为独立基线 target 并让主目录重新编译。权限回归抓到 tempfile 默认目录为 0755，修为创建时显式 0700（失败 `d9df462d-bf1b-4ffc-ad66-dabe4d439aa5`）。特殊 TMPDIR 回归先复现路径进入 patch，再修正行首提取（失败 `1ad22ca0-1015-4a7b-b5dc-e294ab9e38bf`）。更早测试参数/排序错误的日志也保留，不归为原始问题复现。

## 剩余不确定性与交付范围

修复的是应用对 macOS 伪路径的依赖，不是系统内核；不宣称其它 macOS、Linux/Windows 或所有未来 Git 版本已验证。rg 仍用匿名 FD，本次不将其跨独立进程伪路径行为算作新平台验收。真实 CLI、浏览器、模型和大数据等原有 ignored 专项未执行。元数据诊断不是 Git 每字节读取追踪。

没有 Git index/工作区文件实际删除证据；修复保持只读浏览契约。原始症状复现、独立干扰因果对照、去除危险路径及最终回归已满足用户本次授权的关闭条件，而非仅凭重复通过。2026-10-09 的交付为 `codex/issue-9-staged-diff` 上的本地修复 commit；当时未 push、未创建 PR、未 merge，也没有远端 CI 结果。Issue 实际状态与关闭说明以其链接为准；后续合并验证另记如下。

## 2026-10-15：合并前 CI 与历史索引热点

用户授权合并并删除原分支后，精确修复提交 `e48d731` 推送并创建 [PR #15](https://github.com/windsyu/codex-plugin/pull/15)。其 [PR 检查](https://github.com/windsyu/codex-plugin/actions/runs/38666265849)在两项历史库大正文用例的 8 秒就绪断言失败；[push 检查](https://github.com/windsyu/codex-plugin/actions/runs/38666227557)仅其中缓存预算用例失败。两轮所有 workspace/FD 回归通过。历史失败早于首项 workspace 测试约一分钟；独立审查未发现新增 FD 压力污染历史线程的路径。

未修改的 `main` / `b0ae129` 在当天相同 `macos-26-arm64` / `20260907.0351.1` 镜像上的[基线对照](https://github.com/windsyu/codex-plugin/actions/runs/38667589631)完整通过。故不能称基线稳定失败，也不能把失败简单归为 runner 故障。两项用例各含约 2.7 MB 正文；其 `indexing / 0` 状态可表示首篇仍在解析/脱敏，不能单凭该状态断言死锁。

定位到的确定性热点是 `TextRedactor` 只按首字节筛候选：连续 `x` 均误命中 `xox*` 的首字节，逐字进入完整状态机。先加入测试专用慢路径计数，原实现对 4096 个 `x` 产生 4096 步而失败（受管诊断 `4baa1a5c-8ca1-4946-8b3d-df26b344c8f0`），再改为首字节对应的第二字节低六位位图。位碰撞只能增加候选，单字节 secret 整行置位；通用前缀覆盖首两字节全部大小写，块尾候选仍交给原状态机。非 ASCII 候选停点仍为 UTF-8 首字节。pending、mask、tool 模式及全部脱敏规则不变，没有新增外部配置或数据格式。

新增常规回归验证慢路径步数为常数级（整体扫描仍为 O(n)），并逐次比较 push 输出、pending、mask、finish：大小写组合、位碰撞、单字节/重叠 secret、UTF-8、块尾以及长 lookalike 后的真实凭证。脱敏子集 9 通过 / 1 忽略，原历史库子集 27 通过 / 2 忽略。未缩小夹具、延长 8 秒期限、降低四线程并发或跳过原断言。

同一本机 debug 用例诊断（总用例耗时，非单独索引 CPU/正式性能认证）：

| 用例 | 原实现 | 新筛选 |
| --- | --- | --- |
| cache_body_and_response_budgets… | 4.65 s | 0.81 s |
| source_windows_read_past_cache…（含按需分页） | 10.07 s | 1.03 s |
| 历史库子集，四线程 | 13.35 s | 4.11 s |

原始诊断 `a8659bc3-1d15-47ac-a898-b118a173a162`，优化后 `b7af30eb-4ed1-490e-b81e-2301e51d8c35`；全部合成且隔离 HOME/USERPROFILE/CODEX_HOME，不接触私人历史。加速支持热点解释，但单轮基线/耗时差异不证明所有 CI 波动都只有同一原因。完整集成状态以当前 PR 的标准检查为准。

补充修复后完整 `node scripts/dev.mjs check --offline --ci` 通过：工具 80、Web 234、Rust 456 passed / 0 failed / 56 ignored，fmt、类型检查、Clippy all-targets `-D warnings`、两个 binary 的 debug/release 构建通过；受管任务 `83e25c8e-5e53-4db3-a18e-3f9e3dd4ee54`（隔离运行器 `33f0acab-2287-4e4b-98ff-63f458a79479`）。追加报告后文档 107 项复查通过，实际暂存四文件二进制检查通过；独立安全复审无新增阻断项。远端当前提交的检查仍须单独通过后才合并。

范围限制：优化只在没有 pending/mask 且非 tool 模式时启用；若前一块留下 `x`，后续连续 `x` 仍可能走原慢路径，不承诺所有切块都同样加速。差分验证保留既有 configured-secret 匹配优先级，不将等价性当作所有既有脱敏语义绝对安全的证明。原有 ignored 专项未因本次常规通过而追认为已执行。

### 第二轮 CI：录制器退出测试的观察竞争

性能修复提交 `54f8253` 的 [push 检查](https://github.com/windsyu/codex-plugin/actions/runs/38669648607)完整通过：Rust 456 项、历史原失败用例及 workspace/FD 回归均通过，分支历史二进制策略检查通过。同期 [PR 检查](https://github.com/windsyu/codex-plugin/actions/runs/38669654770)中历史和 FD 回归也通过，但录制器退出用例在预期 `unclean` 时读到了 `active`，因此没有合并。

原用例在设置 3 秒暂停后固定睡眠 150 ms，Drop 的 2 秒等待超时后再固定睡眠 1100 ms，即假定后台线程已经释放运行锁。线程调度和 fsync 没有这个完成保证；仍持有 flock 的运行应继续报告 `active`，不能为了测试通过提前宣称非活动。受控地保持线程暂停，可稳定复现完全相同的断言失败（`05062a4c-1351-48e9-90d0-accda96ad8ef`）；这验证了用例的观察竞争，不声称已追踪远端每次调度或 I/O。

修复只涉及 `cfg(test)`：一次性通道门闩确认工作线程进入暂停，Drop 后确认超时及仍活动，显式放行后在原测试辅助函数的 5 秒上限内等待线程完成并 join，再验证 `unclean`、`uncertainTail=true` 且不存在 `lifecycle.json`。通道发端断开也会放行，避免断言失败遗留永久暂停线程。保留 Drop 小于 2300 ms 的原断言和实际 2 秒退出预算；不改生产录制、运行锁、提交、历史状态或 CI 并发。

录制器测试族 14/14 通过（`a2feb6d4-d260-4733-96d8-cfdfe3b1d0f5`）。另做一次自动恢复的负向 mutation：临时将超时路径的退出许可撤销改为不撤销，新测试稳定以 `ended` / `unclean` 失败（`e0a65bb5-5e4e-4e67-9167-b6216abdcb73`），随后恢复生产代码；该失败不计作最终版本结果，证明同步测试未削弱原缺陷检测。恢复后连续 10 次精确回归通过（`873f50e8-092e-4cb8-93dd-24c9cd391aae`）；独立只读复审实际 diff 无新增合并阻断项。当前完整源码的分项检查结果如下，新提交的远端检查仍须通过后才合并。

独立复审保留的既有边界：本用例覆盖超时发生在最终提交开始前的迟到 worker；原 `journal::Writer::commit_when` 在检查退出许可后写 `meta.json`，若该写入跨过退出超时，存在落盘 `ended=true` 的潜在窗口。`lifecycle` 再检查许可不能替代 history 对 `meta.ended` 的读取；这是未做故障注入验证的原有生产一致性问题，不是本次 `active` 失败原因，也不是测试改动引入。下一项独立工作应在检查许可与 meta 原子替换之间注入阻塞，验证并设计超时后的结束语义，不在本次测试修复中悄悄更改持久化契约。

本轮 `node scripts/dev.mjs check --offline --ci` 的工具 80、Web 234、fmt、类型检查与 Clippy 通过，但 Rust 阶段一个未改动配置用例在第二次 `Prepared::load` 返回 `config_busy`，整条命令退出非零，不能记为通过（`56b1265e-c154-43bd-b6a9-d5a042ebd8f4`）。配置测试族随后 15/15 通过（`6d4e6cb8-adcd-4359-9723-1562285c0535`），当前源码的完整 Rust 四线程对照 456 passed / 0 failed / 56 ignored（`dc0e16ed-a88a-4a3e-89af-d703b5c43d67`）。复用源码未变的前置检查，并通过标准 `build --offline --release` / `build --offline` 补齐两个 binary 的 release/debug 构建（`37fac2e6-db80-4316-9a16-1f5a6aae4af4` / `830b7173-337d-4e29-8dc3-f17d39e59543`）。文档和实际暂存二进制策略在提交前另行复查。

对该配置失败，源码确认加载结束仅 close 锁 File，所有 flock 错误均映射 `config_busy`，当次未记录原始 errno。2026-10-15 独立合成管道握手探针证实：子进程保留 fork 继承的 CLOEXEC FD 时，父进程仅 close 后重新 open 加锁返回 errno 35 / EWOULDBLOCK；显式 LOCK_UN 再 close 可获取，子进程退出后两组均可获取。两组在父进程真实持锁时都拒绝新 writer。现场探针源码/结果保存在临时证据目录 `/private/tmp/codex-config-flock-probe.aEYrMn`，不是项目运行依赖。这只能证明旧锁引用延长机制成立，不能追溯认定当次失败就是该原因，也不是已验证的生产修复。没有发现并发覆盖配置的证据；独立审查接受保留失败与未确定因果、完整 Rust/剩余构建及新 head 远端检查通过后继续本 PR，不通过重试写入或修改配置互斥语义掩盖该问题。
