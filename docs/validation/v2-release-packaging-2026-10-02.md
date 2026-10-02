# V2 0.3.0 Release 包装验证（2026-10-02）

[实施计划](../v2-implementation-plan.md#10-v2-release-包装) · [安装指南](../guides/installation.md) · [发布维护](../development/releasing.md)

## 范围与来源

用户要求将已完成 V2 包装为 release，确认 `code-view` 无参数直接启动当前目录项目；指定目录使用 `code-view <directory>`，历史首页保留 `code-view --history` / `codex-view`。版本为 `0.3.0`，Rust package、Web package、锁文件和 Observer compatibility manifest 同步版本，schema 保持 20，现有历史 reader 20/25 和工作台配置契约不变。

验证基线为 `1c66746fd3e597117e8290a77bb40342f4d631ac` 加 `codex/v2-release-packaging` 本地改动；manifest 明确 `sourceDirty: true`。环境为 macOS / arm64、Rust 1.95.0、Node 22.23.2；支持声明仍限已有 macOS / Apple Silicon 和 unmanaged-custom 路由。不将本机成功扩大到 Intel/Linux/Windows、其他 provider 或完整手机矩阵。

上述本地包装验证没有迁移数据库、替换现有服务、修改原生配置、调用真实模型、安装到真实用户前缀或修改 shell 配置；当时没有提交、push、创建 PR/tag 或发布 GitHub Release。用户随后于 2026-10-02 明确授权评审后正式发布，发布门槛与后续记录见末节。

主要修改文件：

| 范围 | 文件 |
| --- | --- |
| 用户入口与安装 | `src/bin/codex-view.rs`、`scripts/release/code-view`、`scripts/release/install.sh` |
| 打包与产物验证 | `scripts/dev.mjs`、`scripts/lib/release.mjs`、`scripts/lib/release-smoke.mjs`、`.gitignore` |
| 回归 | `scripts/tests/release-launcher.test.mjs`、`scripts/tests/release.test.mjs`，以及 binary 内版本测试 |
| 版本 | Cargo/Web manifests 与锁文件、`compatibility/codex-41ece455.json` |
| 使用及维护文档 | README、CHANGELOG、第三方声明、安装/发布指南、支持范围、实施计划与索引 |

## 检查结果

```sh
node scripts/dev.mjs doctor
node scripts/dev.mjs bootstrap
node scripts/dev.mjs check --offline
CARGO_BUILD_TARGET=x86_64-unknown-linux-gnu node scripts/dev.mjs package --offline
node scripts/dev.mjs docs
git diff --check
```

| 检查 | 结果 |
| --- | --- |
| 完整 `check --offline` | 45 项维护工具、234 项 Web、445 项 Rust 全通过；56 项显式 ignored 未运行 |
| 格式 / Clippy / 类型 / Web / debug / release | 全通过 |
| 实际打包、解压与安装 | 通过；中文及空格目录，独立 HOME/USERPROFILE/CODEX_HOME |
| 项目 / 历史 / 复用 / 内嵌资源 / 退出 | 9 类包级验证通过，见下节 |
| 许可 | 281 个依赖条目；实际通知文件、vt100 补丁和 Codicons 归属随包并随安装保留 |
| 动态依赖 | 两个 Rust binary 仅依赖 `/usr/lib` 和 `/System/Library` 下系统库 |
| Git 内容策略 | 默认 `binaries` 暂存区为零文件，另对全部修改/未跟踪源文件调用同一内容分类器，未发现禁止二进制；产物位于 ignored target |
| 文档与差异 | 相对链接、锚点、标题、代码块及 `git diff --check` 通过 |

最终完整检查受管任务为 `444fd1fb-43b8-4931-8efc-764daf2e0e34`，包级验证任务为 `2364d4c7-5636-4340-9e7f-10b6f1e0ab29`；日志与产物置于各自 `target/artifacts/<UUID>`。包任务按独立构建产物保留 30 天，所有者需另存长期分发副本。

## 真实发布包验证

归档名为 `code-view-0.3.0-aarch64-apple-darwin.tar.gz`，约 10 MiB。SHA256：

```text
a0366b22f79b0e67c5d8de2779bb7c73005c8cff3fe695217e0321465e58d6f9
```

`package` 从显式 `--target aarch64-apple-darwin` 的输出构建归档；即使环境将默认 target 设为 Linux x86_64，仍构建并选择正确的 macOS arm64 产物。独立审查补充了相同版本、不同内容的新旧 binary 并存测试，实际执行打包确认只选显式 target 的新产物；非受测 host 在构建前拒绝。

验证直接解压归档并运行其中的安装器，以临时 PATH 调用安装后的 `code-view`，不依赖仓库源码或 `web/dist`。官方 CLI 使用合成 shell 替身，记录实际 cwd 并运行 PTY cat；不发送模型请求。

1. 包内 SHA256 校验与用户前缀安装成功；入口权限为 755，版本输出为 `codex-view 0.3.0`。
2. 无参数从中文/空格项目目录启动，CLI 的真实 cwd 与该项目一致。
3. 获取内嵌首页和引用的 JavaScript，HTTP 200 且正文存在；配对后应用 API 可读。
4. 同项目重复启动复用相同 CLI PID；历史入口不增加 Run。
5. 指定另一中文目录创建第二个 Run，两个 CLI 的 cwd 分别正确。
6. SIGINT 后启动器成功退出、两个自有 CLI PID 消失、私有 entry 文件删除。

安装专项还覆盖缺失材料、校验损坏、已存在目标、显式重装、文件/目录冲突、符号链接目标/祖先/递归资源、保留未知资源与不修改 shell 配置。安装采用写前预检，不提供跨文件事务；磁盘错误等写入失败可能留下部分安装。

## 失败、修正与未验证项

初次打包被版本门槛拒绝：Clap 默认输出 package 名称，已显式设置 `codex-view` 名称并新增版本回归。完整检查先发现新声明未符合 rustfmt，随后发现旧 compatibility manifest 仍为 0.2.0；已格式化并仅同步该 manifest 的产品版本，最终完整检查通过，没有放宽断言或修改数据库 schema。失败日志保留。

独立审查发现 Cargo 默认 target 可能改变输出目录，使打包拾取旧的同版本 binary；已锁定构建与读取目录，并有行为回归及真实错误默认 target 环境的复验。修正后未发现其他阻碍本地包装的问题。

`npm audit` 报告已有 6 项依赖审计项：开发工具 Vite/Vitest 及其依赖包含 moderate/high/critical，运行依赖 DOMPurify 为 low（IN_PLACE / afterSanitize hook 场景）。源码当前使用字符串清洗，未设置 IN_PLACE，也未注册 afterSanitize hook；本次没有升级依赖，不能将打包成功当成审计项已消除。前端开发工具不随运行包分发，维护环境的依赖升级留待独立跟进。

该依赖维护事项已登记为 [Issue #6](https://github.com/windsyu/codex-plugin/issues/6)，不在本次包装中静默升级或降低安全断言。

本次没有重跑真实模型、安装版官方 CLI/Chrome、原生目录窗口和真手机专项；相关功能未改变，保留此前 R6 验收证据与限制。自动验证证明本地包/安装入口和合成 CLI 集成，未证明 GitHub CI、Apple 签名/notarization 或正式发布已完成。

## 正式发布评审与门槛

2026-10-02 用户进一步授权评审、正式发布并上传 GitHub。独立发布评审重跑 16 项包装/安装专项全部通过，逐项核验已验证归档的 487 项文件校验和及 281 个许可索引材料，未发现必须发布前修复的可达缺陷；此前 Cargo target finding 已关闭。

本次正式交付对应 [Issue #5](https://github.com/windsyu/codex-plugin/issues/5)。获取远端 refs 后，全已知分支历史检查 1,213 个唯一 blob 未发现禁止二进制；待提交内容单独核验，不重写已有历史。

正式包须在评审后的 main 提交通过 GitHub CI 后重新生成，不能直接上传上述 `sourceDirty: true` 的本地验证包。最终 manifest 须为 `sourceDirty: false`，源码提交与 `v0.3.0` tag 一致，并再次通过同一解压安装/启动验证。实际发布状态、主分支 CI 链接、正式归档的摘要及下载以 [v0.3.0 GitHub Release](https://github.com/windsyu/codex-plugin/releases/tag/v0.3.0) 为准；本节不预先宣称远端已经成功。
