# 安装与快速启动

[使用指南](usage.md) · [支持范围](../codex-native-cli-workbench-support.md) · [发布维护](../development/releasing.md)

`0.3.0` 发布包面向已验证的 macOS / Apple Silicon。网页和原生目录选择助手已嵌入程序，运行时不需要源码、Rust 或 Node.js。启动项目仍需自行安装并配置官方 Codex CLI；当前模型接入限 `unmanaged-custom` Responses 静态 bearer。代码搜索需要 `rg`，Git 阅读需要 `git`。

## 安装发布包

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

安装器先校验包内内容、平台与目标冲突，再复制到指定前缀的 `bin` 和 `share/code-view`；默认前缀也是 `~/.local`，无需 sudo。不会编辑 shell 配置；要在新终端中使用，把上述 PATH 行加入自己的 shell 配置。安装器遇到已有文件会停止，明确升级时使用 `./install.sh --prefix "$HOME/.local" --force`。升级前先在原启动器中 Ctrl-C 退出旧应用，再启动新版本。

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
