# ADR 0041：用明确关联的原生记录补齐命令最终结果

- Status：Accepted（直接命令增量；不代表 R2 全部通过）
- Date：2026-09-19

## Context

模型请求能携带上一步工具结果，但长命令可能先返回进程 ID，模型先结束回复，命令随后才退出。若只观察后续模型请求，网页会一直停留在“执行中”。模型的 `response.completed` 既不是命令退出，也不是整个 Codex Turn 完成。

只读官方参考仓库的实际 commit 为 `633ab199cfd724aa78013c006b27a2b3d049fc3b`。`protocol/src/items.rs` 定义 `CommandExecutionItem`，`protocol/src/protocol.rs` 定义带显式 thread/turn 的 `ItemCompletedEvent`；`rollout/src/policy.rs` 持久化规范 `item_completed`，目前不持久化 `item_started` 和旧 `exec_command_begin/end`。`core/src/tools/events.rs` 与 `core/src/unified_exec/async_watcher.rs` 使用原始 call ID 和进程 ID 发布最终合并输出、退出码与耗时。

这是当前源码实现，不是跨版本稳定性承诺。安装版 CLI `0.154.0` 的临时 home/合成命令实验实际确认：最后一次模型请求之后仍会写入规范 `CommandExecution` 完成记录。实验与浏览器断言见[验证报告](../validation/native-cli-r2-native-results-2026-09-19.md)。

## Decision

1. 扩展已有独立 RolloutReader，在相同只读文件边界内读取规范命令事件。网络、PTY 和网页请求均不等待文件读取；不把文件通知或原生记录用作模型转发前置条件。
2. 只接受明确 thread、turn、native item ID，以及受测 source/status 的记录。按同一 HTTP conversation 的 thread + turn + call ID、唯一候选和已有进程 ID（若有）关联原生 `exec_command`，原位更新工具卡。原生 cwd 可补充命令目录。
3. 同一键及内容指纹重复不再发布；相异最终结果保留各自来源并标冲突，不采用最后到达者覆盖。开始事件不得把已观察最终结果降回执行中；网络中的旧运行中结果不得覆盖原生最终结果。
4. 结果来源使用带标签的 `model_request` / `native_rollout` union。原生来源只有 sourceRef、byteOffset、nativeItemId、processId，不伪造后续 request ID。未匹配的原生命令可在同轮请求详情中独立阅读。
5. 只有受测的直接命令关联进入本次范围。Code Mode 子命令不通过 ID 前缀、工具名或扫描代码绑定父调用。其他工具状态维持已有后续请求证据；缺少明确工具取消事件时不制造 cancelled。
6. 原生 `completed/failed/declined` 分别显示执行成功、执行失败、已拒绝执行。parser 对明确 `item_started/in_progress` 有封闭支持，但当前安装版 rollout 并不提供可依赖的持久开始事件；真实运行中状态主要来自原生工具结果的运行中 envelope。`turn_aborted`、EOF、无 token 均不能自动取消工具。
7. 待关联命令最多 128 条/1MiB；LiveHub 原生记录最多 128 条/4MiB。输出和 argv 预览分别最多 64KiB，argv 最多 128 项，cwd 最多 4096 字节。先脱敏再裁剪，未知/超限保留带来源位置的诊断，淘汰后要求重新快照。
8. 已知原生 byte/token/char 截断标记使预览显示不完整提示，不能改变执行状态。这些标记来自文本，也可能由命令主动打印，文案明确为“预览超限或来源含截断标记”；不承诺完整日志在工作台可取。

## Alternatives

- 只等下一次模型请求：无法处理模型已经结束、命令仍运行的情况，已由安装版实验复现。
- 从终端 ANSI 或 `write_stdin` 文本猜关联：终端没有稳定调用域，poll 的 call ID 也不等于原始启动调用；保留原生屏幕作为输入和诊断入口，不用文本猜执行事实。
- 接回 App Server 或修改官方 CLI：违反当前普通 CLI/模型代理架构，增加控制和升级耦合，本次不采用。
- 读取全部原生工具和 Code Mode 父子关系：需要逐种协议事实和实际版本验证，继续作为 R2 后续工作；不能先用 ID 命名习惯代替契约。

## Consequences

长命令可以在没有下一次模型请求时补齐原卡；真实 `write_stdin` 轮询也不会制造第二张原命令卡。网页仍能明确区分模型生成、执行证据和来源不完整性。

原生文件的读取是补充观察，存在文件迟到、被替换、超大行和缓存淘汰的边界，不提供无损保证。当前 reader 最大完整行仍为 1MiB；超大记录提示缺口，不扫描片段猜退出码。当前代码只适配受测规范记录，不绑定 Codex SQLite 私有表，不读取归档历史，不开启 Recorder 或迁移旧数据。

新 `/workbench/v1` 的未发布工具 DTO 增加原生记录和来源标签；旧 `/v1`、旧数据库、官方 CLI、全局配置和用户服务均不变。工作台服务重启恢复属于 R3，本决定不把内存结果称作已保存。
