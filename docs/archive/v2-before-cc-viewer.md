# cc-viewer 重构前的 V2 历史索引

> 整理日期：2026-09-18。本文只用于解释现有代码、数据与历史证据，不是当前开发计划。
> 当前开发以[工作台方案](../codex-native-cli-workbench.md)、[实施计划](../v2-implementation-plan.md)和 [ADR 0039](../decisions/0039-cc-viewer-style-runtime.md)为准。

## 1. 固定代码基线

本轮从 `main@930172493c163a903621ca14c540d1ca10dd4f3e` 出发。此提交含 V1 Observer、旧 V2 Controller 演进与官方 CLI Session Runtime；数据库 schema 为 20。本文整理和文档删除没有改变这些代码、数据库或用户运行中的服务。

旧运行链为 Worker → 专属 App Server/guard → 私有 audited proxy → `codex --remote` PTY；持久化 Thread/InputLease/TurnOwner、逐命令 audit/CAS 与 raw-first 提交构成旧控制约束。V1 通过 rollout 导入查询历史。

这些机制解释旧数据，不约束新 Run/模型代理。新方案保留旧记录的只读访问，不要求新控制协议兼容，也不将 cc-viewer 试用记成旧或新 Codex 路径通过验收。

## 2. 已有验证及其限度

下表是被移除报告的摘要，**本次没有重跑这些测试**。完整命令、案例和失败修复见固定基线中的 `docs/v2-validation.md`。

| 历史阶段 | 报告中的证据 | 不可外推的范围 |
| --- | --- | --- |
| Controller / v0.2.0 | typed command、ledger/audit、CAS、审批、图片、SSE 与重启恢复的自动化/夹具证据；生成协议基线 0.149.1 | 非当前模型代理兼容；不能恢复网页 Composer 为新计划 |
| Session Kernel / 2026-09-02–03 | 官方 0.146.1 的原生终端、交互、resize、格式/重连；部分实验连接由操作员启动的 App Server | 该外部服务连接方式后来退出生产；不是当前支持路径 |
| 单路径 Runtime / 2026-09-04 | 198 Rust、80 Web、15 系统 Chrome 测试；format/lint/type/build 通过；隔离真实 CLI 生命周期实验 | 生命周期实验无凭证，停在认证界面，不证明真实模型整轮成功；容量 2M 测试未跑 |
| 启动输入/目录选择 / 2026-09-07 | 81 Web、16 系统 Chrome 测试；类型/前端与 Rust build；Hooks 界面真实键盘验证 | 没有提交模型任务；前端改动未重跑 Rust 全套 |

旧报告还区分 fixture、真实本机 App Server、外部 Tailnet 未实测、安装版本与源码版本。研究源码 `633ab199cfd724aa78013c006b27a2b3d049fc3b`、安装版 0.146.1 与生成 schema 0.149.1 是不同证据，不能合并成版本支持承诺。cc-viewer 调研时另记录安装版 Codex 0.154.0，尚未验证新模型代理。

## 3. 移除的旧文档与 ADR

以下已纳入 Git 的文件从当前文档树删除，完整原文可从上述固定提交读取。只保留此索引，不建立第二棵旧计划目录。

| 旧路径（相对仓库） | 原作用 | 本次处理 |
| --- | --- | --- |
| `docs/codex-local-gateway-v2-detailed-design.md` | Controller 详细设计 | 由新工作台方案/详细设计替代 |
| `docs/codex-tui-session-kernel-refactor.md` | 受管 TUI 内核及 V3 总体设计 | 删除；V3 未来重新规划 |
| `docs/codex-tui-session-kernel-slices.md` | Slice 1–12 任务与状态 | 删除；新状态只在 R0–R5 计划 |
| `docs/v2-validation.md` | 多代实现混合的长篇验收报告 | 摘要见上节，全文留 Git |
| `docs/decisions/0013-v2-controller-boundary.md` | 旧控制边界 | 历史决定，不约束模型代理 |
| `docs/decisions/0014-command-ledger-and-audit.md` | 控制账本和审计 | 新 journal 不继承此控制承诺 |
| `docs/decisions/0015-v2-typed-conversation-dispatch.md` | 类型化网页发送 | 由原生 CLI 输入取代 |
| `docs/decisions/0016-capability-aware-settings-and-slash.md` | 网页设置/Slash | 交回原生 CLI |
| `docs/decisions/0017-plan-goal-and-local-control-cards.md` | Plan/Goal 控制卡 | 不在新网页重建状态机 |
| `docs/decisions/0018-pending-request-cas.md` | pending request CAS | 新网页不应答原生请求 |
| `docs/decisions/0019-gateway-restart-reconciliation.md` | 旧命令重启对账 | 新 Run 不恢复旧控制执行 |
| `docs/decisions/0020-codex-tui-session-kernel.md` | Worker/所有权内核 | 由 ADR 0039 替代 |
| `docs/decisions/0021-terminal-capability-broker.md` | 终端能力处理 | 独立安全/呈现经验可借鉴，不保留旧内核约束 |
| `docs/decisions/0022-gateway-owned-app-server-session.md` | 专属 App Server 单路径 | 由普通 CLI + 模型代理取代 |

例如，在仓库内读取删除前的原文：

```bash
git show 930172493c163a903621ca14c540d1ca10dd4f3e:docs/v2-validation.md
git show 930172493c163a903621ca14c540d1ca10dd4f3e:docs/codex-tui-session-kernel-slices.md
```

以下三个文件是本轮尚未提交的中间文档，**不在上述 Git 基线中**；其必要结论已合并后删除：

| 中间文档 | 留存内容 |
| --- | --- |
| `docs/archive/pre-cc-viewer-v2-development-constraints.md` | 重构前约束的重复副本；原始版本可从基线的 `docs/codex-local-gateway-v2-development-constraints.md` 读取 |
| `docs/decisions/0038-native-cli-live-workbench.md` | 曾计划沿用 Worker/lease/raw-first，补齐旧 proxy 投影和通知后 GET，N1–N5 未实现即被撤回；“原生 CLI 唯一输入”保留，其他冲突决定由 ADR 0039 取代 |
| `docs/cc-viewer-feature-reference.md` | 混合参考研究与本项目规格；研究进入独立 cc-viewer 说明，F01–F12/RQ01–RQ08 的范围与验收进入新实施计划 |

ADR 编号不复用；0038 表示已撤回草案。0023–0037 所属其他历史分支不在本次 main 基线中，不因会话里的历史文字而成为当前已实现功能或未完成清单。

## 4. 数据与代码退役边界

- 删除文档不删除旧 raw/audit、数据库、Codex rollout、migration 或运行程序。
- 旧 schema 20 和迁移历史继续支持既有只读数据；旧 `/v1` 保持只读。
- 当前代码仍可按 README 构建和运行。只有新实施计划 R5 验收后才退役旧控制模块，先识别仍被 V1 使用的依赖。
- 源码中的历史 ADR 编号可以继续解释老实现；新代码不能因旧 ADR 要求恢复租约/控制账本或外部 App Server。
- 需要历史事实时从固定 Git commit 查看，不将整套旧文档恢复为活动规范；需要重新采纳的决定必须写入当前设计并验证。

## 5. 2026-09-21 旧控制代码退役

按 ADR 0039、R5 和用户再次明确授权，删除 `src/session/`、`src/controller/`、旧 `src/live/`、`src/http/session_routes.rs`、旧 domain/store 的 gateway/session 写入、`web/src/session/` 与旧 Composer/控制客户端。`src/main.rs` 不再启动 guard/App Server，也不恢复旧控制账本或清理旧图片。移除两份旧 V2/Session 兼容 manifest 和 runtime 专用 fixture/测试；这些历史文件仍可从固定基线 `9301724` 读取。

V1 导入、历史查询/导出、脱敏和 projection 保留；schema 1–20 migration、原 audit 表和 V1 兼容 manifest 保留。网页和 API 的退役负向测试替代旧运行内核测试，不通过保留空模块维持旧结构。此次未操作用户数据库和服务；证据见[退役验证](../validation/native-cli-r5-kernel-retirement-2026-09-21.md)。
