# ADR 0039: 按 cc-viewer 重构为普通 CLI、模型代理与异步观察

## Status

Adopted design under user-authorized full refactor；实施进度与验收证据统一见 [V2 实施计划](../v2-implementation-plan.md)。设计被采纳不代表真实 provider 兼容和性能假设已经通过。

取代已撤回 ADR 0038 的增量实现路线，以及 ADR 0020/0022 对新运行路径的外部 App Server、持久所有权、raw-first 转发和控制审计要求。旧设计文件已按用户要求清理，必要结论和 Git 追溯见[旧 V2 历史索引](../archive/v2-before-cc-viewer.md)；删除文档不表示删除代码、数据或否认已有验证。

## Date

2026-09-18

## Context

用户在实际 ccv 试用与第一版方案后指出，复用 Worker/InputLease/VT checkpoint 等约束仍让方案受现有实现影响，并明确允许完全重构，使体验和实现机理向 cc-viewer 靠拢。

main 的 proxy 每条 envelope 同步调用 Writer；writer 收首条后最多等 50ms 凑批，提交后才释放调用者。第一版又计划网页合并通知后 GET 正文，延续了原控制平台的成本。尚无端到端性能测量，不能将源码等待窗口等同已测 p95。

cc-viewer 的主路线是目录启动、PTY 原生 CLI、模型请求拦截、临时流式内容直推、日志历史。它也有同步 blob 保存和 Claude Hooks，不是完全无阻塞或可以逐行照搬的实现。

## Decision

1. 新默认目标为当前目录 launcher + 普通官方 Codex CLI + PTY，不由项目启动外部 App Server，不再使用 `codex --remote`。
2. 在 CLI 与原配置模型服务之间建立显式本机 HTTP/SSE/WS 代理。保留官方 CLI、provider/认证、payload、重试、权限和已选传输；只为本次进程覆盖经过验证的 base URL。新路径撤销旧“不做模型 API MITM”的笼统禁令；不采用系统 TLS 劫持或全机代理。
3. 转发优先，观察副本经有界队列解码/脱敏并写内存状态，直接推送带内容的网页事件。转发、PTY 和网页首片不等待数据库或日志；大上下文可以按需读取，正文不能每次通知再 GET。
4. 记录器独立异步写每次运行 journal/snapshot，SQLite 为可重建查询索引。保存失败降级记录而不主动中止 CLI；显示持久化水位和缺口。
5. 原生 TUI 是唯一对话、Slash、审批、追问、设置入口。输入仲裁简化为本机单用户、内存单写连接和明确接管，删除持久 Thread/InputLease、冻结 TurnOwner 与逐命令 Gateway CAS/audit 的前置要求。
6. 请求诊断包含实际捕获的模型上下文、工具和 usage。工具执行结果/任务完成由实际网络或 rollout 证据补足；网络 response 结束不能伪装成 Codex Turn 完成。
7. 使用独立数据目录与 `/workbench/v1` API。旧数据库、审计和历史只读保留，不要求新运行时兼容旧 `/v2` command 协议。R5 达标后退役旧控制路径，不长期维护两套内核。
8. V3 IM、远程多用户和独立网页 Composer 不进入此轮。保留 loopback、认证、内容安全、凭证脱敏和显式退出/继续，不因为移除控制内核而取消这些边界。

## Alternatives

- **保留旧 proxy，仅取消同步入库：** 可减少延迟并获得丰富 App Server 事件，但仍要求外部 App Server/remote TUI；没有实现本轮要求的目录启动、普通 CLI 与模型请求观察路线，不作为默认。
- **两种 capture 后端长期并行：** 扩大身份、恢复和兼容矩阵，暂不采用。R0 必要 profile 失败应记录原因并修订选择，不能静默回退或在用户不知情时切传输。
- **只优化 rollout watcher：** 可用于 durable 补齐，不能保证收到模型流中间态与网络上下文。
- **从 ANSI 重建语义或注入网页输入：** 不能可靠取得调用/响应身份，也违背原生输入方向，不采用。
- **同步保存所有网络数据再转发：** 保留严格审计前提但重新引入热路径阻塞，不采用。

## Consequences

- 新内核可以大幅简化控制职责，但“更快”的具体幅度必须测量；语言或组件数量不构成性能证据。
- 已显示但未落盘的尾部可能在崩溃后丢失。观察超载也可有缺口；不能继续承诺 raw-first 完整控制审计、跨重启 command 幂等或原 attachment 冻结整轮输入权。
- 单写入口仍保护当前 PTY；同一本机可信用户可明确接管，包括原生审批期间。这不适用于未来多主体远程授权模型。
- 模型代理能看到敏感请求和认证，必须隔离转发与脱敏观察；普通本机 listener 不能成为开放代理。
- API key、自定义 provider、ChatGPT 登录、SSE/WS 及辅助路由分别验收；配置入口存在不等于所有安装版兼容。R0 先证明最小闭环，再展开重构。
- 模型侧不保证完整审批状态、工具 stdout 或 Thread/Turn 身份；保留终端及历史补充，UI 诚实表达未知。
- 本次只有设计变更，无 production migration、服务重启、CLI 修改、commit 或远程操作。

## Validation and follow-up

[V2 实施计划](../v2-implementation-plan.md)统一维护交付顺序、逐片验收和状态；[详细设计](../codex-native-cli-workbench-detailed-design.md)给出配置、转发、输入、记录、API 和性能测量契约。R0 先验证当前必要 profile，提供普通 CLI、真实中间态、配置/认证无污染和慢记录器不阻塞转发的证据；现有 ccv 试用不替代此验收。旧 Controller、Session Kernel、W0–W6 与 N1–N5 不再构成新实现待办。

2026-09-21 实施：用户确认保留模型请求/prompt 阅读后，再次授权拆除旧 Session/App Server 内核；代码、`/v2` 和旧网页控制入口已按本决策退役，V1 历史/schema/audit 保留。结果见[退役验证](../validation/native-cli-r5-kernel-retirement-2026-09-21.md)。
