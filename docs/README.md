# 文档总索引

[项目首页](../README.md) · [贡献指南](../CONTRIBUTING.md) · [执行规范](../AGENTS.md)

## 使用与开发

- [安装与快速启动](guides/installation.md)：发布包校验、本地安装及 `code-view` 项目入口。
- [使用指南](guides/usage.md)：启动、终端、手机接入、历史、配置和旧 Observer。
- [支持范围与限制](codex-native-cli-workbench-support.md)：可试用环境、兼容基线与资源边界。
- [开发与维护](development/README.md)：模块、检查、构建产物生命周期、Git 文件规则和发布。
- [Release 包装与发布](development/releasing.md)：许可收集、来源与校验和、正式发布门槛。
- [合成调试与专项测试](development/debugging.md)：安装版 CLI、Chrome 与合成模型的显式验证。

## 产品计划与设计

[V2 实施计划](v2-implementation-plan.md)是 V2 唯一实施状态、需求追踪和逐片验收来源。[V3 实施计划](v3-implementation-plan.md)独立记录未来需求排序与待设计事项，不代表 V3 功能已实现，也不改变 V2 验收。下列文档按各自职责维护，不复制 V2 进度表。

| 文档 | 职责 |
| --- | --- |
| [V3 实施计划](v3-implementation-plan.md) | 先接入 Telegram 渠道，再独立设计跨会话管理；后端按需求补齐 |
| [V3 Telegram 开工检查](v3-channel-preparation.md) | 当前代码能力核对、用户流程草案、首片验收和实施前待定契约；尚未实现 |
| [V3 Telegram 接入契约草案](v3-telegram-channel-contract.md) | 单私聊绑定、来源、持久收件、必要交互和失效规则；Proposed，实验文本路径已接源码，整体未验收 |
| [V3 来源隔离探针](validation/v3-channel-origin-2026-10-09.md) | 原版 CLI、同一 Thread、原生来源与完成证据；不代表生产 Telegram 已实现 |
| [V3 实验运行接入记录](validation/v3-channel-runtime-2026-10-09.md) | 默认关闭的原生运行入口、本机绑定界面和文本收发验证；必要交互与真实 Telegram 尚未验收 |
| [V3 必要交互边界](validation/v3-channel-interactions-2026-10-09.md) | 九个安装版原生用例与实时来源门；后续生产接入另列，精确整轮取消未解决 |
| [V3 授权交互与 callback](validation/v3-channel-callbacks-2026-10-09.md) | 默认关闭的交互授权、typed answer、一次性 callback 和 receipt v3；原版 CLI 加合成操作链及剩余边界 |
| [V3 单题文字回答](validation/v3-channel-question-replies-2026-10-09.md) | 显式 Reply/Other、完整多题回答与无正文 receipt v4；本片专项及剩余验收 |
| [V3 独立排队取消](validation/v3-channel-queue-cancel-2026-10-09.md) | 提交与取消竞争、确认引用、原子 receipt v5 与慢发送隔离；精确整轮取消未完成 |
| [V3 P2 增量验收](validation/v3-channel-p2-acceptance-2026-10-10.md) | 四种文件操作、Service 失效入口与离线只读汇总；真实 Telegram 和阶段验收仍待完成 |
| [V3 TUI 有序审批专项](validation/v3-channel-tui-order-2026-10-10.md) | 实际 PTY 本机允许与渠道拒绝的两个顺序；区分菜单 Cancel、单项 Deny 与同时竞态 |
| [V3 已准入回答与 TUI 重叠专项](validation/v3-channel-tui-inflight-2026-10-10.md) | 独立渠道写背压与实际 PTY、准确工具/文件结果；不宣称双方在原生未决期间同时到达 |
| [V3 输入身份与重新绑定](validation/v3-channel-input-rebind-2026-10-10.md) | 当前提示去重修复、并发与有界输入保留；同一原版 Thread 撤销后重新绑定，不代表重启恢复 |
| [工作台方案](codex-native-cli-workbench.md) | 体验目标、架构与可靠性取舍 |
| [核心约束](codex-local-gateway-v2-development-constraints.md) | V2 边界及工程约束 |
| [详细设计](codex-native-cli-workbench-detailed-design.md) | 接入、代理、输入、保存、API、恢复与性能 |
| [历史首页与项目启动](codex-native-cli-workbench-history-home.md) | Application/Run、跨来源历史、新建与恢复 |
| [历史管理与配置](codex-native-cli-workbench-history-settings.md) | 用户历史保留和同页 JSON 设置 |
| [设备接入](codex-native-cli-workbench-device-access.md) | 显式监听、配对、撤销和设备作用域 |
| [ADR 索引](decisions/README.md) | 已记录架构决定及历史背景 |
| [验证报告索引](validation/README.md) | 每次实测的环境、证据、限制与图片 |
| [原型说明](prototypes/README.md) | 合成交互与视觉参考，不是运行时验收 |

## 研究与归档

- [cc-viewer 功能与实现](cc-viewer-function-and-implementation.md)、[实际试用](cc-viewer-hands-on-2026-09-18.md)：独立参考研究，不直接成为本项目要求。
- [V2 旧设计索引](archive/v2-before-cc-viewer.md)、[V1 开发历史](archive/v1-development-history.md)：非规范性历史，通过固定 Git 基线追溯。

新增文档应归入相应索引；通用链接使用相对路径，外部源码引用固定 commit。移动文件时同时更新入站链接、图片与中文锚点，并运行 `node scripts/dev.mjs docs`。
