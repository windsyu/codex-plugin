# 安装与快速启动

[使用指南](usage.md) · [支持范围](support.md) · [发布维护](../development/releasing.md)

`1.0.1` 发布包面向已验证的 macOS / Apple Silicon，包含后台服务、历史整理和按 Run 加密的可选公网连接器，不包含独立 Relay 服务器。网页和原生目录选择助手已嵌入程序，运行时不需要源码、Rust 或 Node.js。启动项目仍需自行安装并配置官方 Codex CLI；模型路由与认证的受测范围见[支持文档](support.md)，启动预检只检查本次代理转发和安全边界必需的配置。代码搜索需要 `rg`，Git 阅读需要 `git`。

公网使用者须将电脑、Relay 和中央前端配套升级至 API/wire 3，并从门户重新打开工作台；不提供明文降级。仅保留单 origin，账号、登记和 schema 3 数据保留，服务器升级前按[中转指南](public-relay-deployment.md)备份。加密不防御被恶意修改的前端或授权方；`1.0.1` 不表示真实手机、全部 CLI 版本或其他平台已完成验收。

## 快速安装与 PATH

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

[安装脚本](../../scripts/install-macos.sh)通过系统 `curl` 匿名下载本仓库的最新稳定 release，无需 GitHub 账号、GitHub CLI 或编译环境。脚本先解析公开的 latest 重定向并固定版本，再下载同一版本的归档与校验文件；校验归档和包内文件后调用包安装器。下载只使用 HTTPS，忽略用户 curl 配置；下载、版本解析或校验失败不会修改安装文件和 shell 配置。

已有源码 checkout 时，从仓库根目录运行同一个脚本，无需构建：

```sh
bash scripts/install-macos.sh
# 可固定版本或指定绝对安装前缀
bash scripts/install-macos.sh --version v1.0.1 --prefix "$HOME/.codex-view"
# 明确升级已有安装：后台先 code-view stop，前台先 Ctrl-C
bash scripts/install-macos.sh --force
# 只安装程序，由自己管理 PATH
bash scripts/install-macos.sh --no-path
```

源码构建的本地包也可通过同一受管安装器升级，不下载 GitHub release：

```sh
bash scripts/install-macos.sh --archive /absolute/path/code-view-1.0.1-aarch64-apple-darwin.tar.gz \
  --prefix "$HOME/.codex-view" --force --no-path
```

归档旁须有标准 `SHA256SUMS`。版本由归档的标准文件名确定；若同时传 `--version`，具体版本必须一致。该入口复用归档与包内校验、安装锁、服务检查、原子替换、回滚和卸载清单；`--no-path` 保留原安装已登记的 PATH 区块。升级前仍须先正常停止工作台；不直接用手动包安装器覆盖快速安装的文件，以免卸载清单哈希失配。

默认在 `~/.codex-view/bin` 安装三个原有入口及 `code-view-uninstall`，许可与安装清单位于 `~/.codex-view/share/code-view`，无需 sudo。安装与升级只需系统 `curl`，卸载可以离线完成。

脚本在 `${ZDOTDIR:-$HOME}/.zshrc` 与 Bash 当前生效的登录配置添加带 `code-view PATH` 标记的区块，保留其他内容。Bash 按 `~/.bash_profile`、`~/.bash_login`、`~/.profile` 的顺序选择第一个已有文件；全部不存在时才创建 `.bash_profile`，避免遮蔽已有设置。路径通过 shell 转义写入，避免重复追加；已有 dotfile 使用原子替换并保留权限。新 zsh 交互终端或 Bash 登录终端自动加载。安装脚本不能改变父 shell，当前终端执行：

```sh
export PATH="$HOME/.codex-view/bin:$PATH"
code-view --version
```

自定义前缀时使用该前缀的 `bin`。前缀必须是绝对路径，支持空格和中文，拒绝冒号、换行、`.` / `..` 组件与符号链接。dotfile 或其祖先为符号链接时可用 `--no-path` 并手动管理 PATH；升级时 `--no-path` 保留此前已登记的 PATH 区块，便于日后卸载。

## 从旧安装目录切换

曾用旧快速脚本安装到 `~/.local` 时，先退出工作台，再运行 `"$HOME/.local/bin/code-view-uninstall"`，然后执行上面的快速安装命令。新默认安装发现旧清单时会提示停止，避免留下两套命令和 PATH 区块；不自动迁移或删除旧文件。已有自定义前缀仍可通过 `--prefix` 明确管理。

旧手动包安装没有快速卸载清单；如需纳入管理，先用 `bash scripts/install-macos.sh --prefix "$HOME/.local" --force --version v0.3.0` 登记，再运行该前缀的卸载器。用户配置和历史无需搬迁，仍在 `~/.codex-web` 与原生 `CODEX_HOME`。

## 卸载

后台实例先运行 `code-view stop`，前台实例在启动器终端 Ctrl-C；确认各配置/数据作用域的工作台及所属 CLI 已退出后，再执行：

```sh
code-view-uninstall
# PATH 尚未刷新或被手动改过时，也可用绝对路径
"$HOME/.codex-view/bin/code-view-uninstall"
```

自定义前缀使用 `<prefix>/bin/code-view-uninstall`。源码脚本也支持 `bash scripts/install-macos.sh uninstall --prefix <prefix>`。卸载读取安装清单，只删除已登记且未被修改的普通文件，移除安装时登记的精确 PATH 区块，并清理空资源目录；未知文件及 `~/.codex-web`、原生 `CODEX_HOME`、旧数据库保留。

支持后台服务的新版本会在升级/卸载改文件前检查本安装的服务登记；仍在运行或无法验证时拒绝，并提示先停止。卸载只移除本可执行文件所属、已验证且不活动的 LaunchAgent 和服务登记，不自动停止进程。旧 binary 没有服务检查接口时会提示手动确认退出；原有安装文件、PATH 和清单验证仍适用。`autostart` 是显式选择，安装器不会自动开启登录自启。

程序文件被修改、PATH 区块残缺/重复、清单异常或路径被替换为符号链接时，卸载会在写入前停止并报告冲突；保存修改后恢复该文件，或自行移除已改的区块后重试。`--force` 仅用于安装，不跳过卸载保护。普通删除/配置写入失败保留清单和卸载器，修复原因后可以重试。安装用私有 staging、原子文件替换与旧文件快照处理失败回滚；若回滚本身因 I/O 或并发修改未完成，日志指出实际未恢复文件，不报告成功。进程被 SIGKILL 或机器突然断电时不承诺事务恢复。

旧 `v0.3.0` 包直接运行 `install.sh` 的安装没有这份快速安装清单；运行快速安装脚本并明确 `--force --version v0.3.0` 重装后，即可使用卸载命令。脚本不改写已发布资产，原 binary 和用户数据格式保持不变。

已登记的快速安装保留原来的 PATH 管理对象；若早期辅助脚本曾新建 `.bash_profile` 并遮蔽你的 `.bash_login` / `.profile`，先运行其 `code-view-uninstall`，再执行上面的快速安装命令。卸载会移除仅由安装器创建且未添加个人内容的 `.bash_profile`，新安装按上述优先级选择，不自动迁移个人 shell 配置。

## 手动安装发布包

从 [v1.0.1 GitHub Release](https://github.com/windsyu/codex-plugin/releases/tag/v1.0.1) 下载安装包与 `SHA256SUMS`。正式资产的 manifest 记录发布 tag 对应的源码提交，`sourceDirty` 为 `false`。

将 `code-view-1.0.1-aarch64-apple-darwin.tar.gz` 和同目录的 `SHA256SUMS` 放在一起，校验并解压：

```sh
shasum -a 256 -c SHA256SUMS
tar -xzf code-view-1.0.1-aarch64-apple-darwin.tar.gz
cd code-view-1.0.1-aarch64-apple-darwin
./install.sh --prefix "$HOME/.codex-view"
export PATH="$HOME/.codex-view/bin:$PATH"
code-view --version
```

包内安装器先校验内容、平台与目标冲突，再复制到指定前缀的 `bin` 和 `share/code-view`，默认前缀为 `~/.codex-view`，无需 sudo；旧 `v0.3.0` 包内安装器的默认前缀为 `~/.local`。这个手动入口不编辑 shell 配置或生成快速卸载清单；需要 PATH 和卸载管理时使用上面的快速脚本。手动安装器遇到已有文件会停止，明确升级时使用 `./install.sh --prefix "$HOME/.codex-view" --force`。升级前后台实例先 `code-view stop`，前台实例先在原启动器中 Ctrl-C，再安装并启动新版本。

当前源码的包安装器拒绝符号链接、非普通文件或多硬链接的可执行文件目标；`--force` 不绕过这些检查，避免覆盖同一文件的其他安装路径。升级服务检查与安装锁同样适用于手动包入口。

包也可以不安装，直接使用解压目录中的 `bin/code-view`。校验和用于检查传输完整性；当前本地包没有 Apple 签名或 notarization。Linux、Windows 和 Intel macOS 未完成验收。

## 从项目目录启动

```sh
cd /path/to/project
code-view
# 或直接指定已有目录，包含空格时使用引号
code-view "/path/to/another project"
# 所有原启动参数继续可用
code-view --profile named --no-open
# 只阅读历史，不启动 CLI
code-view --history
```

`code-view` 将当前目录作为明确项目启动请求；路径参数以调用命令的目录为基准，不切换到安装目录。已有同目录 Run 时复用，默认开始新对话，不自动续最近会话。首次普通项目启动仍保持前台；已有后台实例时复用它。原生 CLI 负责实际输入与审批，关闭网页保留运行。前台使用 Ctrl-C 退出；需要常驻时先运行 `code-view start`，结束时用 `code-view stop`，详见[后台服务与首次注册](usage.md#后台开启远程注册和退出)。这些命令需安装包含该功能的新构建，已发布旧包不会自动获得源码新增功能。

`codex-view` 无参数仍打开全部历史首页，`code-view open <entryFile>` 可重新打开已运行应用。`code-view --help` 显示快速入口和底层参数。配置、历史及原生数据目录继续沿用现有约定，没有数据迁移；细节见[使用指南](usage.md)。

从源码生成同样的包，运行 `node scripts/dev.mjs package`；本地打包与 GitHub 正式发布的区别见[发布维护](../development/releasing.md)。
