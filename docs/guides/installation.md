# 安装与快速启动

[使用指南](usage.md) · [支持范围](../codex-native-cli-workbench-support.md) · [发布维护](../development/releasing.md)

`0.3.0` 发布包面向已验证的 macOS / Apple Silicon。网页和原生目录选择助手已嵌入程序，运行时不需要源码、Rust 或 Node.js。启动项目仍需自行安装并配置官方 Codex CLI；当前模型接入限 `unmanaged-custom` Responses 静态 bearer。代码搜索需要 `rg`，Git 阅读需要 `git`。

## 快速安装与 PATH

[README 的复制脚本](../../README.md#macos-快速安装与卸载)通过 GitHub CLI 下载本仓库的最新稳定 release，先校验归档和包内校验和，再调用包安装器。仓库目前为私有，需安装 `gh` 并运行 `gh auth login`，账号须有仓库访问权限；脚本不会要求把 token 填入命令。

已有源码 checkout 时，从仓库根目录运行同一个脚本，无需构建：

```sh
bash scripts/install-macos.sh
# 可固定版本或指定绝对安装前缀
bash scripts/install-macos.sh --version v0.3.0 --prefix "$HOME/.local"
# 明确升级已有安装：先 Ctrl-C 退出旧工作台，再运行
bash scripts/install-macos.sh --force
# 只安装程序，由自己管理 PATH
bash scripts/install-macos.sh --no-path
```

默认在 `~/.local/bin` 安装三个原有入口及 `code-view-uninstall`，许可与安装清单位于 `~/.local/share/code-view`，无需 sudo。程序运行不依赖 `gh`；只有下载/升级需要它，卸载可以离线完成。

脚本在 `${ZDOTDIR:-$HOME}/.zshrc` 与 `~/.bash_profile` 添加带 `code-view PATH` 标记的区块，保留其他内容。路径通过 shell 转义写入，避免重复追加；已有 dotfile 使用原子替换并保留权限。新 zsh 交互终端或 Bash 登录终端自动加载。安装脚本不能改变父 shell，当前终端执行：

```sh
export PATH="$HOME/.local/bin:$PATH"
code-view --version
```

自定义前缀时使用该前缀的 `bin`。前缀必须是绝对路径，支持空格和中文，拒绝冒号、换行、`.` / `..` 组件与符号链接。dotfile 或其祖先为符号链接时可用 `--no-path` 并手动管理 PATH；升级时 `--no-path` 保留此前已登记的 PATH 区块，便于日后卸载。

## 卸载

先在启动器终端 Ctrl-C 退出工作台，再执行：

```sh
code-view-uninstall
# PATH 尚未刷新或被手动改过时，也可用绝对路径
"$HOME/.local/bin/code-view-uninstall"
```

自定义前缀使用 `<prefix>/bin/code-view-uninstall`。源码脚本也支持 `bash scripts/install-macos.sh uninstall --prefix <prefix>`。卸载读取安装清单，只删除已登记且未被修改的普通文件，移除安装时登记的精确 PATH 区块，并清理空资源目录；未知文件及 `~/.codex-web`、原生 `CODEX_HOME`、旧数据库保留。

程序文件被修改、PATH 区块残缺/重复、清单异常或路径被替换为符号链接时，卸载会在写入前停止并报告冲突；保存修改后恢复该文件，或自行移除已改的区块后重试。`--force` 仅用于安装，不跳过卸载保护。普通删除/配置写入失败保留清单和卸载器，修复原因后可以重试。安装用私有 staging、原子文件替换与旧文件快照处理失败回滚；若回滚本身因 I/O 或并发修改未完成，日志指出实际未恢复文件，不报告成功。进程被 SIGKILL 或机器突然断电时不承诺事务恢复。

旧 `v0.3.0` 包直接运行 `install.sh` 的安装没有这份快速安装清单；运行快速安装脚本并明确 `--force --version v0.3.0` 重装后，即可使用卸载命令。脚本不改写已发布资产，原 binary 和用户数据格式保持不变。

## 手动安装发布包

从 [v0.3.0 GitHub Release](https://github.com/windsyu/codex-plugin/releases/tag/v0.3.0) 下载安装包与 `SHA256SUMS`。正式资产的 manifest 记录发布 tag 对应的源码提交，`sourceDirty` 为 `false`。

将 `code-view-0.3.0-aarch64-apple-darwin.tar.gz` 和同目录的 `SHA256SUMS` 放在一起，校验并解压：

```sh
shasum -a 256 -c SHA256SUMS
tar -xzf code-view-0.3.0-aarch64-apple-darwin.tar.gz
cd code-view-0.3.0-aarch64-apple-darwin
./install.sh --prefix "$HOME/.local"
export PATH="$HOME/.local/bin:$PATH"
code-view --version
```

包内安装器先校验内容、平台与目标冲突，再复制到指定前缀的 `bin` 和 `share/code-view`；默认前缀也是 `~/.local`，无需 sudo。这个手动入口不编辑 shell 配置或生成快速卸载清单；需要 PATH 和卸载管理时使用上面的快速脚本。手动安装器遇到已有文件会停止，明确升级时使用 `./install.sh --prefix "$HOME/.local" --force`。升级前先在原启动器中 Ctrl-C 退出旧应用，再启动新版本。

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

`code-view` 将当前目录作为明确项目启动请求；路径参数以调用命令的目录为基准，不切换到安装目录。已有同目录 Run 时复用，默认开始新对话，不自动续最近会话。终端启动器保持前台运行；浏览器自动打开工作台，原生 CLI 负责实际输入与审批。关闭网页保留运行；在启动器终端按 Ctrl-C 结束应用及其自有 CLI。

`codex-view` 无参数仍打开全部历史首页，`code-view open <entryFile>` 可重新打开已运行应用。`code-view --help` 显示快速入口和底层参数。配置、历史及原生数据目录继续沿用现有约定，没有数据迁移；细节见[使用指南](usage.md)。

从源码生成同样的包，运行 `node scripts/dev.mjs package`；本地打包与 GitHub 正式发布的区别见[发布维护](../development/releasing.md)。
