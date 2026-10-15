# Changelog

本文件记录面向使用者和贡献者的变化，不代替 [V2 实施计划](docs/v2-implementation-plan.md)或 GitHub Release。未发布内容不表示已经部署。

## Unreleased

- macOS 新增 `code-view start/status/open/stop/autostart`，后台运行工作台、首次电脑注册引导和完整所属项目退出；默认手动启动，登录自启可选。公网启动表单可浏览连接电脑上的目录，选择后继续校验；支持隐藏目录、筛选、分页及权限/列表不完整提示。新增 owner 目录接口与可选能力字段，无数据库迁移，见 [ADR 0063](docs/decisions/0063-background-service-directory-browser.md)。

- 本机工作台与中转各提供正式架构/交互维护入口，整合单 IP 运维到中转部署指南，删除已替代的混合契约和试部署指南；补齐裸 `codex-relayd` 忽略规则，构建二进制不进入 Git。

- 标准构建/检查增加 `--component workbench|relay|all`，分别编译电脑后端与独立中转；完整检查显式覆盖 workspace，本机安装包不再顺带编译中转。增加两端能力与部署操作指南，区分本机模型认证、中转账号、数据与运行依赖；无运行 API/schema 改动，见 [ADR 0062](docs/decisions/0062-independent-component-builds.md)。
- 本机 Run 的“手机接入”改名为“本地局域网接入”，同步导航、面板、端口设置及配对提示，明确直连本机而非公网中转站；LAN/Tailscale、二维码和权限保持不变。
- 公网中转改为默认 `windsyu` 管理员管理账号、临时密码强制改密、全端凭据撤销及电脑登录后自动连接；增加单 IP/单 HTTPS 端口的可信中央前端与多电脑页面授权。schema 2 保留登记/归属但撤销旧授权，schema 3 无损改名旧默认管理员并增加普通账号永久删除；升级前备份，禁止自动恢复旧库复活权限。旧域名模式仍显式保留。参见[契约](docs/architecture/relay.md)、[ADR 0058](docs/decisions/0058-admin-accounts-single-origin-relay.md)与 [ADR 0061](docs/decisions/0061-account-deletion-admin-rename.md)。旧双端口试用的[部署验证](docs/validation/public-relay-ip-trial-2026-10-13.md)不等于新模式或真实手机验收。
- `unmanaged-custom` 模型接入支持由官方 CLI 执行的 `auth.command`：工作台不执行命令、不持有动态 token，代理透明转发 CLI 的认证头，并保留 managed/cloud 路由来源门禁。安装版 CLI 0.162.1 与合成 provider 已覆盖首次 401 后的原生刷新；真实 provider 另验，见 [ADR 0060](docs/decisions/0060-native-provider-command-auth.md)。
- 历史任务标题优先有效原标题，否则提取真实用户请求，已确认注入上下文不抢占；列表显示来源独立的交流片段和时间，选中项目成为主标题。正文默认对话优先，技术过程按需核查，错误/缺口与搜索命中不隐藏；派生 catalog 4 只重建缓存，原始历史及归类状态不变。
- 历史首页新增独立项目搜索、稳定完整路径摘要与明确未归类原因；主要/辅助/运行记录分层，仅可信同原生身份的来源折叠展示，正文与用量仍独立。
- 支持 owner 单条记录“归入已有项目/设为未归类/恢复自动归类”；独立 organization schema 1 状态、强 ETag CAS 与故障降级，不删除或改写原始历史、不改变继续会话的原 cwd。仅重建派生 catalog，JSON 配置 schema 2 不变。
- 恢复 README 的功能、运行条件和完整使用入口；旧 `~/.local` 快速安装需先卸载再安装到新默认目录，不自动迁移用户数据。
- README 提供 macOS 用户级快速安装脚本，使用系统 curl 匿名下载/校验公开稳定 release，默认安装至 `~/.codex-view`，并配置 zsh/Bash 登录终端 PATH；提供按清单保护卸载的 `code-view-uninstall`，保留用户历史和原生数据。
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
