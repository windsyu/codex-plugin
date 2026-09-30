# 开发与工程维护指南

[文档总索引](../README.md) · [贡献流程](../../CONTRIBUTING.md) · [执行规范](../../AGENTS.md)

## 环境和统一命令

开发/CI 基线固定为 Rust 1.95.0（含 rustfmt、Clippy）和 Node 22.23.2，不据此声明最低支持 Rust 版本。macOS 原生目录助手需要 Apple SDK/Clang。首次准备运行 `node scripts/dev.mjs doctor` 和 `node scripts/dev.mjs bootstrap`；依赖安装使用 `npm ci --prefix web`，Cargo 使用 `--locked`，不能为通过检查偷偷更新锁文件。

工作区搜索及其常规测试需要 PATH 中存在 ripgrep（`rg`）；macOS 可运行 `brew install ripgrep` 安装。CI 在检查前显式安装缺失的 ripgrep 并输出版本，避免依赖 runner 镜像的预装工具。

从仓库根目录执行：

| 命令（前缀均为 `node scripts/dev.mjs`） | 用途 |
| --- | --- |
| `doctor` | 检查工具与环境 |
| `bootstrap` | 按锁文件安装前端依赖 |
| `build` / `build --release` | 先构建 Web，再构建两个 Rust binary |
| `check` | 文档、待提交二进制、维护工具、前端和 Rust 常规质量检查 |
| `check --ci` | CI 检查提交树，不把空暂存区当成完成二进制检查 |
| `docs` | 本地文档链接、锚点及结构检查 |
| `test-tools` | 使用合成临时目录运行维护工具测试 |
| `e2e` | 用已安装的系统 Chrome 运行前端浏览器测试 |
| `binaries` | 按内容检查暂存区文件 |
| `binaries --tree HEAD` | 检查指定提交树 |
| `binaries --all-branches` | 检查本地及远端跟踪分支可达历史 |
| `artifacts list` | 盘点受管与未登记产物，列出跳过原因 |
| `artifacts prune` / `artifacts prune --apply` | 预览 / 执行受管过期清理 |
| `cache status` | 查看主缓存大小及清理提示 |
| `cache clean --profile dev` / `release` | 预览指定 Cargo profile 清理，附加 `--apply` 才执行 |

前端资源嵌入 Rust debug/release 产物。前端变更后必须重新构建 Rust 并启动新产物；不能让旧后端读取另一版页面。标准命令不下载浏览器、不自动启动私人 CLI 任务、不修改 GitHub 状态。

## 模块组织：现状与标准

2026-09-27 在 `df4e64c` 审计：单 Cargo package `codex-local-observer`，有 `src/lib.rs`、默认工作台 `src/bin/codex-view.rs` 和兼容入口 `src/main.rs`。`examples/` 提供合成调试，`tests/` 提供集成测试，`vendor/vt100` 是带修改说明的补丁依赖。该布局符合 [Cargo package 约定](https://doc.rust-lang.org/cargo/guide/project-layout.html)。[Workspace](https://doc.rust-lang.org/cargo/reference/workspaces.html)用于组织多个 package，并非 Rust 工程化的必选结构。

| 模块 | 当前责任与边界 |
| --- | --- |
| `workbench` | Application/Run 编排、模型代理、观察解码、PTY、实时展示、异步记录、配置及只读项目浏览 |
| `history` | 原生/工作台/旧库历史目录和只读 reader，不启动 Observer writer 或 migration |
| 旧 binary 的 `domain`、`store`、`http`、`ingest` 等 | V1 导入与只读兼容服务；不得重新成为新运行热路径依赖 |
| `web` | Preact/Vite 工作台与兼容页面，内嵌资源随 Rust 产物交付 |

当前需要改善的是依赖边界：`workbench::application` / `config` 使用 `history::library`，而 `history::library` 使用工作台配置、记录目录、读取与脱敏能力；两者形成模块级双向引用。`workbench/mod.rs` 广泛暴露 `pub mod`；`history` 通过 `#[path]` 复用 `domain/redact.rs`、`domain/classify.rs`，工作台复用 `terminal/mod.rs`。它们在当前 package 内可编译，但增加了未来拆分和接口收敛成本。

新增或重构代码遵循以下标准：

- 模块默认私有；先考虑私有或 `pub(crate)`，有明确外部消费者再公开。binary 负责参数、启动、退出与错误呈现，业务能力放入可测模块。
- 共享类型、纯函数和 I/O 抽象有明确归属，按调用方向组织；避免新增双向依赖或跨目录 `#[path]` 复用。现有共享代码移动前先确认消费者和测试，不复制实现解决依赖。
- 转发、观察、持久化分离；新运行时不依赖旧 Observer 写入、迁移或控制账本。文件大小仅作为审阅信号，按职责、变更耦合和测试边界拆分，不设机械行数门槛。
- workspace 只在独立复用、平台隔离或实测构建收益成立后考虑；先解决依赖环并写 ADR，不把机械拆 crate 当成质量提升。

本次只固化规范，以上模块收敛是后续独立重构，不宣称已经完成，也不改变 R6 验收状态。

## 构建产物生命周期

`target` 同时可能存在缓存和旧任务资料，不能整体视作可删除缓存。新受管产物置于 `target/artifacts/<任务 UUID>`，元数据记录仓库归属、类型、完成时间、结果及固定保留标记。只能清理明确登记、已经完成、校验通过的任务。

| 类别 | 默认策略 |
| --- | --- |
| 成功任务的日志、截图、合成输出 | 完成后 7 天过期 |
| 失败任务的诊断输出 | 完成后 30 天过期 |
| 一次性独立构建目录 | 完成后 30 天过期 |
| 主编译缓存 `target/debug` / `target/release` | 超过 20 GiB 或自首次登记/最后清理超过 30 天时提示；仅显式清理 |
| 活动、固定保留、未完成、元数据异常 | 跳过并说明原因 |
| 未登记旧目录、数据库、备份、Git bundle | 只盘点，不按年龄自动删除 |
| 已提交的必要图片和验证报告 | 不进入过期清理 |

`build` 和 `check` 每日最多执行一次受管过期清理，不安装定时任务。手动清理默认预览，显示候选、大小和保护原因，`--apply` 才执行。大小按文件逻辑长度求和，硬链接可能重复计数，不等同于磁盘占用或预计释放量。路径归属、真实路径、符号链接和活动状态必须先校验；管理命令使用互斥锁，不能确认安全即跳过。主缓存使用 Cargo 的 profile 清理，拒绝影响活动构建/运行和仓库外共享缓存，禁止无参数整体 `cargo clean`。不要手工伪造元数据把用户历史加入清理范围。

`cache status` 的 `ageDays: null` 表示尚无首次登记/清理时间，或记录无法读取，不代表缓存刚创建或已过期。首次受管任务登记建立计时基线；实际缓存可能更旧。时间未知时不触发 30 天条件，但仍可按已知大小提示。

异常退出可能遗留 `target/.artifact-maintenance/lock` 或仍有 lease 的任务。工具不会仅凭 PID 不存在或锁文件很旧就自动解锁/清除 lease。恢复前先核实记录归属、持有者进程及其构建/运行子进程均已停止，并保存锁、元数据和诊断证据；只对确认属于本次维护任务的遗留锁或任务元数据显式处理，保留无法确认的任务。不要将未登记目录、用户历史或其他任务改写为受管数据；不能确认完成时间或结果时继续保留，不伪造成功凭证。

旧产物首次整理只生成分类清单；`observer-data`、数据库、备份和 bundle 保留原位。复现专项测试时优先使用受管任务或独立临时目录，未登记输出由所有者明确处理。这里的构建产物清理与用户的[历史保留设置](../codex-native-cli-workbench-history-settings.md)相互独立。

主缓存清理采用保守的 Cargo 布局检查，并检查数据库/备份内容、进程和打开的文件。未知 build-script 输出（包括部分正常生成的 `.a`、`.rs`、`.o`）也可能被拒绝，需由所有者人工分类；不要删除保护检查或直接改用整体 `cargo clean` 绕过。该命令不承诺自动识别所有 Cargo 版本和依赖生成的文件。

清理前同时校验 Cargo metadata 返回的 `target_directory` 和 `build_directory`，要求均为本仓库的 `target`，字段缺失或路径重定向时拒绝执行；传给 Cargo 的清理命令也固定这两个目录。固定工具链生成的普通 `.rcgu.o` 和 incremental 对象纳入支持，并以真实最小 Cargo 项目的预览零修改及显式清理测试验证；这不把任意未知输出视为可删除文件。

## 测试与交付

常规检查包含前端 TypeScript、单元测试和构建，以及 Rust fmt、Clippy、常规测试和两个 binary 构建。macOS CI 调用同一入口，不能跳过失败来制造通过。检查全部分支历史前先只读核实远端 refs 是否与本地远端跟踪 refs 一致；离线或未获取的分支必须在结论中明确说明。

真实官方 CLI、真实模型、系统 Chrome、大规模数据和 ignored 测试是显式专项，不包含在“常规测试通过”结论中。[专项调试](debugging.md)保留可复现命令，[验证索引](../validation/README.md)保存当时范围和限制。Playwright 使用系统 Chrome，不下载 Chromium。真实 binary 集成测试同时隔离 HOME/USERPROFILE 与 CODEX_HOME，不写真实 `~/.codex` / `~/.codex-web`。

Git 允许真实图片和文本 SVG；禁止其他二进制（含可执行文件、库、字体、压缩包、数据库、bundle），不得改扩展名或 Base64 编码绕过。默认检查暂存区，CI 检查提交树，历史审计检查全部已知分支可达对象；发现历史问题先列明受影响 refs 和迁移方案，历史改写、远端 push 仍须符合当前明确授权。

Base64 检查是有限的内容检查：检查至少 16 字符的单行或规则换行 Base64 文件、载荷至少 64 字符的 Base64 data URI，以及至少 1,024 字符的连续 Base64 token。完整文件检测不移除单词间空格；多行编码要求非末行等宽、每行至少 16 字符且宽度为 4 的倍数，末行不更长，仅末行可有 padding。只有规范解码后属于非图片二进制才拒绝。它不能识别任意编码或分段混淆，人工审查仍需确认没有将产物编码入库。短小的合成协议/解析 fixture 不等同于存储构建产物，不应仅因出现编码字符串就宣称违规或删除。

发布从干净、已评审、CI 通过的 main tag 进行；核验[第三方许可](../../THIRD_PARTY_NOTICES.md)、嵌入资源、兼容基线、迁移说明和[变更日志](../../CHANGELOG.md)。构建成功不等于平台或真实模型验收通过。所有 remote mutation 遵守 [AGENTS](../../AGENTS.md) 的授权约束。
