# macOS 快速安装、PATH 与卸载增量验证（2026-10-02）

## 范围与来源

用户追加 README 可复制安装脚本及便捷卸载命令。实现独立 Bash 辅助入口 `scripts/install-macos.sh`，使用 GitHub CLI 下载现有 release，默认用户前缀 `~/.local`，管理 zsh/Bash 登录终端 PATH，并安装离线 `code-view-uninstall`。旧 `v0.3.0` 归档及 runtime/API/schema 不变；任务跟踪为 [Issue #8](https://github.com/windsyu/codex-plugin/issues/8)。

真实资产来源为 `code-view-0.3.0-aarch64-apple-darwin.tar.gz`，SHA256 `025a651ba66ed3c46953936f158ca5929fa3c838a264c04a986074a644ec9c14`，包 manifest source 为 `81fbd76e3bfcdc9e9b5588b22b30bfc456c1e070`、`sourceDirty=false`，含 281 项许可。辅助脚本的源码 commit 与 CI 由 PR 记录提供，不把包源码 commit 当作本次新增脚本来源。

## 合成行为与故障回归

独立测试文件 `scripts/tests/install-macos.test.mjs` 使用真实 tar.gz、旧包安装器、合成 binary、模拟 gh 与独立 HOME/USERPROFILE/CODEX_HOME；不写真实用户目录，不调用私人 CLI/模型，不把真实 credential 放入 fixture。

28 项用例覆盖：

- 0.3.0 安装与 0.3.1 合成升级、指定版本/前缀、权限与卸载器、强制重装、`--no-path`；
- zsh 与 Bash 的特殊字符 PATH、区块去重、精确原文（含无末尾换行）恢复、ZDOTDIR 变化后的登记路径；
- 用户数据/未知文件保留、修改文件/管理数据拒绝、receipt 格式/穿越、祖先及末端链接、额外管理文件冲突；
- 不支持平台、认证/归档校验失败，已通过校验的越界/穿越/链接归档仍拒绝；
- 删除失败保留清单并重试，PATH 中途写失败原文件不变，旧包复制/首次管理文件复制/原子提交失败回滚；
- 下载期间的已管理文件修改、计划期间与快照期间的 shell 编辑均保留，不以 `--force` 跳过管理保护。
- README 复制命令在 Bash/zsh 下遇到临时文件创建或下载失败立即退出，不执行不完整下载；安装失败不修改父 shell PATH，各路径均清理临时脚本。
- Bash 已有 `.bash_profile` / `.bash_login` / `.profile` 的实际优先级、登录设置与 PATH、未选配置不改、卸载原文字节恢复及活动配置链接拒绝。

初次独立审查实际复现了 PATH 截断写入、升级中断后清单不一致，以及两处并发快照混用问题；均先增加失败回归，再修正为私有 staging、同目录完整临时文件与原子替换、初始清单锁内/逐文件复验、同一 shell 快照生成与比较、已提交且仍匹配新内容的文件受控回滚。最终独立审查未发现残留阻断，Bash 语法和 diff 检查通过。

`node scripts/dev.mjs test-tools`：70 项全部通过，包含上述 25 项。受管诊断任务 `4d0323b9-dcb4-4e70-ac16-f640004f07ef`。

随后最终检查 README 复制命令时，复现了条件组中的 `set -e` 不会终止失败步骤；改为显式退出并增加第 26 项回归。`node --test --test-name-pattern 'README bootstrap' scripts/tests/install-macos.test.mjs` 通过，该用例在系统 Bash 与 zsh 下分别验证下载失败、临时文件失败、安装失败和成功 4 种路径。最终远端标准入口会执行总计 71 项工具检查，以 PR workflow 为准。

README 与复制命令通过 [PR #10](https://github.com/windsyu/codex-plugin/pull/10) 合入；最终本地工具 71/71 和该 PR 的远端标准检查通过。后续真实登录配置复核复现：新增 `.bash_profile` 会让 Bash 忽略原有 `.bash_login` / `.profile`。两个新增回归先失败，再改为遵循 Bash 优先级选择已有配置；`node --test --test-name-pattern 'Bash login profile' scripts/tests/install-macos.test.mjs` 2/2 通过。已登记安装保留原管理对象，旧 helper 遮蔽配置时先卸载再重装，不自动迁移个人设置；最终工具检查增至 73 项，后续 PR/CI 提供实际结果。

## 已发布真实包

在受管任务 `65a61964-cc17-4395-b47f-8ee86bc6c6f9` 创建中文/空格独立 HOME，以已有 GitHub credential 仅在内存授权下载，不记录 token。初始隔离尝试因 macOS keychain 的 HOME 依赖未识别登录；改为独立 gh 配置目录与内存环境注入后通过，fixture 仍为合成。

实际安装清单含 489 个精确文件条目；8 类检查通过：授权下载、归档/包内校验、真实 binary `codex-view 0.3.0`、zsh PATH、Bash PATH、离线卸载、原 dotfile 字节恢复、未知文件/原生数据/工作台历史保留。卸载子进程不注入 GitHub credential。未启动实际 CLI 对话或用户工作台。

## 标准检查与边界

首轮 `node scripts/dev.mjs check --offline` 中，70 项工具、234 项 Web、docs、fmt/Clippy/Web 构建通过；Rust lib 为 342 通过、1 失败、51 ignored。失败为未修改的 `git_nested_project_and_literal_paths_staging_log_and_no_external_execution`，staged diff 只显示删除原文；精确单测独立复验通过，同模块随后 110 次检查也通过，但原因尚未确认，跟踪为 [Issue #9](https://github.com/windsyu/codex-plugin/issues/9)。保留失败任务 `2358e697-acd5-41f0-a5ef-e5ddc46ad30f`，不把该轮记作通过，不宣称已修复该问题。

停止并行真实包安装验证后，重新执行完整 `node scripts/dev.mjs check --offline`，受管任务 `f346d543-7ee0-462c-8377-472391c73ffe` 成功：工具 70、Web 234、Rust 445 项通过，Rust 56 项 ignored；docs 105、fmt、Clippy、类型检查及 Web/debug/release 构建通过。远端 CI 的实际结果由对应 PR 与 main workflow 记录提供。

仅支持已验证 macOS / Apple Silicon；Bash 使用系统 3.2，运行不需要 Node/Rust。下载/升级需 gh 和仓库访问权限，卸载离线。现有运行须先退出。默认只管理 zsh 交互与 Bash 当前生效的登录配置，`--no-path` 供其他配置使用；链接 dotfiles 写前拒绝。

修改或损坏的管理对象不自动删除。回滚发生 I/O 失败或并发编辑时只报告实际剩余，保留并发编辑；不承诺 SIGKILL/断电的持久事务恢复。没有用户数据 migration、真实安装/卸载或自动更新。
