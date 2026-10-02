# Codex Local Gateway

本机原生 Codex 工作台。在项目目录执行 `code-view`，即可打开网页并启动普通官方 Codex CLI：右侧原生终端负责输入、审批、追问和模型/权限设置，中央阅读实时对话与工具结果，左侧查看项目文件与历史。

工作台通过本次 CLI 的模型 HTTP/SSE/WS 代理提供实时阅读，后台异步保存观察记录。官方 CLI 保持原版；保存失败时终端继续可用，页面明确显示缺口和未保存状态。`code-view --history` 或 `codex-view` 打开全部历史首页，浏览历史不创建 CLI。

## 主要功能

- **原生终端交互**：保留 CLI 的目录信任、审批、键盘操作、ANSI 颜色与设置。在网页中操作同一个 CLI，无需换用另一套对话输入协议。
- **实时对话和工具阅读**：模型回复生成过程中即可阅读；工具卡展示参数、结果及拟议文件变更，调用详情可查看对应请求上下文与用量。模型输出、工具执行和整轮完成分别呈现。
- **统一历史**：按项目、来源、主会话/子代理筛选，搜索会话与消息；读取原生会话、工作台记录和已登记的旧 Observer 历史。可核验身份的原生会话提供明确的“继续此会话”入口。
- **文件、搜索与 Git**：阅读当前项目文件、搜索代码、跳转行号，查看已暂存/未暂存变更和提交记录。文件与 Diff 保持只读，打开文件不会中断右侧终端。
- **用量和保存状态**：查看本次运行的 Token、输入/输出、缓存及推理明细；缺失用量不当作零，保存异常和历史覆盖单独提示。
- **多个项目并行**：最多 4 个活动项目，各自拥有 CLI、文件、代理、用量和保存状态。可返回首页、重新进入工作台或只停止一个项目。
- **手机接入**：电脑端明确开启设备访问后，通过二维码配对同一项目的终端与阅读页面；支持可互访的局域网和已连接 Tailscale 的设备。
- **历史与配置管理**：同页设置面板提供数据来源、历史占用、明确确认后的手动清理和可选保留期限。原生 Codex 数据与旧数据库不自动迁移或清理。

## 运行条件与支持范围

发布包支持 **macOS / Apple Silicon（arm64）**，内嵌网页和目录选择助手，运行不需要源码、Rust 或 Node.js。安装只使用系统 `curl`、Bash 和归档工具，无需 GitHub 账号、GitHub CLI 或 sudo。

开始项目需要自行安装并配置官方 Codex CLI。当前已验证接入为 `unmanaged-custom`：原生 `custom` provider、Responses、显式上游地址与静态 bearer；其他 provider、ChatGPT/管理配置及环境/命令认证尚未纳入支持。历史首页无需安装 CLI 即可打开。代码搜索需要 `rg`，Git 阅读需要 `git`；缺少时只影响对应能力。

已有 CLI `0.154.0`、`0.155.1`、`0.156.1`、`0.159.2` 的受测记录；未知版本提示后继续检查配置，不自动升级、降级或修改 CLI。Intel macOS、Linux、Windows 完整运行尚未验收。具体能力与验证边界见[支持范围](docs/codex-native-cli-workbench-support.md)。

## macOS 快速安装与卸载

复制执行以下命令：先下载脚本，再用 Bash 执行，成功后让当前终端立即找到命令。稳定发布包见 [GitHub Releases](https://github.com/windsyu/codex-plugin/releases/latest)。

```sh
(
  installer_file=$(mktemp) || exit
  trap 'rm -f "$installer_file"' EXIT
  curl --disable --fail --silent --show-error --location \
    --proto '=https' --proto-redir '=https' \
    --output "$installer_file" \
    'https://raw.githubusercontent.com/windsyu/codex-plugin/main/scripts/install-macos.sh' || exit
  bash "$installer_file"
) && export PATH="$HOME/.codex-view/bin:$PATH"
```

[安装脚本](scripts/install-macos.sh)匿名下载最新稳定版本、校验归档与包内文件后，将命令安装到 **`~/.codex-view/bin`**，许可与卸载清单位于 `~/.codex-view/share/code-view`。脚本为 zsh 和 Bash 登录终端添加有标记的 PATH 区块，保留已有配置；新终端自动可用。程序配置和历史继续存放在 `~/.codex-web`。

如果曾用旧快速脚本安装到 `~/.local`，先退出工作台并运行 `"$HOME/.local/bin/code-view-uninstall"`，再执行上面的安装命令。脚本会提示这一冲突，不自动搬迁或删除旧安装。指定版本、其他安装目录和升级方式见[安装指南](docs/guides/installation.md)。

## 开始使用

### 在项目目录快速启动

```sh
cd /path/to/project
code-view
# 或直接指定已有项目目录
code-view "/path/to/another project"
# 使用原生命名 profile，不自动打开浏览器
code-view --profile named --no-open
# 检查安装版本
code-view --version
```

无参数 `code-view` 直接启动当前目录项目的新对话；同目录已有活动工作台时复用，不自动续最近会话。在右侧原生终端完成目录信任、输入和审批。使用 `--no-open` 时，根据启动输出的私有入口文件执行 `code-view open <entryFile>` 打开页面。

原启动器终端保持前台运行。关闭网页或点击“← 全部历史”会保留 CLI；首页卡片可重新进入。在首页选择“停止”，或在项目“用量与状态”中选择“停止当前运行”，只结束该项目。**在启动器终端按 Ctrl-C 会结束整个应用及其自有 CLI。**

### 阅读历史、继续会话与切换项目

```sh
code-view --history
# 等价的历史首页入口
codex-view
```

首页可直接阅读已有记录，不启动 CLI。点击“开始新对话”或“打开其他目录”，核对真实项目目录后明确启动；点击“继续此会话”时同样需要核对工作目录。原生会话只有在来源、会话 ID 和文件身份可验证时才能恢复；旧 Observer 副本、工作台记录和继承历史可以阅读，但不从网页恢复。

最多同时运行 4 个项目，切换页面不会替其他项目提交输入。历史扫描与正文搜索有独立覆盖状态，“全部历史”不代表所有来源或正文已扫描完毕。工具详情按需加载；保存的是脱敏阅读记录，不是完整终端录像。

### 文件、用量与设备

左侧“文件 / 搜索代码 / Git”对应当前项目，文件和 Diff 在中央打开，关闭后恢复对话阅读位置。文件支持连续滚动、行号跳转与查找；单文件最大 1 MiB，搜索最多 200 处匹配，超限或截断会提示。底栏 Token 摘要可展开查看本次运行用量与保存状态，切换历史不会把旧用量计入当前运行。

电脑端点击“手机接入 → 开启设备访问”后扫描二维码。局域网要求网络可互访；MagicDNS 要求手机已连接对应 Tailscale，无需配置 Serve。配对只授权当前项目，运行结束后失效；默认仅本机访问，局域网 HTTP 不加密。手机相册上传未提供；本机图片可在原生终端中粘贴完整路径，确认 CLI 出现附件标记后手动提交。

配置位于 `~/.codex-web/config/config.json`，工作台历史位于 `~/.codex-web/history`，独立于原生 `CODEX_HOME`。设置中可管理历史来源、占用与清理；手动删除和自动清理默认关闭，启用后也只处理符合条件的工作台记录。详细操作、恢复条件与设备边界见[使用指南](docs/guides/usage.md)。

## 卸载

先在启动器终端按 Ctrl-C 退出工作台，再执行：

```sh
code-view-uninstall
# 当前 PATH 尚未刷新时
"$HOME/.codex-view/bin/code-view-uninstall"
```

以上是同一卸载命令的两种调用方式，选择其一即可。卸载可离线执行，按安装清单移除程序和对应 PATH 区块，保留 `~/.codex-web`、原生 Codex 数据、个人 shell 配置及未知文件。已修改的受管文件会触发冲突提示，避免误删。自定义前缀使用对应 `<prefix>/bin/code-view-uninstall`；保护与恢复说明见[安装指南](docs/guides/installation.md#卸载)。

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

标准构建先生成 Web 资源，再编译两个 Rust 入口，确保内嵌页面和后端一致。发布构建使用 `node scripts/dev.mjs build --release`，安装包使用 `node scripts/dev.mjs package`，常规检查使用 `node scripts/dev.mjs check`。`codex-observerd` 保留旧 Observer 历史导入、只读网页/API 和显式维护命令；旧历史读取方式见使用指南。

## 文档与参与

- [安装指南](docs/guides/installation.md)：固定版本、升级、自定义目录、手动安装与卸载。
- [使用指南](docs/guides/usage.md)：终端、历史、配置、手机接入和旧历史兼容。
- [开发指南](docs/development/README.md)：模块、工具链、测试、产物和发布。
- [文档总索引](docs/README.md)：设计契约、ADR、验证报告与历史归档；阶段状态以 [V2 实施计划](docs/v2-implementation-plan.md)为准。
- [贡献指南](CONTRIBUTING.md)、[项目执行规范](AGENTS.md)、[安全报告](SECURITY.md)、[第三方许可](THIRD_PARTY_NOTICES.md)与[变更日志](CHANGELOG.md)。

项目使用 [MIT 许可证](LICENSE)。分发包附带依赖的原许可与修改说明。
