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

当前 V2 目标是本机原生 Codex 工作台。2026-09-18 用户明确允许完全重构，要求体验与实现机理向 cc-viewer 靠拢：当前目录启动普通官方 CLI、PTY 直接交互、模型 HTTP/SSE/WS 代理、网页内容直推、后台异步记录。见工作台方案、详细设计与 ADR 0039；实施顺序、验收与状态只在 `docs/v2-implementation-plan.md` 维护。

V1 Observer、v0.2.0 Controller 与 Session Kernel Slice 1–9 是已有历史实现/验证基线，不是新架构必须复用的前提。允许替换 Worker-owned App Server、remote TUI、持久 Thread/InputLease/TurnOwner、command ledger/CAS 和 raw-first 转发。新路径不承诺旧 `/v2` 控制契约；旧数据库/audit 原样保留，`/v1` 继续只读，可由独立兼容层提供历史读取。

V3 IM 保持未来范围，既有多主体控制方案需适配新架构后再评审，不得提前混入当前工作台。官方 CLI 保持原版，不维护补丁，不自动升级。全文中的安全、测试、Git 和数据保护要求继续适用；历史 Session Kernel 规则不得覆盖本次重构决定。

## 3. 事实与设计依据

从[文档总索引](docs/README.md)选择任务相关材料，按职责使用权威来源：

- [V2 实施计划](docs/v2-implementation-plan.md)：唯一需求追踪、逐片任务、验收和状态来源；
- [核心约束](docs/codex-local-gateway-v2-development-constraints.md)、[产品方案](docs/codex-native-cli-workbench.md)与[详细设计](docs/codex-native-cli-workbench-detailed-design.md)：目标和实现契约，专题设计由总索引定位；
- [支持范围](docs/codex-native-cli-workbench-support.md)：实际受测环境、能力与限制；
- [ADR 索引](docs/decisions/README.md)：架构和兼容选择；[验证索引](docs/validation/README.md)：当时的版本、结果和限制；
- [开发指南](docs/development/README.md)：模块、工具链、测试、产物和发布；参考研究、合成原型与历史归档是非规范性依据，不能替代新运行时验收。

需要研究官方源码时，使用自行配置的只读 checkout（可用 `CODEX_SOURCE_DIR` 指向该目录）；不把维护者机器路径作为其他贡献者的前提。该变量仅用于研究命令，不是产品运行配置。

当前设计研究基线：

```text
633ab199cfd724aa78013c006b27a2b3d049fc3b
```

使用源码事实时：

1. 先记录当前实际 commit，不假设参考仓库永远停留在上述版本；
2. 优先查公共协议、schema、测试和实现，三者一致时才视为可靠结论；
3. 明确区分“官方已确认”“源码当前实现”“项目选择”和“待验证假设”；
4. 官方参考仓库只读，除非用户单独明确要求修改；
5. 产品运行时不得依赖该绝对路径；schema 和 fixture 应复制或生成到本项目中；
6. 不根据 macOS App 私有行为、社区帖子或猜测建立关键正确性前提。

## 4. 工作指导原则

### 4.1 先交付可运行纵向切片

优先顺序：

```text
在当前目录启动普通官方 CLI 与本机网页
  → 模型代理保持原生请求/认证/传输
  → 原生输入后网页在响应结束前显示中间内容
  → 证明慢存储与慢页面不阻塞 CLI，并测量时延
  → 工具/上下文阅读、异步历史和恢复
  → 文件/搜索/Git、图片/设备与迁移退役
```

每阶段保持可运行、可演示、可回退。先做有限 R0 spike，不先完成控制账本或所有底层抽象。

### 4.2 用最小方案验证假设

- 优先实现最小闭环，不以“未来可扩展”为由引入未使用的组件；
- 对不可逆或高成本决策先做 spike，并将结论记录为 ADR；
- 对低风险、可替换的细节做合理假设并继续，不进行无价值的反复追问；
- 只有会显著改变范围、数据模型、安全边界或交付结果的歧义才请求用户决策；
- 需求没有明确要求时，不擅自扩大到远程访问、多用户、云部署或 IM。

### 4.3 转发、实时观察与持久化分离

- 网络和 PTY 转发不等待持久化、投影、网页请求或 ACK；
- 有界观察副本解码并脱敏后进入内存展示，再异步记录；
- unknown method/variant 保留安全诊断，无法完整捕获时标记缺口，不丢转发字节；
- journal、snapshot 和索引保留 provenance/格式版本，索引可重建；
- 只在实际 fsync 后推进持久水位，不能以入队或网页显示当作保存成功；
- Codex rollout 继续作为原生 durable history 的来源，不直接绑定其内部 SQLite 私有表；
- 旧 Observer 的 raw/projection/checkpoint 事务约束只适用于旧兼容层，不能回流到新运行热路径。

### 4.4 明确表达不完整性

新 UI/API 分开表达实时捕获、保存和历史覆盖。保存失败、观察缺口、未归属身份、被策略省略和服务退出后丢失的尾部不能伪装为完整。模型 response completed 不代表 Codex Turn completed；模型生成工具参数不代表已执行成功。旧 completeness 枚举可在旧 API 保留，新模型不强制沿用。

### 4.5 安全默认值

- 只监听 loopback，除非另有经过评审的远程安全设计；
- 不提交 token、Cookie、OAuth secret、真实环境变量或用户私人会话；
- fixture 必须合成或脱敏；
- Raw JSON、Markdown、ANSI、HTML 和 SVG 默认作为不可信内容；
- 不把 cwd、diff、完整命令输出或 reasoning 自动发送到外部平台；
- destructive、高权限和外部写操作必须符合当前用户授权范围；
- 旧 `/v1` 保持只读；新工作台使用 `/workbench/v1`，不得悄悄改变旧控制 API 的语义。

## 5. 会话开始流程

每次执行会话开始时：

1. 阅读本文件，从文档总索引选择当前任务相关契约和开发指南；
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

只按 `docs/v2-implementation-plan.md` 的状态表与逐片通过条件推进。先证明普通 CLI + 模型代理 + 网页中间态，再扩展；前片通过才能以下片为主线，不维持多个长期未集成分支。旧 Controller、Session Kernel Slice 1–12、W0–W6、N1–N5 不再作为当前任务清单，不能从历史文字恢复未经当前方案接纳的范围。

### 7.2 编码要求

- 遵循项目已有 formatter、lint 和类型检查；
- 新增行为必须有相应测试；
- 错误必须携带 source、epoch、offset/sequence 等诊断上下文，但不得泄露 payload 正文；
- I/O、解析、标准化、投影和查询保持可独立测试；
- 文件通知只作为 rescan hint；
- 持久化 checkpoint 必须建立在数据已提交或明确缺口记录上；实时 viewSeq 是内存阅读序列，不得冒充持久化水位；
- 不使用 wall clock 作为唯一 ID 或去重键；
- 不使用真实用户的 `~/.codex` 或 `~/.codex-web` 作为自动化测试写入目标；
- 集成测试使用临时 `CODEX_HOME`，并显式注入独立临时用户主目录；真实 binary 的测试子进程须隔离其 HOME/USERPROFILE，不能只替换 CODEX_HOME 后写入真实工作台目录。

### 7.3 避免的做法

- 不在 `/v1` 中注册消息发送、approval、interrupt 或其他控制路由；
- 不做全机 TLS 劫持或开放网络代理；允许 ADR 0039 规定的本次 CLI 显式模型代理，不得为了抓包改变原生认证/权限或强制降级传输；
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
- Playwright/E2E：本机已安装 Chrome，测试应复用本机 Chrome，不额外下载 Chromium 或其他浏览器二进制；
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

一个分支只承载一个可解释的目标。自动化执行者默认使用 `codex/<short-slug>`，人工分支可采用以上主题前缀。

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

## 12. 权威决策与标准维护流程

当前架构以 [ADR 0039](docs/decisions/0039-cc-viewer-style-runtime.md)为准。原生 CLI 保持唯一对话/审批入口；记录失败不得阻断转发，不得把模型响应完成当作整轮完成，不恢复旧控制内核。CLI 版本、用量、调用详情、历史清理、独立用户目录、文件阅读、设备访问和历史首页的决定集中在 [ADR 索引](docs/decisions/README.md)；具体设计通过[文档总索引](docs/README.md)查找，阶段状态只在 [V2 实施计划](docs/v2-implementation-plan.md)维护。研究、原型与旧验收不能替代当前运行时验收。

每次工作遵循：**检查工作树和任务归属 → 判断模块边界 → 使用标准构建及产物管理入口 → 同步权威文档 → 执行验证和二进制检查 → 审查差异、许可与敏感资料 → 报告结果与未验证项。**

### 12.1 模块和文档组织

- 保留单 package、library 加两个 binary，按职责收敛接口；模块默认私有，按消费者选择 `pub(crate)` / `pub`，binary 负责启动编排。共享能力归属明确，不增加模块依赖环或跨目录实现复用。
- 大文件按职责和维护难度拆分，不设机械行数门槛。workspace 拆分须有独立复用、平台隔离或实测构建收益，先解决依赖环并形成 ADR；当前双向引用和公开面改善点见[开发指南](docs/development/README.md)。
- README 只保留定位、支持范围、最短启动与导航；用户操作放 `docs/guides/`，工程维护放 `docs/development/`，设计契约保持现有路径，ADR 与验证分别更新索引。不得复制第二套阶段状态表。
- 文档使用可移植相对链接；外部源码用仓库和固定 commit。历史现场绝对路径只能作为带时间和上下文的证据，不能作为通用操作要求。文档修改运行 `node scripts/dev.mjs docs`。

### 12.2 构建、产物和验证

- 标准入口为 `node scripts/dev.mjs`；首次 `doctor` / `bootstrap`，修改后 `build` / `check`，浏览器专项 `e2e`。开发与 CI 固定 Rust 1.95.0、Node 22.23.2，使用 `npm ci` 和 Cargo `--locked`，不宣称该 Rust 版本是已验证 MSRV。
- 先构建前端再构建 Rust，重新启动新产物；不要让内嵌资源与后端版本不一致。专项与 ignored 测试单列条件及结果，未执行不能算通过；浏览器复用系统 Chrome。
- 新受管任务使用 `target/artifacts/<UUID>` 并登记归属、类型、完成时间、结果与保留标记；成功诊断保留 7 天，失败诊断和独立构建保留 30 天。`build` / `check` 每日最多自动清理一次已登记过期任务，不安装定时任务。
- 清理默认预览，明确 `--apply` 才手动删除；活动、固定保留、状态不明、元数据异常、符号链接越界均跳过。使用互斥锁并校验仓库归属，失败报告实际删除结果。
- 主缓存超过 20 GiB 或距首次登记/最后清理 30 天只提示；显式按 Cargo profile 清理。活动构建/实例和外部共享缓存受保护，禁止无参数整体清空 `target`。
- 未登记旧文件、数据库、`observer-data`、备份、Git bundle 只盘点；不得因其位于 `target` 而删除。已提交图片/报告不参与过期清理。该流程不清理用户工作台历史或原生数据。

### 12.3 Git 与开源交付

- Git 允许真实图片与文本 SVG，禁止其他二进制文件；不得改扩展名或 Base64 编码绕过。禁止项包含可执行文件、库、字体、压缩包、数据库与 bundle；合成 fixture 也须遵守。
- 提交前执行 `node scripts/dev.mjs binaries` 检查暂存区；CI 检查提交树；历史治理执行 `binaries --all-branches` 并核实远端覆盖。没有禁止文件时不重写历史，历史改写与远端操作仍遵循当前明确授权。
- 贡献、私下安全报告、第三方声明与版本变化分别维护在 [CONTRIBUTING](CONTRIBUTING.md)、[SECURITY](SECURITY.md)、[THIRD_PARTY_NOTICES](THIRD_PARTY_NOTICES.md) 和 [CHANGELOG](CHANGELOG.md)。发布必须保留 vt100、Codicons 等分发依赖原许可与修改说明。
- CI、模板和维护工具不能扩大授权：不自动 push、创建 Issue/PR、merge、发布或修改 GitHub 设置。交付区分本地验证和远端实际运行结果。
