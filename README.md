# Codex Local Gateway

本机原生 Codex 工作台：先阅读全部历史，再明确选择项目启动普通官方 CLI 与 PTY；模型 HTTP/SSE/WS 代理提供实时阅读，网页直接推送，后台异步记录。原生终端负责输入、审批、追问和模型/权限设置。

默认入口 `codex-view` 打开全部历史首页，浏览历史不创建 CLI。核验项目后可明确开始新对话或继续可恢复的原生会话；工作台提供对话与工具阅读、调用详情、用量、文件/搜索/Git、手机配对和最多 4 个项目并行。返回首页或关闭网页保留运行，停止项目只结束其 CLI，退出启动器结束全部自有运行。`codex-observerd` 保留旧 Observer 历史导入、只读网页/API 和显式维护命令。

## 支持范围

当前实机支持 macOS；模型接入限已验证的 `unmanaged-custom` Responses 静态 bearer 路由。官方 CLI 保持原版，不自动升级，未知版本提示后继续检查配置。设备访问须显式开启；Windows 完整运行尚未验收。[支持范围与限制](docs/codex-native-cli-workbench-support.md)是能力说明，[V2 实施计划](docs/v2-implementation-plan.md)是唯一阶段状态源。

## macOS 快速安装与卸载

`0.3.0` macOS / Apple Silicon 发布包见 [GitHub Release](https://github.com/windsyu/codex-plugin/releases/tag/v0.3.0)。仓库目前为私有，需要已安装的 GitHub CLI（`gh`），并用有访问权限的账号完成 `gh auth login`。复制执行：

```sh
(
  installer_file=$(mktemp) || exit
  trap 'rm -f "$installer_file"' EXIT
  gh api 'repos/windsyu/codex-plugin/contents/scripts/install-macos.sh?ref=main' \
    -H 'Accept: application/vnd.github.v3.raw+json' > "$installer_file" || exit
  bash "$installer_file"
) && export PATH="$HOME/.local/bin:$PATH"
```

[安装脚本](scripts/install-macos.sh)下载最新稳定包、校验后安装到 `~/.local/bin`，登记卸载清单，并为 zsh 和 Bash 登录终端配置 PATH；新终端自动可用。安装后可在任意已有项目目录快速启动：

```sh
cd /path/to/project
code-view
# 或 code-view /path/to/project；只读首页使用 code-view --history
```

先退出已启动的工作台，再执行卸载；保留用户配置、历史和原生 Codex 数据：

```sh
code-view-uninstall
# 当前 PATH 尚未刷新时："$HOME/.local/bin/code-view-uninstall"
```

指定版本/目录、升级、手动安装和卸载保护见[安装指南](docs/guides/installation.md)。

## 从源码构建

开发基线：Rust 1.95.0、Node 22.23.2；macOS 需 Apple SDK/Clang。首次安装依赖需要网络。

```bash
node scripts/dev.mjs doctor
node scripts/dev.mjs bootstrap
node scripts/dev.mjs build
# 只打开历史首页，不启动 CLI
./target/debug/codex-view
# 在需要启动 CLI 的项目目录使用构建产物
<repository>/target/debug/codex-view --project .
```

浏览首页不要求安装 CLI；开始项目需已安装并配置官方 Codex CLI。工作台默认配置和记录位于 `~/.codex-web`，独立于原生 `CODEX_HOME`。构建先生成 Web 资源，再编译两个 Rust 入口，确保内嵌页面和后端一致。发布构建使用 `node scripts/dev.mjs build --release`，安装包使用 `node scripts/dev.mjs package`；常规检查使用 `node scripts/dev.mjs check`。详情见[开发指南](docs/development/README.md)。

## 文档与参与

- [使用指南](docs/guides/usage.md)：启动、终端、历史、配置、手机接入和旧历史兼容。
- [文档总索引](docs/README.md)：设计契约、ADR、验证报告、原型及历史归档。
- [贡献指南](CONTRIBUTING.md)与[项目执行规范](AGENTS.md)：模块组织、检查、产物保留、Git 和交付流程。
- [安全报告](SECURITY.md)、[第三方许可](THIRD_PARTY_NOTICES.md)、[变更日志](CHANGELOG.md)。

默认仅监听 loopback；原生数据和旧数据库不自动迁移或清理。构建清理只处理登记的开发产物，与工作台用户历史清理是不同机制。Git 允许图片，禁止其他二进制文件。项目使用 [MIT 许可证](LICENSE)。
