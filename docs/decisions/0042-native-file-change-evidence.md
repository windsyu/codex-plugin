# ADR 0042：以规范 FileChange 补齐文件修改结果

- Status：Accepted（R2 文件修改增量，不代表阶段全部通过）
- Date：2026-09-19
- 契约后续变更：2026-09-20 [ADR 0043](0043-unified-reading-items.md)将工具更新统一为 item.replace，本文的证据关联与预算决定继续适用。

## Context

工作台已展示模型 apply_patch 的拟议 Diff。模型输入不能证明文件写入，后续 custom_tool_call_output 又可能只有文字错误，不能仅凭 Success/error 转换执行状态。

只读官方参考仓库实际 commit 为 `633ab199cfd724aa78013c006b27a2b3d049fc3b`。`protocol/src/items.rs` 的 FileChangeItem 有显式 id、changes、status、stdout/stderr；`core/src/tools/events.rs` 发布完成记录，`rollout/src/policy.rs` 持久化规范 item_completed。这是当前源码事实，不是跨版本保证。

安装版 CLI `0.154.0` 的合成实测确认：成功修改和执行阶段的 I/O 失败分别写入 `FileChange.status=completed/failed`，thread/turn/call 与模型请求明确对应。缺失文件的前置校验错误没有该完成记录。在修改审批中按 Esc 则中断轮次并记录 turn_aborted，没有 FileChange 终态；TUI 的取消选项不能当作规范 declined。见[验证记录](../validation/native-cli-r2-file-results-2026-09-19.md)。

## Decision

1. 沿用独立、只读且有界的 RolloutReader，接收 `event_msg/item_completed/FileChange`。缺失或未知 status 留来源位置诊断，不以 item_started、turn_aborted、EOF 或终端文本补出终态。
2. 仅同一明确 HTTP conversation 的 thread + turn + call ID、唯一候选、已生成的原生 custom apply_patch 可关联。外部 namespace、Code Mode 内嵌工具和 WS 未证明关系仍独立保留，不猜父调用。
3. 规范 completed/failed/declined 映射成功/失败/拒绝；declined 有 schema 与合成 fixture 支持，当前实际 TUI 取消路径不提供它。没有规范事实的 output 继续是 result_observed，不因文字看起来像失败就伪造工具终态。
4. 同键同内容指纹只发布一次；冲突完成记录同时保留来源并撤销确定状态。定义变化使原生类别无法证明时也撤销已推断的成败，原记录仍可核对。结果先到或迟到都更新原卡，拟议 Diff 不被执行结果替换。
5. 原生 stdout/stderr 各有 32KiB 预算，先脱敏再截断。分别显示空输出与未捕获，避免长 stdout 吞掉 stderr 的可用预算。不制造 FileChange 未提供的退出码和耗时。
6. NativeFileChange 保留来源、状态和最多 64 个路径/操作/移动目标；每路径 4096 字节，路径总量 64KiB。旧内容、实际 unified_diff、auto_approved 等不复制到此 DTO；路径不被解析、打开或执行。它是原生报告的修改范围，不是当前工作区的 Git 差异。
7. 待关联文件事件最多 128 条/1MiB，LiveHub 最多 128 条/4MiB。原生源行仍限 1MiB。未知文件变体显式为 unknown/omitted；超限或来源变化沿用缺口诊断和重新快照机制。网络、PTY 不等待原生记录或页面读取。
8. snapshot 增加 nativeFileChanges，SSE 增加 native.file_change；原卡另用 tool.replace 更新，ToolResultView 增加可选 streams。未匹配记录在同轮“网络”详情独立阅读，来源不伪造 request ID。

## Alternatives

- 从拟议 Diff、模型完成或 output 中的 Success/error 推断：无法区分校验失败、执行失败和未执行，且文字可能被工具任意打印。
- 把 Esc/turn_aborted 直接映射成每个工具取消或拒绝：安装版实测只证明轮次中断，无法证明逐工具状态。
- 用原生 changes 作为实时工作区 Diff：记录描述当次工具范围，失败可能包含未执行部分，后续工作区也可能再次变化。工作区阅读留在 R4。

## Consequences

原生成功与执行失败现在可在原卡明确标识，stdout/stderr 分区，校验错误仍可阅读。证据缺失与冲突不会伪装成明确终态。

适配受版本限制；审批取消不保证逐工具终态，declined 的其他真实来源仍待支持矩阵验证。没有磁盘 Recorder、CLI 修改、全局配置、旧 `/v1` 或数据库 migration；服务重启恢复仍属于 R3。
