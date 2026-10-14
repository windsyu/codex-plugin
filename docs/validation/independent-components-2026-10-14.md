# 本机工作台与中转独立组件验证（2026-10-14）

## 范围与来源

分支 `codex/public-relay`，基线 `fa094c23b3742a6bb6cb475c7ac924fd7357582f` 加本机未提交增量；既有账号/中央前端改造保留。本次仅调整构建/检查选择、Cargo 默认成员及部署文档，不修改运行 API、认证、schema 或历史数据。设计见 [ADR 0062](../decisions/0062-independent-component-builds.md)，操作见[组件指南](../guides/component-deployment.md)。

本机环境 macOS / Apple Silicon，Node 22.23.2、Rust/Cargo 1.95.0，`doctor` 通过。以下结果限本机环境，不是 Linux 新部署、真实手机或完整公网验收。

## 已执行检查

| 检查 | 结果与证据 |
| --- | --- |
| `node --test scripts/tests/dev.test.mjs scripts/tests/components.test.mjs` | 8 项通过；参数拒绝/默认兼容、两端独立选择、前端先于 Cargo、失败停止、完整 workspace 及共享测试、package 显式目标、实际 Cargo path 依赖边界及反例 |
| `build --component relay --release --offline` | 通过；受管任务 `6e8ee242-61b8-4f0d-aac9-216514a4a239`，先 Web 后 `cargo build ... -p codex-relayd --bins --release`；日志无本机 root package 编译 |
| `build --component workbench --release --offline` | 通过；受管任务 `c3a87d77-82fc-4565-a236-3e7127669e8a`，先 Web 后 `cargo build ... -p codex-local-observer --bins --release`；日志无 relay package 编译 |
| `node scripts/dev.mjs check --offline` | 通过；受管任务 `969c85fb-aeae-48dc-814d-d021f808ae4a`（`check-all`），工具 91 / Web 303 项、TypeScript、fmt、Clippy，通过 14 Rust targets 合计 546 次测试、57 ignored；三个 binary debug/release 通过 |
| 中转及共享包单独 `cargo clippy --locked --offline -p codex-relayd -p codex-relay-protocol -p codex-workbench-assets --all-targets -- -D warnings` | 通过；仅所选产品/共享包的编译范围，无 root 本机 package 检查 |
| `package --component workbench --offline` | 通过；受管任务 `406879c4-9add-4a2c-b344-502b51e86637`，显式 `aarch64-apple-darwin`、仅根产品构建。包内 `bin` 精确为 `code-view` / `codex-view` / `codex-observerd`，无 `codex-relayd`；289 个许可条目和内外校验和。实际归档解压、隔离安装、版本、当前项目、嵌入资源、项目复用、零 CLI 历史、第二项目与 SIGINT 回收 9 项通过 |
| 三个 release binary 的 `--version` 与 `file` | `codex-view 0.3.0`、`codex-observerd 0.3.0`、`codex-relayd 0.1.0`，均为本机 Mach-O arm64；无服务启动 |
| `docs` / `git diff --check` / 二进制内容检查 | 文档 129 篇无问题，diff 无空白错误；标准 `binaries` 暂存区 0 项，另对 146 个实际工作树新增/修改文件运行同一内容分类器，未发现禁止二进制；不是 secret 自动扫描或完整 Git 历史审计 |

组件 build/check 未指定时保持 `all`；直接 Cargo 默认选择本机 root package。完整标准检查显式 `--workspace`，组件 Rust 检查仍覆盖共享协议和静态资源。本机 relay 仅为 dev fixture，不删除跨端授权/流测试，也不进入本机生产构建。

独立 `gpt-6-astra` 高深度只读审查覆盖当前 dirty 工作树的组件 recipe、Cargo 生产依赖、CI/其他工程调用与新文档 paths/flags/认证/部署边界，未发现需修复缺陷；不重审此前运行时认证实现，也不将审查视为测试或部署证据。

## 限制与未执行项

### 正式维护文档与 Git 规则补充核对

同日后续新增本机/中转各自的[架构](../architecture/workbench.md)/[交互](../interactions/workbench.md)及[中转架构](../architecture/relay.md)/[交互](../interactions/relay.md)维护入口，将原混合中转契约拆分、单 IP 试部署说明并入正式运维指南，并删除两个已完整替代的旧文件；ADR 与有效历史验证证据保留，全部入站链接迁移。没有修改运行逻辑、构建 recipe 或 schema；暂存区检查另外清除新门户 CSS 的一个 EOF 空行，不改变样式行为。

`node --test scripts/tests/repository-checks.test.mjs scripts/tests/components.test.mjs scripts/tests/dev.test.mjs` 23 项通过，包括实际 `.gitignore` 排除两端裸 binary/嵌套构建目录/中转 `.sqlite3` 及 sidecar、保留源码启动器的合成回归。文档检查 131 篇、0 错误，`git diff --check` 通过。有限凭证特征检查只命中既有脱敏测试的合成 `ABCD` PEM 标记，未发现实际密钥；这不是完备 secret 扫描。

独立只读审查发现并修正文档的单 origin/历史模式前端边界、Linux TLS 容器访问宿主 loopback 的网络前提、实际数据库忽略规则和接入状态 GET 与写操作的修订要求，并复核四项修正已解决。审查不重新验证此前运行时或真实部署；除上述 CSS 空白外，生产/构建源文件未在前述全量检查后变动，复用该检查证据。本轮没有重启用户任务、部署或真实手机网络验收。

提交准备时标准 `binaries` 对已实际暂存的 153 个新增/修改 blob 检查通过，没有禁止二进制；确认 tracked 文件没有构建目录/安装包/运行数据库。提交树检查在提交完成后另执行，不用此前空暂存区结果代替。

组件 check 的命令编排已由合成测试及完整标准检查覆盖，未再重复两套完整工具/Web 检查。安装包标记 `sourceDirty:true`，不是已发布 main tag；许可数量含 host-resolved Cargo build/test 依赖，不表示这些运行模块均随包分发。

本次不更新远端 Linux 专用中转、不重启当前用户本机任务、不重新执行真实模型/系统 Chrome/真手机网络验收。中转构建得到的本机 Mach-O 不能部署到 Linux；没有新增通用 Linux archive 或自动交叉编译功能。共享前端、协议/API 严格匹配和现有 schema 3 备份恢复边界不变；不存在本轮 migration。构建仍提示主 debug 缓存超过 20 GiB，未执行缓存清理。
