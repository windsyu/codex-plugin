# 文档、工程流程与构建产物治理验证

日期：2026-09-27。实现基线：`df4e64c`；本地分支：`codex/engineering-workflow`。本报告记录工程维护变更的实测结果，不增加产品阶段状态表，也不变更 R6 验收结论。

后续审查发现原测试未覆盖的缓存清理和仓库检查问题；以下保留当时结果，P1/P2 的修正与新增证据见 [2026-09-28 修复验证](engineering-workflow-fixes-2026-09-28.md)。原测试通过不能作为这些边界已经验证的依据。

## 变更范围

- README 收敛为项目入口；增加使用、开发、ADR 和验证索引，保留设计路径、原型、图片及有效引用。
- [AGENTS.md](../../AGENTS.md) 固化模块、标准命令、产物生命周期、二进制政策和交付流程；保留单 package、library 加两个 binary，不修改业务模块边界。
- 增加 `scripts/dev.mjs`、维护工具及合成测试、固定工具链和 macOS CI，以及贡献、安全、行为规范、MIT 许可、第三方声明、变更日志和协作模板。
- 未修改业务 API、数据库 schema、用户服务、依赖锁文件或产品实施计划；没有 push、发布、GitHub 设置修改或历史重写。

## 环境与实际结果

本机 macOS，Node 22.23.2、npm 10.9.8、Rust/Cargo 1.95.0、Apple SDK；浏览器使用已安装 Google Chrome。Rust 版本是开发和 CI 基线，不是经跨版本测试确认的 MSRV。

| 检查 | 实际结果 |
| --- | --- |
| `node scripts/dev.mjs doctor` | 工具链与 Apple SDK 检查通过 |
| `node scripts/dev.mjs bootstrap --offline` | 失败：本机 npm 缓存缺少 `zimmerframe`；不能宣称首次安装可离线完成 |
| `node scripts/dev.mjs bootstrap` | 联网重试成功，安装 228 个包；锁文件未变 |
| `node scripts/dev.mjs check --offline` | 第二次完整执行通过；首次失败见下一节 |
| 维护工具测试 | 23 项通过（产物 11、仓库检查 10、命令参数 2） |
| TypeScript / Vitest / Vite | 类型检查和构建通过；31 个测试文件、234 项测试通过 |
| Rust fmt / Clippy | 通过；Clippy 使用 `--all-targets -- -D warnings` |
| Rust 常规测试 | `--locked --offline --all-targets -- --test-threads=4`：439 通过，0 失败，54 ignored |
| 两个 Rust binary | debug 与 release 均通过 `--locked --offline --bins` 构建；先构建 Web |
| `node scripts/dev.mjs e2e` | 系统 Chrome：10 项通过；未下载浏览器 |
| 文档检查 | 新增本报告后 97 个 Markdown 文件通过，覆盖本地链接、图片引用、中文/重复锚点、标题层级和代码围栏 |
| Workflow / Issue 模板 YAML | 4 个文件解析通过；远端 CI 尚未运行 |
| Git 内容与差异 | 当前提交树和所有已知分支历史无禁止二进制；新增/修改工作文件另按内容检查；`git diff --check` 通过 |

Rust 分项为 library 339、旧 Observer binary 94、退役集成测试 1、examples 5。54 个 ignored 测试没有算作通过；本次未重新执行真实官方 CLI、真实模型、真机设备或大规模数据专项。浏览器 10 项覆盖现有前端 Playwright 套件，不替代原生工作台专项验收。

## 首次失败与复跑

首次常规检查在 `workbench::config::tests::stale_revisions_and_backup_failures_do_not_overwrite_config` 失败：测试在配置保存时收到 `config_busy`，library 结果为 338 通过、1 失败、49 ignored。该测试使用独立临时目录，保存操作有 3 秒锁等待期限。

随后执行以下隔离命令，1 项通过（0.04 秒）：

```sh
cargo test --locked --offline --lib workbench::config::tests::stale_revisions_and_backup_failures_do_not_overwrite_config -- --exact --test-threads=1
```

停止并发历史对象扫描后，重新完整执行 `node scripts/dev.mjs check --offline`，全部常规检查通过。没有修改业务代码、测试断言或跳过失败测试。资源竞争可能影响限时测试，但当前证据不能确认根因；后续若复现，应针对锁持有者与等待时序调查。

本机可复核日志位于以下受管目录（按保留策略过期，不作为永久发布附件）：

- 首次失败：`target/artifacts/f560cde4-a8cc-43bc-8009-021010d71ca8`。
- 完整复跑：`target/artifacts/e466c082-3df6-4f50-86b9-73f715993e38`。
- Chrome 测试：`target/artifacts/f7e0548c-a320-4aee-97e9-3b6917e744f8`。

## 产物保留与清理安全

首次清单：`target/artifacts/76b41c9c-bd5b-4481-a71c-80e742d4238d/inventory.json`，包含 719 个未登记旧条目：未分类 172、未登记元数据 117、备份保护 2、未登记诊断 420、Cargo profile 待人工检查 2、运行数据保护 6。首次盘点的过期受管候选为 0；旧条目全部保留，未清理真实主缓存。

清单分类只是提示，不是删除授权。`observer-data`、备份、数据库、Git bundle、既有图片和报告不加入过期清理。标准命令新建的日志均登记到任务 UUID 目录；成功保留 7 天，失败和已登记独立构建保留 30 天。

测试使用合成临时目录验证精确到期边界、失败/构建保留期、活动/固定保留/未知项、损坏与外仓元数据、符号链接、锁冲突、lease 变化、每日频率和部分删除失败。真实离线 Cargo 临时项目验证 profile 中的数据库、伪装库文件及未知输出在预览和执行模式都被拒绝。

独立安全评审发现并修复了 profile 可能包含用户数据、异常/固定保留元数据可能绕过活动检测的问题；增加回归测试后通过。主缓存清理仍保守拒绝部分合法但无法确认归属的 build-script 输出；需由所有者人工分类，不能移除保护检查绕过。缓存大小按逻辑文件长度汇总，可能重复计入硬链接，不是预计释放磁盘空间。

## Git 历史与图片

`node scripts/dev.mjs binaries --tree HEAD` 检查基线 618 个文件；`binaries --all-branches` 检查本地分支、远端跟踪分支及 tags 可达的 1,109 个历史文件对象。审计识别 139 个 PNG，未发现非图片二进制。stash 与工具辅助 refs 不属于分支审计范围，未修改。

只读核实的远端分支与本地缓存一致：

| 远端分支 | 提交 |
| --- | --- |
| `main` | `930172493c163a903621ca14c540d1ca10dd4f3e` |
| `fix/session-startup-input-project` | `4da04e467b62aad61ff7eaaedbf1fabb36c5e1c7` |
| `feat/v2-controller-foundation` | `9aa49168d40ae53244dfbf72d75c1fbc1c39bb97` |

零项待清除，因此没有删除已提交文件或重写历史。二进制测试验证真实图片/SVG、伪装扩展名、无扩展名、Base64、暂存内容与工作树差异、其他分支中已删除的历史文件，以及浅克隆拒绝行为。本次改动尚未暂存，默认暂存检查的 0 项不能作为新文件已检查的依据；交付时另外检查实际新增和修改文件的内容。检查能力及 Base64 边界见[开发指南](../development/README.md)。

## 已知依赖告警与后续工作

联网安装及 `npm audit --json --prefix web` 报告现有 Web 开发/测试依赖有 5 项告警：3 moderate（`@vitest/mocker`、`esbuild`、`vite-node`），1 high（Vite 5.4.21），1 critical（Vitest 2.1.9）。其中 Vitest UI 的公告为 [GHSA-5xrq-8626-4rwp](https://github.com/advisories/GHSA-5xrq-8626-4rwp)，Vite 的公告包含 [GHSA-fx2h-pf6j-xcff](https://github.com/advisories/GHSA-fx2h-pf6j-xcff)。这是 audit 对锁定开发依赖的报告，不是已经证明生产工作台可被利用的结论。

本次没有自动执行强制依赖升级。后续应独立升级 Vite/Vitest 及关联依赖，核对公告适用条件，复跑类型、前端单测/构建和系统 Chrome 测试，并复核 audit。基础 CI 尚未包含独立依赖漏洞门禁。主缓存人工整理、模块依赖收敛及真实专项验收也未在本次冒充完成。
