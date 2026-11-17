# Codex View 前端设计

[文档总索引](../../README.md) · [工作台方案](../workbench.md) · [本机交互](../../interactions/workbench.md) · [中转交互](../../interactions/relay.md)

工作台以用户提供的 `index.jpg` 和 Open Design 原稿为起点，补齐已实现的远程功能后，由 Open Design 重新完成商业版布局与视觉。实际前端维护在 `web/` 和 `relay/static/`；本目录保留持续有效的设计契约与生产映射。

## 维护入口

| 材料 | 职责 |
| --- | --- |
| [布局与实现映射](workbench-ui.md) | 产品导航、连续阅读、辅助面板、可调终端、视觉 token、响应式及验收边界 |
| [远程连接设计](remote-access.md) | 公网连接、门户电脑选择、Run 配对、身份差异与异常状态 |
| [工作台详细设计](../workbench-detailed-design.md) | 运行证据、模型与工具状态、实时阅读、保存和历史契约 |
| [历史首页](../history-home.md)与[会话整理](../session-organization.md) | 项目归属、跨来源记录与显式恢复 |
| [本机架构](../../architecture/workbench.md)与[中转架构](../../architecture/relay.md) | 组件边界、生产代码和自动化验证入口 |

视觉数值维护在 [tokens.css](../../../web/src/workbench/tokens.css)和[门户样式](../../../relay/static/portal.css)。演示中的模型名、时间、Diff、账号、二维码和终端反馈不能作为产品运行事实；权限、输入与恢复遵守现行交互契约。

## 本地设计资料

根目录 `index.jpg`、本目录的 `reference/`、`opendesign/`、`prototype/`、`commercial/`、来源清单、阶段任务书及原型专用 probe 由 `.gitignore` 排除。文件可保留在本机，标准构建与产品回归不依赖它们。正式组件、样式、合成产品 fixture 和回归脚本继续由 Git 管理。

需要追溯原稿时，Open Design 项目 ID 为 `53f89600-7bba-40ee-8be8-bec1b9a01287`。在本次 macOS 环境中，应用数据位于 `~/Library/Application Support/Open Design/` 下的 `namespaces/release-stable/data/projects/<项目 ID>/`；该位置仅用于设计来源研究，不是产品配置或其他贡献者的运行前提。

## 更新与验证

修改生产布局或行为时同步上述设计与交互契约。原生 CLI 继续作为唯一输入和审批入口；保留 Application/Run 身份、远程固定授权和保存/覆盖信息。

文档检查使用 `node scripts/dev.mjs docs`，标准检查使用 `node scripts/dev.mjs check`。生产布局专项为 `node web/e2e/product-design-probe.mjs`，历史专项加 `--history-only`；它们使用系统 Chrome 与隔离合成服务，输出登记在 `target/artifacts/<UUID>`。真实 CLI、手机键盘、网络和账号场景另列条件与结果。
