# 文档总索引

[项目首页](../README.md) · [贡献指南](../CONTRIBUTING.md) · [执行规范](../AGENTS.md)

## 使用与开发

- [组件选择与 AI agent 部署入口](guides/component-deployment.md)：本机后端和中转的能力、独立构建、数据边界及分端部署顺序。
- [本机工作台部署与接入](guides/local-workbench-deployment.md)：只部署 codex-plugin 电脑后端、原生 CLI 认证、局域网及可选公网连接器。
- [安装与快速启动](guides/installation.md)：发布包校验、本地安装、后台服务及 `code-view` 项目入口。
- [使用指南](guides/usage.md)：启动、终端、手机接入、历史、配置和旧 Observer。
- [公网中继部署与运维](guides/public-relay-deployment.md)：windsyu 管理员、账号删除、单端口中央前端、schema 3 备份恢复；域名兼容与真实手机验收另列。
- [支持范围与限制](codex-native-cli-workbench-support.md)：可试用环境、兼容基线与资源边界。
- [开发与维护](development/README.md)：模块、检查、构建产物生命周期、Git 文件规则和发布。
- [Release 包装与发布](development/releasing.md)：许可收集、来源与校验和、正式发布门槛。
- [合成调试与专项测试](development/debugging.md)：安装版 CLI、Chrome 与合成模型的显式验证。

## 产品设计与技术契约

仓库保留持续维护的设计、技术契约、操作指南和支持限制。任务进度、测试输出、截图及部署现场记录放在 Issue/PR、CI 或受管产物中；旧记录可通过 Git 历史追溯。

### 两个组件的维护入口

| 组件 | 架构说明 | 交互说明 | 部署说明 |
| --- | --- | --- | --- |
| 本机工作台 `workbench` | [模块、运行与数据边界](architecture/workbench.md) | [页面、原生终端与接入流程](interactions/workbench.md) | [本机部署](guides/local-workbench-deployment.md) |
| 公网中转 `relay` | [依赖、身份、安全与 schema](architecture/relay.md) | [账号、电脑认证与固定页面 API](interactions/relay.md) | [Linux / 单 IP / 历史兼容](guides/public-relay-deployment.md) |

AI agent 先读 [AGENTS](../AGENTS.md)，再按改动归属阅读组件架构和交互；跨端协议或认证变更两组都读。下列专题提供更细契约，ADR 记录理由，支持文档明确受测范围；不把历史检查结果当成当前行为保证。

### 专题契约与依据

| 文档 | 职责 |
| --- | --- |
| [工作台方案](codex-native-cli-workbench.md) | 体验目标、架构与可靠性取舍 |
| [核心约束](codex-local-gateway-v2-development-constraints.md) | V2 边界及工程约束 |
| [详细设计](codex-native-cli-workbench-detailed-design.md) | 接入、代理、输入、保存、API、恢复与性能 |
| [历史首页与项目启动](codex-native-cli-workbench-history-home.md) | Application/Run、跨来源历史、新建与恢复 |
| [项目会话整理](codex-native-cli-workbench-session-organization.md) | 明确项目上下文、辅助/来源分层、单条可撤销归属的实现契约；不做 worktree |
| [历史管理与配置](codex-native-cli-workbench-history-settings.md) | 用户历史保留和独立设置页的 JSON 配置契约 |
| [设备接入](codex-native-cli-workbench-device-access.md) | 显式监听、配对、撤销和设备作用域 |
| [ADR 索引](decisions/README.md) | 已记录架构决定及历史背景 |
| [独立功能页面与中转断线恢复](decisions/0059-workbench-routes-and-relay-recovery.md) | 本机/中转路由、同 Run 终端保活与不重放输入；不代替部署验收 |
| [旧 Observer 兼容](architecture/observer-compatibility.md) | 旧只读 API、覆盖语义、脱敏与清理保护 |

## 参考研究

- [cc-viewer 功能与实现](cc-viewer-function-and-implementation.md)：固定源码基线的实现分析，不直接成为本项目要求。

新增文档应归入相应索引；通用链接使用相对路径，外部源码引用固定 commit。移动文件时同时更新入站链接、图片与中文锚点，并运行 `node scripts/dev.mjs docs`。
