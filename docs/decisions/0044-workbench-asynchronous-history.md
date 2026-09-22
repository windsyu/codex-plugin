# ADR 0044：工作台异步记录与可验证历史

- Status：Accepted；整片验收与试用状态以实施计划为准
- Date：2026-09-20

默认目录部分由 [ADR 0049](0049-workbench-user-directory.md) 更新为用户主目录下 `.codex-web/history`；下文保留原决定，记录格式与异步保存契约继续适用。

## Context

用户已验收 R2 并要求继续 R3。工作台已有独立转发、观察、原生历史 reader 和实时阅读线程，尚无磁盘历史。旧 Observer 的 raw-first 事务与控制审计不能成为新热路径依赖。保存失败必须可见，但不能停止 CLI 或阻塞实时阅读。

## Decision

1. 新目录默认为 `$CODEX_HOME/workbench-data-v1`，正式入口允许 `--data-dir` 指定独立目录。旧数据库不迁移。目录 0700、文件 0600，目录描述符固定访问根；普通文件、blob 和子目录拒绝符号链接与宽松权限。
2. 单独 Recorder 线程接收有界事件数和字节队列。LiveHub 只用 `try_send`；队列满通过独立原子标记报告，转发、PTY、页面与记录器互不等待磁盘。只记录已脱敏的公共 ViewEvent、上下文文档及 provenance，不保存原始认证、终端输入、完整 PTY、私有工具关联字段。
3. 每次启动生成新 runEpoch。Run 下的 `meta.json` 是版本化提交标记；`observations.<segment-uuid>.jsonl` 带格式版本、recordSeq、viewSeq、runEpoch 和记录摘要。recordSeq 包含非实时上下文文档，不能冒充用户输入或网页 viewSeq。大文档和分段基线使用内容寻址 blob，先同步 blob，再写引用它的日志。
4. 提交先同步日志，再原子替换并同步 meta 和父目录，成功后才发布保存水位。连续水位 `persistedThroughViewSeq` 与最近分段 `savedThroughViewSeq` 分开。正常退出给 Recorder 2 秒；超时不无限 join，最终标记在 journal fsync 后再检查退出许可，没有确认提交时不声称正常保存。已进入系统调用的 I/O 无法取消。进程异常终止时只读提交标记以内的有效记录，完整但未提交的尾部也不冒充已保存。
5. 记录失败或丢队列内容时冻结已确认连续水位。恢复使用新的 segment、明确 view/record 缺口范围和当前内存快照；不能声称缺失 delta 已恢复。内存阅读淘汰使用显式 `view_reset` 保存同一边界的快照与事件，避免恢复缓存把旧 revision 误当作新消息；旧日志仍可按更早的 viewSeq 阅读。
6. `recorder.status` 是独立 SSE 控制消息，不占 viewSeq、不入 journal。实时快照也携带状态；正常状态变化不制造待保存内容。底栏分别显示捕获、保存、历史覆盖。
7. 独立历史工作线程处理有界查询队列。按项目身份隔离列表和读取，页面请求不扫描 Codex store。SQLite `index.sqlite` 是从已提交 Run 元数据重建的列表索引；损坏或删除可以重建，不据它恢复未保存正文。`snapshot.json` 是可重建加速资料，日志和分段基线仍为验证来源。
8. 历史页面复用角色、Markdown、工具和上下文呈现，显示正常结束、未正常结束、损坏记录和保存缺口。历史与当前终端分开：切历史不重挂终端，不恢复旧 CLI、不重放任何输入，返回实时阅读保留选择。长运行可向前读取更早的保存窗口。
9. 原生 rollout reader 继续在后台按明确 thread/turn 关联，补充 `archived_sessions` 和归档移动；保存原位置 provenance，坏行及退出时半行可诊断。无法确认的跨来源关系保持独立，不按时间或文本修复历史。

## Alternatives

- 在 ViewEvent 推送前等待 SQLite：与 ADR 0039 的转发和观察隔离冲突。
- 仅保存最终快照：丢失中间来源、故障边界和更早阅读窗口，无法验证已保存的连续范围。
- 以完整 JSON 行或 `write` 成功作为保存：不能证明数据已同步，异常退出时会虚报完整性。
- 在 Web handler 内扫描原生 home：会让网页请求决定全库扫描和 I/O 生命周期。
- 使用旧控制状态恢复 CLI 或输入：用户只授权恢复阅读资料，且无法证明未确认输入可以安全重发。

## Consequences

增加独立的版本化存储与历史读取契约；它们不改变官方 CLI、旧 `/v1` 或旧数据。存储故障会产生明确缺口，原生会话继续。历史中的终态只表示最后保存的观察事实；未正常退出时无法精确统计未保存尾部。数据目录需要由当前用户独占，索引与快照不能替代日志验证。

实施与验收统一记录于 [R3](../v2-implementation-plan.md#r3异步历史保存状态与故障恢复)。
