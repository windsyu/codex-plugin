# Changelog

本文件记录面向使用者和贡献者的变化，不代替 [V2 实施计划](docs/v2-implementation-plan.md)或 GitHub Release。未发布内容不表示已经部署。

## Unreleased

- README 提供 macOS 用户级快速安装脚本，使用已登录的 GitHub CLI 下载/校验稳定 release，并配置 zsh/Bash 登录终端 PATH；提供按清单保护卸载的 `code-view-uninstall`，保留用户历史和原生数据。
- Bash PATH 使用现有生效的登录配置文件，避免新建 `.bash_profile` 遮蔽 `.bash_login` / `.profile`。

## 0.3.0（2026-10-02）

- 提供 macOS / Apple Silicon release 安装包及 `code-view` 快速项目入口：无参数启动当前目录，也支持指定路径；历史首页使用 `code-view --history` 或原有 `codex-view`。
- 增加 `codex-view --version`、本地前缀安装器、来源 manifest、包内/归档 SHA256 和锁定依赖许可收集；运行包内嵌页面与目录选择助手，无需编译工具链。

- `codex-view` 无参数改为打开全部历史首页，阅读不启动 CLI；原先在当前目录直接进入终端的操作改用 `--project .`。网页支持明确新建/继续可恢复的原生会话、最多 4 个独立项目和逐项目停止，返回首页或关闭网页保留运行。
- 统一读取原生会话、工作台记录与登记的 schema 20/25 旧 Observer 历史；后台目录、脱敏搜索和按需正文保留来源与覆盖说明。未归属记录保持可达，继承历史暂只读不恢复；全局历史不开放跨项目删除或手机启动管理。
- 工作台 JSON schema 1 只读兼容，用户保存时备份后写 schema 2；配置和记录默认位于独立 `~/.codex-web`，原始历史不自动迁移。私有应用入口使用 v2 并继续接受旧 v1；产品运行 API 改为 `/workbench/v1/runs/{runId}/…`，无 Run 前缀的旧运行路径返回 404。
- CLI 诊断的受测版本增加 0.156.1/0.159.2；[0.159.2 的合成上游综合回归](docs/validation/native-cli-r6-g4-cli-01592-2026-09-30.md)覆盖 24 项安装版 CLI 场景。版本号仍不作为启动准入，不扩大 provider、平台或真手机支持，也不代替完整 R6 用户试用。
- 整理文档入口、开发与使用指南，明确模块边界和贡献流程。
- 增加统一维护命令、受管产物过期清理、主缓存显式清理和 Git 非图片二进制检查。
- 固定开发工具链，增加基础 CI、安全报告渠道与第三方许可索引。

早期开发与版本背景见 [V1 历史](docs/archive/v1-development-history.md)、[旧 V2 历史索引](docs/archive/v2-before-cc-viewer.md)及 Git 提交；不在此追认未经核验的发布日期。
