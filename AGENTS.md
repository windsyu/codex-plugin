# Codex Local Gateway 项目执行指南

本文约束本仓库内未来的设计、开发、测试和交付会话。所有执行者在开始工作前必须阅读本文，并将其作为项目级默认工作方式。

## 1. 指令优先级

发生冲突时按以下顺序处理：

1. 当前会话中的 system、developer 和用户明确指令；
2. 本文件 `AGENTS.md`；
3. 已确认的需求与设计文档；
4. 代码中的既有模式和惯例；
5. 执行者自己的默认偏好。

不要用本文件覆盖用户在当前任务中的明确决定。发现冲突时应指出冲突及影响，再按更高优先级执行。

## 2. 项目目标与版本边界

项目目标是实现本机运行的 Codex Local Observer & Gateway：

- V1：读取、整理、搜索和展示本机 Codex 会话；
- V2：在安全、可审计的前提下控制确定的 Codex live source；
- V3：通过 Telegram、飞书、企业微信、Discord 等 IM 接入 V2 Gateway。

V1 本地 MVP 已完成其只读基础目标。当前交付重点是 **V2 Local Gateway 对话与控制能力**；V2 工作必须遵守 `docs/codex-local-gateway-v2-development-constraints.md`，同时保持 V1 只读能力兼容。

V1 的历史设计、ADR 和验证记录已经压缩到 `docs/archive/v1-development-history.md`。该归档只用于解释现有代码和兼容性，不是当前开发指令，也不得作为恢复 V1 开发范围的依据。

V2 必须继续保持 `/v1` 只读兼容和 store-first 观察能力，但控制、对话、权限、source、cwd、附件和 Slash commands 的当前决定只以 V2 核心约束为准。V3 除非用户明确授权，不得提前混入 V2 实现。

## 3. 事实与设计依据

项目文档：

- `docs/codex-local-gateway-v2-development-constraints.md`：V2 当前开发核心约束；
- `docs/archive/v1-development-history.md`：非规范性的 V1 历史归档，仅用于兼容性和追溯。

Codex 官方源码的本地参考路径：

```text
/Users/windsyu/workspace/codex
```

当前设计研究基线：

```text
41ece455b7fa7166f4fc38522952afdaa2604e18
```

使用源码事实时：

1. 先记录当前实际 commit，不假设参考仓库永远停留在上述版本；
2. 优先查公共协议、schema、测试和实现，三者一致时才视为可靠结论；
3. 明确区分“官方已确认”“源码当前实现”“项目选择”和“待验证假设”；
4. 不修改 `/Users/windsyu/workspace/codex`，除非用户单独明确要求；
5. 产品运行时不得依赖该绝对路径；schema 和 fixture 应复制或生成到本项目中；
6. 不根据 macOS App 私有行为、社区帖子或猜测建立关键正确性前提。

## 4. 工作指导原则

### 4.1 先交付可运行纵向切片

优先顺序：

```text
可启动
  → 可连接一个确定的现有 App Server source
  → 可持久化并审计一条 Gateway command
  → 可创建/继续 Thread 并完成一轮对话
  → 可实时展示 Turn 与最终答复
  → 可使用受支持的 Slash commands 和交互请求
  → 再做图片、兼容、性能和安全加固
```

每个阶段都应保持项目可运行、可演示、可回退。不要先完成所有底层抽象再第一次展示用户价值。

### 4.2 用最小方案验证假设

- 优先实现最小闭环，不以“未来可扩展”为由引入未使用的组件；
- 对不可逆或高成本决策先做 spike，并将结论记录为 ADR；
- 对低风险、可替换的细节做合理假设并继续，不进行无价值的反复追问；
- 只有会显著改变范围、数据模型、安全边界或交付结果的歧义才请求用户决策；
- 需求没有明确要求时，不擅自扩大到远程访问、多用户、云部署或 IM。

### 4.3 原始数据优先、投影可重建

- 外部数据先形成 append-only raw event，再更新投影；
- unknown method、variant 或字段必须保留，不能因 decode 失败丢弃；
- 标准化不得覆盖原始 provenance；
- checkpoint、raw event 和 projection 必须保持事务一致；
- JSONL 是 Codex durable history 的主要事实源，Observer SQLite 是自身事件日志和查询投影；
- 不直接绑定 Codex 内部 SQLite 私有表结构。

### 4.4 明确表达不完整性

无法恢复的实时 delta、approval、question 或 ephemeral 数据不能伪装为完整。API 和 UI 应区分：

```text
live_complete
live_partial
durable_complete
durable_partial
metadata_only
ephemeral_lost
```

### 4.5 安全默认值

- 只监听 loopback，除非另有经过评审的远程安全设计；
- 不提交 token、Cookie、OAuth secret、真实环境变量或用户私人会话；
- fixture 必须合成或脱敏；
- Raw JSON、Markdown、ANSI、HTML 和 SVG 默认作为不可信内容；
- 不把 cwd、diff、完整命令输出或 reasoning 自动发送到外部平台；
- destructive、高权限和外部写操作必须符合当前用户授权范围；
- `/v1` 不注册控制 Codex 的 mutation API；V2 mutation 只能位于 `/v2` 并遵守核心约束。

## 5. 会话开始流程

每次执行会话开始时：

1. 阅读本文件及当前任务相关文档；
2. 执行 `git status --short`，识别用户已有变更；
3. 查看当前分支、remote 和最近提交；
4. 不覆盖、不清理、不回退不属于当前任务的修改；
5. 用 `rg` / `rg --files` 定位代码和文档；
6. 明确本次任务属于设计、实现、诊断、评审还是发布；
7. 若是较大实现，先拆成可验证的小步骤；
8. 开始编码前确认当前任务没有越过 V1/V2/V3 边界。

如果目录尚未初始化 Git，或尚未配置 GitHub remote，应报告状态。创建 GitHub 仓库、添加远端、推送分支或创建 PR 属于外部状态变更，必须符合用户在当前任务中的授权。

## 6. 设计工作流

### 6.1 设计顺序

```text
用户价值与成功标准
  → 范围与非目标
  → 可验证事实
  → 关键决策与替代方案
  → 数据和接口契约
  → 异常、安全与恢复
  → 测试与验收
  → 实施切片
```

### 6.2 设计文档要求

- 设计必须区分事实、决定、假设和风险；
- 对关键数据流使用小型 Mermaid 图，简单内容不为画图而画图；
- API、数据库和事件模型应给出具体契约；
- 所有设计都要包含明确非目标；
- 设计变化影响已确认需求时，同时更新需求追踪；
- 文档描述必须与实际代码同步，不能长期保留已失效的未来时态。

### 6.3 ADR

对以下决策创建 `docs/decisions/NNNN-<slug>.md`：

- 技术栈或主要框架；
- 持久化模型和 migration 策略；
- 可能造成数据不可兼容的 schema 决策；
- 安全边界和认证方式；
- 与 Codex 协议强耦合的选择；
- 放弃一个已实现方向的决定。

ADR 至少包含：Context、Decision、Alternatives、Consequences、Status、Date。

## 7. 实现工作流

### 7.1 推荐切片

V2 新开发必须按 `docs/codex-local-gateway-v2-development-constraints.md` 第 12 节的纵向切片推进。只有前一个切片达到验收条件后，才把下一个切片作为主线。允许并行准备 fixture 或文档，但不要维持多个长期未集成的大分支。

### 7.2 编码要求

- 遵循项目已有 formatter、lint 和类型检查；
- 新增行为必须有相应测试；
- 错误必须携带 source、epoch、offset/sequence 等诊断上下文，但不得泄露 payload 正文；
- I/O、解析、标准化、投影和查询保持可独立测试；
- 文件通知只作为 rescan hint；
- 任何 checkpoint 推进都必须建立在数据已提交或已明确记录错误之上；
- 不使用 wall clock 作为唯一 ID 或去重键；
- 不使用真实用户的 `~/.codex` 作为自动化测试写入目标；
- 集成测试使用临时 `CODEX_HOME`。

### 7.3 避免的做法

- 不在 `/v1` 中注册消息发送、approval、interrupt 或其他控制路由；
- 不做模型 API MITM；
- 不让 Web 请求路径直接扫描 Codex store；
- 不在 adapter 内散落业务投影规则；
- 不因一个坏 JSON 行停止整个 source；
- 不静默吞掉 unknown event；
- 不为“看起来完整”而伪造实时状态；
- 不未经验证复制 Codex 内部 crate 到生产依赖。

## 8. 测试与质量门槛

### 8.1 每次变更的最低验证

- 文档变更：检查链接、标题层级、代码块和术语一致性；
- parser/adapter：unit test + fixture test；
- storage/migration：幂等 migration + crash/replay 测试；
- API：契约测试、分页/cursor、错误码；
- UI：关键状态渲染、unknown item、安全转义；
- 安全相关：负向测试必须存在；
- bug fix：先增加能复现问题的回归测试。

### 8.2 MVP Definition of Done

一个切片只有在以下条件满足时才算完成：

- 用户可见结果或内部能力符合已确认验收；
- 相关自动化测试通过；
- formatter/lint/type check 通过；
- 没有意外修改 Codex 原始数据；
- 新配置、命令或 API 已记录；
- 错误和降级状态可以诊断；
- Git diff 中没有无关文件、secret、生成垃圾或私人 fixture；
- GitHub Issue/PR 能解释为什么改、改了什么、如何验证。

测试无法运行时，必须说明原因、已执行的替代验证和剩余风险，不能只写“未测试”。

## 9. Git 与 GitHub 工作流

GitHub 是项目版本控制和协作记录的事实源。即使单人开发，也使用 Issue、分支、逻辑提交和 PR 保留决策与评审轨迹。

### 9.1 仓库规则

- 默认主分支：`main`；
- `main` 始终保持可构建、可测试；
- 不直接在 `main` 上开发较大功能；
- 不使用 `git reset --hard`、强制 checkout 或其他破坏用户工作的命令；
- 不 force-push 共享分支；
- 不提交 `.DS_Store`、secret、本地数据库、blob、真实 rollout 或构建产物；
- 仓库初始化后立即配置合适的 `.gitignore`。

### 9.2 Issue

非微小变更应对应 GitHub Issue。Issue 至少描述：

- 背景和用户价值；
- 范围与非目标；
- 验收条件；
- 安全/兼容风险；
- 依赖和阻塞；
- 对应设计章节或 ADR。

### 9.3 分支命名

```text
feat/<short-slug>
fix/<short-slug>
docs/<short-slug>
refactor/<short-slug>
test/<short-slug>
chore/<short-slug>
spike/<short-slug>
```

一个分支只承载一个可解释的目标。

### 9.4 Commit

使用 Conventional Commits：

```text
feat: import rollout session metadata
fix: preserve partial jsonl line across rescans
docs: record observer storage decision
test: cover archived rollout rename
chore: add project lint configuration
```

提交要求：

- 一个 commit 表达一个逻辑变化；
- 提交前检查 diff 和测试结果；
- 不把重构、格式化和行为变化混成难以评审的大提交；
- 不修改、压缩或重写用户已有提交，除非用户明确要求；
- commit message 说明结果，不记录执行者的思考过程。

### 9.5 Pull Request

PR 至少包含：

```text
Summary
Motivation
Scope / Non-goals
Implementation
Validation
Security / Privacy impact
Compatibility / Migration impact
Screenshots（UI 变化时）
Follow-ups
```

PR 应保持小而完整。优先提交可运行纵向切片，不提交长期无法运行的半成品。需要后续工作的内容创建 Issue，不用模糊 TODO 隐藏。

### 9.6 Remote 操作

- push、创建/修改 Issue、创建 PR、merge、release 等操作应通过 GitHub/`gh` 完成；
- 执行前确认 remote、仓库和目标分支，避免操作错误项目；
- 当前用户请求未授权远程 mutation 时，只准备本地分支/提交建议并报告下一步；
- 禁止未经明确授权 merge PR、发布 release、删除分支或修改仓库权限；
- CI 失败时先定位原因，不通过跳过测试或降低门槛伪造通过。

### 9.7 版本与发布

- pre-1.0 使用语义化版本：`v0.x.y`；
- `v0.1.0` 对应首个可演示的 V1 MVP；
- release 必须从干净、已评审、CI 通过的 `main` tag；
- release notes 列出能力、限制、兼容基线、migration 和已知问题；
- 不承诺超出 capability manifest 和测试覆盖的 Codex 版本兼容性。

## 10. 文档与代码同步

以下变化必须同步更新文档：

- `/v1` 兼容基线或只读行为变化；
- ObserverEvent、Thread、Turn、Item schema 变化；
- SQLite migration；
- REST/WS/SSE 契约；
- completeness 规则；
- Codex 兼容基线；
- 安全默认值；
- V2/V3 接口预留发生破坏性变化。

实现与详细设计不一致时：

1. 若实现修正了已证实错误，更新设计并记录理由；
2. 若实现只是临时偏离，创建 Issue 并明确技术债期限；
3. 不允许代码和文档长期各自宣称不同事实。

## 11. 会话结束与交付

每次执行会话结束时应报告：

- 完成的用户可见结果；
- 修改的文件；
- 运行的测试/检查及结果；
- 未完成项与剩余风险；
- 是否涉及 migration、配置或兼容变化；
- 当前分支、commit/PR 状态（如果适用）；
- 下一步最小可执行任务。

不要只报告“完成”。交付信息应足以让下一次会话不需要重新研究已经解决的问题。

若任务被阻塞，应先完成所有安全、只读、范围内的调查，再准确说明阻塞条件。不得用“最好再确认一下”代替可自行验证的事实。

## 12. 当前项目决策摘要

用户已经明确确认：

- 产品按 V1、V2、V3 演进；
- V1 本地 MVP 已完成基础交付，当前开发目标是 V2 Local Gateway；
- 项目使用 Git + GitHub 进行版本控制。

当前 V2 已确认的关键边界：

- V1 是已完成的只读兼容基础，历史说明只存在于非规范性归档；
- V2 只连接已存在的 App Server，不负责启动或守护进程；
- V2 复用 V1 bearer、Cookie 和 Tailscale 登录，不增加 control token；
- 所有已验证的 Tailnet 用户拥有与本机登录相同的 V2 mutation 能力；
- `/v1` 保持只读，V2 mutation 只注册在 `/v2`；
- V2 首版范围、任意 cwd、图片和 Slash commands 以核心约束为准；
- V3 IM Bridge 保持独立未来范围。

默认技术基线可以通过 Issue、设计评审和 ADR 调整。任何变化都应在同一个 PR 中更新本节和对应设计文档。
